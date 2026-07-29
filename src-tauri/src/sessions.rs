//! Discovery of resumable AI agent sessions on the local machine.
//!
//! Claude Code:    ~/.claude/projects/<encoded-path>/<uuid>.jsonl
//! Copilot CLI:    ~/.copilot/session-state/<uuid>/workspace.yaml
//!
//! Both scanners are best-effort: unreadable or malformed entries are
//! skipped, and a missing tool simply yields an empty list. New agent
//! sources can be added by implementing another `scan_*` function and
//! extending `list_sessions`.

use serde::{Deserialize, Serialize};
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

// ---------- copilot autopilot launch posture (#456) ----------
//
// A solo copilot pane launched via loomux's launcher carries its Autopilot
// toggle state as `--autopilot --allow-all-tools --allow-all-paths` on the
// spawned command line (`single_pane_autopilot_flags`) — but a session
// resumed from the Sessions tab is rebuilt from scratch as a bare
// `copilot --resume <id>` (`scan_copilot` below), reading only copilot's OWN
// `~/.copilot/session-state` files, which know nothing about how loomux
// originally launched it. Restoring an autopilot session that way silently
// drops it into plain interactive mode (#456). This is loomux's own record
// of what the toggle was set to, captured at the one moment that
// information exists — launch time — so `scan_copilot` can re-derive the
// posture instead of guessing.
//
// Keyed by cwd, not by copilot's session id: unlike Claude, copilot never
// hands loomux a session id at launch (it mints its own, invisibly, and
// `spawn_copilot_session_watcher` in orchestration/mod.rs learns it after
// the fact only for GROUP agents) — a solo pane has no id to key on until
// long after this record needs to exist. Precise per-session keying is
// tracked as a follow-up (#457, restore-path unification) rather than
// reimplementing that watcher machinery here.
//
// THE RULE THIS MODULE ENFORCES: on a permission decision, ambiguity
// resolves to the smaller grant, never the larger one — and that includes
// under STORE PRESSURE (review B1) and across FILESYSTEM CASE SENSITIVITY
// (review B2), not just in the ordinary lookup. Because the key is only a
// cwd, TWO copilot sessions launched in the same folder at different times
// can disagree (toggle on, then later off, or vice versa) — cwd alone can't
// tell which record belongs to which session being restored. Ideally this
// would resolve by matching each record against the resumed session's own
// start time, but copilot's `workspace.yaml` documents no reliable creation
// timestamp we can act on (undocumented internal format — see the module
// doc's docs-not-inference rule) and the file's OS birth time is not a safe
// proxy (copilot may rewrite it turn-to-turn, resetting it). So: a cwd with
// only ONE posture ever recorded resolves to that posture; a cwd where BOTH
// true and false have been recorded is CONFLICTED and resolves to `None`
// (no flags) — losing autopilot on restore is a shift+tab away, but silently
// granting `--allow-all-paths` to a session the user deliberately launched
// without it is not something they could ever notice or undo.
//
// Conflict is derived and stored AT WRITE TIME, as one sticky enum value per
// cwd (`CopilotPosture::Conflicted`) — never re-derived at read time from a
// list of raw records. This is what makes the guarantee survive eviction
// (review B1, caught with a runnable counter-test against the original flat
// per-write log: capping-and-evicting individual records could drop the
// OFF half of a conflict and leave a lone ON record, silently flipping a
// permanently-ambiguous cwd back to granting autopilot). With one entry per
// cwd, the cap counts CWDS and evicts the least-recently-TOUCHED one whole —
// "touched" meaning written OR re-confirmed, so an actively-used folder is
// never the eviction target — and eviction can only ever move a cwd from
// {True | False | Conflicted} to NO RECORD, which resolves to `None` right
// alongside every other "nothing to go on" case. There is no path from
// eviction to a larger grant than the cwd already had.
const COPILOT_POSTURE_CAP: usize = 300;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum CopilotPosture {
    True,
    False,
    /// Sticky: once a cwd sees both `True` and `False` writes, it stays
    /// `Conflicted` forever (until evicted entirely) — never flips back to a
    /// single value no matter what's written or evicted afterward.
    Conflicted,
}

