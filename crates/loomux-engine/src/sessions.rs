//! The pure discovery core of `src-tauri/src/sessions.rs` (#888 slice A4,
//! batch 14) — locating a CLI's session store, reading one session's record out
//! of it, and resolving a session's own recorded working directory by id.
//!
//! Claude Code:    `~/.claude/projects/<encoded-path>/<uuid>.jsonl`
//! Copilot CLI:    `~/.copilot/session-state/<uuid>/workspace.yaml`
//! pi:             `~/.pi/agent/sessions/--<encoded-path>--/<ts>_<uuid>.jsonl`
//! codex:          `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<uuid>.jsonl`
//!
//! Everything here is best-effort in the same sense the whole scanner is:
//! unreadable or malformed entries are skipped, and a missing tool simply
//! yields nothing.
//!
//! **This is an item lift, not a file split.** `src-tauri/src/sessions.rs`
//! keeps its `#[tauri::command]`s (`list_sessions`,
//! `record_{claude,copilot}_launch_posture`), the launch-intent posture store
//! behind them, the `session-index.json` cache, the candidate machinery and the
//! opencode scanner — all of which reach `crate::uistate`, `crate::opencodedb`,
//! `crate::blocking` or Tauri itself. What crossed is the half none of that
//! touches: `std` + `serde_json` + `dirs`, plus [`crate::groupid::GroupId`] and
//! [`crate::pathseg::PathSegment`], both already on this side.
//!
//! The staying half calls back in through a curated item-list re-export
//! (#988) in `src-tauri/src/sessions.rs`, which copies each item's old
//! visibility keyword rather than publishing a module — see that file's
//! re-export block for the table.

use crate::groupid::GroupId;
use crate::pathseg::PathSegment;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Detect loomux orchestration signatures in a transcript message. Kickoffs
/// name the role and group; `[orrerix]` notices (worker reports, exit
/// notices, board edits) are only ever typed into orchestrator panes.
///
/// **This is the one group id in the codebase whose source is agent-writable**
/// (#904). A transcript is a file the agent CLI writes from the agent's own
/// conversation, so any text an agent emits can contain the kickoff phrase and
/// choose what follows it. The scraped id then travels: `Parsed.orch_gid` →
/// persisted `IndexEntry` → `SessionInfo.orch_group` → the frontend → back in
/// as `resume_orch_session(group_hint)`, which joins it onto the orchestration
/// root. The `take_while` charset below was the *only* thing standing between
/// that loop and a path traversal, and it is not a specification — so the
/// result is now run through [`GroupId::parse`], the one place that decides
/// what a group id may be. A scrape that fails it reads as "no group", exactly
/// like a kickoff phrase with nothing after it.
#[doc(hidden)] // pub for integration tests
pub fn detect_orch_signature(text: &str) -> Option<(&'static str, Option<String>)> {
    // BOTH product names, and this is the one dual-accept in #1153 that can
    // never be retired (phase 3). Everything else the rename touches is a
    // string we will write again on the next launch; this one is scraped out
    // of a transcript an agent CLI wrote in the PAST and will never rewrite.
    // Drop the legacy spelling and every session recorded before the flag day
    // silently loses its orchestration identity — no error, no red, just a
    // user's group that stops offering to resume.
    for (before, after, role) in [
        ("the orchestrator of ", " agent group ", "orchestrator"),
        (" worker agent in ", " group ", "worker"),
        (" reviewer agent in ", " group ", "reviewer"),
    ] {
        for name in [crate::brand::NAME, crate::brand::LEGACY_NAME] {
            let phrase = format!("{before}{name}{after}");
            if let Some(i) = text.find(&phrase) {
                let gid: String = text[i + phrase.len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                    .collect();
                return Some((role, GroupId::parse(&gid).ok().map(|g| g.into_string())));
            }
        }
    }
    // A marker followed by a space, case-sensitively — deliberately stricter
    // than `brand::leading_notice_marker`, and unchanged from what this line
    // did before the rename. The SET of markers is still written down once;
    // only how tightly this caller matches them is its own business.
    let head = text.trim_start();
    if crate::brand::NOTICE_MARKERS
        .iter()
        .any(|m| head.strip_prefix(*m).is_some_and(|rest| rest.starts_with(' ')))
    {
        return Some(("orchestrator", None));
    }
    None
}

fn mtime_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Extract plain text from a message `content` field, which is either a
/// string or an array of {type:"text"} blocks.
///
/// Read by BOTH transcript scanners (#2126): claude and pi disagree about every
/// other part of their line format, and agree exactly here — pi's `TextContent`
/// is `{ type: "text"; text: string }` and its user content is
/// `string | (TextContent | ImageContent)[]` (`DOCS` `session-format.md`), which
/// is this function's domain unchanged. An image block yields `None` on both,
/// so a picture-only turn is not mistaken for a title.
fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => blocks.iter().find_map(|b| {
            (b.get("type")?.as_str()? == "text")
                .then(|| b.get("text")?.as_str().map(str::to_string))
                .flatten()
        }),
        _ => None,
    }
}

/// `pub`: `parse_candidate` and `scan_opencode` stayed in `src-tauri` and both
/// title their rows with this, so the crate boundary is what forces the keyword
/// — there is no narrower visibility that still reaches them.
pub fn tidy_title(raw: &str, limit: usize) -> String {
    let line = raw.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut t: String = line.trim().chars().take(limit).collect();
    if line.trim().chars().count() > limit {
        t.push('…');
    }
    t
}

/// Pull title/cwd/orchestration-identity out of a session jsonl by scanning
/// its head. Summary lines and the first real (non-meta, non-command) user
/// prompt are the best title candidates; loomux kickoff/notice signatures
/// in any early user message identify orchestration sessions.
///
/// `pub` for the same reason as [`tidy_title`]: `parse_candidate` stayed
/// behind and this is how it reads a claude row.
pub fn scan_claude_jsonl(path: &Path) -> (String, String, Option<(String, Option<String>)>) {
    let mut title = String::new();
    let mut summary = String::new();
    let mut cwd = String::new();
    let mut orch: Option<(String, Option<String>)> = None;

    let Ok(file) = fs::File::open(path) else {
        return (title, cwd, orch);
    };
    let reader = BufReader::new(file);

    for line in reader.lines().take(60).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                cwd = c.to_string();
            }
        }
        match v.get("type").and_then(Value::as_str) {
            Some("summary") => {
                if let Some(s) = v.get("summary").and_then(Value::as_str) {
                    summary = s.to_string();
                }
            }
            Some("user") => {
                let Some(text) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(content_text)
                else {
                    continue;
                };
                // A precise kickoff match (role + group) beats a bare
                // [orrerix]-notice match (role only).
                if orch.as_ref().map_or(true, |(_, g)| g.is_none()) {
                    if let Some((role, gid)) = detect_orch_signature(&text) {
                        if orch.is_none() || gid.is_some() {
                            orch = Some((role.to_string(), gid));
                        }
                    }
                }
                let is_meta = v.get("isMeta").and_then(Value::as_bool).unwrap_or(false);
                if is_meta || !title.is_empty() {
                    continue;
                }
                let trimmed = text.trim();
                // Skip injected command/caveat wrappers.
                if !trimmed.is_empty() && !trimmed.starts_with('<') {
                    title = tidy_title(trimmed, 90);
                }
            }
            _ => {}
        }
    }

    if title.is_empty() {
        title = if summary.is_empty() {
            "(no prompt)".to_string()
        } else {
            tidy_title(&summary, 90)
        };
    }
    (title, cwd, orch)
}

/// Minimal single-level YAML field lookup — enough for workspace.yaml
/// without pulling in a YAML dependency.
///
/// `orchestration::digest` reuses this to read a Copilot session's title out of
/// `workspace.yaml` rather than re-deriving the same lookup (#250/#324 slice
/// B); it stayed in `src-tauri`, so the keyword is `pub` here and the
/// `pub(crate)` reach it had is restored by the re-export's own keyword.
pub fn yaml_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    text.lines().find_map(|l| {
        l.strip_prefix(&prefix)
            .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
    })
}

/// Root of copilot's per-session state, honoring `COPILOT_HOME` so tests and
/// the orchestration spawn/trust paths agree on where copilot keeps its
/// files. `COPILOT_HOME` names the `.copilot` directory itself (matching
/// `pre_trust_copilot_folder`), under which sessions live in `session-state`.
pub fn copilot_session_state_root() -> Option<PathBuf> {
    // Test seam wins over COPILOT_HOME (see `set_copilot_session_state_root_for_test`).
    if let Some(r) = COPILOT_SESSION_STATE_ROOT_OVERRIDE.with(|c| c.borrow().clone()) {
        return Some(r);
    }
    std::env::var("COPILOT_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".copilot")))
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.join("session-state"))
}

/// **The single declared assembly point for a copilot session directory**
/// (#925), and the only place a session id is joined onto the session-state
/// root. `group_dir_at`'s counterpart for this family, and it takes a
/// [`PathSegment`] for the same reason: holding one is proof the id names
/// exactly one child of `root`.
///
/// # What it replaced, and why a predicate was not enough
///
/// This join used to be a bare `root.join(session_id)` guarded, at a distance,
/// by `digest::is_safe_session_id` — a predicate that rejected `/`, `\`, `.`
/// and `..` and nothing else. `"C:"` passed it, and on Windows `Path::join`
/// **replaces** the receiver when the argument carries a `Prefix` component, so
/// the "session directory" became `C:` and every read below it resolved
/// drive-relative to the process's own current directory, outside the
/// session-state root entirely. No separator was needed, which is exactly the
/// class of hole an enumerated blocklist keeps leaving open and an alphabet
/// closes by construction.
///
/// The refusal now lives in the type, so it cannot be reached from here at all.
pub fn copilot_session_dir_at(root: &Path, session: &PathSegment) -> PathBuf {
    root.join(session.as_str())
}

