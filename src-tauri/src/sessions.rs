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
use std::collections::{HashMap, HashSet};
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

/// One session file the scan *might* index, discovered from directory metadata
/// alone — nothing inside the file has been read yet.
///
/// #493: collecting candidates before parsing any of them is what lets the scan
/// sort by mtime and cut to `LIST_LIMIT` FIRST, so the expensive part (a
/// head-parse per file) costs `O(LIST_LIMIT)` instead of `O(every session ever
/// recorded on this machine)`. On the machine #493 was measured on, that alone
/// is 826 head-parses down to 300 — and the rows dropped by the truncate were
/// always parsed for nothing, since `list_sessions` has capped its result at
/// 300 since long before this change.
struct Candidate {
    /// The file whose `(mtime, len)` both keys and validates this session's
    /// index entry: claude's `<id>.jsonl`, copilot's `workspace.yaml`.
    path: PathBuf,
    /// "claude" | "copilot" — which parser this candidate needs.
    source: &'static str,
    /// Claude's filename IS the session id, so it's free at collection time;
    /// copilot's only authoritative id lives inside `workspace.yaml` (see
    /// `parse_candidate`), so it stays `None` until the file is parsed.
    id: Option<String>,
    modified_ms: u64,
    len: u64,
}

/// `(mtime_ms, len)` from an already-enumerated directory entry. Uses
/// `DirEntry::metadata` rather than a fresh `fs::metadata` path lookup: on
/// Windows the values come from the directory enumeration the caller is already
/// paying for, so a candidate costs no extra file open.
fn entry_meta(entry: &fs::DirEntry) -> Option<(u64, u64)> {
    let m = entry.metadata().ok()?;
    let ms = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some((ms, m.len()))
}

/// Every claude session file, as metadata-only candidates.
///
/// #457: routes through the SAME testable root lookup `find_claude_session_cwd`
/// already uses, rather than a second, untestable `dirs::home_dir()` inline
/// (the pre-existing gap that made this function unable to honor
/// `set_claude_projects_root_for_test` at all).
///
/// #493: a file whose metadata can't be read at all is skipped rather than
/// listed with a zero timestamp. Pre-#493 such a file was listed (with
/// `modified_ms: 0`, so dead last in the sort) — which in practice meant it was
/// dropped by the same 300-row truncate anyway on any store big enough for the
/// distinction to be reachable.
fn collect_claude_candidates(out: &mut Vec<Candidate>) {
    let Some(root) = claude_projects_root() else {
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
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
                continue;
            };
            let Some((modified_ms, len)) = entry_meta(&file) else {
                continue;
            };
            out.push(Candidate { path, source: "claude", id: Some(id), modified_ms, len });
        }
    }
}

