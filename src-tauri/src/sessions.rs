//! Discovery of resumable AI agent sessions on the local machine.
//!
//! Claude Code:    ~/.claude/projects/<encoded-path>/<uuid>.jsonl
//! Copilot CLI:    ~/.copilot/session-state/<uuid>/workspace.yaml
//!
//! Both scanners are best-effort: unreadable or malformed entries are
//! skipped, and a missing tool simply yields an empty list. New agent
//! sources can be added by implementing another `scan_*` function and
//! extending `list_sessions`.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Serialize)]
pub struct SessionInfo {
    /// Session id understood by the agent's `--resume` flag.
    pub id: String,
    /// Which agent owns the session: "claude" | "copilot".
    pub source: String,
    /// Human-readable one-liner (first prompt or session name).
    pub title: String,
    /// Working directory the session ran in.
    pub cwd: String,
    /// Last-modified time, unix millis.
    pub modified_ms: u64,
    /// Shell command line that resumes this session.
    pub resume_command: String,
    /// Orchestration role detected from the transcript's loomux kickoff or
    /// notice signatures ("orchestrator" | "worker" | "reviewer"). Content
    /// fallback for sessions that predate the durable roster.
    pub orch_role: Option<String>,
    /// Orchestration group detected alongside `orch_role`.
    pub orch_group: Option<String>,
}