/// One copilot session read from its `session-state/<dir>/workspace.yaml`.
///
/// `pub` because [`read_copilot_session`] returns it and that function had to
/// widen (below). Its field table is the narrowest one that compiles:
/// `parse_candidate` — which stayed in `src-tauri` — reads `id`, `title` and
/// `cwd`, so those three are `pub`; `modified_ms` has no consumer outside this
/// module and stays private, which also means nothing outside can construct
/// one.
pub struct CopilotSession {
    pub id: String,
    pub title: String,
    pub cwd: String,
    modified_ms: u64,
}

/// Parse a single session directory. `None` when `workspace.yaml` is missing
/// (session not yet written) or carries no `id`.
///
/// `pub`: `parse_candidate` stayed in `src-tauri` and this is its copilot arm.
pub fn read_copilot_session(dir: &Path) -> Option<CopilotSession> {
    let ws = dir.join("workspace.yaml");
    let text = fs::read_to_string(&ws).ok()?;
    let id = yaml_field(&text, "id")?;
    let title = yaml_field(&text, "name")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Copilot session".to_string());
    let cwd = yaml_field(&text, "cwd").unwrap_or_default();
    Some(CopilotSession { id, title, cwd, modified_ms: mtime_ms(&ws) })
}

/// Path comparison for a CLI session store's recorded working directory:
/// Windows is case- and slash-insensitive, and a trailing separator must not
/// matter.
///
/// opencode needs the identical rule and must not carry a second copy of it
/// (#722 slice C): its `session.directory` column is written with forward
/// slashes where a pane's cwd arrives with backslashes, which is exactly the
/// mismatch this normalizes — the same shape as copilot's `workspace.yaml`
/// `cwd`, so it is the same function, not a sibling that could drift. That
/// caller (`opencodedb.rs`) stayed in `src-tauri`, so the keyword is `pub`
/// here and its `pub(crate)` reach is restored by the re-export's own keyword.
pub fn norm_path(s: &str) -> String {
    s.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Session ids currently present under `root` — the baseline snapshot taken
/// before spawning a copilot agent, so the session it later creates can be
/// told apart from pre-existing ones.
pub fn copilot_session_ids(root: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Ok(entries) = fs::read_dir(root) else {
        return ids;
    };
    for entry in entries.flatten() {
        if let Some(s) = read_copilot_session(&entry.path()) {
            ids.insert(s.id);
        }
    }
    ids
}

/// Every session id claude's store holds, in ONE pass over the projects root
/// (#1592).
///
/// [`find_claude_session_cwd`] answers "is THIS id here?" with a filename probe
/// per project directory, which is cheap once and quadratic when a caller asks
/// it per group: a listing over N groups re-enumerated the same P project
/// directories N times, and a MISS — the stale group, which is exactly the
/// common case in a long history — pays all P probes. This pays P directory
/// listings once, so the same listing is O(store + groups) instead of
/// O(store × groups).
///
/// **Membership only, deliberately.** The per-id lookup returns the session's
/// recorded `cwd` because a RESUME needs somewhere to launch; a LISTING only
/// needs to know the store has the id at all (`Ok(Some(_))`), and reading each
/// session's head to recover a cwd nobody asked for would put back the cost
/// this removes. A caller that needs the cwd still calls `find_session_cwd`.
///
/// The id is the file STEM, matching the layout `find_claude_session_cwd`
/// joins (`<project>/<id>.jsonl`), so the two halves cannot disagree about
/// which files name a session.
pub fn claude_session_ids(root: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Ok(projects) = fs::read_dir(root) else {
        return ids;
    };
    for project in projects.flatten() {
        let Ok(files) = fs::read_dir(project.path()) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            // `Path::is_file()`, not merely "has the extension", and not
            // `DirEntry::file_type()` either (#1592 review N4 and its round-2
            // correction). The per-group probe this replaces admits a candidate
            // on `candidate.is_file()`, so this has to ask the IDENTICAL
            // question or the two halves disagree — which they can do in both
            // directions:
            //
            //  - extension alone: a DIRECTORY named `<id>.jsonl` is a member
            //    here and a miss there, a false `resumable: true` on a row
            //    whose Resume the backend then refuses;
            //  - `DirEntry::file_type()`: it reports the entry WITHOUT
            //    following symlinks, so a symlinked session file is a miss
            //    here and a hit there — a false `resumable: false`, which is
            //    worse, because Resume just silently is not offered and
            //    nothing says why.
            //
            // `Path::is_file()` follows symlinks, so it is the one spelling
            // that matches. This costs a `stat` per candidate file rather than
            // reusing the directory entry's cached type; that is the price of
            // the claim below being true rather than nearly true, and it is
            // paid once per store per listing rather than once per group.
            if !path.is_file() {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                ids.insert(stem.to_string());
            }
        }
    }
    ids
}

/// The copilot session most likely created by a just-spawned pane: one absent
/// from `baseline`, preferring a session whose recorded cwd matches `cwd`
/// (disambiguating agents spawned concurrently in different worktrees),
/// newest by mtime. `None` until copilot has written a new session's
/// `workspace.yaml` — the caller polls.
pub fn newest_new_copilot_session(
    root: &Path,
    baseline: &HashSet<String>,
    cwd: &str,
) -> Option<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return None;
    };
    let fresh: Vec<CopilotSession> = entries
        .flatten()
        .filter_map(|e| read_copilot_session(&e.path()))
        .filter(|s| !baseline.contains(&s.id))
        .collect();
    let want = norm_path(cwd);
    let cwd_matches = |s: &&CopilotSession| !want.is_empty() && norm_path(&s.cwd) == want;
    // Prefer a cwd match; fall back to the newest fresh session when copilot
    // hasn't recorded a matching cwd yet.
    fresh
        .iter()
        .filter(cwd_matches)
        .max_by_key(|s| s.modified_ms)
        .or_else(|| fresh.iter().max_by_key(|s| s.modified_ms))
        .map(|s| s.id.clone())
}

/// Locate a session's own recorded cwd by scanning a CLI's session store
/// directly BY ID, rather than trusting a caller's (possibly stale) cached
/// copy (#412). This is the fix for "restore says the session can't be
/// found, but it's plainly in the CLI's own history": Claude Code's
/// `--resume <id>` only searches the LAUNCH cwd's project directory and its
/// live git worktrees (per the CLI reference — "passing a session ID
/// searches only the current project directory and its git worktrees"), so
/// a worktree that moved or was deleted since the session ran makes the old
/// cwd wrong, and `list_sessions`'s full-store scan (which finds the
/// session fine, from ANY cwd) and a scoped `--resume` disagree. Reading the
/// cwd back out of the session's OWN record sidesteps both a stale worktree
/// path and any casing/separator drift between what loomux cached and what
/// the CLI itself wrote, by handing the CLI back the exact string it wrote
/// for itself.
///
/// This is the best available signal, not a guarantee of resolvability
/// (#412 review N2): what `--resume` actually searches is the session's
/// CONTAINING project directory, which is not always simply "the munged form
/// of this `cwd` field" — a session under Claude's own (unrelated to git)
/// `.claude/worktrees/<name>` feature records that inner path as its `cwd`
/// while still being stored under the PARENT project's directory. Verified
/// against a real store: 2 of 691 sessions on the machine this was checked
/// against are such a case. Resuming one of those from the returned `cwd`
/// can still fail inside the CLI, honestly (the returned directory exists,
/// so no tag catches it) — a known, low-frequency gap, not silently assumed
/// away.
///
/// `Ok(Some(cwd))` — the store has this session and it recorded a cwd.
/// `Ok(None)` — the id isn't in this CLI's store at all (never existed here,
/// or was cleared). `Err` only when the store's root exists but can't be
/// listed — a real I/O problem, distinguishable from "nothing recorded yet".
///
/// Bounded and cheap either way, but not identically cheap per CLI (#412
/// review N6): claude is a filename check per entry, no file read except the
/// one match; copilot has no filename-is-the-id shortcut (see
/// `find_copilot_session_cwd`), so a miss costs one `workspace.yaml` parse
/// per session directory. Still one directory listing plus a bounded number
/// of small reads — never a scan of every session's full content the way
/// `list_sessions` pays for its title/role extraction.
/// **The parse happens here, once, for both halves (#925).** The claude half
/// interpolates the id into a file name (`{session_id}.jsonl`) and joins it onto
/// each project directory, so an id that is not a single path component walks
/// that join out of the projects root. Refusing reads as `Ok(None)` — the same
/// answer this function already gives for an id no store has heard of, which is
/// exactly what an unusable id is.
///
/// This site was found by `no_raw_identifier_is_interpolated_into_a_file_name`
/// rather than by reading the code, which is the argument for having that scan:
/// it is one `format!` in a different module from the digest arm everyone was
/// looking at.
pub fn find_session_cwd(source: &str, session_id: &str) -> Result<Option<String>, String> {
    let Ok(session) = PathSegment::parse(session_id) else {
        return Ok(None);
    };
    if source == "copilot" {
        return match copilot_session_state_root() {
            Some(root) => find_copilot_session_cwd(&root, &session),
            None => Ok(None),
        };
    }
    // #2126 P2. Named BEFORE the claude fallback, and that ordering is the
    // whole defect this arm fixes: the dispatch used to be "copilot, else
    // claude", so a `pi` id — a UUID, indistinguishable from a claude one by
    // shape — was probed for in `~/.claude/projects`, where it can only ever
    // miss. A miss reads as `Ok(None)`, i.e. "no such session", so the wrong
    // store was consulted with nothing red to say so.
    if source == "pi" {
        return match pi_sessions_root() {
            Some(root) => find_pi_session_cwd(&root, &session),
            None => Ok(None),
        };
    }
    // #2515 C2, and named BEFORE the claude fallback for exactly the reason
    // the pi arm above is: a codex THREAD id is a UUID, indistinguishable by
    // shape from a claude session id, so without this arm one would be probed
    // for in `~/.claude/projects` -- where it can only ever miss, and a miss
    // reads as `Ok(None)`, i.e. "no such session", with nothing red to say the
    // wrong store was consulted.
    if source == "codex" {
        return match codex_sessions_root() {
            Some(root) => find_codex_session_cwd(&root, &session),
            None => Ok(None),
        };
    }
    match claude_projects_root() {
        Some(root) => find_claude_session_cwd(&root, &session),
        None => Ok(None),
    }
}