/// Every copilot session directory, as metadata-only candidates. One
/// `fs::metadata` per session directory (the `workspace.yaml` inside it, whose
/// mtime is the session's timestamp — same file the pre-#493 scan timestamped
/// from), and no read of its contents: a directory with no `workspace.yaml`
/// (session not yet written) drops out here exactly as it used to drop out of
/// `read_copilot_session`.
fn collect_copilot_candidates(out: &mut Vec<Candidate>) {
    let Some(root) = copilot_session_state_root() else {
        return;
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let ws = entry.path().join("workspace.yaml");
        let Ok(m) = fs::metadata(&ws) else {
            continue;
        };
        let modified_ms = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        out.push(Candidate { path: ws, source: "copilot", id: None, modified_ms, len: m.len() });
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

// ---------- launch-intent store: autopilot posture (#456, generalized #457) ----------
//
// #457's corrected premise, stated here because the next person to touch
// this file should read it before touching restore paths again: replaying a
// loomux-RECORDED launch command (the tab-restore path, `panerestore.ts`'s
// `agentResumeCommand`/`agentFreshCommand`) is NOT the anti-pattern — it
// carries every flag baked into that command forward by construction, so
// the *next* per-CLI launch semantic added there survives for free. The
// actual anti-pattern, and the one #456's investigation actually named, is
// `scan_claude`/`scan_copilot` RECONSTRUCTING a resume command from the CLI
// VENDOR'S OWN session files — which cannot know what loomux originally
// launched with, because that information never lived there. This module is
// loomux's own record of launch intent, captured at the one moment that
// information exists (launch time), so the scanners can re-derive it
// instead of guessing from a foreign source.
//
// Before this generalization, `scan_claude` carried NO record at all: every
// Sessions-tab resume of a claude session emitted a bare `claude --resume
// <id>`, unconditionally — not "one flag lost" like the copilot case #456
// diagnosed, but every flag, always. That was never identified as its own
// bug before this file's #457 pass.
//
// Keyed two ways, chosen per CLI by what loomux actually knows at launch:
//
//   - `IntentKey::Session` — claude solo panes always mint a session id
//     before launch (`launcher.ts`), so the record can key on it exactly.
//     A session id is unique by construction (no code path mints the same
//     id for two different launches), so a `Session`-keyed entry can NEVER
//     become `Conflicted` — see `record_claude_launch_posture_impl`, and
//     `session_keyed_entries_are_never_conflicted` for the pin. This makes
//     claude strictly better off than copilot here, and retires — for the
//     claude case only — the eviction-ambiguity residual #460 documented as
//     a follow-up for this issue.
//   - `IntentKey::Cwd` — copilot solo panes never get an id at launch (it
//     mints its own, invisibly, and `spawn_copilot_session_watcher` in
//     orchestration/mod.rs learns one after the fact only for GROUP
//     agents), so this reuses #460's original cwd-keyed, conflict-tracked
//     machinery verbatim, just moved under this wider key type. Precise
//     per-session keying for copilot solo is still tracked as further
//     follow-up, not attempted here — it needs the same class of watcher
//     machinery #460 already deferred.
//
// THE RULE THIS MODULE ENFORCES (#460, now binding on BOTH key shapes): on a
// permission decision, ambiguity resolves to the smaller grant, never the
// larger one — and that includes under STORE PRESSURE (review B1) and
// across FILESYSTEM CASE SENSITIVITY (review B2) for the cwd-keyed half, not
// just in the ordinary lookup. Because a `Cwd` key is only a cwd, TWO
// copilot sessions launched in the same folder at different times can
// disagree (toggle on, then later off, or vice versa) — cwd alone can't
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
// without it is not something they could ever notice or undo. A claude
// session with NO recorded intent (foreign, pre-upgrade with no migrated
// record, or evicted) resolves the same way, to `None` — flags only where
// there is a recorded intent saying so, never by inference, never by
// default. This is a genuine behavior WIDENING versus before this PR (a
// claude Sessions-tab restore previously carried no flags ever; now it can,
// where — and only where — loomux itself recorded that it should).
//
// Conflict is derived and stored AT WRITE TIME, as one sticky enum value per
// key (`Posture::Conflicted`) — never re-derived at read time from a list of
// raw records. This is what makes the guarantee survive eviction (review
// B1, caught with a runnable counter-test against the original flat
// per-write log: capping-and-evicting individual records could drop the
// OFF half of a conflict and leave a lone ON record, silently flipping a
// permanently-ambiguous cwd back to granting autopilot). With one entry per
// key, the cap counts ENTRIES (of either key shape, one shared pool) and
// evicts the least-recently-TOUCHED one whole — "touched" meaning written OR
// re-confirmed, so an actively-used folder/session is never the eviction
// target — and eviction can only ever move a key from {True | False |
// Conflicted} to NO RECORD, which resolves to `None` right alongside every
// other "nothing to go on" case. There is no path from eviction to a larger
// grant than the key already had.
//
// SOFT MIGRATION (#457): this store's file was `copilot-posture.json`
// (copilot-only, cwd-only) before this PR. `load_launch_intent` below reads
// that file, read-only, EXACTLY ONCE — only when the new file has never
// been written on this machine — and folds its entries in as `Cwd`-keyed
// copilot records. A cold reset (ignore the old file, start empty) would be
// safe by the same "no record → no flags" rule above, but it would also
// silently re-inflict the exact annoyance #456 was filed to fix ("I have to
// toggle autopilot manually, per folder") on the very release that fixes
// it — a bad trade for a few lines of migration code, so this reads the old
// file instead of resetting.
//
// Migration does NOT re-derive the platform key (`posture_key`/
// `posture_key_for`) — it copies each legacy entry's `cwd` field VERBATIM
// into the new store (see `load_launch_intent`'s match arm below). This is
// deliberate, not an oversight: `cfg!(windows)` is a COMPILE-TIME constant
// baked into one built binary, so a single loomux install's write-time
// keying and read-time keying (migration or ordinary lookup, doesn't
// matter) are always performed by the SAME code in the SAME process —
// there is no runtime path where they could disagree on which arm to use.
// The only way a legacy key and a lookup could apply DIFFERENT arms is the
// underlying file traveling between a case-folding (Windows) and a
// case-sensitive (macOS/Linux) install — and `data_root()` resolves to
// `dirs::data_dir()`, an OS-NATIVE per-machine path (`%APPDATA%` vs.
// `~/Library/Application Support` vs. XDG) that does not coincide across
// platforms by default; getting two different-OS installs to share this
// file at all requires deliberately pointing `LOOMUX_DATA_DIR` at a synced
// location on both. That scoping property is #460's, unchanged by this PR:
// the original single-arm-per-store design never supported cross-platform
// key portability, migration or not, and a verbatim-copy migration can't
// make that any better OR any worse than it already was — it only has to
// preserve whatever a same-binary write already produced, which it does by
// construction. Pinned (not just asserted) by
// `soft_migration_preserves_the_legacy_cwd_key_exactly_regardless_of_which_
// platform_wrote_it`, mutation-verified against re-normalizing a second
// time (see that test's own doc comment for why a same-host test alone
// can't catch that mutation).
const LAUNCH_INTENT_CAP: usize = 300;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Posture {
    True,
    False,
    /// Sticky: once a key sees both `True` and `False` writes, it stays
    /// `Conflicted` forever (until evicted entirely) — never flips back to a
    /// single value no matter what's written or evicted afterward. Only
    /// ever reached via `IntentKey::Cwd` — see the module doc's claim that a
    /// `Session`-keyed entry can never become this.
    Conflicted,
}

/// What a launch-intent entry is keyed by — chosen per CLI, see the module
/// doc above for why each CLI gets the shape it does.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum IntentKey {
    /// claude solo: the session id minted at launch (`launcher.ts`) — exact,
    /// no ambiguity possible.
    Session { id: String },
    /// copilot solo: the launch cwd, normalized via `posture_key` — #460's
    /// original keying, reused verbatim under this wider enum.
    Cwd { cli: String, cwd: String },
}

#[derive(Clone, Serialize, Deserialize)]
struct LaunchIntentEntry {
    key: IntentKey,
    autopilot: Posture,
    /// Bumped on every write to this key, including a repeat of the same
    /// value — this is what "touched" means for LRU eviction, not merely
    /// "created".
    touched_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct LaunchIntentStore {
    entries: Vec<LaunchIntentEntry>,
}

/// The pre-#457 shape of this store: copilot-only, cwd-only, no `kind` tag.
/// Read-only migration source for `load_launch_intent` — never written
/// again once `launch-intent.json` exists. Field names match the original
/// exactly so `serde_json` can parse a real pre-upgrade file on disk.
#[derive(Deserialize)]
struct LegacyCopilotPostureEntry {
    cwd: String,
    posture: Posture,
    touched_ms: u64,
}

#[derive(Default, Deserialize)]
struct LegacyCopilotPostureStore {
    entries: Vec<LegacyCopilotPostureEntry>,
}

thread_local! {
    /// Test seam for `launch_intent_path()`, same thread-scoping rationale
    /// as `COPILOT_SESSION_STATE_ROOT_OVERRIDE` above — a real env-var
    /// override would race a concurrently-running test on another thread.
    static LAUNCH_INTENT_PATH_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    /// Test seam for `legacy_copilot_posture_path()` — the pre-#457 file
    /// `load_launch_intent`'s soft migration reads from. Separate cell from
    /// the one above: a migration test needs to fixture BOTH paths
    /// independently (new file absent, old file present with content).
    static LEGACY_COPILOT_POSTURE_PATH_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam: fixture the file `launch_intent_path()` returns, for the
/// calling thread only. See `set_claude_projects_root_for_test` for why.
#[doc(hidden)] // pub for integration tests
pub fn set_launch_intent_path_for_test(path: Option<PathBuf>) {
    LAUNCH_INTENT_PATH_OVERRIDE.with(|c| *c.borrow_mut() = path);
}

/// Test-only seam: fixture the file `legacy_copilot_posture_path()` returns,
/// for the calling thread only — see `set_launch_intent_path_for_test`.
#[doc(hidden)] // pub for integration tests
pub fn set_legacy_copilot_posture_path_for_test(path: Option<PathBuf>) {
    LEGACY_COPILOT_POSTURE_PATH_OVERRIDE.with(|c| *c.borrow_mut() = path);
}

fn launch_intent_path() -> PathBuf {
    if let Some(p) = LAUNCH_INTENT_PATH_OVERRIDE.with(|c| c.borrow().clone()) {
        return p;
    }
    crate::obs::data_root().join("launch-intent.json")
}

/// The pre-#457 store this module's soft migration reads from, read-only —
/// see the module doc's "SOFT MIGRATION" section.
fn legacy_copilot_posture_path() -> PathBuf {
    if let Some(p) = LEGACY_COPILOT_POSTURE_PATH_OVERRIDE.with(|c| c.borrow().clone()) {
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

/// Best-effort load: a missing NEW-file-and-no-legacy-file is "no history
/// yet" (empty store); a corrupt new file is quarantined (via
/// `uistate::load_or_quarantine`, the same fail-safe `tabs.json`/
/// `settings.json` use) and treated as empty too — a lost intent history
/// degrades to the safe "no record" behavior below, never a crash or a
/// stale grant.
///
/// SOFT MIGRATION (#457, see module doc): when `launch-intent.json` has
/// never been written on this machine, read `copilot-posture.json` instead
/// — read-only, and only on this "new file doesn't exist yet" branch, never
/// as a fallback for a new file that exists but failed to parse (that stays
/// "quarantine and start empty", the same as every other store here — a
/// corrupt CURRENT file must never resurrect a possibly-stale legacy one).
/// The very next `record_*_launch_posture` write lands on the new path, so
/// this branch is taken at most once per machine.
fn load_launch_intent() -> LaunchIntentStore {
    let path = launch_intent_path();
    if path.exists() {
        return crate::uistate::load_or_quarantine(&path)
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
    }
    let legacy: Option<LegacyCopilotPostureStore> =
        crate::uistate::load_or_quarantine(&legacy_copilot_posture_path())
            .and_then(|raw| serde_json::from_str(&raw).ok());
    match legacy {
        Some(store) => LaunchIntentStore {
            entries: store
                .entries
                .into_iter()
                .map(|e| LaunchIntentEntry {
                    key: IntentKey::Cwd { cli: "copilot".to_string(), cwd: e.cwd },
                    autopilot: e.posture,
                    touched_ms: e.touched_ms,
                })
                .collect(),
        },
        None => LaunchIntentStore::default(),
    }
}

/// Evict the least-recently-touched entries wholesale (never a partial
/// record of one) until back at `LAUNCH_INTENT_CAP` — shared by both write
/// paths (claude session-keyed, copilot cwd-keyed) so the cap is one shared
/// pool, not tracked separately per key shape.
fn cap_and_evict(store: &mut LaunchIntentStore) {
    if store.entries.len() > LAUNCH_INTENT_CAP {
        store.entries.sort_by_key(|e| e.touched_ms);
        let excess = store.entries.len() - LAUNCH_INTENT_CAP;
        store.entries.drain(0..excess);
    }
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
/// section's ambiguity rule. Best-effort and capped (review B1: one entry
/// per key, LRU-evicted whole, never a flat log of individual writes) — this
/// is a convenience record, not a durable-correctness store, and must never
/// block or fail a launch.
#[tauri::command]
pub fn record_copilot_launch_posture(cwd: String, autopilot: bool) -> Result<(), String> {
    record_copilot_launch_posture_impl(&cwd, autopilot)
}

fn record_copilot_launch_posture_impl(cwd: &str, autopilot: bool) -> Result<(), String> {
    let mut store = load_launch_intent();
    let key = IntentKey::Cwd { cli: "copilot".to_string(), cwd: posture_key(cwd) };
    let now = now_ms();
    let incoming = if autopilot { Posture::True } else { Posture::False };
    match store.entries.iter_mut().find(|e| e.key == key) {
        Some(entry) => {
            // Already conflicted stays conflicted; a fresh disagreement
            // BECOMES conflicted; agreement just refreshes the touch time.
            if entry.autopilot != incoming {
                entry.autopilot = Posture::Conflicted;
            }
            entry.touched_ms = now;
        }
        None => store.entries.push(LaunchIntentEntry { key, autopilot: incoming, touched_ms: now }),
    }
    cap_and_evict(&mut store);
    let body = serde_json::to_string(&store).map_err(|e| e.to_string())?;
    crate::uistate::write_atomic(&launch_intent_path(), &body)
}

/// Record what the Autopilot toggle was set to for a solo CLAUDE launch that
/// minted `session_id` (#457) — claude's half of the module doc's launch-
/// intent record, keyed exactly (no ambiguity, ever) instead of by cwd.
/// Same best-effort contract as the copilot command above. A blank id is a
/// no-op: nothing reliable to key on (mirrors the copilot command's cwd
/// guard, `posture_in`'s empty-string check).
#[tauri::command]
pub fn record_claude_launch_posture(session_id: String, autopilot: bool) -> Result<(), String> {
    record_claude_launch_posture_impl(&session_id, autopilot)
}

fn record_claude_launch_posture_impl(session_id: &str, autopilot: bool) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Ok(());
    }
    let mut store = load_launch_intent();
    let key = IntentKey::Session { id: session_id.to_string() };
    let now = now_ms();
    let incoming = if autopilot { Posture::True } else { Posture::False };
    match store.entries.iter_mut().find(|e| e.key == key) {
        // Deliberately NEVER sets `Conflicted` here, unlike the cwd-keyed
        // branch above: a session id is unique by construction (no code
        // path mints the same id for two different launches), so two writes
        // for the same key can only be a repeat of the SAME launch's own
        // record — there is no legitimate disagreement to detect. A repeat
        // write just overwrites the value and refreshes the touch time.
        // This is what makes a `Session`-keyed entry provably never
        // `Conflicted` — see `session_keyed_entries_are_never_conflicted`.
        Some(entry) => {
            entry.autopilot = incoming;
            entry.touched_ms = now;
        }
        None => store.entries.push(LaunchIntentEntry { key, autopilot: incoming, touched_ms: now }),
    }
    cap_and_evict(&mut store);
    let body = serde_json::to_string(&store).map_err(|e| e.to_string())?;
    crate::uistate::write_atomic(&launch_intent_path(), &body)
}

/// The recorded posture for `key`, per the ambiguity rule documented above
/// the module section: `Some(v)` only when this key's stored posture is
/// unambiguously `v`; `None` (no flags) when there is no record at all, OR
/// the stored posture is `Conflicted`. Takes an already-loaded store so a
/// caller resolving many sessions in one pass (`scan_claude`/`scan_copilot`)
/// reads the file once, not once per session (review NB3).
fn intent_for(store: &LaunchIntentStore, key: &IntentKey) -> Option<bool> {
    match store.entries.iter().find(|e| &e.key == key)?.autopilot {
        Posture::True => Some(true),
        Posture::False => Some(false),
        Posture::Conflicted => None,
    }
}

/// `intent_for` for copilot's cwd-keyed half — applies the same `posture_key`
/// normalization at lookup time that `record_copilot_launch_posture_impl`
/// applies at write time (must stay the same normalization on both sides,
/// or a write and its own later lookup could silently disagree). An empty
/// cwd never matches (review B2's empty-key guard, carried forward).
fn copilot_posture_in(store: &LaunchIntentStore, cwd: &str) -> Option<bool> {
    let want = posture_key(cwd);
    if want.is_empty() {
        return None;
    }
    intent_for(store, &IntentKey::Cwd { cli: "copilot".to_string(), cwd: want })
}

/// `intent_for` for claude's session-keyed half. An empty id never matches,
/// mirroring `copilot_posture_in`'s empty-cwd guard.
fn claude_posture_in(store: &LaunchIntentStore, session_id: &str) -> Option<bool> {
    if session_id.trim().is_empty() {
        return None;
    }
    intent_for(store, &IntentKey::Session { id: session_id.to_string() })
}

/// `copilot_posture_in`, loading the store fresh — for the (rare,
/// single-lookup) caller that doesn't already have one loaded. `scan_copilot`
/// below loads once and calls `copilot_posture_in` directly instead.
fn copilot_launch_posture(cwd: &str) -> Option<bool> {
    copilot_posture_in(&load_launch_intent(), cwd)
}

/// The Sessions-tab resume command for `cli`/`session_id`, re-deriving
/// loomux's own recorded launch intent (#457) instead of reconstructing from
/// the CLI's own session files — see the module doc's corrected premise.
/// `cwd` is only consulted for `cli == "copilot"` (the one CLI keyed by cwd
/// rather than session id — see `IntentKey`'s doc). No record (never
/// launched by loomux, evicted, or genuinely conflicting history) → bare
/// resume, never a guess — #460's rule, now enforced identically for both
/// CLIs. Reuses the SAME flag atoms a fresh launch builds
/// (`single_pane_autopilot_flags`/`COPILOT_GROUP_AUTOPILOT_FLAGS`) — one
/// seam, never a second copy that could drift.
fn build_resume_command(cli: &str, session_id: &str, cwd: &str, store: &LaunchIntentStore) -> String {
    match cli {
        "copilot" => {
            let base = format!("copilot --resume={session_id}"); // #458: `=` form, untouched by #457
            if copilot_posture_in(store, cwd) == Some(true) {
                format!("{base} {}", crate::orchestration::COPILOT_GROUP_AUTOPILOT_FLAGS)
            } else {
                base
            }
        }
        _ => {
            let base = format!("claude --resume {session_id}");
            if claude_posture_in(store, session_id) == Some(true) {
                format!("{base} {}", crate::orchestration::single_pane_autopilot_flags("claude"))
            } else {
                base
            }
        }
    }
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

/// Most rows `list_sessions` returns — and, since #493, also the scan's PARSE
/// BUDGET. The cap itself is not new (the pre-#493 scan sorted by mtime and
/// truncated to the same 300); what's new is that the rows beyond it are no
/// longer parsed first and thrown away.
///
/// `pub` so the #493 tests can pin "parses are bounded by the row limit" against
/// the limit itself rather than against a hard-coded 300 that would silently
/// stop meaning anything if this changed.
pub const LIST_LIMIT: usize = 300;

/// What one session file's head-parse yielded — everything that depends on the
/// file's CONTENT and nothing that doesn't (see `to_session_info` for the
/// deliberately-not-cached derivations).
struct Parsed {
    id: String,
    title: String,
    cwd: String,
    /// Orchestration role detected in the transcript, and the group id the
    /// transcript itself named (a kickoff). `orch_role: Some`/`orch_gid: None`
    /// is the notice-only detection — role known, group not stated.
    orch_role: Option<String>,
    orch_gid: Option<String>,
}

/// One session's cached head-parse (#493). Keyed by the session file's own
/// path; validated by `(modified_ms, len)`, so an appended-to or replaced
/// transcript is re-parsed and only a byte-for-byte-unchanged file is trusted.
///
/// What is deliberately NOT in here: `resume_command` and the on-disk group
/// check. Both are derived from state that changes independently of the
/// transcript (loomux's launch-intent record, a group directory that can be
/// deleted), so caching them would let this file answer with something that was
/// true once. They're re-derived on every scan instead — see `to_session_info`.
#[derive(Serialize, Deserialize, Clone)]
struct IndexEntry {
    /// Lossy-stringified for JSON. A path that doesn't survive that round trip
    /// (not reachable through Windows' own UTF-16 paths in practice) simply never
    /// matches its candidate again, so that one session is re-parsed every scan —
    /// a cost, never a wrong row.
    path: String,
    modified_ms: u64,
    len: u64,
    id: String,
    title: String,
    cwd: String,
    #[serde(default)]
    orch_role: Option<String>,
    #[serde(default)]
    orch_gid: Option<String>,
}

/// The persisted index. `version` is a hard gate, not a hint: a file written by
/// a different shape of this struct is discarded wholesale rather than
/// partially trusted, which is what makes adding a field to `IndexEntry` later
/// a safe one-line change instead of a migration.
#[derive(Default, Serialize, Deserialize)]
struct SessionIndex {
    version: u32,
    entries: Vec<IndexEntry>,
}

const SESSION_INDEX_VERSION: u32 = 1;

thread_local! {
    /// Test seam for `session_index_path()`, same thread-scoping rationale as
    /// `LAUNCH_INTENT_PATH_OVERRIDE` — and doubly necessary here: a test that
    /// scanned against the REAL index would both read another run's cached rows
    /// and write its fixture's rows back over the developer's own file.
    static SESSION_INDEX_PATH_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam: fixture the file the session index lives in, for the calling
/// thread only. See `set_claude_projects_root_for_test` for why this is a real
/// `pub` function rather than `#[cfg(test)]`.
#[doc(hidden)] // pub for integration tests
pub fn set_session_index_path_for_test(path: Option<PathBuf>) {
    SESSION_INDEX_PATH_OVERRIDE.with(|c| *c.borrow_mut() = path);
}

fn session_index_path() -> PathBuf {
    if let Some(p) = SESSION_INDEX_PATH_OVERRIDE.with(|c| c.borrow().clone()) {
        return p;
    }
    crate::obs::data_root().join("session-index.json")
}

/// Best-effort load, keyed by path for O(1) lookup during the scan. Every
/// failure mode — absent, corrupt, or written by another version — degrades to
/// an empty index, i.e. "parse everything this once", never to a wrong answer.
/// A corrupt file is quarantined by `load_or_quarantine`, the same fail-safe
/// `tabs.json`/`launch-intent.json` use.
fn load_session_index() -> HashMap<String, IndexEntry> {
    let path = session_index_path();
    let Some(raw) = crate::uistate::load_or_quarantine(&path) else {
        return HashMap::new();
    };
    let Ok(index) = serde_json::from_str::<SessionIndex>(&raw) else {
        return HashMap::new();
    };
    if index.version != SESSION_INDEX_VERSION {
        return HashMap::new();
    }
    index.entries.into_iter().map(|e| (e.path.clone(), e)).collect()
}

/// Persist the index, atomically (`write_atomic`: a crash mid-write leaves the
/// old valid file, never a truncated one — the #133 hazard).
///
/// Entries are sorted by path so the serialized bytes are a function of the
/// content alone, which is what lets the caller skip the write entirely when
/// nothing changed. Best-effort: a failed write costs the NEXT scan its cache,
/// nothing more, so it is never surfaced as an error to the UI.
fn save_session_index(entries: Vec<IndexEntry>) {
    let mut entries = entries;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let index = SessionIndex { version: SESSION_INDEX_VERSION, entries };
    if let Ok(json) = serde_json::to_string(&index) {
        let _ = crate::uistate::write_atomic(&session_index_path(), &json);
    }
}

/// Read one candidate's head. `None` drops the row: a copilot directory whose
/// `workspace.yaml` is gone or carries no `id` (exactly the pre-#493
/// `read_copilot_session` behavior). A claude row is never dropped here — an
/// unreadable jsonl yields the same "(no prompt)" row it did before, since its
/// id comes from the filename and needs no parse at all.
///
/// A dropped candidate gets no index entry, so it is re-read on every scan.
/// That's deliberate: caching "this wasn't parseable" would mean a session
/// finishing its `workspace.yaml` write stayed invisible until something else
/// changed. The cost is bounded by how many malformed files exist.
fn parse_candidate(c: &Candidate) -> Option<Parsed> {
    if c.source == "claude" {
        let (title, cwd, orch) = scan_claude_jsonl(&c.path);
        let (orch_role, orch_gid) = match orch {
            Some((role, gid)) => (Some(role), gid),
            None => (None, None),
        };
        return Some(Parsed { id: c.id.clone()?, title, cwd, orch_role, orch_gid });
    }
    let s = read_copilot_session(c.path.parent()?)?;
    Some(Parsed {
        id: s.id,
        title: tidy_title(&s.title, 90),
        cwd: s.cwd,
        // Copilot transcripts carry no loomux kickoff to detect a role from —
        // the pre-#493 scan set both of these to None for every copilot row too.
        orch_role: None,
        orch_gid: None,
    })
}

/// Build the row the frontend sees from a cached/just-parsed head plus the
/// state that must NOT be cached with it.
///
/// #457: `resume_command` re-derives loomux's own recorded launch intent
/// (autopilot posture, if any unambiguous record exists) via the shared
/// `build_resume_command` — see the module doc above. `=`, never a space,
/// between `--resume` and a copilot id (#458): copilot's CLI reference
/// documents the flag as optional-value — `` `-r`, `--resume[=VALUE]` ``
/// (raw-fetched via
/// `curl -sL https://docs.github.com/api/article/body?pathname=/en/copilot/reference/copilot-cli-reference/cli-command-reference`,
/// grepped for `--resume`) — and the CLI's OWN generated hint after a
/// `-p`/`--prompt` run is spelled the same unambiguous way: "The exit summary
/// includes a `copilot --resume=SESSION-ID` hint for continuing the session."
/// The docs never show `--resume <id>` as a literal invocation (one unrelated
/// prose line pairs `--remote` with `--resume <TASK-ID>` informally, not as a
/// syntax example), so whether the space form is silently mis-parsed by the
/// underlying arg parser is UNVERIFIED rather than confirmed broken.
/// `--resume=<id>` is documented, costs nothing, and can never be misread as a
/// bare `--resume` (its own documented failure mode: an interactive picker, or
/// — where no TTY is available for one — a loud error, never a silent
/// wrong-session attach) plus a stray positional.
fn to_session_info(source: &str, e: &IndexEntry, intent: &LaunchIntentStore) -> SessionInfo {
    // Notice-only detections carry no group id; derive it from the session's
    // cwd, keeping it only if that group exists on disk. #493: this is derived
    // EVERY scan and never cached — a group directory can be deleted while the
    // transcript that named it stays byte-identical, so a cached `orch_group`
    // would keep claiming a group that isn't there any more.
    let (orch_role, orch_group) = match (&e.orch_role, &e.orch_gid) {
        (Some(role), Some(gid)) => (Some(role.clone()), Some(gid.clone())),
        (Some(role), None) if !e.cwd.is_empty() => {
            let gid = crate::orchestration::group_id_for_repo(&e.cwd);
            let exists = crate::orchestration::OrchRegistry::default_root()
                .join(&gid)
                .join("group.json")
                .is_file();
            (Some(role.clone()), exists.then_some(gid))
        }
        (Some(role), None) => (Some(role.clone()), None),
        (None, _) => (None, None),
    };
    SessionInfo {
        resume_command: build_resume_command(source, &e.id, &e.cwd, intent),
        id: e.id.clone(),
        source: source.to_string(),
        title: e.title.clone(),
        cwd: e.cwd.clone(),
        modified_ms: e.modified_ms,
        orch_role,
        orch_group,
    }
}

/// What one scan actually did. Exposed (via `list_sessions_for_test`) so the
/// #493 tests can pin the SHAPE of the work — "no file was opened twice", "the
/// parse count is bounded by the row limit, not by history" — instead of
/// asserting on a wall-clock duration, which would be flaky on CI and would
/// pass for the wrong reason on a fast disk.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    /// Session files found on disk, before the row limit.
    pub files_seen: usize,
    /// Rows returned.
    pub rows: usize,
    /// Files whose head was actually opened and parsed this scan.
    pub parsed: usize,
    /// Files served from the persisted index without being opened.
    pub reused: usize,
}

/// Scan of every recorded Claude/Copilot session on disk, bounded two ways
/// (#493).
///
/// The cost this replaces (issue #342, measured in #493): a machine with a long
/// orchestration history accumulates thousands of `.claude/projects/**/*.jsonl`
/// files across every past project, and the pre-#493 scan opened and head-read
/// EVERY one of them on EVERY scan — 826 files in 13–17s on the machine #493
/// was reported from, for a list that has always shown at most 300 rows.
///
/// Two bounds, in this order:
///
///  1. **Metadata first.** Candidates are collected from directory enumeration
///     alone, sorted by mtime, and cut to `LIST_LIMIT` BEFORE anything is
///     parsed. The rows a growing history adds now cost one `stat` each, not a
///     head-parse — so the scan stops degrading monotonically with history,
///     which was #493's third question.
///  2. **A persisted index.** Each survivor's head-parse is cached in
///     `session-index.json`, keyed by path and validated by `(mtime, len)`, so
///     an unchanged file is never opened again on a later launch. A steady-state
///     launch parses only what actually changed since the last one.
///
/// The row set is unchanged: same "newest 300 by mtime" the pre-#493 scan
/// returned, in the same order (same comparator over the same enumeration
/// order), because sorting on metadata sorts on the identical `modified_ms` the
/// rows carried before.
///
/// A breadcrumb records the timing and the parsed/reused split, so a
/// slow-startup report still has an actual number to point at (and so a
/// regression that quietly stops using the index is visible in the log).
fn scan_sessions() -> (Vec<SessionInfo>, ScanStats) {
    let mut candidates = Vec::new();
    collect_claude_candidates(&mut candidates);
    collect_copilot_candidates(&mut candidates);
    let files_seen = candidates.len();
    candidates.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    candidates.truncate(LIST_LIMIT);

    let cached = load_session_index();
    // #456 review NB3 / #457: one load for the whole scan, not one per session
    // — the store is read-only here, so there's nothing to keep fresh across
    // iterations.
    let intent = load_launch_intent();
    let mut out = Vec::with_capacity(candidates.len());
    let mut fresh: Vec<IndexEntry> = Vec::with_capacity(candidates.len());
    let mut parsed = 0usize;
    let mut reused = 0usize;

    for c in &candidates {
        let key = c.path.to_string_lossy().into_owned();
        let hit = cached
            .get(&key)
            .filter(|e| e.modified_ms == c.modified_ms && e.len == c.len)
            .cloned();
        let entry = match hit {
            Some(e) => {
                reused += 1;
                e
            }
            None => {
                parsed += 1;
                let Some(p) = parse_candidate(c) else {
                    continue;
                };
                IndexEntry {
                    path: key,
                    modified_ms: c.modified_ms,
                    len: c.len,
                    id: p.id,
                    title: p.title,
                    cwd: p.cwd,
                    orch_role: p.orch_role,
                    orch_gid: p.orch_gid,
                }
            }
        };
        out.push(to_session_info(c.source, &entry, &intent));
        fresh.push(entry);
    }

    // Nothing parsed AND the same entry count means the index on disk already
    // equals what we'd write (the only way an entry can differ is by having been
    // re-parsed), so a steady-state launch does no write at all. The index is
    // also self-pruning: it only ever holds the rows this scan returned, so it
    // can't grow past `LIST_LIMIT` however long the history gets.
    if parsed > 0 || fresh.len() != cached.len() {
        save_session_index(fresh);
    }

    let stats = ScanStats { files_seen, rows: out.len(), parsed, reused };
    (out, stats)
}

/// Test-only entry point: the scan plus the stats a #493 test asserts on,
/// synchronously on the CALLING thread — which is also what makes the
/// thread-local test seams (`set_claude_projects_root_for_test`,
/// `set_session_index_path_for_test`, …) apply to it at all, unlike the
/// `spawn_blocking` production path below.
#[doc(hidden)] // pub for integration tests
pub fn list_sessions_for_test() -> (Vec<SessionInfo>, ScanStats) {
    scan_sessions()
}

fn list_sessions_sync() -> Vec<SessionInfo> {
    let start = std::time::Instant::now();
    let (sessions, s) = scan_sessions();
    crate::obs::breadcrumb(
        "startup",
        &format!(
            "list_sessions: {} file(s) seen, {} listed ({} parsed, {} from index) in {:?}",
            s.files_seen,
            s.rows,
            s.parsed,
            s.reused,
            start.elapsed()
        ),
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
mod launch_intent_tests {
    // #456/#457: the launch-intent record (claude session-keyed + copilot
    // cwd-keyed) + the ambiguity rule it feeds `scan_claude`/`scan_copilot`.
    // Each test binds BOTH `set_launch_intent_path_for_test` (the new store)
    // and `set_legacy_copilot_posture_path_for_test` (the migration source)
    // to fresh, not-yet-existing files inside a private tempdir, so tests
    // never share (or race on) the real `<data root>` files, and never
    // silently trigger a migration read against the real
    // `copilot-posture.json` on the machine running the suite.
    use super::{
        claude_posture_in, copilot_launch_posture, load_launch_intent, posture_key_for,
        record_claude_launch_posture_impl, record_copilot_launch_posture_impl, scan_sessions,
        set_claude_projects_root_for_test, set_copilot_session_state_root_for_test,
        set_launch_intent_path_for_test, set_legacy_copilot_posture_path_for_test,
        set_session_index_path_for_test, IntentKey, Posture, SessionInfo,
    };
    use std::fs;

    /// `claude_posture_in`, loading the store fresh — the claude-side
    /// counterpart of `copilot_launch_posture`, test-only (no production
    /// caller needs a fresh-load single lookup for claude; `scan_claude`
    /// loads once for the whole scan like `scan_copilot` does).
    #[cfg(test)]
    fn claude_launch_posture(session_id: &str) -> Option<bool> {
        claude_posture_in(&load_launch_intent(), session_id)
    }

    /// Bind the launch-intent store's test seams to fresh, not-yet-existing
    /// files inside a fresh tempdir (both the new store AND the legacy
    /// migration source, so a test that isn't specifically about migration
    /// starts from a deterministic "no record anywhere" state), and return
    /// the guard the caller must hold to keep the tempdir alive.
    fn posture_seam() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        set_launch_intent_path_for_test(Some(d.path().join("launch-intent.json")));
        set_legacy_copilot_posture_path_for_test(Some(d.path().join("copilot-posture.json")));
        // #493: the scan now consults a persisted index. Bound to this tempdir
        // too, so a scan test neither reads another run's cached rows nor writes
        // its fixtures over the developer's real `session-index.json`.
        set_session_index_path_for_test(Some(d.path().join("session-index.json")));
        d
    }

    fn clear_seam() {
        set_launch_intent_path_for_test(None);
        set_legacy_copilot_posture_path_for_test(None);
        set_session_index_path_for_test(None);
    }

    /// The rows the real `list_sessions` would return, for the scan tests
    /// below. #493 merged the per-CLI scans into one pass, so each of these
    /// tests must also fixture the OTHER CLI's root to an empty tempdir (they
    /// do) — otherwise the scan would walk the developer's real
    /// `~/.claude`/`~/.copilot` history, which is slow, non-deterministic, and
    /// big enough to push the test's own fixtures past the row limit.
    fn scan_rows() -> Vec<SessionInfo> {
        scan_sessions().0
    }

    #[test]
    fn no_record_is_none() {
        let _d = posture_seam();
        assert_eq!(copilot_launch_posture("C:/work/x"), None);
        assert_eq!(claude_launch_posture("some-session-id"), None, "same rule for claude's session key");
        clear_seam();
    }

    #[test]
    fn a_single_recorded_value_round_trips() {
        let _d = posture_seam();
        record_copilot_launch_posture_impl("C:/work/x", true).unwrap();
        assert_eq!(copilot_launch_posture("C:/work/x"), Some(true));

        record_copilot_launch_posture_impl("C:/work/y", false).unwrap();
        assert_eq!(copilot_launch_posture("C:/work/y"), Some(false));
        clear_seam();
    }

    #[test]
    fn claude_session_key_round_trips() {
        let _d = posture_seam();
        record_claude_launch_posture_impl("sess-a", true).unwrap();
        assert_eq!(claude_launch_posture("sess-a"), Some(true));

        record_claude_launch_posture_impl("sess-b", false).unwrap();
        assert_eq!(claude_launch_posture("sess-b"), Some(false));
        // Distinct ids never collide, unlike two copilot sessions sharing a cwd.
        assert_eq!(claude_launch_posture("sess-a"), Some(true), "sess-b's write must not disturb sess-a");
        clear_seam();
    }

    /// THE property the orchestrator asked to see pinned explicitly (design
    /// intake, #457): a `Session`-keyed entry can NEVER become `Conflicted`,
    /// because a session id is unique by construction — two writes for the
    /// SAME id can only be a repeat of the same launch's own record, never a
    /// genuine disagreement between two different sessions the way two
    /// copilot launches can disagree in the same cwd. A disagreeing repeat
    /// write (which should never happen in practice, since launcher.ts
    /// writes each minted id's record exactly once) still resolves to the
    /// LATEST value, never to `None` — proving by observation that this key
    /// shape has no ambiguous state to fall into, unlike `conflicting_
    /// records_for_the_same_cwd_resolve_to_none_not_latest` below.
    #[test]
    fn session_keyed_entries_are_never_conflicted() {
        let _d = posture_seam();
        record_claude_launch_posture_impl("sess-x", true).unwrap();
        record_claude_launch_posture_impl("sess-x", false).unwrap();
        assert_eq!(
            claude_launch_posture("sess-x"),
            Some(false),
            "a repeat write for the SAME session id overwrites (last write wins) — it must never \
             resolve to None the way a genuinely ambiguous cwd-keyed record does"
        );
        clear_seam();
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
        clear_seam();
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
        clear_seam();
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
        clear_seam();
    }

    #[test]
    fn store_caps_and_evicts_the_least_recently_touched_cwd() {
        let _d = posture_seam();
        // One past the cap, each a distinct cwd so none collide/cancel out.
        for i in 0..=super::LAUNCH_INTENT_CAP {
            record_copilot_launch_posture_impl(&format!("C:/work/{i}"), true).unwrap();
        }
        let store = super::load_launch_intent();
        assert_eq!(store.entries.len(), super::LAUNCH_INTENT_CAP, "must stay capped, not grow unbounded — one entry per cwd");
        // The very first recorded (cwd "C:/work/0") was never touched again — evicted.
        assert_eq!(copilot_launch_posture("C:/work/0"), None);
        // The most recently touched survives.
        assert_eq!(copilot_launch_posture(&format!("C:/work/{}", super::LAUNCH_INTENT_CAP)), Some(true));
        clear_seam();
    }

    #[test]
    fn re_touching_a_cwd_protects_it_from_eviction() {
        // A repeat write of the SAME value must count as a touch (bumping
        // eviction priority), not merely dedupe silently — otherwise an
        // actively-relaunched folder could still be evicted ahead of one
        // nobody has opened in months, which defeats the point of LRU.
        let _d = posture_seam();
        record_copilot_launch_posture_impl("C:/work/active", true).unwrap();
        for i in 0..super::LAUNCH_INTENT_CAP {
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
        clear_seam();
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
        for i in 0..super::LAUNCH_INTENT_CAP - 1 {
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
            for i in 0..super::LAUNCH_INTENT_CAP {
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
        clear_seam();
    }

    /// THE property the module doc states but, until this test, nothing
    /// pinned: **eviction is one shared pool across BOTH key shapes**
    /// (`cap_and_evict` sorts on `touched_ms` alone — it has no idea
    /// `IntentKey::Session` and `IntentKey::Cwd` are different variants).
    /// Every eviction test above (`store_caps_and_evicts_the_least_recently_
    /// touched_cwd`, `re_touching_a_cwd_protects_it_from_eviction`,
    /// `conflicted_cwd_never_yields_flags_no_matter_how_much_other_activity_
    /// follows`) only ever populated ONE key shape at a time — none of them
    /// could have caught a shape-biased eviction change.
    ///
    /// This matters specifically, not generically: **eviction is exactly
    /// where #460's own B1 finding broke this store's guarantee before** —
    /// cap eviction silently un-conflicting a directory into a grant, caught
    /// only by a runnable counter-test that drove the store past its cap and
    /// asserted the SPECIFIC value the broken design produced. #457 widened
    /// the key space eviction operates over; leaving the mixed-shape case
    /// unpinned in the PR that does the widening is exactly how a future
    /// "prefer evicting session keys" (or the reverse) optimization changes
    /// LRU behavior with nothing going red.
    ///
    /// Both directions, because a one-directional pin could pass by
    /// accident (e.g. a mutation that only biases eviction ONE way):
    /// an old `Session` entry evicted by a volume of fresh `Cwd` writes, and
    /// an old `Cwd` entry evicted by a volume of fresh `Session` writes —
    /// each proving eviction crosses the shape boundary, not merely that it
    /// works within one shape.
    ///
    /// Mutation-verified: sorting eviction by `(is_session, touched_ms)`
    /// instead of `touched_ms` alone — biasing eviction toward `Session`
    /// entries regardless of freshness, the exact "optimization" this test
    /// exists to catch — leaves the FIRST direction's assertion accidentally
    /// still true (the lone session entry gets evicted either way) but makes
    /// the SECOND direction's assertion fail: the old `Cwd` entry survives
    /// (a stale grant kept alive) while a freshly-written `Session` entry is
    /// evicted instead. This is exactly why both directions are asserted,
    /// not one.
    #[test]
    fn mixed_key_shapes_share_one_eviction_pool() {
        let _d = posture_seam();
        // Direction 1: one old Session entry, then enough fresh Cwd writes
        // to push the store one past the cap.
        record_claude_launch_posture_impl("sess-old", true).unwrap();
        for i in 0..super::LAUNCH_INTENT_CAP {
            record_copilot_launch_posture_impl(&format!("C:/work/{i}"), true).unwrap();
        }
        assert_eq!(
            claude_launch_posture("sess-old"),
            None,
            "a Session-keyed entry must be evictable under Cwd-keyed pressure — one shared pool, \
             not a separate, effectively-uncapped bucket per key shape"
        );
        assert_eq!(
            copilot_launch_posture(&format!("C:/work/{}", super::LAUNCH_INTENT_CAP - 1)),
            Some(true),
            "the newest Cwd entry must survive — eviction removed exactly the oldest overall"
        );
        clear_seam();

        // Direction 2: the reverse — one old Cwd entry, then enough fresh
        // Session writes to push the store one past the cap.
        let _d2 = posture_seam();
        record_copilot_launch_posture_impl("C:/work/old", true).unwrap();
        for i in 0..super::LAUNCH_INTENT_CAP {
            record_claude_launch_posture_impl(&format!("sess-{i}"), true).unwrap();
        }
        assert_eq!(
            copilot_launch_posture("C:/work/old"),
            None,
            "same property, opposite direction — a Cwd-keyed entry must be evictable under \
             Session-keyed pressure"
        );
        assert_eq!(
            claude_launch_posture(&format!("sess-{}", super::LAUNCH_INTENT_CAP - 1)),
            Some(true),
            "the newest Session entry must survive"
        );
        clear_seam();
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
        // Empty claude root: this test is about copilot rows, and #493's single
        // scan pass would otherwise reach the real `~/.claude` (see `scan_rows`).
        let claude_root = tempfile::tempdir().unwrap();
        set_claude_projects_root_for_test(Some(claude_root.path().to_path_buf()));
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

        let out = scan_rows();
        let by_id = |id: &str| out.iter().find(|s| s.id == id).unwrap();

        assert_eq!(
            by_id("sess-on").resume_command,
            format!("copilot --resume=sess-on {}", crate::orchestration::COPILOT_GROUP_AUTOPILOT_FLAGS)
        );
        assert_eq!(by_id("sess-off").resume_command, "copilot --resume=sess-off");
        assert_eq!(by_id("sess-unknown").resume_command, "copilot --resume=sess-unknown");
        assert_eq!(
            by_id("sess-ambiguous").resume_command,
            "copilot --resume=sess-ambiguous",
            "an ambiguous history must never grant autopilot on restore, even via the latest record"
        );

        // #458 pin: whichever posture branch produced it, the emitted
        // command must use copilot's documented `--resume=<id>` form and
        // must never regress to a bare space, which copilot's CLI reference
        // (`-r`, `--resume[=VALUE]`) documents as an OPTIONAL-value flag —
        // a space-separated id risks parsing as a bare `--resume` plus a
        // stray positional rather than the flag's value.
        for s in &out {
            assert!(
                s.resume_command.contains("--resume="),
                "resume_command must use the documented --resume=<id> form, got: {}",
                s.resume_command
            );
            assert!(
                !s.resume_command.contains("--resume "),
                "resume_command must never use a space between --resume and the id \
                 (copilot documents --resume as optional-value; a space form risks being \
                 parsed as bare --resume plus a stray positional), got: {}",
                s.resume_command
            );
        }

        set_copilot_session_state_root_for_test(None);
        set_claude_projects_root_for_test(None);
        clear_seam();
        drop(posture_dir);
    }

    /// The claude half of the same regression pin, generalized by #457: a
    /// Sessions-tab resume of a CLAUDE session must carry the same autopilot
    /// flags a fresh launch in that cwd would, when — and only when —
    /// loomux's own record (keyed by the session's OWN id here, not a cwd)
    /// says so. Before #457 this could never happen at all: `scan_claude`
    /// emitted a bare `claude --resume <id>` unconditionally. Exercises
    /// `scan_claude` end to end, same shape as the copilot test above.
    #[test]
    fn scan_claude_restores_autopilot_flags_only_when_recorded() {
        let root = tempfile::tempdir().unwrap();
        set_claude_projects_root_for_test(Some(root.path().to_path_buf()));
        // Empty copilot root, for the mirror-image reason the copilot test
        // fixtures an empty claude one (see `scan_rows`).
        let copilot_root = tempfile::tempdir().unwrap();
        set_copilot_session_state_root_for_test(Some(copilot_root.path().to_path_buf()));
        let _d = posture_seam();

        let write_session = |id: &str, cwd: &str| {
            let proj = root.path().join(format!("proj-{id}"));
            fs::create_dir_all(&proj).unwrap();
            fs::write(
                proj.join(format!("{id}.jsonl")),
                format!("{{\"type\":\"user\",\"cwd\":{cwd:?},\"message\":{{\"content\":\"hi\"}}}}\n"),
            )
            .unwrap();
        };

        write_session("sess-on", "C:/work/on");
        record_claude_launch_posture_impl("sess-on", true).unwrap();

        write_session("sess-off", "C:/work/off");
        record_claude_launch_posture_impl("sess-off", false).unwrap();

        // No record at all: bare resume — the pre-#457 behavior for EVERY
        // claude session, now scoped to only the ones nothing was ever
        // recorded for.
        write_session("sess-unknown", "C:/work/unknown");

        let out = scan_rows();
        let by_id = |id: &str| out.iter().find(|s| s.id == id).unwrap();

        assert_eq!(
            by_id("sess-on").resume_command,
            format!("claude --resume sess-on {}", crate::orchestration::single_pane_autopilot_flags("claude"))
        );
        assert_eq!(by_id("sess-off").resume_command, "claude --resume sess-off");
        assert_eq!(by_id("sess-unknown").resume_command, "claude --resume sess-unknown");

        set_claude_projects_root_for_test(None);
        set_copilot_session_state_root_for_test(None);
        clear_seam();
    }

    /// THE requirement 2 property (design-intake reply, #457): a claude
    /// session this module has NO recorded intent for — foreign (never
    /// launched by loomux at all), pre-upgrade (existed before this PR, so
    /// no id-keyed record could ever have been written for it), or evicted —
    /// must resolve to nothing, exactly like `no_record_is_none` above, but
    /// pinned specifically against `build_resume_command`/`scan_claude`
    /// end-to-end rather than just the lookup helper, since that's the
    /// surface a reviewer actually cares about: this PR must never WIDEN
    /// what a restore grants beyond what loomux itself recorded.
    #[test]
    fn scan_claude_grants_nothing_to_a_session_with_no_recorded_intent() {
        let root = tempfile::tempdir().unwrap();
        set_claude_projects_root_for_test(Some(root.path().to_path_buf()));
        let copilot_root = tempfile::tempdir().unwrap(); // empty — see `scan_rows`
        set_copilot_session_state_root_for_test(Some(copilot_root.path().to_path_buf()));
        let _d = posture_seam(); // store exists but is empty — nothing recorded for anyone

        let proj = root.path().join("proj-foreign");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("foreign-session.jsonl"),
            "{\"type\":\"user\",\"cwd\":\"C:/work/foreign\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();

        let out = scan_rows();
        assert_eq!(
            out.iter().find(|s| s.id == "foreign-session").unwrap().resume_command,
            "claude --resume foreign-session",
            "a session with no recorded launch intent must restore bare — never inferred flags"
        );

        set_claude_projects_root_for_test(None);
        set_copilot_session_state_root_for_test(None);
        clear_seam();
    }

    /// THE soft-migration requirement (design-intake reply, #457): when the
    /// new `launch-intent.json` has never been written on this machine, a
    /// pre-#457 `copilot-posture.json` must still be read — a cold reset
    /// would be safe (no record → no flags) but would re-inflict the exact
    /// #456-reported annoyance on the release that fixes it, so this is
    /// pinned as a behavior, not left to the safe-default rule to merely
    /// happen to cover.
    #[test]
    fn soft_migration_reads_legacy_copilot_posture_file_when_new_store_is_absent() {
        let d = tempfile::tempdir().unwrap();
        let new_path = d.path().join("launch-intent.json");
        let legacy_path = d.path().join("copilot-posture.json");
        set_launch_intent_path_for_test(Some(new_path.clone()));
        set_legacy_copilot_posture_path_for_test(Some(legacy_path.clone()));

        // A real pre-#457 file, written in its OWN (copilot-only, cwd-only,
        // untagged) shape — never the new store's shape. The stored `cwd` is
        // the ALREADY-NORMALIZED permission key a real write would have
        // produced (`posture_key`, applied at write time) — computed here via
        // `posture_key_for` rather than hand-typed, since that normalization
        // is platform-dependent (backslash+lowercase on Windows, exact-match
        // elsewhere per review B2) and a literal Windows-style key would only
        // coincidentally match a lookup on non-Windows CI.
        let migrated_key = posture_key_for("c:/work/migrated", cfg!(windows));
        let migrated_off_key = posture_key_for("c:/work/migrated-off", cfg!(windows));
        fs::write(
            &legacy_path,
            format!(
                r#"{{"entries": [
                    {{"cwd": {}, "posture": "True", "touched_ms": 1}},
                    {{"cwd": {}, "posture": "False", "touched_ms": 2}}
                ]}}"#,
                serde_json::to_string(&migrated_key).unwrap(),
                serde_json::to_string(&migrated_off_key).unwrap(),
            ),
        )
        .unwrap();
        assert!(!new_path.exists(), "precondition: the new store must not exist yet");

        assert_eq!(
            copilot_launch_posture("c:/work/migrated"),
            Some(true),
            "a pre-#457 record must survive the upgrade, not reset to no-history"
        );
        assert_eq!(copilot_launch_posture("c:/work/migrated-off"), Some(false));

        // The migration is READ-ONLY: a read alone must not create or alter
        // the legacy file, and must not fabricate the new file either (only
        // an actual write does that — see the next assertion).
        assert!(!new_path.exists(), "a read-only migration must never itself create the new file");

        // The very next write lands on the NEW path and carries the migrated
        // entries forward (not just the newly-written one) — so a second
        // read never needs the legacy file again.
        record_copilot_launch_posture_impl("c:/work/fresh", true).unwrap();
        assert!(new_path.exists(), "a write must create the new store");
        assert_eq!(copilot_launch_posture("c:/work/fresh"), Some(true));
        assert_eq!(
            copilot_launch_posture("c:/work/migrated"),
            Some(true),
            "the migrated record must survive being merged into a real write, not get dropped"
        );

        set_launch_intent_path_for_test(None);
        set_legacy_copilot_posture_path_for_test(None);
    }

    /// THE property the test above only proved for whichever platform
    /// happened to build it: `soft_migration_reads_legacy_copilot_posture_
    /// file_when_new_store_is_absent` computes its fixture's on-disk key via
    /// `posture_key_for(cwd, cfg!(windows))` and reads it back through the
    /// REAL, `cfg!(windows)`-gated `copilot_launch_posture` — so on any given
    /// CI host it only ever exercises ONE of `posture_key_for`'s two arms,
    /// the SAME one, on both sides. A legacy file written by a Windows
    /// loomux (backslash, lowercased keys) and one written by a macOS/Linux
    /// loomux (exact-match keys) are different on-disk shapes, and this test
    /// proves migration is faithful to BOTH, unconditionally, on every host —
    /// not by exercising the real lookup (which can't be steered off its own
    /// build platform), but by inspecting `load_launch_intent`'s STRUCTURAL
    /// output directly: does the migrated store contain an entry whose key
    /// carries the legacy `cwd` string EXACTLY, untouched by any
    /// re-normalization? Migration is a pure passthrough of the legacy
    /// entry's `cwd` field (never re-keyed — see `load_launch_intent`'s
    /// match arm) precisely so this holds regardless of which platform wrote
    /// the file being migrated. Caught this exact gap: the sibling test
    /// above passed locally on a Windows-only dev machine and failed on CI's
    /// ubuntu/macOS runners — full-platform CI, not local verification, is
    /// what this project treats as authoritative for platform-gated code
    /// exactly because of failures shaped like this one.
    ///
    /// Mutation-verified against the exact blind spot this exists to close:
    /// temporarily re-normalizing the migrated `cwd` (`posture_key(&e.cwd)`
    /// instead of `e.cwd` verbatim) leaves the sibling end-to-end test
    /// GREEN on a Windows host — `posture_key` is idempotent on an
    /// already-Windows-normalized key, so re-normalizing it a second time is
    /// invisible there — while THIS test goes red immediately on the
    /// non-Windows-written key, on every host, because it is never
    /// idempotent under the Windows arm.
    #[test]
    fn soft_migration_preserves_the_legacy_cwd_key_exactly_regardless_of_which_platform_wrote_it() {
        let d = tempfile::tempdir().unwrap();
        let new_path = d.path().join("launch-intent.json");
        let legacy_path = d.path().join("copilot-posture.json");
        set_launch_intent_path_for_test(Some(new_path));
        set_legacy_copilot_posture_path_for_test(Some(legacy_path.clone()));

        // BOTH arms, explicit — never `cfg!(windows)` here: the Windows arm
        // (backslash, lowercased) and the non-Windows arm (exact-match,
        // unmodified) of `posture_key_for`, applied to the SAME logical
        // folder, so this fixture is what EITHER platform's pre-#457 loomux
        // would genuinely have written for it.
        let windows_written_key = posture_key_for("C:/Work/From-Windows", true);
        let unix_written_key = posture_key_for("/home/user/from-unix", false);
        assert_ne!(
            windows_written_key, unix_written_key,
            "precondition: the two arms must actually differ, or this test proves nothing"
        );

        fs::write(
            &legacy_path,
            format!(
                r#"{{"entries": [
                    {{"cwd": {}, "posture": "True", "touched_ms": 1}},
                    {{"cwd": {}, "posture": "False", "touched_ms": 2}}
                ]}}"#,
                serde_json::to_string(&windows_written_key).unwrap(),
                serde_json::to_string(&unix_written_key).unwrap(),
            ),
        )
        .unwrap();

        let migrated = load_launch_intent();
        let has = |cwd: &str, want: Posture| {
            migrated.entries.iter().any(|e| {
                e.key == (IntentKey::Cwd { cli: "copilot".to_string(), cwd: cwd.to_string() })
                    && e.autopilot == want
            })
        };
        assert!(
            has(&windows_written_key, Posture::True),
            "a Windows-written legacy key must migrate byte-for-byte, even read on a non-Windows host"
        );
        assert!(
            has(&unix_written_key, Posture::False),
            "a non-Windows-written legacy key must migrate byte-for-byte, even read on a Windows host"
        );
        assert_eq!(migrated.entries.len(), 2, "no entry invented, none dropped, none merged");

        set_launch_intent_path_for_test(None);
        set_legacy_copilot_posture_path_for_test(None);
    }

    /// The new store's existence — not its parseability — gates migration:
    /// once `launch-intent.json` exists at all (even corrupt), a legacy
    /// `copilot-posture.json` is never consulted, so a CURRENT corrupt file
    /// can't be silently "recovered" from a possibly-stale old one. This is
    /// the one case `any_unparseable_or_malformed_store_state_grants_nothing`
    /// below doesn't cover (that test never populates a legacy file at all).
    #[test]
    fn a_corrupt_new_store_never_falls_back_to_the_legacy_file() {
        let d = tempfile::tempdir().unwrap();
        let new_path = d.path().join("launch-intent.json");
        let legacy_path = d.path().join("copilot-posture.json");
        set_launch_intent_path_for_test(Some(new_path.clone()));
        set_legacy_copilot_posture_path_for_test(Some(legacy_path.clone()));

        fs::write(&legacy_path, r#"{"entries": [{"cwd": "c:\\work\\x", "posture": "True", "touched_ms": 1}]}"#)
            .unwrap();
        fs::write(&new_path, "this is not json{{{").unwrap();

        assert_eq!(
            copilot_launch_posture("c:/work/x"),
            None,
            "a corrupt CURRENT store must degrade to empty, never fall back to the legacy file — \
             falling back here would resurrect state the new file may have deliberately dropped"
        );

        set_launch_intent_path_for_test(None);
        set_legacy_copilot_posture_path_for_test(None);
    }

    /// THE property rev-6 asked for (round 2, close-out review of 918f1fb):
    /// a posture-store file the code cannot fully understand — corrupt
    /// bytes, a valid-JSON-but-wrong shape, a leftover file from this
    /// module's OWN pre-fix schema, a malformed entry, an unrecognized
    /// `posture` value, a truncated record — must NEVER grant flags for any
    /// cwd. rev-6 verified this by hand with a 3-fixture scratch test
    /// (fresh / corrupt / wrong-shape-including-a-round-1-schema-leftover /
    /// unknown-variant, all resolving to no flags) and flagged that nothing
    /// shipped pinned it — a future edit to the store's parsing (lenient
    /// per-entry recovery, a new `Posture` variant handled by a catch-all)
    /// could silently reopen exactly the grant path B1 closed, and nothing
    /// would fail. This lifts that scratch test's shape (`fs::write` a raw
    /// store file, assert the lookup) and generalizes it to the INVARIANT
    /// rather than shipping isolated fixtures: asserted over a spread of
    /// distinct failure classes AND over several cwds per fixture, so the
    /// assertion is about what the STORE can ever produce, not one lookup.
    ///
    /// All fixtures are written at the NEW path — this is deliberately
    /// distinct from the migration tests above: the new file EXISTS here
    /// (even the pre-#456-fix and pre-#457 legacy-shaped fixtures), so
    /// existence-gated migration (see the module doc) must never kick in —
    /// a malformed CURRENT file degrades to empty, it never falls back.
    ///
    /// Mutation-verified: temporarily replacing `load_launch_intent`'s
    /// atomic-parse-or-empty contract with a lenient per-entry salvage that
    /// defaults a missing/unrecognized `posture` to `True` makes this red on
    /// exactly the fixtures that exercise that gap.
    #[test]
    fn any_unparseable_or_malformed_store_state_grants_nothing() {
        let malformed_fixtures: &[(&str, &str)] = &[
            ("not JSON at all", "this is not json{{{"),
            ("valid JSON, wrong top-level shape entirely", r#"{"totally": "unexpected shape"}"#),
            ("entries present but not an array", r#"{"entries": "nope"}"#),
            (
                "a leftover file from this module's OWN pre-#456-fix schema (rev-6's named case)",
                r#"{"entries": [{"cwd": "c:\\work\\x", "autopilot": true, "recorded_ms": 1}]}"#,
            ),
            (
                "a leftover file from this module's pre-#457 (copilot-only, untagged-key) schema, \
                 sitting at the NEW path rather than being migrated from the legacy one",
                r#"{"entries": [{"cwd": "c:\\work\\x", "posture": "True", "touched_ms": 1}]}"#,
            ),
            (
                "one well-formed entry, one entry missing the key's kind tag",
                r#"{"entries": [
                    {"key": {"kind": "Cwd", "cli": "copilot", "cwd": "c:\\work\\x"}, "autopilot": "True", "touched_ms": 1},
                    {"key": {"cli": "copilot", "cwd": "c:\\work\\y"}, "autopilot": "True", "touched_ms": 2}
                ]}"#,
            ),
            (
                "an unrecognized posture variant",
                r#"{"entries": [{"key": {"kind": "Cwd", "cli": "copilot", "cwd": "c:\\work\\x"}, "autopilot": "SomethingElse", "touched_ms": 1}]}"#,
            ),
            (
                "an entry whose touched_ms is the wrong type",
                r#"{"entries": [{"key": {"kind": "Cwd", "cli": "copilot", "cwd": "c:\\work\\x"}, "autopilot": "True", "touched_ms": "soon"}]}"#,
            ),
            ("truncated mid-record", r#"{"entries": [{"key": {"kind": "Cwd", "post"#),
        ];

        for (label, content) in malformed_fixtures {
            let d = tempfile::tempdir().unwrap();
            set_launch_intent_path_for_test(Some(d.path().join("launch-intent.json")));
            set_legacy_copilot_posture_path_for_test(Some(d.path().join("copilot-posture.json"))); // absent — irrelevant here
            fs::write(d.path().join("launch-intent.json"), content).unwrap();
            for cwd in ["c:/work/x", "c:/work/y", "C:/work/X", "c:/elsewhere"] {
                assert_eq!(
                    copilot_launch_posture(cwd),
                    None,
                    "malformed store ({label}) must never grant flags — cwd {cwd:?}"
                );
            }
            assert_eq!(
                claude_launch_posture("any-session-id"),
                None,
                "malformed store ({label}) must never grant flags on the claude side either"
            );
            clear_seam();
        }
    }
}