#[derive(Clone, Serialize, Deserialize)]
struct CopilotPostureEntry {
    /// The store's PERMISSION key — see `posture_key`'s doc comment for why
    /// this is deliberately NOT the same normalization `norm_path` (session-
    /// cwd MATCHING) uses.
    cwd: String,
    posture: CopilotPosture,
    /// Bumped on every write to this cwd, including a repeat of the same
    /// value — this is what "touched" means for LRU eviction, not merely
    /// "created".
    touched_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct CopilotPostureStore {
    entries: Vec<CopilotPostureEntry>,
}

thread_local! {
    /// Test seam for `copilot_posture_path()`, same thread-scoping rationale
    /// as `COPILOT_SESSION_STATE_ROOT_OVERRIDE` above — a real env-var
    /// override would race a concurrently-running test on another thread.
    static COPILOT_POSTURE_PATH_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam: fixture the file `copilot_posture_path()` returns, for the
/// calling thread only. See `set_claude_projects_root_for_test` for why.
#[doc(hidden)] // pub for integration tests
pub fn set_copilot_posture_path_for_test(path: Option<PathBuf>) {
    COPILOT_POSTURE_PATH_OVERRIDE.with(|c| *c.borrow_mut() = path);
}

fn copilot_posture_path() -> PathBuf {
    if let Some(p) = COPILOT_POSTURE_PATH_OVERRIDE.with(|c| c.borrow().clone()) {
        return p;
    }
    crate::obs::data_root().join("copilot-posture.json")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Best-effort load: a missing file is "no history yet" (empty store); a
/// corrupt one is quarantined (via `uistate::load_or_quarantine`, the same
/// fail-safe `tabs.json`/`settings.json` use) and treated as empty too — a
/// lost posture history degrades to the safe "no record" behavior below,
/// never a crash or a stale grant.
fn load_copilot_posture() -> CopilotPostureStore {
    crate::uistate::load_or_quarantine(&copilot_posture_path())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Normalize a cwd into the posture store's PERMISSION key (review B2).
/// Deliberately DIFFERENT from `norm_path` above, which is right for
/// SESSION-CWD MATCHING (a miss there just falls back to "newest session
/// wins" — low-stakes) but wrong reused as a permission key: `norm_path`
/// unconditionally case-folds, which is correct on Windows (the filesystem
/// itself is case-insensitive) but WRONG on Linux/macOS, where `/foo` and
/// `/Foo` are genuinely different directories — folding them onto one key
/// would let a session from one inherit the other's `--allow-all-paths`
/// grant, a cross-directory permission leak. Case-folding here happens ONLY
/// under `windows`; everywhere else the key is exact-match, so a
/// case-differing path simply fails to match and resolves to `None` (no
/// flags) — fails safe on every platform, one rule, no platform branching in
/// the CALLERS. Trailing-separator trimming is safe unconditionally (it
/// never collapses two distinct directories onto one key).
///
/// `windows` is a parameter (not `cfg!(windows)` inlined) so both branches
/// are directly unit-testable from any host, per review B2's ask for a
/// mutation-verified pin — see `copilot_posture_tests`.
fn posture_key_for(s: &str, windows: bool) -> String {
    if windows {
        s.replace('/', "\\").trim_end_matches('\\').to_lowercase()
    } else {
        s.trim_end_matches('/').to_string()
    }
}

fn posture_key(s: &str) -> String {
    posture_key_for(s, cfg!(windows))
}

/// Record what the Autopilot toggle was set to for a solo copilot launch in
/// `cwd`, so a later Sessions-tab resume of a session from this folder can
/// re-derive the posture (#456) instead of guessing. Called for BOTH toggle
/// states — recording `false` matters exactly as much as recording `true`,
/// since a later restore must be able to tell "explicitly off" from "no
/// record". A cwd with only one posture ever written stays that posture; a
/// cwd that sees both becomes (and stays) `Conflicted` — see the module
/// section's ambiguity rule. Best-effort and capped at `COPILOT_POSTURE_CAP`
/// CWDS (review B1: one entry per cwd, LRU-evicted whole, never a flat log
/// of individual writes) — this is a convenience record, not a
/// durable-correctness store, and must never block or fail a launch.
#[tauri::command]
pub fn record_copilot_launch_posture(cwd: String, autopilot: bool) -> Result<(), String> {
    record_copilot_launch_posture_impl(&cwd, autopilot)
}

fn record_copilot_launch_posture_impl(cwd: &str, autopilot: bool) -> Result<(), String> {
    let mut store = load_copilot_posture();
    let key = posture_key(cwd);
    let now = now_ms();
    let incoming = if autopilot { CopilotPosture::True } else { CopilotPosture::False };
    match store.entries.iter_mut().find(|e| e.cwd == key) {
        Some(entry) => {
            // Already conflicted stays conflicted; a fresh disagreement
            // BECOMES conflicted; agreement just refreshes the touch time.
            if entry.posture != incoming {
                entry.posture = CopilotPosture::Conflicted;
            }
            entry.touched_ms = now;
        }
        None => store.entries.push(CopilotPostureEntry { cwd: key, posture: incoming, touched_ms: now }),
    }
    if store.entries.len() > COPILOT_POSTURE_CAP {
        // Evict the least-recently-TOUCHED cwd wholesale — never a partial
        // record of one — until back at the cap.
        store.entries.sort_by_key(|e| e.touched_ms);
        let excess = store.entries.len() - COPILOT_POSTURE_CAP;
        store.entries.drain(0..excess);
    }
    let body = serde_json::to_string(&store).map_err(|e| e.to_string())?;
    crate::uistate::write_atomic(&copilot_posture_path(), &body)
}

/// The autopilot posture to assume for a copilot session resumed from `cwd`,
/// per the ambiguity rule documented above the module section: `Some(v)`
/// only when this cwd's stored posture is unambiguously `v`; `None` (no
/// flags) when there is no record at all, OR the stored posture is
/// `Conflicted`. Takes an already-loaded store so a caller resolving many
/// cwds in one pass (`scan_copilot`) reads the file once, not once per
/// session (review NB3).
fn posture_in(store: &CopilotPostureStore, cwd: &str) -> Option<bool> {
    let want = posture_key(cwd);
    if want.is_empty() {
        return None;
    }
    match store.entries.iter().find(|e| e.cwd == want)?.posture {
        CopilotPosture::True => Some(true),
        CopilotPosture::False => Some(false),
        CopilotPosture::Conflicted => None,
    }
}

/// `posture_in`, loading the store fresh — for the (rare, single-lookup)
/// caller that doesn't already have one loaded. `scan_copilot` below loads
/// once and calls `posture_in` directly instead.
fn copilot_launch_posture(cwd: &str) -> Option<bool> {
    posture_in(&load_copilot_posture(), cwd)
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
    // #456 review NB3: one load for the whole scan, not one per session —
    // the store is read-only here, so there's nothing to keep fresh across
    // iterations.
    let posture_store = load_copilot_posture();
    for entry in entries.flatten() {
        let Some(s) = read_copilot_session(&entry.path()) else {
            continue;
        };
        // #456: re-derive the autopilot posture loomux launched this cwd
        // with (if any unambiguous record exists — see the module section
        // above `record_copilot_launch_posture`) rather than handing back a
        // bare `--resume`, which silently drops a session out of autopilot
        // on restore. Reuses the SAME flags string a fresh launch builds
        // (`single_pane_autopilot_flags`/`COPILOT_GROUP_AUTOPILOT_FLAGS`) —
        // one seam, never a second copy that could drift.
        let resume_command = if posture_in(&posture_store, &s.cwd) == Some(true) {
            format!(
                "copilot --resume {} {}",
                s.id,
                crate::orchestration::COPILOT_GROUP_AUTOPILOT_FLAGS
            )
        } else {
            format!("copilot --resume {}", s.id)
        };
        out.push(SessionInfo {
            resume_command,
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

#[cfg(test)]
mod copilot_posture_tests {
    // #456: the copilot-launch-posture record + the `scan_copilot` ambiguity
    // rule it feeds. Each test binds `set_copilot_posture_path_for_test` to a
    // fresh temp file so tests never share (or race on) the real
    // `<data root>/copilot-posture.json`.
    use super::{
        copilot_launch_posture, posture_key_for, record_copilot_launch_posture_impl, scan_copilot,
        set_copilot_posture_path_for_test, set_copilot_session_state_root_for_test,
    };
    use std::fs;

    /// Bind the posture-store test seam to a not-yet-existing file inside a
    /// fresh tempdir, and return the guard the caller must hold to keep the
    /// tempdir alive for the test's duration.
    fn posture_seam() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        set_copilot_posture_path_for_test(Some(d.path().join("copilot-posture.json")));
        d
    }

    #[test]
    fn no_record_is_none() {
        let _d = posture_seam();
        assert_eq!(copilot_launch_posture("C:/work/x"), None);
        set_copilot_posture_path_for_test(None);
    }

    #[test]
    fn a_single_recorded_value_round_trips() {
        let _d = posture_seam();
        record_copilot_launch_posture_impl("C:/work/x", true).unwrap();
        assert_eq!(copilot_launch_posture("C:/work/x"), Some(true));

        record_copilot_launch_posture_impl("C:/work/y", false).unwrap();
        assert_eq!(copilot_launch_posture("C:/work/y"), Some(false));
        set_copilot_posture_path_for_test(None);
    }

    #[test]
    fn lookup_is_case_and_slash_insensitive_on_windows_only() {
        // The real posture_key follows the actual build target (cfg!(windows)),
        // so this end-to-end test's expectation must too — it's genuinely
        // different behavior on Windows vs. everywhere else (review B2), not a
        // bug on either side.
        let _d = posture_seam();
        record_copilot_launch_posture_impl("C:/Work/Project", true).unwrap();
        let looked_up = copilot_launch_posture("c:\\work\\project");
        if cfg!(windows) {
            assert_eq!(looked_up, Some(true), "Windows: case/slash-insensitive path equality is correct");
        } else {
            assert_eq!(looked_up, None, "non-Windows: a case-differing path must NOT match (review B2)");
        }
        set_copilot_posture_path_for_test(None);
    }

    /// THE B2 property (review): a posture record only ever applies to the
    /// EXACT directory it was recorded for — never to a merely
    /// similarly-spelled one. Case is the concrete way this broke (review
    /// B2): folding case in the permission key would let `/Proj` and `/proj`
    /// — genuinely DIFFERENT directories on a case-sensitive filesystem —
    /// collide onto one key, so a session from one could inherit the
    /// other's `--allow-all-paths` grant. Exercised directly against BOTH
    /// branches of `posture_key_for` (not `cfg(windows)`-gated), so this is
    /// mutation-verified and enforced on every host that runs the suite —
    /// including this one — rather than only on whichever OS happens to
    /// build it.
    #[test]
    fn posture_key_never_folds_two_distinct_directories_into_one_on_a_case_sensitive_platform() {
        // Windows (case-insensitive filesystem): folding is correct — these
        // spellings genuinely name the SAME directory.
        assert_eq!(
            posture_key_for("C:/Work/Project", true),
            posture_key_for("c:\\work\\project", true),
            "same directory, different spelling, same platform key — expected on Windows"
        );
        // Everywhere else (case-sensitive filesystem): these are DIFFERENT
        // directories and must never produce the same key.
        assert_ne!(
            posture_key_for("/home/user/Project", false),
            posture_key_for("/home/user/project", false),
            "different directories must never fold onto the same permission key off Windows"
        );
    }

    #[test]
    fn empty_cwd_never_matches() {
        let _d = posture_seam();
        record_copilot_launch_posture_impl("", true).unwrap();
        assert_eq!(copilot_launch_posture(""), None);
        set_copilot_posture_path_for_test(None);
    }

    /// THE rule this whole module exists to enforce: a folder launched with
    /// autopilot on, then later off (or vice versa), must NOT resolve to
    /// whichever came last — that would silently hand `--allow-all-paths` to
    /// a session the human deliberately launched without it the moment an
    /// OLDER, differently-postured session in the same folder gets restored.
    /// Disagreement must resolve to `None` (no flags), permanently, for that
    /// cwd — the smaller grant, never the larger one.
    #[test]
    fn conflicting_records_for_the_same_cwd_resolve_to_none_not_latest() {
        let _d = posture_seam();
        record_copilot_launch_posture_impl("C:/work/x", true).unwrap();
        record_copilot_launch_posture_impl("C:/work/x", false).unwrap();
        assert_eq!(
            copilot_launch_posture("C:/work/x"),
            None,
            "ambiguous history must never resolve to the larger (autopilot) grant"
        );

        // Same in the other order — order must not matter, only agreement.
        let _d2 = posture_seam();
        record_copilot_launch_posture_impl("C:/work/y", false).unwrap();
        record_copilot_launch_posture_impl("C:/work/y", true).unwrap();
        assert_eq!(copilot_launch_posture("C:/work/y"), None);
        set_copilot_posture_path_for_test(None);
    }

    #[test]
    fn store_caps_and_evicts_the_least_recently_touched_cwd() {
        let _d = posture_seam();
        // One past the cap, each a distinct cwd so none collide/cancel out.
        for i in 0..=super::COPILOT_POSTURE_CAP {
            record_copilot_launch_posture_impl(&format!("C:/work/{i}"), true).unwrap();
        }
        let store = super::load_copilot_posture();
        assert_eq!(store.entries.len(), super::COPILOT_POSTURE_CAP, "must stay capped, not grow unbounded — one entry per cwd");
        // The very first recorded (cwd "C:/work/0") was never touched again — evicted.
        assert_eq!(copilot_launch_posture("C:/work/0"), None);
        // The most recently touched survives.
        assert_eq!(copilot_launch_posture(&format!("C:/work/{}", super::COPILOT_POSTURE_CAP)), Some(true));
        set_copilot_posture_path_for_test(None);
    }

    #[test]
    fn re_touching_a_cwd_protects_it_from_eviction() {
        // A repeat write of the SAME value must count as a touch (bumping
        // eviction priority), not merely dedupe silently — otherwise an
        // actively-relaunched folder could still be evicted ahead of one
        // nobody has opened in months, which defeats the point of LRU.
        let _d = posture_seam();
        record_copilot_launch_posture_impl("C:/work/active", true).unwrap();
        for i in 0..super::COPILOT_POSTURE_CAP {
            record_copilot_launch_posture_impl(&format!("C:/work/filler{i}"), true).unwrap();
            // Re-confirm the active cwd on every iteration — it must never
            // be the least-recently-touched entry.
            record_copilot_launch_posture_impl("C:/work/active", true).unwrap();
        }
        assert_eq!(
            copilot_launch_posture("C:/work/active"),
            Some(true),
            "a repeatedly re-touched cwd must survive eviction even under sustained store pressure"
        );
        set_copilot_posture_path_for_test(None);
    }

    /// THE B1 property (review): a cwd with conflicting posture history
    /// never yields flags — no matter how much OTHER store activity happens
    /// afterward, including enough to push the store arbitrarily far past
    /// its cap. Mutation-verified against the pre-fix design: reverting to
    /// "one entry per WRITE, oldest evicted individually" (rather than one
    /// sticky `Conflicted` entry per cwd, decided at write time) makes this
    /// red — the OFF half of the conflict ages out of the flat log first,
    /// leaving a lone surviving ON record that resolves to `Some(true)`.
    /// This generalizes the review's own repro (which pushed just short of
    /// one cap's worth of other activity) by pushing several cap's worth,
    /// proving the guarantee holds under sustained pressure, not merely at
    /// the boundary.
    #[test]
    fn conflicted_cwd_never_yields_flags_no_matter_how_much_other_activity_follows() {
        let _d = posture_seam();
        record_copilot_launch_posture_impl("C:/work/conflicted", false).unwrap();
        record_copilot_launch_posture_impl("C:/work/conflicted", true).unwrap();
        assert_eq!(copilot_launch_posture("C:/work/conflicted"), None);

        // The precise pressure that exposes a flat, per-write log (review
        // B1's actual failure shape): exactly enough OTHER activity to push
        // eviction to the boundary where it would claim just ONE record —
        // the older of the two conflicting writes — leaving a lone survivor.
        // A test that pushes activity far past this boundary evicts BOTH
        // sides together and passes for the wrong reason (nothing left to
        // resolve at all) — this exact size is what must be asserted at.
        for i in 0..super::COPILOT_POSTURE_CAP - 1 {
            record_copilot_launch_posture_impl(&format!("C:/other/{i}"), true).unwrap();
        }
        assert_eq!(
            copilot_launch_posture("C:/work/conflicted"),
            None,
            "eviction at the exact boundary that claims only the older half of a conflict must \
             not resolve the cwd to the surviving (larger-grant) half"
        );

        // Now generalize past the boundary: keep pushing activity for a long
        // stretch afterward — the property must hold arbitrarily far out,
        // not just at the one boundary above.
        for round in 0..3 {
            for i in 0..super::COPILOT_POSTURE_CAP {
                record_copilot_launch_posture_impl(&format!("C:/later/{round}/{i}"), true).unwrap();
            }
        }
        assert_eq!(
            copilot_launch_posture("C:/work/conflicted"),
            None,
            "a conflicted cwd must never resolve to a grant, no matter how much other store \
             activity happens after it — eviction may only ever move it to NO record, never to \
             a single surviving value"
        );
        set_copilot_posture_path_for_test(None);
    }

    /// The restore-path regression pin (#456): a Sessions-tab resume of a
    /// copilot session must carry the SAME autopilot flags a fresh launch in
    /// that folder would, when — and only when — loomux's own record is
    /// unambiguous. This exercises `scan_copilot` end to end, the exact
    /// function `list_sessions` (and so the Sessions tab / app restore) call.
    #[test]
    fn scan_copilot_restores_autopilot_flags_only_when_unambiguous() {
        let session_root = tempfile::tempdir().unwrap();
        set_copilot_session_state_root_for_test(Some(session_root.path().to_path_buf()));
        let posture_dir = posture_seam();

        let write_session = |id: &str, cwd: &str| {
            let dir = session_root.path().join(id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("workspace.yaml"),
                format!("id: {id}\nname: test session\ncwd: {cwd}\n"),
            )
            .unwrap();
        };

        // Unambiguous ON: the resumed command must carry the same flags a
        // fresh launch builds (`COPILOT_GROUP_AUTOPILOT_FLAGS`).
        write_session("sess-on", "C:/work/on");
        record_copilot_launch_posture_impl("C:/work/on", true).unwrap();

        // Unambiguous OFF: bare resume, exactly today's behavior.
        write_session("sess-off", "C:/work/off");
        record_copilot_launch_posture_impl("C:/work/off", false).unwrap();

        // No record at all: bare resume — the pre-#456 behavior, safe default.
        write_session("sess-unknown", "C:/work/unknown");

        // Ambiguous: bare resume, per the smaller-grant-wins rule, even
        // though the MOST RECENT record here is `true`.
        write_session("sess-ambiguous", "C:/work/ambiguous");
        record_copilot_launch_posture_impl("C:/work/ambiguous", false).unwrap();
        record_copilot_launch_posture_impl("C:/work/ambiguous", true).unwrap();

        let mut out = Vec::new();
        scan_copilot(&mut out);
        let by_id = |id: &str| out.iter().find(|s| s.id == id).unwrap();

        assert_eq!(
            by_id("sess-on").resume_command,
            format!("copilot --resume sess-on {}", crate::orchestration::COPILOT_GROUP_AUTOPILOT_FLAGS)
        );
        assert_eq!(by_id("sess-off").resume_command, "copilot --resume sess-off");
        assert_eq!(by_id("sess-unknown").resume_command, "copilot --resume sess-unknown");
        assert_eq!(
            by_id("sess-ambiguous").resume_command,
            "copilot --resume sess-ambiguous",
            "an ambiguous history must never grant autopilot on restore, even via the latest record"
        );

        set_copilot_session_state_root_for_test(None);
        set_copilot_posture_path_for_test(None);
        drop(posture_dir);
    }
}