thread_local! {
    /// Test seam for `claude_projects_root()` (#412 review B2). Scoped to the
    /// CALLING THREAD ONLY: Rust's default test harness runs each `#[test]`
    /// on its own OS thread, so — unlike a process-wide env var (what this
    /// replaced; `std::env::set_var` racing a concurrent reader in another
    /// test's thread is real, unsynchronized-mutation undefined behavior, not
    /// just a style concern) — a value set here can never leak into a
    /// concurrently-running test. `None` (the default) means "use the real
    /// `~/.claude/projects`".
    static CLAUDE_PROJECTS_ROOT_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam: fixture the directory `find_session_cwd`'s claude half
/// scans, for the calling thread only. Not `#[cfg(test)]` — integration tests
/// (`tests/orchestration.rs`) link this crate as an ordinary dependency,
/// where `cfg(test)` is never active, so the hook has to be a real (if
/// `#[doc(hidden)]`) function to be reachable from there.
#[doc(hidden)] // pub for integration tests
pub fn set_claude_projects_root_for_test(root: Option<PathBuf>) {
    CLAUDE_PROJECTS_ROOT_OVERRIDE.with(|c| *c.borrow_mut() = root);
}

/// `pub`: `collect_claude_candidates` stayed in `src-tauri` and routes through
/// this same testable root lookup rather than a second, untestable
/// `dirs::home_dir()` inline (#457) — so the crate boundary is what forces the
/// keyword on a function that was bare module-private before.
pub fn claude_projects_root() -> Option<PathBuf> {
    if let Some(r) = CLAUDE_PROJECTS_ROOT_OVERRIDE.with(|c| c.borrow().clone()) {
        return Some(r);
    }
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// `find_session_cwd`'s claude half, taking the projects root explicitly so
/// it's testable against a temp directory instead of the real `~/.claude`.
///
/// `Ok(Some(""))` — deliberately distinct from `Ok(None)` — when the session
/// FILE exists but its first ≤60 lines carry no `cwd` (0 of 691 real sessions
/// on the machine this was verified against, but a lie either way: the
/// session is NOT "not found", it's found with an unknown workspace, and the
/// caller (`resolve_resume_cwd`) already has a tag for exactly that —
/// `resume-workspace-missing`, since an empty string is never a real
/// directory — rather than the misleading `resume-not-found`.
fn find_claude_session_cwd(root: &Path, session_id: &PathSegment) -> Result<Option<String>, String> {
    if !root.exists() {
        return Ok(None); // no ~/.claude/projects yet — nothing recorded, not an error
    }
    let entries = fs::read_dir(root).map_err(|e| format!("cannot read {}: {e}", root.display()))?;
    for project in entries.flatten() {
        // Takes a `PathSegment` (#925): this `format!` becomes a file name that
        // is then joined onto a directory this process did not choose.
        let candidate = project.path().join(format!("{session_id}.jsonl"));
        if candidate.is_file() {
            let (_, cwd, _) = scan_claude_jsonl(&candidate);
            return Ok(Some(cwd));
        }
    }
    Ok(None)
}

thread_local! {
    /// Test seam for `copilot_session_state_root()` (#412 review B2), same
    /// contract and same thread-scoping rationale as
    /// `CLAUDE_PROJECTS_ROOT_OVERRIDE` above. Checked BEFORE `COPILOT_HOME`:
    /// unlike the claude seam, `COPILOT_HOME` is a genuine (pre-existing,
    /// non-test) production override, so a test using this hook must still
    /// win over a `COPILOT_HOME` the developer happens to have set locally.
    static COPILOT_SESSION_STATE_ROOT_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam: fixture the directory `copilot_session_state_root()`
/// returns, for the calling thread only. See
/// `set_claude_projects_root_for_test` for why this exists instead of an env
/// var.
#[doc(hidden)] // pub for integration tests
pub fn set_copilot_session_state_root_for_test(root: Option<PathBuf>) {
    COPILOT_SESSION_STATE_ROOT_OVERRIDE.with(|c| *c.borrow_mut() = root);
}

// ---------- pi (#2126 P2) ----------
//
// pi keeps one JSONL file per session under a per-cwd directory whose name is
// the working directory with every path separator and colon replaced by `-`,
// wrapped in `--`:
//
//     ~/.pi/agent/sessions/--C-Projects-loomux--/<timestamp>_<uuid>.jsonl
//
// So the layout is claude-shaped (a file per session, one directory level
// down) with one difference that matters here: the file NAME is not the id, it
// is `<timestamp>_<uuid>`. Everything below therefore matches on the `_<id>`
// SUFFIX rather than on the stem, and never on a prefix — see
// `find_pi_session_cwd`.
//
// Facts pinned against `earendil-works/pi@b79e4cc8` (= tag `v0.84.4`),
// `packages/coding-agent/docs/session-format.md` §File Location and its
// SessionHeader / SessionMessageEntry shapes; quoted in `doc/design/pi.md`.

thread_local! {
    /// Test seam for `pi_sessions_root()`, same thread-scoping rationale as
    /// `CLAUDE_PROJECTS_ROOT_OVERRIDE`. Checked BEFORE the environment, for the
    /// same reason the copilot seam is: `PI_CODING_AGENT_SESSION_DIR` is a real
    /// production override a developer may have set, and a test must still win
    /// over it.
    static PI_SESSIONS_ROOT_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam: fixture the directory `find_session_cwd`'s pi half scans,
/// for the calling thread only. See `set_claude_projects_root_for_test` for why
/// this is a real `pub` function rather than `#[cfg(test)]`.
#[doc(hidden)] // pub for integration tests
pub fn set_pi_sessions_root_for_test(root: Option<PathBuf>) {
    PI_SESSIONS_ROOT_OVERRIDE.with(|c| *c.borrow_mut() = root);
}

/// The sessions root a `pi` launched from THIS process's environment would
/// write to, as a pure function of the three inputs the vendor's own resolution
/// reads — so every branch is testable without `std::env::set_var`, which is
/// unsynchronized mutation racing every other test thread in the binary (the
/// same argument `opencode_store_from` makes).
///
/// `PI_CODING_AGENT_SESSION_DIR` names the sessions directory itself and wins;
/// otherwise sessions live under `<PI_CODING_AGENT_DIR or ~/.pi/agent>/sessions`
/// (`DOCS` `environment-variables.md`, `session-format.md` §File Location).
///
/// **The `sessionDir` SETTINGS key is deliberately not read**, and that is a
/// disclosed residual rather than an oversight: pi resolves `--session-dir` >
/// `PI_CODING_AGENT_SESSION_DIR` > settings `sessionDir` (`DOCS`
/// `settings.md` §Sessions), and reading the third would mean parsing
/// `~/.pi/agent/settings.json` — a second vendor file whose schema loomux would
/// then be pinned to. A human who has moved their store that way sees no pi
/// rows in the Sessions tab; they see no WRONG rows, which is the failure mode
/// that matters. `doc/design/pi.md` records it.
#[doc(hidden)] // pub for integration tests
pub fn pi_sessions_root_from(
    env_session_dir: Option<&str>,
    env_agent_dir: Option<&str>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let named = |v: Option<&str>| v.map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from);
    if let Some(dir) = named(env_session_dir) {
        return Some(dir);
    }
    let agent = match named(env_agent_dir) {
        Some(d) => d,
        None => home?.join(".pi").join("agent"),
    };
    Some(agent.join("sessions"))
}

/// `pub`: `collect_pi_candidates` stayed in `src-tauri` and routes through this
/// same testable root lookup rather than a second, untestable `dirs::home_dir()`
/// inline — the gap #457 closed for claude, not reopened for pi. It also routes
/// through [`walk_pi_session_files`], so the two consumers of a pi store cannot
/// disagree about where its files are — the drift review round 1 found.
pub fn pi_sessions_root() -> Option<PathBuf> {
    if let Some(r) = PI_SESSIONS_ROOT_OVERRIDE.with(|c| c.borrow().clone()) {
        return Some(r);
    }
    pi_sessions_root_from(
        std::env::var("PI_CODING_AGENT_SESSION_DIR").ok().as_deref(),
        std::env::var("PI_CODING_AGENT_DIR").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

/// The file-name suffix that names one pi session: `_<id>.jsonl`.
///
/// **Suffix, never prefix, and the leading `_` is load-bearing.** pi's file name
/// is `<timestamp>_<uuid>`, so the id sits at the end; matching `<id>.jsonl`
/// without the separator would let a session whose id merely ENDS with another's
/// answer for it, and a bare `contains` would match an id embedded in the
/// timestamp half. Takes a [`PathSegment`] because the result is compared
/// against a file name inside a directory this process did not choose — the same
/// reason `find_claude_session_cwd`'s `format!` does.
fn pi_file_suffix(session_id: &PathSegment) -> String {
    format!("_{session_id}.jsonl")
}

/// `find_session_cwd`'s pi half, taking the sessions root explicitly so it is
/// testable against a temp directory instead of the real `~/.pi`.
///
/// Same `Ok(Some(""))` vs `Ok(None)` distinction as the claude half, for the
/// same reason: a session whose header carries no `cwd` is FOUND with an unknown
/// workspace, not absent, and `resolve_resume_cwd` has a distinct tag for that.
///
/// **A never-prompted session has no file at all.** pi defers file creation to
/// the first assistant response (`SOURCE` `session-manager.ts`), so an id this
/// returns `Ok(None)` for may still be a live pane's — the same "no transcript
/// until prompted" fact `doc/design/session-id-learning.md` records for claude.
/// Walk every session file a pi store can hold, in **both** layouts pi writes,
/// calling `visit` on each and stopping at its first `Some` (#2126, review
/// round 1 finding 1).
///
/// # The two layouts, and why tolerating both is the fix rather than a hedge
///
/// pi's default store nests one level — `<root>/--<cwd>--/<ts>_<id>.jsonl`
/// (`DOCS` `session-format.md` §File Location). **Under `--session-dir` or
/// `PI_CODING_AGENT_SESSION_DIR` it does not**: the named directory is used
/// verbatim and files land FLAT in it, with no per-cwd segment at all —
/// `SessionManager.create` takes `dir = normalizePath(sessionDir)` when one is
/// supplied (`SOURCE` `core/session-manager.ts:1521`), `newSession` joins
/// `<that dir>/<ts>_<id>.jsonl` (`:954`), and pi's own `list` reads it with a
/// FLAT `readdir` (`listSessionsFromDir`, `:824`) while only the no-override
/// `listAll` walks subdirectories (`:1670`).
///
/// The first version of this scanner walked two levels unconditionally, so for
/// anyone with the environment variable set every `.jsonl` was `read_dir`-ed as
/// if it were a directory, failed, and was skipped — **zero rows, and a by-id
/// lookup answering `Ok(None)`, i.e. "no such session", for a session that
/// exists.** That is precisely the failure class this whole slice exists to fix
/// (probe the wrong shape; the miss reads as absent), reproduced one store
/// layout over.
///
/// Both shapes are accepted unconditionally rather than selected by which root
/// won, for three reasons: the resolver would have to carry a second return
/// value through the test seam to say which; a store that has been used BOTH
/// ways (a human who set the variable after a while, or unset it) still lists
/// everything; and there is nothing to lose, because pi writes no `.jsonl`
/// directly under a default root and no directory under an override root, so
/// each arm finds nothing where the other one owns the layout.
///
/// **No cwd filtering, deliberately.** pi's own `list` filters an override
/// store's sessions against the header `cwd` because it is answering "what can
/// I resume from HERE". Both callers here are answering something else — the
/// browser lists every session and shows each one's own cwd, and the by-id
/// lookup is keyed on the id — so a filter would only hide real rows.
/// `pub` for the same reason as [`tidy_title`]: `collect_pi_candidates` stayed
/// in `src-tauri` and is the other caller.
pub fn walk_pi_session_files<T>(root: &Path, mut visit: impl FnMut(&Path) -> Option<T>) -> Option<T> {
    let is_session_file = |p: &Path| p.extension().and_then(|e| e.to_str()) == Some("jsonl");
    let entries = fs::read_dir(root).ok()?;
    let mut nested: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // The override layout: a session file sitting directly in the root.
        if is_session_file(&path) {
            if let Some(found) = visit(&path) {
                return Some(found);
            }
        } else {
            nested.push(path);
        }
    }
    // The default layout: one directory per cwd. Walked second, and only over
    // the entries the flat pass did not already claim, so neither shape pays a
    // failed `read_dir` per file of the other.
    for project in nested {
        let Ok(files) = fs::read_dir(&project) else {
            continue; // not a directory, and not a `.jsonl` either — ignore it
        };
        for file in files.flatten() {
            let path = file.path();
            if is_session_file(&path) {
                if let Some(found) = visit(&path) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn find_pi_session_cwd(root: &Path, session_id: &PathSegment) -> Result<Option<String>, String> {
    if !root.exists() {
        return Ok(None); // pi has never run here — nothing recorded, not an error
    }
    let suffix = pi_file_suffix(session_id);
    Ok(walk_pi_session_files(root, |path| {
        let name = path.file_name().and_then(|s| s.to_str())?;
        name.ends_with(&suffix).then(|| scan_pi_jsonl(path).cwd)
    }))
}

/// What one pi session file's head-scan yielded.
///
/// A named struct rather than [`scan_claude_jsonl`]'s tuple because pi's scan
/// answers FOUR questions, not three — it also recovers the header's own `id`,
/// which is the value a `pi --session <id>` resume is matched against — and a
/// four-tuple of `String, String, String, Option<…>` is three positions any
/// caller can transpose with nothing red to say so.
///
/// Every field is `pub`: `parse_candidate` stayed in `src-tauri` and reads all
/// four. `id`/`cwd`/`title` are empty strings, never `Option`, matching what the
/// claude scanner already returns for the same absences — "found, but the file
/// does not say" is a real answer here (see [`find_pi_session_cwd`]'s
/// `Ok(Some(""))` note) and a distinct one from "no such session".
pub struct PiSessionHead {
    /// The `id` on the header line, or empty when the file carries none.
    pub id: String,
    /// First non-meta user prompt, tidied — or `(no prompt)`.
    pub title: String,
    /// The header's `cwd`, or empty.
    pub cwd: String,
    /// Orchestration role + group scraped from a kickoff or notice.
    pub orch: Option<(String, Option<String>)>,
}

/// Pull id/title/cwd/orchestration-identity out of a pi session jsonl by scanning
/// its head — the pi counterpart of [`scan_claude_jsonl`], and deliberately not
/// a generalisation of it: the two formats agree on the ONE sub-object this
/// needs (`message.content`, a string or an array of `{type:"text",text}`
/// blocks, which is why [`content_text`] is reused verbatim) and on nothing
/// else. claude tags each line with a top-level `type` of `user`/`summary` and
/// carries `cwd` on every entry; pi tags every conversation line `message` and
/// puts the role INSIDE `message`, with `cwd` on the header line alone.
///
/// `cwd` comes from the header, which pi writes as the first line
/// (`DOCS` `session-format.md`: "First line of the file. Metadata only") — but
/// this looks for it on any of the first 60 lines rather than line 1 only, so a
/// leading blank or a partially-flushed line costs the row its cwd instead of
/// costing it the whole scan.
///
/// **No summary fallback.** pi's `branch_summary` entry is a compaction
/// artefact, not a session title, so an unprompted session titles itself
/// "(no prompt)" exactly as a claude one does rather than borrowing text the
/// human never wrote.
///
/// `pub` for the same reason as [`tidy_title`]: `parse_candidate` stayed in
/// `src-tauri` and this is how it reads a pi row.
pub fn scan_pi_jsonl(path: &Path) -> PiSessionHead {
    let mut id = String::new();
    let mut title = String::new();
    let mut cwd = String::new();
    let mut orch: Option<(String, Option<String>)> = None;

    let Ok(file) = fs::File::open(path) else {
        return PiSessionHead { id, title, cwd, orch };
    };
    for line in BufReader::new(file).lines().take(60).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("session") => {
                if cwd.is_empty() {
                    if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                        cwd = c.to_string();
                    }
                }
                if id.is_empty() {
                    if let Some(i) = v.get("id").and_then(Value::as_str) {
                        id = i.to_string();
                    }
                }
            }
            Some("message") => {
                let Some(msg) = v.get("message") else { continue };
                if msg.get("role").and_then(Value::as_str) != Some("user") {
                    continue;
                }
                let Some(text) = msg.get("content").and_then(content_text) else {
                    continue;
                };
                // Same precedence as the claude scanner: a kickoff (role AND
                // group) beats a bare `[orrerix]`-notice match (role only).
                if orch.as_ref().map_or(true, |(_, g)| g.is_none()) {
                    if let Some((role, gid)) = detect_orch_signature(&text) {
                        if orch.is_none() || gid.is_some() {
                            orch = Some((role.to_string(), gid));
                        }
                    }
                }
                if !title.is_empty() {
                    continue;
                }
                let trimmed = text.trim();
                // Skip injected command/caveat wrappers, as the claude scanner
                // does — pi has no `isMeta` flag to consult, so the `<` test is
                // the whole of it here.
                if !trimmed.is_empty() && !trimmed.starts_with('<') {
                    title = tidy_title(trimmed, 90);
                }
            }
            _ => {}
        }
    }

    if title.is_empty() {
        title = "(no prompt)".to_string();
    }
    PiSessionHead { id, title, cwd, orch }
}

/// `find_session_cwd`'s copilot half, taking the session-state root
/// explicitly so it's testable against a temp directory. Unlike Claude's
/// filename-is-the-id layout, a copilot session's directory name isn't
/// guaranteed to equal its id (only `workspace.yaml`'s own `id:` field is
/// authoritative — see `scan_copilot`), so this matches on the PARSED id,
/// not the directory name. Same `Ok(Some(""))` vs `Ok(None)` distinction as
/// the claude half, for the same reason.
fn find_copilot_session_cwd(root: &Path, session_id: &PathSegment) -> Result<Option<String>, String> {
    if !root.exists() {
        return Ok(None);
    }
    let entries = fs::read_dir(root).map_err(|e| format!("cannot read {}: {e}", root.display()))?;
    for entry in entries.flatten() {
        if let Some(s) = read_copilot_session(&entry.path()) {
            // Compared, never joined — this half matches on the PARSED id, not
            // the directory name — but it takes the validated type anyway so
            // the two halves cannot disagree about what a session id may be.
            if s.id == session_id.as_str() {
                return Ok(Some(s.cwd));
            }
        }
    }
    Ok(None)
}

// ---------- codex (#2515 C2) ----------
//
// Codex keeps one JSONL "rollout" file per thread, under a date-directory tree
// inside its own home:
//
//     $CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<thread id>.jsonl
//
// Facts pinned against `openai/codex` at tag `rust-v0.153.4`, read blob by blob
// through the GitHub blob API; quoted in `doc/design/codex.md`. Three of them
// shape everything below, and two are corrections to what this slice was
// planned from (posted on #2515 before any of this was written):
//
//  1. THE DATE DIRECTORIES ARE LOCAL-DATED. `precompute_new_rollout_path`
//     (`rollout/src/recorder.rs`) builds them from `OffsetDateTime::now_local()`,
//     so "today" has two answers either side of local midnight and a
//     UTC-derived guess is wrong for part of every day. Nothing here computes a
//     date at all -- `walk_codex_session_files` enumerates whatever directories
//     exist -- precisely so that question never has to be answered.
//
//  2. THE FILE NAME CARRIES TWO IDS, NOT AN ID AND A COUNTER. The slice plan
//     called the second half `_{n}`; `RolloutFileName::render`
//     (`rollout/src/rollout_file_name.rs`) emits
//     `format!("rollout-{timestamp}-{}.jsonl", self.thread_id)` when the thread
//     id and rollout id agree and
//     `format!("rollout-{timestamp}-{}_{}.jsonl", self.thread_id, self.rollout_id)`
//     when they do not -- which is what `thread/revert` produces. The trailing
//     half is a second UUID, so the id this module matches is not a fixed-width
//     field; see `codex_rollout_thread_id`.
//
//  3. A ROLLOUT OLDER THAN A WEEK IS COMPRESSED IN PLACE, and this is the fact
//     a `*.jsonl` walk gets silently wrong. `rollout/src/compression.rs` starts
//     `spawn_rollout_compression_worker` -- "a best-effort background job that
//     compresses cold local rollout files", fire-and-forget, with no
//     configuration gate -- and it rewrites `<name>.jsonl` to `<name>.jsonl.zst`
//     once `MIN_ROLLOUT_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60)`
//     has passed. A walk matching only `.jsonl` would therefore list the last
//     seven days of sessions and answer `Ok(None)` -- "no such session" -- for
//     every older one: the probe-the-wrong-shape, miss-reads-as-absent failure
//     this whole dispatch exists to fix, reproduced one file extension over,
//     and arriving on a schedule rather than by chance. Both representations
//     are walked; see `walk_codex_session_files` for what a compressed file can
//     and cannot answer.

thread_local! {
    /// Test seam for `codex_sessions_root()`, with the same thread-scoping
    /// rationale as `CLAUDE_PROJECTS_ROOT_OVERRIDE` and checked BEFORE the
    /// environment for the same reason the copilot and pi seams are:
    /// `CODEX_HOME` is a genuine production override a developer may have set,
    /// and a test must still win over it.
    static CODEX_SESSIONS_ROOT_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam: fixture the directory `find_session_cwd`'s codex half scans,
/// for the calling thread only. See `set_claude_projects_root_for_test` for why
/// this is a real `pub` function rather than `#[cfg(test)]`.
#[doc(hidden)] // pub for integration tests
pub fn set_codex_sessions_root_for_test(root: Option<PathBuf>) {
    CODEX_SESSIONS_ROOT_OVERRIDE.with(|c| *c.borrow_mut() = root);
}

/// The sessions root a `codex` launched from THIS process's environment would
/// write to, as a pure function of the two inputs the vendor's own resolution
/// reads -- so every branch is testable without `std::env::set_var`, which is
/// unsynchronized mutation racing every other test thread in the binary (the
/// argument `pi_sessions_root_from` and `opencode_store_from` both make).
///
/// `CODEX_HOME` names codex's HOME, not its sessions directory; the sessions
/// live one level inside it, at `<CODEX_HOME>/sessions`
/// (`SESSIONS_SUBDIR = "sessions"`, `rollout/src/lib.rs`).
///
/// **EMPTY is unset -- empty, not blank.** The vendor's `find_codex_home`
/// (`utils/home-dir/src/lib.rs`) reads
/// `std::env::var("CODEX_HOME").ok().filter(|val| !val.is_empty())` and falls
/// back to `~/.codex`. It tests `is_empty()`, never `trim()`, so a value of one
/// space is a real (and hopeless) path to codex itself. Trimming here would be
/// this module disagreeing with the tool it is reading after; not trimming
/// costs nothing, because a whitespace path is not a directory and the caller
/// already answers "no store" to that. The slice plan said "blank"; the vendor
/// says empty, and the vendor wins.
///
/// **A `CODEX_HOME` that is not a directory yields no store, and never a
/// fallback.** The vendor hard-errors on one ("CODEX_HOME points to {val:?},
/// but that path is not a directory"). This cannot error -- `find_session_cwd`
/// reserves `Err` for a store that exists and cannot be listed -- so it answers
/// with a root whose `exists()` fails, i.e. nothing found. What it must never
/// do is silently fall back to `~/.codex`: that would read a DIFFERENT store
/// from the one codex would refuse to run against at all, and hand back rows
/// for sessions the configured store does not contain.
#[doc(hidden)] // pub for integration tests
pub fn codex_sessions_root_from(
    env_codex_home: Option<&str>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let codex_home = match env_codex_home.filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home?.join(".codex"),
    };
    Some(codex_home.join("sessions"))
}

/// `pub`: `collect_codex_candidates` stayed in `src-tauri` and routes through
/// this same testable root lookup rather than a second, untestable
/// `dirs::home_dir()` inline -- the #457 gap, not reopened for codex. It also
/// routes through [`walk_codex_session_files`], so the browser and the by-id
/// lookup cannot disagree about where a codex store keeps its files, or about
/// which representations of one count.
pub fn codex_sessions_root() -> Option<PathBuf> {
    if let Some(r) = CODEX_SESSIONS_ROOT_OVERRIDE.with(|c| c.borrow().clone()) {
        return Some(r);
    }
    codex_sessions_root_from(
        std::env::var("CODEX_HOME").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

/// The suffix codex's compression worker appends when it rewrites a cold
/// rollout in place (`COMPRESSED_SUFFIX`, `rollout/src/compression.rs`).
const CODEX_COMPRESSED_SUFFIX: &str = ".zst";

/// The canonical `.jsonl` name of a rollout file, whichever representation is
/// on disk -- `None` for a name that is not a rollout's at all.
///
/// This is the vendor's own `parse_rollout_file_name`
/// (`rollout/src/compression.rs`) read back in Rust:
///
/// ```text
/// let name = name.strip_suffix(COMPRESSED_SUFFIX).unwrap_or(name);
/// if name.starts_with("rollout-") && name.ends_with(".jsonl") { Some(name) } else { None }
/// ```
///
/// Stripping `.zst` HERE, once, is what lets everything downstream reason about
/// ONE name shape instead of two -- the same split the vendor keeps, where a
/// discovered `RolloutFile` carries a physical `path` beside a
/// `plain_file_name` that is "always the canonical `.jsonl` filename used for
/// timestamp and id parsing".
fn codex_plain_rollout_name(name: &str) -> Option<&str> {
    let plain = name.strip_suffix(CODEX_COMPRESSED_SUFFIX).unwrap_or(name);
    (plain.starts_with("rollout-") && plain.ends_with(".jsonl")).then_some(plain)
}

/// Whether a walked rollout path is the COMPRESSED representation, and so has
/// no content readable here.
///
/// `pub` because `collect_codex_candidates` stayed in `src-tauri` and must ask
/// the same question this module's own scanner asks, rather than re-deriving
/// "does it end in .zst" a second time and drifting the day the vendor adds a
/// second codec.
pub fn codex_rollout_is_compressed(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| n.ends_with(CODEX_COMPRESSED_SUFFIX))
}

/// The THREAD id a canonical rollout file name carries, or `None` when the name
/// does not have the shape.
///
/// `RolloutFileName::parse` is the pin: strip `rollout-`, strip `.jsonl`, take
/// `core[..19]` as the timestamp, require `core[19..20] == "-"`, then read the
/// ids out of `core[20..]` as `ids.split_once('_').unwrap_or((ids, ids))`. So
/// the thread id is everything from offset 20 up to the FIRST `_`, or all of it
/// when there is none.
///
/// **The trailing half of a `_`-bearing name is the ROLLOUT id, not a sequence
/// number** (correction 2 in this section's header). `codex resume <id>` names
/// a THREAD -- "Session id (UUID) or session name", resolved by
/// `find_thread_path_by_id_str` over the whole store -- so the leading half is
/// the one a lookup compares against, and a reverted thread keeps answering for
/// its own id.
///
/// Deliberately NOT a UUID validity check, and deliberately looser than the
/// vendor about the timestamp: this answers "what does this file name claim",
/// and the claim is then compared against a session id that has already been
/// through [`PathSegment`]. The vendor additionally requires `core[..19]` to
/// PARSE as a date; requiring that here would drop a file whose name is
/// otherwise well-formed because its timestamp is odd, which fails toward
/// hiding a real session. Failing toward listing one is the right direction for
/// a browser, and for a lookup that is settled by equality anyway.
fn codex_rollout_thread_id(plain_name: &str) -> Option<&str> {
    let core = plain_name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    if core.get(19..20)? != "-" {
        return None;
    }
    let ids = core.get(20..)?;
    let thread = ids.split_once('_').map_or(ids, |(thread, _)| thread);
    (!thread.is_empty()).then_some(thread)
}

/// The THREAD id a rollout FILE NAME carries, in whichever representation it is
/// on disk — the one entry point the rest of the workspace uses, so nobody has
/// to know that the `.zst` strip and the id grammar are two steps.
///
/// `pub` because `collect_codex_candidates` stayed in `src-tauri` and needs a
/// row's id BEFORE anything is parsed — which for a compressed rollout is the
/// only id there will ever be, since its header cannot be read on this side.
/// The two halves stay private so the `.zst`-then-grammar order lives in exactly
/// one place.
pub fn codex_rollout_thread_id_of(file_name: &str) -> Option<&str> {
    codex_rollout_thread_id(codex_plain_rollout_name(file_name)?)
}

/// Walk every rollout file a codex store can hold -- BOTH representations --
/// calling `visit` on each and stopping at its first `Some`.
///
/// The tree is `<root>/YYYY/MM/DD/rollout-*.jsonl[.zst]`, so this is a
/// depth-BOUNDED three-level descent rather than a recursive walk: a store is a
/// directory of years, of months, of days, of files, and nothing deeper is ever
/// looked at. The bound is what keeps a browser refresh from descending into
/// something arbitrarily deep that happens to sit under a human's `CODEX_HOME`.
///
/// **The directory NAMES are not parsed, matched or dated**, and that is the
/// point rather than laziness -- see correction 1 in this section's header: the
/// vendor dates them from local time, so any date this side computed would be
/// wrong for part of every day. Enumerating what exists asks nothing about
/// dates.
///
/// **`archived_sessions/` is not walked** -- a disclosed residual, not an
/// oversight. It is a SIBLING subtree of `sessions/` of the same shape
/// (`ARCHIVED_SESSIONS_SUBDIR`, `rollout/src/lib.rs`), holding what `codex
/// archive` moved out of the way. A human who archived a session asked for it
/// to stop appearing; listing it would undo that, and a by-id lookup finding
/// one would offer to resume a session they retired. They see no archived rows,
/// and they see no WRONG rows, which is the failure mode that matters.
///
/// **A compressed rollout IS visited, and its content is not readable here.**
/// `<name>.jsonl.zst` is zstd, and decompressing it for one header line means a
/// new `src-tauri` dependency and its getrandom audit (constraint 2). So a
/// compressed file is passed to `visit` -- its NAME still carries the thread id,
/// which is the whole of what an id lookup needs -- and every content-derived
/// field lands on the same "found, but the file does not say" answers a torn
/// header already gets. Listing it with an unknown workspace is strictly better
/// than what a `.jsonl`-only walk gives, which is not listing a week-old
/// session at all.
///
/// **A plain file HIDES its compressed sibling**, mirroring the vendor's own
/// `should_skip_compressed_sibling`. Compression publishes by writing the
/// `.zst` and then removing the `.jsonl`, so a window exists in which both are
/// on disk and they are ONE session; without this the browser shows it twice.
///
/// `pub` for the same reason [`walk_pi_session_files`] is: the other consumer,
/// `collect_codex_candidates`, stayed in `src-tauri`.
pub fn walk_codex_session_files<T>(
    root: &Path,
    mut visit: impl FnMut(&Path) -> Option<T>,
) -> Option<T> {
    let Ok(years) = fs::read_dir(root) else {
        return None;
    };
    for year in years.flatten() {
        let Ok(months) = fs::read_dir(year.path()) else {
            continue;
        };
        for month in months.flatten() {
            let Ok(days) = fs::read_dir(month.path()) else {
                continue;
            };
            for day in days.flatten() {
                let Ok(files) = fs::read_dir(day.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let path = file.path();
                    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    let Some(plain) = codex_plain_rollout_name(name) else {
                        continue;
                    };
                    // The plain sibling wins -- see the doc comment above. The
                    // length test is how "this name was compressed" is asked
                    // without a second `ends_with`: `plain` is a subslice of
                    // `name`, shorter exactly when a suffix was stripped.
                    let compressed = plain.len() != name.len();
                    if compressed && path.with_file_name(plain).exists() {
                        continue;
                    }
                    if let Some(found) = visit(&path) {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

/// Read one bounded first line out of a file, or `None` when it cannot be
/// opened.
///
/// The ONE derivation of "read this file's header line and nothing else",
/// shared by [`find_codex_session_cwd`] here and by
/// `orchestration::pi_session_cwd_in_dir`, which is where it was extracted from
/// (#2515 C2). Both answer the same question about a JSONL transcript -- what
/// does its first line say -- for CLIs whose transcripts are append-only and
/// unbounded, and both are reached from a listing path that runs on every
/// session-browser refresh. Reading the whole file to answer a question the
/// first line answers makes the cost of a listing scale with how much WORK a
/// session did, which is the wrong axis entirely.
///
/// `read_line` on a `BufReader` stops at the first `\n`, so the tail is never
/// touched; `cap` is for the pathological case where a corrupt or mid-write
/// file carries no newline at all, which would otherwise make "the first line"
/// mean "everything".
///
/// A file that exists but is empty yields `Some("")` -- it opened, and it said
/// nothing -- which is a different answer from `None`, i.e. it is not there at
/// all. Callers distinguish the two: an empty header is "found, workspace
/// unknown", never "no such session".
pub fn bounded_first_line(path: &Path, cap: u64) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file.take(cap)).read_line(&mut line).ok()?;
    Some(line)
}

/// How much of a codex rollout [`find_codex_session_cwd`] will read looking for
/// its `session_meta` line.
///
/// The header is the first line and is a few hundred bytes in practice, so this
/// is not a budget anyone is expected to spend -- it is the ceiling for the one
/// case where `read_line` would otherwise not stop: a corrupt or mid-write file
/// with no newline in it at all. Generous on purpose (constraint 8 -- nothing
/// here is tuned to this repo's own transcripts), and equal to the pi header cap
/// it shares [`bounded_first_line`] with, because it bounds the same hazard
/// rather than a measured codex figure.
///
/// It fails toward "no recorded cwd" rather than toward a wrong one: a
/// truncated read cannot parse as JSON, so the session still lists and still
/// says its workspace is unknown, which is the answer a header-less file
/// already gets.
pub const CODEX_SESSION_HEADER_MAX_BYTES: u64 = 64 * 1024;

/// The `session_meta` header of one codex rollout, parsed -- `None` when the
/// file is compressed, unreadable, empty, or its first line is not a
/// `session_meta` object.
///
/// The line's shape is `SessionMeta` written through `RolloutItemWire`
/// (`history/src/rollout_payload.rs`, `#[serde(tag = "type", rename_all =
/// "snake_case")]`), so on disk it is
/// `{"type":"session_meta","payload":{"id":...,"cwd":...,...}}`. `payload.id` is
/// the recorder's `conversation_id`, i.e. the THREAD id -- the same value the
/// file name carries and the same one `codex resume <id>` is matched against.
///
/// **A compressed rollout returns `None` without being opened.** That is not the
/// same fact as a torn header -- it is "this representation cannot be read here
/// at all" -- but every caller gives the two the same answer, because the
/// consequence for a reader is identical: the session is real and its header
/// fields are unknown.
fn codex_header(path: &Path) -> Option<Value> {
    if codex_rollout_is_compressed(path) {
        return None;
    }
    let line = bounded_first_line(path, CODEX_SESSION_HEADER_MAX_BYTES)?;
    let v = serde_json::from_str::<Value>(line.trim_end()).ok()?;
    (v.get("type").and_then(Value::as_str) == Some("session_meta")).then_some(v)
}

/// `find_session_cwd`'s codex half, taking the sessions root explicitly so it is
/// testable against a temp directory instead of the real `~/.codex`.
///
/// Same `Ok(Some(""))` vs `Ok(None)` distinction as every other half, for the
/// same reason: a session whose header carries no `cwd` -- or whose rollout has
/// been compressed and so cannot be read here at all -- is FOUND with an unknown
/// workspace, not absent, and `resolve_resume_cwd` has a distinct tag for that.
///
/// **The file name proposes and the header disposes.** The name is matched first
/// because it is free (`codex_rollout_thread_id`, no read at all), and a name
/// that matches is then CONFIRMED against `payload.id` whenever the header can
/// be read: a header naming a DIFFERENT thread means this file is not the
/// session asked for, whatever its name says, and the walk continues. That is
/// the "header wins" rule, and it is what stops a hand-copied or renamed rollout
/// answering for a session it does not contain.
///
/// **Residual, and it is the price of the cheap half.** A file whose NAME does
/// not match is never opened, so a rollout renamed away from its own thread id
/// is not found even though its header would say so. Closing that would mean
/// reading every header in the store on every miss -- turning a free lookup into
/// one bounded read per session ever recorded -- for a case only a human editing
/// the vendor's store by hand can produce. pi's half makes the same trade for
/// the same reason.
fn find_codex_session_cwd(root: &Path, session_id: &PathSegment) -> Result<Option<String>, String> {
    if !root.exists() {
        return Ok(None); // codex has never run here -- nothing recorded, not an error
    }
    Ok(walk_codex_session_files(root, |path| {
        let name = path.file_name().and_then(|s| s.to_str())?;
        let plain = codex_plain_rollout_name(name)?;
        if codex_rollout_thread_id(plain)? != session_id.as_str() {
            return None;
        }
        match codex_header(path) {
            // Readable header: it decides. One naming another thread
            // disqualifies the file outright rather than falling back to the
            // name that pointed here.
            Some(v) => {
                let id = v.pointer("/payload/id").and_then(Value::as_str);
                if id.is_some_and(|id| id != session_id.as_str()) {
                    return None;
                }
                Some(
                    v.pointer("/payload/cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )
            }
            // Compressed, torn, or not a `session_meta` line: found, workspace
            // unknown. Deliberately NOT `None`, which would read as "no such
            // session" for a session that demonstrably exists.
            None => None,
        }
    }))
}

/// What one codex rollout's head-scan yielded -- the codex twin of
/// [`PiSessionHead`], and a named struct for the same reason: four positions of
/// mostly-`String` is three transpositions any caller can make with nothing red
/// to say so.
///
/// Every field is `pub`: `parse_candidate` stayed in `src-tauri` and reads all
/// four. `id`/`cwd` are empty strings rather than `Option`, matching what the
/// claude and pi scanners already return for the same absences -- "found, but
/// the file does not say" is a real answer here and a distinct one from "no such
/// session".
pub struct CodexSessionHead {
    /// The header's `payload.id`, or empty when the file carries none (or is
    /// compressed, and so carries none readable here).
    pub id: String,
    /// First user message's text, tidied -- or `(no prompt)`.
    pub title: String,
    /// The header's `payload.cwd`, or empty.
    pub cwd: String,
    /// Orchestration role + group scraped from a kickoff or notice.
    pub orch: Option<(String, Option<String>)>,
}

/// Extract plain text from a codex message `content` array.
///
/// **Deliberately not [`content_text`]**, and this is the one place codex's line
/// format diverges from both formats that function serves. Its doc is explicit
/// that claude and pi agree on `{type:"text",text}` and that this is its whole
/// domain; codex's `ContentItem` (`protocol/src/models.rs`, `#[serde(tag =
/// "type", rename_all = "snake_case")]`) has no `text` variant at all -- a user
/// turn's blocks are `input_text`, an assistant's `output_text`. Widening the
/// shared function to accept `input_text` would change what claude and pi rows
/// title themselves with in order to fix a codex problem, which is a wider blast
/// radius than a six-line function.
///
/// Only `input_text` is read, because the only caller wants a USER turn's text.
/// An image-only turn yields `None` and is not mistaken for a title, exactly as
/// on the other two scanners.
fn codex_content_text(content: &Value) -> Option<String> {
    let Value::Array(blocks) = content else {
        return None;
    };
    blocks.iter().find_map(|b| {
        (b.get("type")?.as_str()? == "input_text")
            .then(|| b.get("text")?.as_str().map(str::to_string))
            .flatten()
    })
}

/// Pull id/title/cwd/orchestration-identity out of a codex rollout by scanning
/// its head -- the codex counterpart of [`scan_pi_jsonl`] and, like it,
/// deliberately not a generalisation of either sibling. The three formats agree
/// on nothing structural: claude tags each line with a top-level
/// `user`/`summary`, pi tags conversation lines `message` with the role inside,
/// and codex wraps everything as `{"type":...,"payload":...}` where a
/// conversation turn is `response_item` and the role sits two levels down.
///
/// The header is codex's first line, but this looks for it across the first 60
/// lines rather than line 1 only, so a leading blank or a partially-flushed line
/// costs the row its header fields instead of costing it the whole scan -- the
/// same tolerance the pi scanner has.
///
/// **A compressed rollout yields an EMPTY head, not a missing row.** It comes
/// back with no id, no cwd and the `(no prompt)` title, which the caller turns
/// into a row whose workspace is unknown; the id it lists under comes from the
/// file NAME, which a `.zst` still carries. See [`walk_codex_session_files`] for
/// why nothing is decompressed here.
///
/// `pub` for the same reason as [`tidy_title`]: `parse_candidate` stayed in
/// `src-tauri` and this is how it reads a codex row.
pub fn scan_codex_jsonl(path: &Path) -> CodexSessionHead {
    let mut id = String::new();
    let mut title = String::new();
    let mut cwd = String::new();
    let mut orch: Option<(String, Option<String>)> = None;

    // Compressed, or unopenable: the row still exists, and every field it would
    // have carried is simply unknown. Same shape as a torn header.
    if codex_rollout_is_compressed(path) {
        return CodexSessionHead { id, title: "(no prompt)".to_string(), cwd, orch };
    }
    let Ok(file) = fs::File::open(path) else {
        return CodexSessionHead { id, title: "(no prompt)".to_string(), cwd, orch };
    };
    for line in BufReader::new(file).lines().take(60).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if cwd.is_empty() {
                    if let Some(c) = v.pointer("/payload/cwd").and_then(Value::as_str) {
                        cwd = c.to_string();
                    }
                }
                if id.is_empty() {
                    if let Some(i) = v.pointer("/payload/id").and_then(Value::as_str) {
                        id = i.to_string();
                    }
                }
            }
            Some("response_item") => {
                let Some(payload) = v.get("payload") else {
                    continue;
                };
                // `ResponseItem::Message` -- the only variant carrying a role and
                // a content array. Every other one (a function call, a tool
                // output) is skipped by this test rather than by enumerating
                // them, so a variant added upstream cannot start titling rows.
                if payload.get("type").and_then(Value::as_str) != Some("message") {
                    continue;
                }
                if payload.get("role").and_then(Value::as_str) != Some("user") {
                    continue;
                }
                let Some(text) = payload.get("content").and_then(codex_content_text) else {
                    continue;
                };
                // Same precedence as the claude and pi scanners: a kickoff (role
                // AND group) beats a bare `[orrerix]`-notice match (role only).
                if orch.as_ref().map_or(true, |(_, g)| g.is_none()) {
                    if let Some((role, gid)) = detect_orch_signature(&text) {
                        if orch.is_none() || gid.is_some() {
                            orch = Some((role.to_string(), gid));
                        }
                    }
                }
                if !title.is_empty() {
                    continue;
                }
                let trimmed = text.trim();
                // Skip injected wrappers, as both sibling scanners do -- codex
                // has no `isMeta` flag to consult either, so the `<` test is the
                // whole of it here.
                if !trimmed.is_empty() && !trimmed.starts_with('<') {
                    title = tidy_title(trimmed, 90);
                }
            }
            _ => {}
        }
    }

    if title.is_empty() {
        title = "(no prompt)".to_string();
    }
    CodexSessionHead { id, title, cwd, orch }
}

#[cfg(test)]
mod orch_signature_tests {
    use super::detect_orch_signature;

    #[test]
    fn kickoffs_yield_role_and_group() {
        let (role, gid) = detect_orch_signature(
            "You are the orchestrator of orrerix agent group sempkg-74fe4043 for the repository C:\\x.",
        )
        .unwrap();
        assert_eq!(role, "orchestrator");
        assert_eq!(gid.as_deref(), Some("sempkg-74fe4043"));

        let (role, gid) = detect_orch_signature(
            "You are \"worker 1\" (w-2), a worker agent in orrerix group sempkg-74fe4043 for repository X.",
        )
        .unwrap();
        assert_eq!(role, "worker");
        assert_eq!(gid.as_deref(), Some("sempkg-74fe4043"));

        let (role, _) = detect_orch_signature(
            "You are \"reviewer 1\" (rev-3), a reviewer agent in orrerix group g-1 for repository X.",
        )
        .unwrap();
        assert_eq!(role, "reviewer");
    }

    /// #1153 phase 3, and the one dual-accept in this rename that can never
    /// be retired. A transcript is written ONCE by the agent CLI and read for
    /// as long as the user keeps the session; nothing rewrites the sessions a
    /// user already has. Drop either spelling and every pre-rename session
    /// silently loses its role and its group — no error, no red, just a group
    /// that stops offering to resume. All three role shapes are covered
    /// because they are three separate entries in the phrase table.
    #[test]
    fn a_transcript_recorded_before_the_rename_still_names_its_role_and_group() {
        let (role, gid) = detect_orch_signature(
            "You are the orchestrator of loomux agent group sempkg-74fe4043 for the repository C:\\x.",
        )
        .unwrap();
        assert_eq!(role, "orchestrator");
        assert_eq!(gid.as_deref(), Some("sempkg-74fe4043"));

        let (role, gid) = detect_orch_signature(
            "You are \"worker 1\" (w-2), a worker agent in loomux group sempkg-74fe4043 for repository X.",
        )
        .unwrap();
        assert_eq!(role, "worker");
        assert_eq!(gid.as_deref(), Some("sempkg-74fe4043"));

        let (role, _) = detect_orch_signature(
            "You are \"reviewer 1\" (rev-3), a reviewer agent in loomux group g-1 for repository X.",
        )
        .unwrap();
        assert_eq!(role, "reviewer");

        let (role, gid) = detect_orch_signature("[loomux] w-2 reports progress: ready").unwrap();
        assert_eq!(role, "orchestrator", "a pre-rename notice row still marks an orchestrator pane");
        assert!(gid.is_none());
    }

    #[test]
    fn loomux_notices_identify_orchestrators_without_group() {
        // Reports/exit notices are only ever typed into orchestrator panes;
        // this is how pre-session-tracking orchestrator sessions (whose
        // kickoff may even have been lost) are still identified.
        let (role, gid) = detect_orch_signature("[orrerix] w-2 reports progress: ready").unwrap();
        assert_eq!(role, "orchestrator");
        assert!(gid.is_none());
        assert!(detect_orch_signature("please fix the login bug").is_none());
        for name in [crate::brand::NAME, crate::brand::LEGACY_NAME] {
            assert!(
                detect_orch_signature(&format!("the word {name} alone should not match")).is_none(),
                "prose mentioning {name} must not mark a session"
            );
        }
        assert!(
            detect_orch_signature("[orrerix]no space after the marker").is_none(),
            "the marker arm requires a following space, and dual-accept must not have widened that"
        );
    }
}

#[cfg(test)]
mod resume_store_tests {
    use super::{
        find_claude_session_cwd, find_copilot_session_cwd, find_session_cwd,
        set_claude_projects_root_for_test, set_copilot_session_state_root_for_test, PathSegment,
    };
    use std::fs;
    use std::path::PathBuf;

    /// The two store-lookup halves take a validated session id (#925). These
    /// fixtures all use ids that are valid segments already, so this is a
    /// spelling convenience, not a change of what they assert.
    fn seg(s: &str) -> PathSegment {
        PathSegment::parse(s).expect("test fixture ids must be valid segments")
    }

    /// Per-test scratch dir under the OS temp root: std-based, no `tempfile`
    /// (deliberately, for this PR's new tests — matches the pattern
    /// `tests/workflowfile.rs`/`tests/lessonsfile.rs` already use), keyed by
    /// a tag plus this process's id so parallel test runs never collide.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loomux-sessions-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn claude_session_found_by_id_across_project_dirs() {
        // The exact shape of #412's confirmed repro: a session's project
        // directory name has nothing to do with the id being searched for —
        // only the FILENAME does — so a scan by id finds it regardless of
        // which cwd it's nested under.
        let root = scratch_dir("claude-found");
        let proj = root.join("C--Projects-loomux-worktrees-fix-360-sessions-occlusion");
        fs::create_dir_all(&proj).unwrap();
        let id = "ed42fcec-c894-4db3-8a44-1363ca15f900";
        fs::write(
            proj.join(format!("{id}.jsonl")),
            "{\"type\":\"user\",\"cwd\":\"C:\\\\Projects\\\\loomux-worktrees\\\\fix\\\\360-sessions-occlusion\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        let found = find_claude_session_cwd(&root, &seg(id)).unwrap();
        assert_eq!(
            found.as_deref(),
            Some("C:\\Projects\\loomux-worktrees\\fix\\360-sessions-occlusion"),
            "must recover the session's OWN recorded cwd, not guess one from the dirname"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_session_not_in_store_is_none_not_an_error() {
        let root = scratch_dir("claude-missing");
        fs::create_dir_all(root.join("C--Projects-other")).unwrap();
        assert_eq!(find_claude_session_cwd(&root, &seg("nope-not-here")).unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_missing_projects_root_is_none_not_an_error() {
        let root = scratch_dir("claude-no-root").join("does-not-exist");
        assert_eq!(find_claude_session_cwd(&root, &seg("any-id")).unwrap(), None);
    }

    #[test]
    fn claude_store_root_unreadable_is_a_real_error() {
        // A root that EXISTS but is a plain file (not a directory) makes
        // `read_dir` fail for a real reason — distinguishable from "no
        // projects yet", which must stay `Ok(None)` (previous test).
        let root = scratch_dir("claude-unreadable");
        let not_a_dir = root.join("not-a-dir");
        fs::write(&not_a_dir, b"nope").unwrap();
        assert!(find_claude_session_cwd(&not_a_dir, &seg("any-id")).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copilot_session_found_by_id_not_by_dirname() {
        let root = scratch_dir("copilot-found");
        // Directory name deliberately does NOT match the session id — only
        // `workspace.yaml`'s own `id:` field is authoritative.
        let dir = root.join("some-unrelated-dirname");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("workspace.yaml"), "id: abcd-1234\nname: work\ncwd: C:/work/x\n").unwrap();
        let found = find_copilot_session_cwd(&root, &seg("abcd-1234")).unwrap();
        assert_eq!(found.as_deref(), Some("C:/work/x"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copilot_session_not_found_is_none() {
        let root = scratch_dir("copilot-missing");
        assert_eq!(find_copilot_session_cwd(&root, &seg("nope")).unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }

    /// **A session id must not walk the store lookup out of the projects root**
    /// (#925).
    ///
    /// The claude half joins `{session_id}.jsonl` onto *each project directory*,
    /// so a `..` in the id climbs out of the projects root entirely. This test
    /// builds the escape rather than asserting a refusal in the abstract: the
    /// decoy transcript is placed OUTSIDE the projects root, and reached by a
    /// relative id, so before the fix the lookup found it and returned its
    /// recorded cwd.
    ///
    /// A version of this test that only checked "a traversal id returns None"
    /// would have been vacuous — a traversal id that resolves to nothing
    /// returns `None` either way. The decoy is what makes the assertion mean
    /// "did not reach", and the positive control below is what tells
    /// "refused" apart from "this fixture never resolved anything".
    #[test]
    fn a_traversal_session_id_cannot_reach_a_transcript_outside_the_projects_root() {
        let base = scratch_dir("claude-escape");
        let projects = base.join("projects");
        let proj = projects.join("C--some-project");
        fs::create_dir_all(&proj).unwrap();

        // The decoy, one level ABOVE the projects root.
        fs::write(
            base.join("outside.jsonl"),
            "{\"type\":\"user\",\"cwd\":\"C:/ESCAPED\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();

        set_claude_projects_root_for_test(Some(projects.clone()));

        // `<projects>/C--some-project/../../outside.jsonl` == `<base>/outside.jsonl`.
        assert_eq!(
            find_session_cwd("claude", "../../outside").unwrap(),
            None,
            "a traversal session id must not resolve to a transcript outside the projects root"
        );

        // Positive control: an ordinary id in the same fixture DOES resolve, so
        // the refusal above is containment rather than a dead lookup.
        fs::write(
            proj.join("real-session-1.jsonl"),
            "{\"type\":\"user\",\"cwd\":\"C:/legit\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        assert_eq!(
            find_session_cwd("claude", "real-session-1").unwrap().as_deref(),
            Some("C:/legit"),
            "a well-formed session id must still resolve normally"
        );

        set_claude_projects_root_for_test(None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_session_found_with_no_recorded_cwd_is_distinct_from_not_found() {
        // #412 review N6: a session whose record carries no `cwd` at all must
        // NOT collapse into `Ok(None)` — that's indistinguishable from "never
        // existed in this store", and the two need different tags upstream
        // (`resume-workspace-missing` vs `resume-not-found`).
        let root = scratch_dir("claude-empty-cwd");
        let proj = root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        let id = "no-cwd-here";
        fs::write(proj.join(format!("{id}.jsonl")), "{\"type\":\"summary\",\"summary\":\"hi\"}\n").unwrap();
        assert_eq!(
            find_claude_session_cwd(&root, &seg(id)).unwrap(),
            Some(String::new()),
            "found the session, but it has no cwd — Some(\"\"), never None"
        );
        let _ = fs::remove_dir_all(&root);

        let root2 = scratch_dir("copilot-empty-cwd");
        let dir = root2.join("d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("workspace.yaml"), "id: no-cwd-copilot\nname: x\n").unwrap();
        assert_eq!(
            find_copilot_session_cwd(&root2, &seg("no-cwd-copilot")).unwrap(),
            Some(String::new())
        );
        let _ = fs::remove_dir_all(&root2);
    }

    #[test]
    fn the_test_seams_actually_redirect_find_session_cwd() {
        // Pins the infrastructure #412 review B1/B2's integration tests rely
        // on: `find_session_cwd` (the public dispatcher, not the root-taking
        // internals the other tests in this module call directly) must
        // actually consult the thread-local override, for both CLIs.
        let claude_root = scratch_dir("seam-claude");
        fixture_via_write(&claude_root, "claude-seam-id", "C:/fixtured/claude");
        set_claude_projects_root_for_test(Some(claude_root.clone()));
        assert_eq!(
            find_session_cwd("claude", "claude-seam-id").unwrap().as_deref(),
            Some("C:/fixtured/claude")
        );
        set_claude_projects_root_for_test(None);
        let _ = fs::remove_dir_all(&claude_root);

        let copilot_root = scratch_dir("seam-copilot");
        let dir = copilot_root.join("d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("workspace.yaml"), "id: copilot-seam-id\nname: x\ncwd: C:/fixtured/copilot\n")
            .unwrap();
        set_copilot_session_state_root_for_test(Some(copilot_root.clone()));
        assert_eq!(
            find_session_cwd("copilot", "copilot-seam-id").unwrap().as_deref(),
            Some("C:/fixtured/copilot")
        );
        set_copilot_session_state_root_for_test(None);
        let _ = fs::remove_dir_all(&copilot_root);
    }

    fn fixture_via_write(root: &std::path::Path, id: &str, cwd: &str) {
        let proj = root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join(format!("{id}.jsonl")),
            format!("{{\"type\":\"user\",\"cwd\":{cwd:?},\"message\":{{\"content\":\"hi\"}}}}\n"),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod copilot_session_tests {
    use super::{copilot_session_ids, newest_new_copilot_session};
    use std::collections::HashSet;
    use std::fs;
    use std::path::Path;

    /// Create `session-state/<id>/workspace.yaml` with the given fields. The
    /// mtime bump makes "newest" deterministic without a real clock.
    fn write_session(root: &Path, id: &str, cwd: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("workspace.yaml"),
            format!("id: {id}\nname: work on {id}\ncwd: {cwd}\n"),
        )
        .unwrap();
    }

    /// Force `b`'s workspace.yaml to be strictly newer than `a`'s, so mtime
    /// ordering is stable regardless of filesystem timestamp granularity.
    fn make_newer(root: &Path, older: &str, newer: &str) {
        use std::time::{Duration, SystemTime};
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let set = |id: &str, t: SystemTime| {
            let f = fs::File::options()
                .write(true)
                .open(root.join(id).join("workspace.yaml"))
                .unwrap();
            f.set_modified(t).unwrap();
        };
        set(older, base);
        set(newer, base + Duration::from_secs(10));
    }

    #[test]
    fn baseline_snapshots_existing_ids() {
        let d = tempfile::tempdir().unwrap();
        write_session(d.path(), "aaaa", "C:/work/a");
        write_session(d.path(), "bbbb", "C:/work/b");
        // A stray dir without workspace.yaml is ignored, not counted.
        fs::create_dir_all(d.path().join("cccc")).unwrap();
        let ids = copilot_session_ids(d.path());
        assert_eq!(ids, HashSet::from(["aaaa".to_string(), "bbbb".to_string()]));
    }

    #[test]
    fn newest_new_session_ignores_baseline() {
        let d = tempfile::tempdir().unwrap();
        write_session(d.path(), "old1", "C:/work/x");
        let baseline = copilot_session_ids(d.path());
        // Nothing new yet.
        assert_eq!(newest_new_copilot_session(d.path(), &baseline, "C:/work/x"), None);
        // A fresh session appears; it — not the pre-existing one — is picked.
        write_session(d.path(), "new1", "C:/work/x");
        assert_eq!(
            newest_new_copilot_session(d.path(), &baseline, "C:/work/x").as_deref(),
            Some("new1")
        );
    }

    #[test]
    fn cwd_match_wins_over_a_newer_unrelated_session() {
        let d = tempfile::tempdir().unwrap();
        let baseline = HashSet::new();
        // Two fresh sessions; the newer one ran in a different workspace.
        write_session(d.path(), "mine", "C:/work/mine");
        write_session(d.path(), "other", "C:/work/other");
        make_newer(d.path(), "mine", "other");
        // Ask for the pane whose cwd is C:/work/mine (case/slash-insensitive).
        assert_eq!(
            newest_new_copilot_session(d.path(), &baseline, "c:\\work\\mine").as_deref(),
            Some("mine"),
            "a cwd match must beat a newer session from another workspace"
        );
        // With no cwd hint, the newest fresh session wins instead.
        assert_eq!(
            newest_new_copilot_session(d.path(), &baseline, "").as_deref(),
            Some("other")
        );
    }

    #[test]
    fn missing_root_is_empty_not_a_panic() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("does-not-exist");
        assert!(copilot_session_ids(&root).is_empty());
        assert_eq!(newest_new_copilot_session(&root, &HashSet::new(), "C:/x"), None);
    }
}