/// Detect loomux orchestration signatures in a transcript message. Kickoffs
/// name the role and group; `[loomux]` notices (worker reports, exit
/// notices, board edits) are only ever typed into orchestrator panes.
pub(crate) fn detect_orch_signature(text: &str) -> Option<(&'static str, Option<String>)> {
    for (phrase, role) in [
        ("the orchestrator of loomux agent group ", "orchestrator"),
        (" worker agent in loomux group ", "worker"),
        (" reviewer agent in loomux group ", "reviewer"),
    ] {
        if let Some(i) = text.find(phrase) {
            let gid: String = text[i + phrase.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .collect();
            return Some((role, (!gid.is_empty()).then_some(gid)));
        }
    }
    if text.trim_start().starts_with("[loomux] ") {
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

/// Extract plain text from a Claude message `content` field, which is
/// either a string or an array of {type:"text"} blocks.
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

fn tidy_title(raw: &str, limit: usize) -> String {
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
fn scan_claude_jsonl(path: &Path) -> (String, String, Option<(String, Option<String>)>) {
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
                // [loomux]-notice match (role only).
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

fn scan_claude(out: &mut Vec<SessionInfo>) {
    let Some(root) = dirs::home_dir().map(|h| h.join(".claude").join("projects")) else {
        return;
    };
    let Ok(projects) = fs::read_dir(&root) else {
        return;
    };
    for project in projects.flatten() {
        let Ok(files) = fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let (title, cwd, orch) = scan_claude_jsonl(&path);
            // Notice-only detections carry no group id; derive it from the
            // session's cwd, keeping it only if that group exists on disk.
            let (orch_role, orch_group) = match orch {
                Some((role, Some(gid))) => (Some(role), Some(gid)),
                Some((role, None)) if !cwd.is_empty() => {
                    let gid = crate::orchestration::group_id_for_repo(&cwd);
                    let exists = crate::orchestration::OrchRegistry::default_root()
                        .join(&gid)
                        .join("group.json")
                        .is_file();
                    (Some(role), exists.then_some(gid))
                }
                Some((role, None)) => (Some(role), None),
                None => (None, None),
            };
            out.push(SessionInfo {
                resume_command: format!("claude --resume {id}"),
                id: id.to_string(),
                source: "claude".to_string(),
                title,
                cwd,
                modified_ms: mtime_ms(&path),
                orch_role,
                orch_group,
            });
        }
    }
}

/// Minimal single-level YAML field lookup — enough for workspace.yaml
/// without pulling in a YAML dependency.
///
/// `pub(crate)`: `orchestration::digest` reuses this to read a Copilot
/// session's title out of `workspace.yaml` rather than re-deriving the same
/// lookup (#250/#324 slice B).
pub(crate) fn yaml_field(text: &str, key: &str) -> Option<String> {
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
pub(crate) fn copilot_session_state_root() -> Option<PathBuf> {
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

/// One copilot session read from its `session-state/<dir>/workspace.yaml`.
struct CopilotSession {
    id: String,
    title: String,
    cwd: String,
    modified_ms: u64,
}

/// Parse a single session directory. `None` when `workspace.yaml` is missing
/// (session not yet written) or carries no `id`.
fn read_copilot_session(dir: &Path) -> Option<CopilotSession> {
    let ws = dir.join("workspace.yaml");
    let text = fs::read_to_string(&ws).ok()?;
    let id = yaml_field(&text, "id")?;
    let title = yaml_field(&text, "name")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Copilot session".to_string());
    let cwd = yaml_field(&text, "cwd").unwrap_or_default();
    Some(CopilotSession { id, title, cwd, modified_ms: mtime_ms(&ws) })
}

/// Path comparison for copilot cwds: Windows is case- and slash-insensitive,
/// and a trailing separator must not matter.
fn norm_path(s: &str) -> String {
    s.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Session ids currently present under `root` — the baseline snapshot taken
/// before spawning a copilot agent, so the session it later creates can be
/// told apart from pre-existing ones.
pub(crate) fn copilot_session_ids(root: &Path) -> HashSet<String> {
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

/// The copilot session most likely created by a just-spawned pane: one absent
/// from `baseline`, preferring a session whose recorded cwd matches `cwd`
/// (disambiguating agents spawned concurrently in different worktrees),
/// newest by mtime. `None` until copilot has written a new session's
/// `workspace.yaml` — the caller polls.
pub(crate) fn newest_new_copilot_session(
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
pub fn find_session_cwd(source: &str, session_id: &str) -> Result<Option<String>, String> {
    if source == "copilot" {
        return match copilot_session_state_root() {
            Some(root) => find_copilot_session_cwd(&root, session_id),
            None => Ok(None),
        };
    }
    match claude_projects_root() {
        Some(root) => find_claude_session_cwd(&root, session_id),
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

fn claude_projects_root() -> Option<PathBuf> {
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
fn find_claude_session_cwd(root: &Path, session_id: &str) -> Result<Option<String>, String> {
    if !root.exists() {
        return Ok(None); // no ~/.claude/projects yet — nothing recorded, not an error
    }
    let entries = fs::read_dir(root).map_err(|e| format!("cannot read {}: {e}", root.display()))?;
    for project in entries.flatten() {
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

/// `find_session_cwd`'s copilot half, taking the session-state root
/// explicitly so it's testable against a temp directory. Unlike Claude's
/// filename-is-the-id layout, a copilot session's directory name isn't
/// guaranteed to equal its id (only `workspace.yaml`'s own `id:` field is
/// authoritative — see `scan_copilot`), so this matches on the PARSED id,
/// not the directory name. Same `Ok(Some(""))` vs `Ok(None)` distinction as
/// the claude half, for the same reason.
fn find_copilot_session_cwd(root: &Path, session_id: &str) -> Result<Option<String>, String> {
    if !root.exists() {
        return Ok(None);
    }
    let entries = fs::read_dir(root).map_err(|e| format!("cannot read {}: {e}", root.display()))?;
    for entry in entries.flatten() {
        if let Some(s) = read_copilot_session(&entry.path()) {
            if s.id == session_id {
                return Ok(Some(s.cwd));
            }
        }
    }
    Ok(None)
}

fn scan_copilot(out: &mut Vec<SessionInfo>) {
    let Some(root) = copilot_session_state_root() else {
        return;
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(s) = read_copilot_session(&entry.path()) else {
            continue;
        };
        out.push(SessionInfo {
            resume_command: format!("copilot --resume {}", s.id),
            id: s.id,
            source: "copilot".to_string(),
            title: tidy_title(&s.title, 90),
            cwd: s.cwd,
            modified_ms: s.modified_ms,
            orch_role: None,
            orch_group: None,
        });
    }
}

/// Full scan of every recorded Claude/Copilot session file on disk. Real,
/// unbounded-by-count I/O (issue #342): a machine with a long orchestration
/// history can accumulate thousands of `.claude/projects/**/*.jsonl` files
/// across every past project, and each one costs an open + up to 60 lines
/// read. A breadcrumb records how long this took and how many files it found,
/// so a slow-startup report has an actual number to point at instead of "it
/// felt slow" — this is the scan `main.ts`'s boot restore used to await
/// before it would open a single pane.
fn list_sessions_sync() -> Vec<SessionInfo> {
    let start = std::time::Instant::now();
    let mut sessions = Vec::new();
    scan_claude(&mut sessions);
    scan_copilot(&mut sessions);
    let found = sessions.len();
    sessions.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    sessions.truncate(300);
    crate::obs::breadcrumb(
        "startup",
        &format!("list_sessions: {found} session file(s) scanned in {:?}", start.elapsed()),
    );
    sessions
}

/// Tauri dispatches a *synchronous* `#[tauri::command]` by calling it directly
/// on the webview main thread (see the identical note in `git.rs`, issue
/// #207/#399) — so the full disk scan above ran on the UI thread every time
/// this was invoked. Off-thread via `spawn_blocking`; a panicked scan degrades
/// to an empty list rather than propagating, matching every existing caller's
/// already-tolerant "best-effort, assume resumable on failure" handling.
#[tauri::command]
pub async fn list_sessions() -> Vec<SessionInfo> {
    tauri::async_runtime::spawn_blocking(list_sessions_sync)
        .await
        .unwrap_or_default()
}

#[cfg(test)]
mod orch_signature_tests {
    use super::detect_orch_signature;

    #[test]
    fn kickoffs_yield_role_and_group() {
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
    }

    #[test]
    fn loomux_notices_identify_orchestrators_without_group() {
        // Reports/exit notices are only ever typed into orchestrator panes;
        // this is how pre-session-tracking orchestrator sessions (whose
        // kickoff may even have been lost) are still identified.
        let (role, gid) = detect_orch_signature("[loomux] w-2 reports progress: ready").unwrap();
        assert_eq!(role, "orchestrator");
        assert!(gid.is_none());
        assert!(detect_orch_signature("please fix the login bug").is_none());
        assert!(
            detect_orch_signature("the word loomux alone should not match").is_none(),
            "prose mentioning loomux must not mark a session"
        );
    }
}

#[cfg(test)]
mod resume_store_tests {
    use super::{
        find_claude_session_cwd, find_copilot_session_cwd, find_session_cwd,
        set_claude_projects_root_for_test, set_copilot_session_state_root_for_test,
    };
    use std::fs;
    use std::path::PathBuf;

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
        let found = find_claude_session_cwd(&root, id).unwrap();
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
        assert_eq!(find_claude_session_cwd(&root, "nope-not-here").unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_missing_projects_root_is_none_not_an_error() {
        let root = scratch_dir("claude-no-root").join("does-not-exist");
        assert_eq!(find_claude_session_cwd(&root, "any-id").unwrap(), None);
    }

    #[test]
    fn claude_store_root_unreadable_is_a_real_error() {
        // A root that EXISTS but is a plain file (not a directory) makes
        // `read_dir` fail for a real reason — distinguishable from "no
        // projects yet", which must stay `Ok(None)` (previous test).
        let root = scratch_dir("claude-unreadable");
        let not_a_dir = root.join("not-a-dir");
        fs::write(&not_a_dir, b"nope").unwrap();
        assert!(find_claude_session_cwd(&not_a_dir, "any-id").is_err());
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
        let found = find_copilot_session_cwd(&root, "abcd-1234").unwrap();
        assert_eq!(found.as_deref(), Some("C:/work/x"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copilot_session_not_found_is_none() {
        let root = scratch_dir("copilot-missing");
        assert_eq!(find_copilot_session_cwd(&root, "nope").unwrap(), None);
        let _ = fs::remove_dir_all(&root);
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
            find_claude_session_cwd(&root, id).unwrap(),
            Some(String::new()),
            "found the session, but it has no cwd — Some(\"\"), never None"
        );
        let _ = fs::remove_dir_all(&root);

        let root2 = scratch_dir("copilot-empty-cwd");
        let dir = root2.join("d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("workspace.yaml"), "id: no-cwd-copilot\nname: x\n").unwrap();
        assert_eq!(
            find_copilot_session_cwd(&root2, "no-cwd-copilot").unwrap(),
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
