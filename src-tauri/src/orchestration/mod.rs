//! Orchestrator/worker agent groups: registry, guardrails, persistence,
//! audit log, and visible prompt delivery.
//!
//! An orchestration *group* is one orchestrator pane plus the worker and
//! reviewer panes it manages, all running `claude` CLIs connected to the
//! loomux MCP server (see `mcp.rs`) with per-agent identity tokens. Panes
//! are frontend-owned, so spawning round-trips: registry emits
//! `orch-spawn-request` → frontend opens the pane → `bind_agent` reports the
//! pty id back and unblocks the spawner.
//!
//! Inter-agent communication is deliberately *typed into the recipient's
//! CLI* (bracketed paste + Enter) rather than delivered out of band: the
//! human sees every prompt exactly as if they had written it, can steer any
//! pane, and the audit log (`audit.jsonl`) records the full text.

pub mod mcp;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ORCHESTRATOR_TPL: &str = include_str!("templates/orchestrator.md");
const WORKER_TPL: &str = include_str!("templates/worker.md");
const REVIEWER_TPL: &str = include_str!("templates/reviewer.md");

/// Hard ceiling on `max_agents` regardless of what the launcher asks for.
const MAX_AGENTS_CEILING: u32 = 12;
/// How long the frontend gets to open a pane and report its pty id.
const BIND_TIMEOUT: Duration = Duration::from_secs(20);
/// Gap between the bracketed paste and the Enter that submits it.
const PASTE_SUBMIT_DELAY: Duration = Duration::from_millis(500);

// Submission discipline: copilot ignores Enter while its agent is running
// (the pasted text just sits in the input box — observed live with a worker
// report landing mid-turn), so before pressing Enter the pane must be quiet
// (turn finished). Enter on an empty box is a no-op in both CLIs, so a
// couple of spaced blind retries are safe and cover late busy-locks.
/// Output must be idle this long before Enter is pressed.
const SUBMIT_QUIET: Duration = Duration::from_millis(1000);
/// Max time to wait for quiet before pressing Enter anyway.
const SUBMIT_MAX_WAIT: Duration = Duration::from_secs(45);
/// Spaced blind Enter retries after the first (no-ops once submitted).
const SUBMIT_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(2500), Duration::from_millis(4500)];

// Kickoff readiness: a fixed boot delay loses the race on a loaded machine
// (a CLI that boots slower than the delay flushes the pasted prompt along
// with its startup stdin buffer — observed live with a reviewer spawned
// while a worker ran cargo test). Instead, watch the pane's output ring and
// paste only once the CLI has painted its UI and gone quiet.
/// Minimum wait before even checking (lets the process start writing).
const READY_MIN_WAIT: Duration = Duration::from_millis(1500);
/// Output must be idle this long (UI finished painting) to count as ready.
const READY_QUIET: Duration = Duration::from_millis(1200);
/// Minimum bytes of output before a CLI can be considered painted.
const READY_MIN_OUTPUT: usize = 512;
/// Give up waiting and paste anyway after this long.
const READY_MAX_WAIT: Duration = Duration::from_secs(25);
/// Poll interval for the readiness check.
const READY_POLL: Duration = Duration::from_millis(250);

// Echo verification: a paste that landed makes the TUI redraw its input box
// (observable as output bytes). A paste that produced no output within the
// window was flushed by a CLI whose stdin reader wasn't attached yet
// (observed live with copilot, whose input attaches well after its UI
// paints) — wait and retype.
/// How long a paste has to produce echo output before it counts as eaten.
const ECHO_WINDOW: Duration = Duration::from_millis(2000);
/// Minimum output growth that counts as the input box echoing the paste.
const ECHO_MIN_BYTES: u64 = 8;
/// Pause before retyping after an eaten paste (input attach may be close).
const ECHO_RETRY_DELAY: Duration = Duration::from_millis(1500);
/// Total attempts before typing blind and letting the human see the result.
const ECHO_ATTEMPTS: u32 = 3;
/// Upper bound for `set_state` payloads.
const MAX_STATE_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Orchestrator,
    Worker,
    Reviewer,
}

impl Role {
    fn prefix(self) -> &'static str {
        match self {
            Role::Orchestrator => "orch",
            Role::Worker => "w",
            Role::Reviewer => "rev",
        }
    }
    fn template(self) -> &'static str {
        match self {
            Role::Orchestrator => ORCHESTRATOR_TPL,
            Role::Worker => WORKER_TPL,
            Role::Reviewer => REVIEWER_TPL,
        }
    }
    fn instructions_file(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator.md",
            Role::Worker => "worker.md",
            Role::Reviewer => "reviewer.md",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Starting,
    Running,
    Dead,
}

/// Which agent CLI a group runs. Each needs an adapter in
/// `build_agent_command` + `write_mcp_config`; anything unknown falls back
/// to Claude (explicitly, in `clamped`, never silently at spawn time).
pub const SUPPORTED_CLIS: [&str; 2] = ["claude", "copilot"];

#[derive(Clone, Debug)]
pub struct Guardrails {
    pub max_agents: u32,
    /// "claude" | "copilot" — see `SUPPORTED_CLIS`.
    pub agent_cli: String,
    pub worker_model: String,
    pub reviewer_model: String,
    pub orchestrator_model: String,
    /// Additionally pre-approve `git`/`gh` shell commands for the group's
    /// agents. Never maps to `--dangerously-skip-permissions`: bypass mode
    /// shows a confirm dialog whose default answer is "exit", which the
    /// kickoff typing would accept, killing the pane.
    pub auto_ops: bool,
}

impl Guardrails {
    #[doc(hidden)] // pub for integration tests (unit tests can't load the UI stack; see tests/smoke.rs)
    pub fn clamped(mut self) -> Self {
        self.max_agents = self.max_agents.clamp(1, MAX_AGENTS_CEILING);
        if !SUPPORTED_CLIS.contains(&self.agent_cli.as_str()) {
            self.agent_cli = "claude".into();
        }
        // Copilot picks its own best model with "auto"; Claude needs a tier.
        let (worker_fb, orch_fb) = if self.agent_cli == "copilot" {
            ("auto", "auto")
        } else {
            ("sonnet", "opus")
        };
        self.worker_model = sanitize_model(&self.worker_model, worker_fb);
        self.reviewer_model = sanitize_model(&self.reviewer_model, worker_fb);
        self.orchestrator_model = sanitize_model(&self.orchestrator_model, orch_fb);
        self
    }
}

/// Models are interpolated into a shell command line; restrict them to
/// identifier-ish characters so a crafted "model" can't smuggle arguments.
fn sanitize_model(m: &str, fallback: &str) -> String {
    let cleaned: String = m
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

#[derive(Clone)]
pub struct GroupInfo {
    pub id: String,
    pub repo: String,
    pub guardrails: Guardrails,
}

#[derive(Clone, Debug)]
pub struct AgentEntry {
    pub id: String,
    pub group: String,
    pub name: String,
    pub role: Role,
    pub token: String,
    pub status: AgentStatus,
    pub pty_id: Option<u32>,
    pub task: String,
    /// The agent CLI's conversation session id. For Claude, loomux assigns
    /// it at spawn (`--session-id`), so a finished worker's session can be
    /// resumed later for follow-ups on its task without a cold start.
    pub session_id: Option<String>,
    /// Working directory the pane runs in; resume must reuse it so the
    /// resumed session's file operations land where the work happened.
    pub cwd: String,
}

/// Work-item statuses shown on the task board. Kept as strings (not an
/// enum) so the wire/JSON forms stay obvious; validated on every write.
pub const TASK_STATUSES: [&str; 7] = [
    "queued",        // planned, not started
    "in-progress",   // a worker is on it
    "review",        // reviewer agent engaged
    "pr",            // PR open, review loop finished
    "human-testing", // done pending the human's validation
    "done",          // merged / accepted by the human
    "blocked",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskNote {
    pub ts_ms: u64,
    pub author: String,
    pub text: String,
}

/// One work item on a group's task board (`tasks.json`, array order =
/// priority). Maintained by the orchestrator via MCP tools and by the human
/// via the pane's task-board overlay; each side is notified of the other's
/// edits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub issue: Option<String>,
    #[serde(default)]
    pub pr: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    /// Agent CLI session that did/does this work; lets the orchestrator
    /// resume it for follow-ups instead of cold-starting or disturbing a
    /// busy worker.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub notes: Vec<TaskNote>,
    #[serde(default)]
    pub updated_ms: u64,
}

/// Field edits for `upsert_task`; `None` leaves a field untouched.
#[derive(Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub status: Option<String>,
    pub issue: Option<String>,
    pub pr: Option<String>,
    pub assignee: Option<String>,
    pub session: Option<String>,
    pub note: Option<String>,
}

/// Durable roster entry (`agents.json` per group): which sessions belonged
/// to which role. This is what lets the session browser mark orchestrator/
/// worker sessions and restore a whole orchestration after loomux restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub role: String,
    pub name: String,
    pub session: Option<String>,
    pub cwd: String,
    pub status: String,
    pub updated_ms: u64,
}

/// A recorded session's orchestration identity, for the session browser.
#[derive(Clone, Serialize)]
pub struct SessionRole {
    pub session_id: String,
    pub group_id: String,
    pub role: String,
    pub agent_name: String,
    /// Whether that group currently has live agents in this app instance.
    pub group_live: bool,
}

/// Identity resolved from an MCP request's token header.
#[derive(Clone, Debug)]
pub struct Caller {
    pub agent_id: String,
    pub group: String,
    pub role: Role,
}

/// Payload asking the frontend to open a pane for an agent. Also the return
/// value of `create_orchestration` (the orchestrator's own pane).
#[derive(Clone, Debug, Serialize)]
pub struct SpawnRequest {
    pub group_id: String,
    pub agent_id: String,
    pub role: Role,
    pub name: String,
    pub cwd: String,
    pub command: String,
}

pub struct OrchRegistry {
    /// Root of persistent state: `<root>/<group>/{group.json,state.json,audit.jsonl,configs/}`.
    root: PathBuf,
    /// Absent in unit tests: spawning then skips the pane round-trip.
    app: Mutex<Option<AppHandle>>,
    groups: Mutex<HashMap<String, GroupInfo>>,
    agents: Mutex<HashMap<String, AgentEntry>>,
    by_token: Mutex<HashMap<String, String>>,
    by_pty: Mutex<HashMap<u32, String>>,
    pending_binds: Mutex<HashMap<String, mpsc::Sender<u32>>>,
    port: AtomicU16,
    seq: AtomicU32,
    /// Per-pane delivery locks so two prompts to the SAME pane can't
    /// interleave keystrokes, while a slow delivery (waiting out a busy
    /// CLI) doesn't block deliveries to other panes.
    delivery: Mutex<HashMap<u32, Arc<Mutex<()>>>>,
    /// Serializes task-board read-modify-write cycles (MCP threads and the
    /// human UI mutate the same tasks.json).
    tasks_lock: Mutex<()>,
    /// Serializes group creation + orchestrator registration: the group id
    /// is chosen by liveness, and a group only becomes live once its
    /// orchestrator is registered — without this, two concurrent launches
    /// on one repo would share an id.
    creation: Mutex<()>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 128-bit hex token from std's OS-seeded `RandomState` (each instance draws
/// fresh OS entropy) mixed with time. Deliberately not getrandom-based: see
/// the Cargo.toml note on bcryptprimitives/ProcessPrng. Tokens authenticate
/// same-user localhost agents; that adversary can read the config files
/// anyway, so this strength is proportionate.
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(32);
    for i in 0..2u64 {
        let mut h = std::hash::RandomState::new().build_hasher();
        h.write_u64(now_ms());
        h.write_u64(i);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// UUIDv4-format session id from the same entropy source as `new_token`
/// (Claude's `--session-id` requires a valid UUID).
fn new_session_uuid() -> String {
    let hex = new_token(); // 32 hex chars
    let b = hex.as_bytes();
    let s = |r: std::ops::Range<usize>| std::str::from_utf8(&b[r]).unwrap();
    // Stamp version (4) and variant (8) nibbles per RFC 4122.
    format!(
        "{}-{}-4{}-8{}-{}",
        s(0..8),
        s(8..12),
        s(13..16),
        s(17..20),
        s(20..32)
    )
}

/// Session ids get interpolated into a shell command line; validate (not
/// filter — a mangled id would silently resume the wrong session).
fn sanitize_session(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty() && t.len() <= 64 && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .then(|| t.to_string())
}

/// Add a folder to copilot's `trustedFolders` config, returning the new
/// file content — or None when nothing should be written (already trusted,
/// or the existing config is unparseable and must not be clobbered). The
/// file is JSONC-ish: leading `//` comment lines before a JSON object;
/// comments and unknown fields are preserved.
pub fn add_trusted_folder(config_text: &str, folder: &str) -> Option<String> {
    let mut comment_len = 0;
    for line in config_text.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("//") || t.is_empty() {
            comment_len += line.len();
        } else {
            break;
        }
    }
    let (comments, body) = config_text.split_at(comment_len);
    let mut v: Value = if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(body).ok()?
    };
    let arr = v
        .as_object_mut()?
        .entry("trustedFolders")
        .or_insert_with(|| json!([]))
        .as_array_mut()?;
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    if arr.iter().any(|e| e.as_str().is_some_and(|s| norm(s) == norm(folder))) {
        return None;
    }
    arr.push(json!(folder));
    Some(format!("{comments}{}\n", serde_json::to_string_pretty(&v).ok()?))
}

/// Pre-trust an agent's workspace in copilot's config so its pane doesn't
/// boot into a folder-trust dialog — which eats the kickoff paste and gets
/// blind-answered by the submit retries. Best-effort: on any failure the
/// dialog simply appears as before.
fn pre_trust_copilot_folder(folder: &str) {
    let home = std::env::var("COPILOT_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".copilot")))
        .unwrap_or_default();
    if home.as_os_str().is_empty() {
        return;
    }
    let path = home.join("config.json");
    let text = fs::read_to_string(&path).unwrap_or_default();
    if let Some(updated) = add_trusted_folder(&text, folder) {
        let _ = fs::create_dir_all(&home);
        let _ = fs::write(&path, updated);
    }
}

/// Stable, filesystem-safe group id for a repo path, so relaunching an
/// orchestrator on the same repo reattaches to the same state directory.
pub(crate) fn group_id_for_repo(repo: &str) -> String {
    let norm = repo.replace('\\', "/").to_lowercase();
    let norm = norm.trim_end_matches('/');
    // FNV-1a 64
    let mut h: u64 = 0xcbf29ce484222325;
    for b in norm.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let slug: String = norm
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    let slug = if slug.is_empty() { "repo".into() } else { slug };
    format!("{slug}-{:08x}", (h >> 32) as u32 ^ h as u32)
}

/// Size cap after which the audit log rolls over to `audit.1.jsonl` (one
/// generation kept). Full prompt texts land in the audit, so it grows fast.
const AUDIT_ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// Roll `audit.jsonl` over to `audit.1.jsonl` once it exceeds `cap`.
/// Factored out so the threshold behavior is testable with a tiny cap.
#[doc(hidden)] // pub for integration tests
pub fn rotate_audit_if_needed(dir: &Path, cap: u64) {
    let path = dir.join("audit.jsonl");
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > cap {
        let _ = fs::rename(&path, dir.join("audit.1.jsonl")); // replaces the old generation
    }
}

/// Audit-log writer usable from background threads (delivery outcomes)
/// without holding a registry reference.
fn append_audit(root: &Path, group: &str, actor: &str, action: &str, detail: Value) {
    let dir = root.join(group);
    let line = json!({ "ts_ms": now_ms(), "actor": actor, "action": action, "detail": detail });
    let _ = fs::create_dir_all(&dir);
    rotate_audit_if_needed(&dir, AUDIT_ROTATE_BYTES);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join("audit.jsonl")) {
        let _ = writeln!(f, "{line}");
    }
}

/// One parsed audit-log line, for the in-app timeline viewer. Mirrors the
/// shape written by `append_audit`; `detail` stays an opaque JSON value so the
/// frontend can render per-action without the backend knowing every schema.
#[derive(Clone, Debug, Serialize)]
pub struct AuditEntry {
    pub ts_ms: u64,
    pub actor: String,
    pub action: String,
    pub detail: Value,
}

/// Parse audit JSONL text into entries, in file order (oldest first), skipping
/// malformed lines. Pure so ordering/robustness is testable without touching
/// the filesystem or a registry.
#[doc(hidden)] // pub for integration tests
pub fn parse_audit_lines(text: &str) -> Vec<AuditEntry> {
    text.lines()
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            let v: Value = serde_json::from_str(line).ok()?;
            Some(AuditEntry {
                ts_ms: v["ts_ms"].as_u64().unwrap_or(0),
                actor: v["actor"].as_str().unwrap_or("").to_string(),
                action: v["action"].as_str().unwrap_or("").to_string(),
                detail: v.get("detail").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

/// Upper bound on entries returned to the viewer: the audit grows fast (full
/// prompt texts) and only the most recent slice is worth rendering. Keeps the
/// payload bounded even against a rotated + current pair near the 8 MB cap.
const AUDIT_VIEW_LIMIT: usize = 5000;

fn render_template(tpl: &str, vars: &[(&str, &str)]) -> String {
    let mut out = tpl.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Strip ANSI escape sequences (CSI, OSC, two-byte ESC) and carriage
/// returns so `get_output` returns readable text from raw terminal bytes.
pub fn strip_ansi(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            i += 1;
            match bytes.get(i) {
                Some(b'[') => {
                    // CSI: parameters/intermediates until a final byte 0x40-0x7E.
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                Some(b']') => {
                    // OSC: until BEL or ESC \.
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => i += 2 - 1, // two-byte escape: skip the introducer
                None => {}
            }
            continue;
        }
        if b == b'\r' || (b < 0x20 && b != b'\n' && b != b'\t') {
            i += 1;
            continue;
        }
        // Decode this UTF-8 unit; fall back to skipping the byte.
        let len = match b {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        if let Ok(s) = std::str::from_utf8(&bytes[i..(i + len).min(bytes.len())]) {
            out.push_str(s);
        }
        i += len;
    }
    out
}

/// Decide whether a freshly spawned CLI is ready to receive typed input,
/// from its output volume and how long that output has been stable. Pure so
/// the thresholds are testable; the polling loop lives in `deliver_prompt`.
pub fn cli_ready(output_len: usize, quiet_for: Duration, elapsed: Duration) -> bool {
    elapsed >= READY_MIN_WAIT && output_len >= READY_MIN_OUTPUT && quiet_for >= READY_QUIET
}

/// Wrap prompt text in a bracketed paste so multi-line prompts land in the
/// CLI's input box instead of submitting at the first newline. The Enter is
/// sent separately after `PASTE_SUBMIT_DELAY`.
pub fn bracketed_paste(text: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(text.len() + 12);
    v.extend_from_slice(b"\x1b[200~");
    v.extend_from_slice(text.replace("\r\n", "\n").as_bytes());
    v.extend_from_slice(b"\x1b[201~");
    v
}

impl OrchRegistry {
    pub fn new(root: PathBuf) -> Self {
        let _ = fs::create_dir_all(&root);
        Self {
            root,
            app: Mutex::new(None),
            groups: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            by_token: Mutex::new(HashMap::new()),
            by_pty: Mutex::new(HashMap::new()),
            pending_binds: Mutex::new(HashMap::new()),
            port: AtomicU16::new(0),
            seq: AtomicU32::new(0),
            delivery: Mutex::new(HashMap::new()),
            tasks_lock: Mutex::new(()),
            creation: Mutex::new(()),
        }
    }

    /// Default persistent root: `<user data dir>/loomux/orchestration`.
    pub fn default_root() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("loomux")
            .join("orchestration")
    }

    pub fn set_app(&self, app: AppHandle) {
        *self.app.lock().unwrap() = Some(app);
    }

    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    fn group_dir(&self, group: &str) -> PathBuf {
        self.root.join(group)
    }

    // ---------- audit ----------

    /// Append one JSON line to the group's audit log. Best-effort: auditing
    /// must never take the orchestration down.
    pub fn audit(&self, group: &str, actor: &str, action: &str, detail: Value) {
        append_audit(&self.root, group, actor, action, detail);
    }

    /// Read a group's audit timeline for the in-app viewer, oldest first.
    /// Reads the rotated generation (`audit.1.jsonl`) before the current one
    /// so a rotation doesn't drop history mid-session, then keeps only the
    /// most recent `AUDIT_VIEW_LIMIT` entries. Missing files read as empty.
    pub fn audit_log(&self, group: &str) -> Vec<AuditEntry> {
        let dir = self.group_dir(group);
        let mut text = String::new();
        for name in ["audit.1.jsonl", "audit.jsonl"] {
            if let Ok(t) = fs::read_to_string(dir.join(name)) {
                text.push_str(&t);
                if !text.ends_with('\n') {
                    text.push('\n'); // guard against a rotated file with no trailing newline
                }
            }
        }
        let mut entries = parse_audit_lines(&text);
        if entries.len() > AUDIT_VIEW_LIMIT {
            entries.drain(0..entries.len() - AUDIT_VIEW_LIMIT);
        }
        entries
    }

    // ---------- durable state ----------

    pub fn get_state(&self, group: &str) -> String {
        fs::read_to_string(self.group_dir(group).join("state.json"))
            .unwrap_or_else(|_| "{}".to_string())
    }

    pub fn set_state(&self, group: &str, state: &str) -> Result<(), String> {
        if state.len() > MAX_STATE_BYTES {
            return Err(format!("state too large ({} bytes, max {MAX_STATE_BYTES})", state.len()));
        }
        serde_json::from_str::<Value>(state).map_err(|e| format!("state must be valid JSON: {e}"))?;
        let dir = self.group_dir(group);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        fs::write(dir.join("state.json"), state).map_err(|e| e.to_string())?;
        self.audit(group, "loomux", "state-write", json!({ "bytes": state.len() }));
        Ok(())
    }

    // ---------- task board ----------

    pub fn tasks(&self, group: &str) -> Vec<Task> {
        fs::read_to_string(self.group_dir(group).join("tasks.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write_tasks(&self, group: &str, tasks: &[Task]) -> Result<(), String> {
        let dir = self.group_dir(group);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        fs::write(dir.join("tasks.json"), serde_json::to_string_pretty(tasks).unwrap())
            .map_err(|e| e.to_string())?;
        self.emit_tasks_changed(group);
        Ok(())
    }

    fn emit_tasks_changed(&self, group: &str) {
        if let Some(app) = self.app.lock().unwrap().clone() {
            let _ = app.emit("orch-tasks-changed", json!({ "group_id": group }));
        }
    }

    /// Create (id = None, title required) or edit a task. Notes append; all
    /// other patch fields replace. Returns the resulting task.
    pub fn upsert_task(
        &self,
        group: &str,
        actor: &str,
        id: Option<&str>,
        patch: TaskPatch,
    ) -> Result<Task, String> {
        if let Some(s) = patch.status.as_deref() {
            if !TASK_STATUSES.contains(&s) {
                return Err(format!("invalid status {s:?} — use one of {}", TASK_STATUSES.join(" | ")));
            }
        }
        let _guard = self.tasks_lock.lock().unwrap();
        let mut tasks = self.tasks(group);
        let idx = match id {
            Some(id) => Some(
                tasks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or_else(|| format!("unknown task: {id}"))?,
            ),
            None => None,
        };
        let task = match idx {
            Some(i) => &mut tasks[i],
            None => {
                let title = patch
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or("a new task needs a title")?;
                let max: u32 = tasks
                    .iter()
                    .filter_map(|t| t.id.strip_prefix("t-").and_then(|n| n.parse().ok()))
                    .max()
                    .unwrap_or(0);
                tasks.push(Task {
                    id: format!("t-{}", max + 1),
                    title: title.to_string(),
                    status: "queued".into(),
                    issue: None,
                    pr: None,
                    assignee: None,
                    session: None,
                    notes: vec![],
                    updated_ms: 0,
                });
                tasks.last_mut().unwrap()
            }
        };
        if let Some(t) = patch.title {
            let t = t.trim();
            if !t.is_empty() {
                task.title = t.to_string();
            }
        }
        if let Some(s) = patch.status {
            task.status = s;
        }
        if patch.issue.is_some() {
            task.issue = patch.issue.filter(|s| !s.trim().is_empty());
        }
        if patch.pr.is_some() {
            task.pr = patch.pr.filter(|s| !s.trim().is_empty());
        }
        if patch.assignee.is_some() {
            task.assignee = patch.assignee.filter(|s| !s.trim().is_empty());
        }
        if patch.session.is_some() {
            task.session = patch.session.filter(|s| !s.trim().is_empty());
        }
        if let Some(text) = patch.note {
            let text = text.trim().to_string();
            if !text.is_empty() {
                task.notes.push(TaskNote { ts_ms: now_ms(), author: actor.to_string(), text });
            }
        }
        task.updated_ms = now_ms();
        let snapshot = task.clone();
        self.write_tasks(group, &tasks)?;
        self.audit(group, actor, "task-upsert", serde_json::to_value(&snapshot).unwrap());
        Ok(snapshot)
    }

    pub fn delete_task(&self, group: &str, actor: &str, id: &str) -> Result<(), String> {
        let _guard = self.tasks_lock.lock().unwrap();
        let mut tasks = self.tasks(group);
        let before = tasks.len();
        tasks.retain(|t| t.id != id);
        if tasks.len() == before {
            return Err(format!("unknown task: {id}"));
        }
        self.write_tasks(group, &tasks)?;
        self.audit(group, actor, "task-delete", json!({ "id": id }));
        Ok(())
    }

    /// Reorder by explicit id list (board order = priority). Ids not
    /// mentioned keep their relative order after the mentioned ones.
    pub fn reorder_tasks(&self, group: &str, actor: &str, ids: &[String]) -> Result<(), String> {
        let _guard = self.tasks_lock.lock().unwrap();
        let mut tasks = self.tasks(group);
        let mut ordered: Vec<Task> = Vec::with_capacity(tasks.len());
        for id in ids {
            if let Some(pos) = tasks.iter().position(|t| &t.id == id) {
                ordered.push(tasks.remove(pos));
            }
        }
        ordered.append(&mut tasks);
        self.write_tasks(group, &ordered)?;
        self.audit(group, actor, "task-reorder", json!({ "order": ids }));
        Ok(())
    }

    /// Tell the orchestrator the human touched the board (best-effort; the
    /// board itself is the source of truth via list_tasks).
    fn notify_board_edit(&self, group: &str, summary: &str) {
        let _ = self.deliver_to_orchestrator(
            group,
            &format!("[loomux] the human updated the task board: {summary}. Call list_tasks to sync."),
            "human",
        );
    }

    // ---------- durable roster (session ↔ role mapping, resume) ----------

    /// Upsert an agent into the group's `agents.json`. Best-effort like the
    /// audit log; shares the file lock with the task board.
    fn persist_agent_record(&self, entry: &AgentEntry, status: &str) {
        let _guard = self.tasks_lock.lock().unwrap();
        let path = self.group_dir(&entry.group).join("agents.json");
        let mut list: Vec<AgentRecord> = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let record = AgentRecord {
            id: entry.id.clone(),
            role: match entry.role {
                Role::Orchestrator => "orchestrator".into(),
                Role::Worker => "worker".into(),
                Role::Reviewer => "reviewer".into(),
            },
            name: entry.name.clone(),
            session: entry.session_id.clone(),
            cwd: entry.cwd.clone(),
            status: status.to_string(),
            updated_ms: now_ms(),
        };
        // Match by (id, session): agent ids restart at 1 every app run, so
        // a bare-id match would overwrite a previous run's record and lose
        // that session's identity.
        match list.iter_mut().find(|r| r.id == record.id && r.session == record.session) {
            Some(r) => *r = record,
            None => list.push(record),
        }
        let _ = fs::create_dir_all(self.group_dir(&entry.group));
        let _ = fs::write(&path, serde_json::to_string_pretty(&list).unwrap());
    }

    fn group_records(&self, group: &str) -> Vec<AgentRecord> {
        fs::read_to_string(self.group_dir(group).join("agents.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Roster entries derived from `agent-spawn` audit lines. Backfill for
    /// groups created before agents.json existed — their session-to-role
    /// mapping lives only in the audit log.
    fn records_from_audit(&self, group: &str) -> Vec<AgentRecord> {
        // Oldest first so newer spawns win the (id, session) upsert; the
        // rotated generation holds the older entries.
        let mut text = String::new();
        for name in ["audit.1.jsonl", "audit.jsonl"] {
            if let Ok(t) = fs::read_to_string(self.group_dir(group).join(name)) {
                text.push_str(&t);
            }
        }
        let mut out: Vec<AgentRecord> = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["action"] != "agent-spawn" {
                continue;
            }
            let d = &v["detail"];
            let Some(session) = d["session"].as_str() else { continue };
            let role = d["role"].as_str().unwrap_or("worker").to_string();
            let record = AgentRecord {
                id: d["agent"].as_str().unwrap_or("").to_string(),
                name: d["name"]
                    .as_str()
                    .unwrap_or(if role == "orchestrator" { "orchestrator" } else { "agent" })
                    .to_string(),
                role,
                session: Some(session.to_string()),
                cwd: d["cwd"].as_str().unwrap_or("").to_string(),
                // The audit alone can't tell liveness; group_live covers it.
                status: "unknown".into(),
                updated_ms: v["ts_ms"].as_u64().unwrap_or(0),
            };
            match out.iter_mut().find(|r| r.id == record.id && r.session == record.session) {
                Some(r) => *r = record,
                None => out.push(record),
            }
        }
        out
    }

    /// Roster + audit backfill, deduped by session (roster wins). Sessions
    /// are the stable key; agent ids recycle across app runs.
    fn merged_records(&self, group: &str) -> Vec<AgentRecord> {
        let mut records = self.group_records(group);
        for r in self.records_from_audit(group) {
            let dup = records.iter().any(|x| match (&x.session, &r.session) {
                (Some(a), Some(b)) => a == b,
                _ => x.id == r.id,
            });
            if !dup {
                records.push(r);
            }
        }
        records
    }

    /// Every recorded session across all groups on disk, with role identity
    /// — drives the session browser's ORCH/W/REV badges and restore flow.
    pub fn session_roles(&self) -> Vec<SessionRole> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return out;
        };
        for e in entries.flatten() {
            let group_id = e.file_name().to_string_lossy().into_owned();
            if !e.path().join("group.json").is_file() {
                continue;
            }
            let live = self.group_is_live(&group_id);
            for r in self.merged_records(&group_id) {
                if let Some(session) = r.session {
                    out.push(SessionRole {
                        session_id: session,
                        group_id: group_id.clone(),
                        role: r.role,
                        agent_name: r.name,
                        group_live: live,
                    });
                }
            }
        }
        out
    }

    /// Load a group's persisted identity (repo + guardrails) from group.json.
    fn load_group_file(&self, group: &str) -> Option<(String, Guardrails)> {
        let v: Value =
            serde_json::from_str(&fs::read_to_string(self.group_dir(group).join("group.json")).ok()?).ok()?;
        let repo = v["repo"].as_str()?.to_string();
        let g = &v["guardrails"];
        let s = |k: &str, fb: &str| g[k].as_str().unwrap_or(fb).to_string();
        Some((
            repo,
            Guardrails {
                max_agents: g["max_agents"].as_u64().unwrap_or(4) as u32,
                agent_cli: s("agent_cli", "claude"),
                worker_model: s("worker_model", ""),
                reviewer_model: s("reviewer_model", ""),
                orchestrator_model: s("orchestrator_model", ""),
                auto_ops: g["auto_ops"].as_bool().unwrap_or(true),
            },
        ))
    }

    // ---------- groups & agents ----------

    /// Create (or reattach to) the group for `repo`. State and audit history
    /// persist under the repo-derived group id; guardrails are refreshed from
    /// the new launch.
    pub fn create_group(&self, repo: &str, guardrails: Guardrails) -> Result<GroupInfo, String> {
        let guardrails = guardrails.clamped();
        // Base id is repo-derived so a relaunch resumes the same state dir —
        // but a repo can host several *concurrent* orchestrations, and those
        // must never share a group (their orchestrators would receive each
        // other's worker reports). Take the first id without live agents.
        let base = group_id_for_repo(repo);
        let id = (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
            .find(|candidate| !self.group_is_live(candidate))
            .unwrap();
        let dir = self.group_dir(&id);
        fs::create_dir_all(dir.join("configs")).map_err(|e| e.to_string())?;
        let resumed = dir.join("group.json").is_file();
        let info = GroupInfo { id: id.clone(), repo: repo.to_string(), guardrails };
        fs::write(
            dir.join("group.json"),
            serde_json::to_string_pretty(&json!({
                "group_id": info.id,
                "repo": info.repo,
                "created_ms": now_ms(),
                "guardrails": {
                    "max_agents": info.guardrails.max_agents,
                    "agent_cli": info.guardrails.agent_cli,
                    "worker_model": info.guardrails.worker_model,
                    "reviewer_model": info.guardrails.reviewer_model,
                    "orchestrator_model": info.guardrails.orchestrator_model,
                    "auto_ops": info.guardrails.auto_ops,
                },
            }))
            .unwrap(),
        )
        .map_err(|e| e.to_string())?;
        self.write_instruction_files(&info)?;
        self.groups.lock().unwrap().insert(id.clone(), info.clone());
        self.audit(&id, "loomux", if resumed { "group-resume" } else { "group-create" },
            json!({ "repo": repo, "max_agents": info.guardrails.max_agents,
                    "worker_model": info.guardrails.worker_model }));
        Ok(info)
    }

    pub fn group(&self, id: &str) -> Option<GroupInfo> {
        self.groups.lock().unwrap().get(id).cloned()
    }

    /// A group is live while any of its agents is not dead.
    fn group_is_live(&self, id: &str) -> bool {
        self.agents
            .lock()
            .unwrap()
            .values()
            .any(|a| a.group == id && a.status != AgentStatus::Dead)
    }

    /// Render the role instruction docs into the group dir so kickoff
    /// prompts can reference them by path instead of pasting pages of text.
    fn write_instruction_files(&self, g: &GroupInfo) -> Result<(), String> {
        let vars = [
            ("REPO", g.repo.as_str()),
            ("GROUP_ID", g.id.as_str()),
            ("MAX_AGENTS", &g.guardrails.max_agents.to_string()),
            ("WORKER_MODEL", g.guardrails.worker_model.as_str()),
            ("REVIEWER_MODEL", g.guardrails.reviewer_model.as_str()),
        ];
        let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, *v)).collect();
        let dir = self.group_dir(&g.id);
        for role in [Role::Orchestrator, Role::Worker, Role::Reviewer] {
            fs::write(dir.join(role.instructions_file()), render_template(role.template(), &vars))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn resolve_token(&self, token: &str) -> Option<Caller> {
        let id = self.by_token.lock().unwrap().get(token).cloned()?;
        let agents = self.agents.lock().unwrap();
        let a = agents.get(&id)?;
        if a.status == AgentStatus::Dead {
            return None;
        }
        Some(Caller { agent_id: a.id.clone(), group: a.group.clone(), role: a.role })
    }

    pub fn agent(&self, id: &str) -> Option<AgentEntry> {
        self.agents.lock().unwrap().get(id).cloned()
    }

    fn live_delegate_count(&self, group: &str) -> u32 {
        self.agents
            .lock()
            .unwrap()
            .values()
            .filter(|a| a.group == group && a.role != Role::Orchestrator && a.status != AgentStatus::Dead)
            .count() as u32
    }

    /// Write the per-agent MCP config the agent CLI connects with. Claude
    /// and Copilot share the same core schema; Copilot additionally expects
    /// a `tools` allowlist inside the server entry.
    fn write_mcp_config(
        &self,
        group: &str,
        agent_id: &str,
        token: &str,
        cli: &str,
    ) -> Result<PathBuf, String> {
        let port = self.port();
        if port == 0 {
            return Err("loomux MCP server is not running".into());
        }
        let mut server = json!({
            "type": "http",
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "headers": { "X-Loomux-Agent": token },
        });
        if cli == "copilot" {
            server["tools"] = json!(["*"]);
        }
        let cfg = json!({ "mcpServers": { "loomux": server } });
        let dir = self.group_dir(group).join("configs");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{agent_id}.json"));
        fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Build an agent's launch command for the group's CLI. Baseline
    /// permissions minimize the approvals needed just to *initialize*: the
    /// group state dir is added as a workspace (so reading the instructions
    /// file never prompts) and the loomux MCP tools are pre-approved (so
    /// `report` etc. never prompt). `auto_ops` additionally pre-approves
    /// git/gh commands so the branch→commit→PR flow runs unattended;
    /// everything else still asks the human.
    #[allow(clippy::too_many_arguments)]
    #[doc(hidden)] // pub for integration tests
    pub fn build_agent_command(
        &self,
        cli: &str,
        model: &str,
        auto_ops: bool,
        cfg: &Path,
        group_dir: &Path,
        workdir: &Path,
        session: Option<&str>,
        resume: bool,
    ) -> String {
        match cli {
            "copilot" => {
                // Copilot has `--resume <id>` but no way to pre-assign an
                // id, so sessions aren't tracked for it (yet).
                let resume_flag = match (session, resume) {
                    (Some(s), true) => format!("--resume {s} "),
                    _ => String::new(),
                };
                // NOTE: the @ (copilot's file-path marker) must sit INSIDE
                // the quotes — the pane shell is PowerShell, where a bare
                // `@"` opens a here-string and the whole line dies with a
                // ParserError before copilot ever runs.
                // --no-auto-update: a mid-boot self-update restarts the
                // CLI and flushes anything typed into the first instance.
                // --add-dir <workdir>: pre-trusts the agent's workspace so
                // panes don't stall on a folder-trust prompt.
                let mut cmd = format!(
                    "copilot {resume_flag}--additional-mcp-config \"@{}\" --model {model} \
                     --add-dir \"{}\" --add-dir \"{}\" --allow-tool loomux --no-auto-update",
                    cfg.display(),
                    group_dir.display(),
                    workdir.display()
                );
                if auto_ops {
                    // Copilot's own unattended mode: autopilot + all tools
                    // + no path-verification prompts.
                    cmd.push_str(" --autopilot --allow-all-tools --allow-all-paths");
                } else {
                    cmd.push_str(" --allow-tool \"shell(git:*)\" --allow-tool \"shell(gh:*)\"");
                }
                cmd
            }
            // "claude" and the explicit fallback for anything unrecognized.
            _ => {
                // Assigning the session id up front is what makes per-task
                // sessions resumable later: loomux never has to fish the id
                // out of the CLI.
                let session_flag = match (session, resume) {
                    (Some(s), true) => format!("--resume {s} "),
                    (Some(s), false) => format!("--session-id {s} "),
                    (None, _) => String::new(),
                };
                // "Auto" preset = Claude Code's native auto permission mode
                // (what the human uses interactively); "edits" = acceptEdits.
                let perm = if auto_ops { "auto" } else { "acceptEdits" };
                let mut cmd = format!(
                    "claude {session_flag}--mcp-config \"{}\" --strict-mcp-config --model {model} \
                     --permission-mode {perm} --add-dir \"{}\" --allowedTools mcp__loomux",
                    cfg.display(),
                    group_dir.display()
                );
                if auto_ops {
                    // Both rule spellings: docs use `Bash(git:*)`, the CLI
                    // help shows `Bash(git *)`; unmatched patterns are inert.
                    cmd.push_str(" \"Bash(git:*)\" \"Bash(git *)\" \"Bash(gh:*)\" \"Bash(gh *)\"");
                }
                cmd
            }
        }
    }

    /// Register an agent, emit the pane spawn request, wait for the frontend
    /// bind, then type the kickoff prompt. Enforces the group guardrails.
    /// `task` empty = idle agent awaiting assignment.
    pub fn spawn_agent(
        &self,
        group_id: &str,
        role: Role,
        name: &str,
        task: &str,
        use_worktree: bool,
        branch: Option<String>,
    ) -> Result<AgentEntry, String> {
        self.spawn_agent_ex(group_id, role, name, task, use_worktree, branch, None, None)
    }

    /// Full spawn: `resume_session` reopens a previous session (follow-ups
    /// on a finished task) instead of cold-starting; `cwd_override` places
    /// the pane where that work originally happened (e.g. its worktree).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_agent_ex(
        &self,
        group_id: &str,
        role: Role,
        name: &str,
        task: &str,
        use_worktree: bool,
        branch: Option<String>,
        resume_session: Option<String>,
        cwd_override: Option<String>,
    ) -> Result<AgentEntry, String> {
        let group = self.group(group_id).ok_or("unknown group")?;

        // Guardrail: live delegate cap (the orchestrator itself is exempt).
        if role != Role::Orchestrator {
            let live = self.live_delegate_count(group_id);
            if live >= group.guardrails.max_agents {
                return Err(format!(
                    "guardrail: {live} live agents already (max {}). Reuse an idle agent or kill one first.",
                    group.guardrails.max_agents
                ));
            }
        }

        // Guardrail: the model is pinned per role at group creation.
        let model = match role {
            Role::Orchestrator => &group.guardrails.orchestrator_model,
            Role::Worker => &group.guardrails.worker_model,
            Role::Reviewer => &group.guardrails.reviewer_model,
        };

        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let agent_id = format!("{}-{seq}", role.prefix());
        let token = new_token();
        let display = {
            let n = name.trim();
            let n = if n.is_empty() { agent_id.as_str() } else { n };
            n.chars().take(40).collect::<String>()
        };

        // Workspace: dedicated worktree (branch of the same name) or the repo
        // itself, where the worker is instructed to branch before touching
        // anything.
        // Session identity: resumes reuse the given id; fresh Claude agents
        // get a pre-assigned UUID so their session is resumable later.
        let resume = resume_session.is_some();
        let session_id = match resume_session {
            Some(s) => Some(sanitize_session(&s).ok_or("invalid resume session id")?),
            None => (group.guardrails.agent_cli == "claude").then(new_session_uuid),
        };

        let branch_name = branch
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| format!("agent/{agent_id}"));
        let cwd_override = cwd_override.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        let (cwd, branch_note) = if let Some(c) = cwd_override {
            if !Path::new(&c).is_dir() {
                return Err(format!("cwd does not exist: {c}"));
            }
            (c, String::new())
        } else if use_worktree && role != Role::Orchestrator {
            let wt = crate::git::git_worktree_add(group.repo.clone(), branch_name.clone())?;
            (wt.clone(), format!(
                "Your working directory is a dedicated git worktree at {wt} already checked out on branch '{branch_name}'."
            ))
        } else if role == Role::Orchestrator {
            (group.repo.clone(), String::new())
        } else if role == Role::Reviewer {
            (group.repo.clone(), "You review; you do not create branches or push. Inspect PRs via gh (checking out the PR branch locally is fine).".to_string())
        } else {
            (group.repo.clone(), format!(
                "Work in the repo itself; create branch '{branch_name}' off the default branch before changing anything. Never commit to the default branch."
            ))
        };

        if group.guardrails.agent_cli == "copilot" {
            pre_trust_copilot_folder(&cwd);
        }
        let cfg = self.write_mcp_config(group_id, &agent_id, &token, &group.guardrails.agent_cli)?;
        let command = self.build_agent_command(
            &group.guardrails.agent_cli,
            model,
            group.guardrails.auto_ops,
            &cfg,
            &self.group_dir(group_id),
            Path::new(&cwd),
            session_id.as_deref(),
            resume,
        );

        let entry = AgentEntry {
            id: agent_id.clone(),
            group: group_id.to_string(),
            name: display.clone(),
            role,
            token: token.clone(),
            status: AgentStatus::Starting,
            pty_id: None,
            task: task.to_string(),
            session_id: session_id.clone(),
            cwd: cwd.clone(),
        };
        {
            // Re-check the cap under the same lock as the insert: the early
            // check above fast-fails before worktree creation, but only this
            // one is race-free against concurrent spawns.
            let mut agents = self.agents.lock().unwrap();
            if role != Role::Orchestrator {
                let live = agents
                    .values()
                    .filter(|a| {
                        a.group == group_id
                            && a.role != Role::Orchestrator
                            && a.status != AgentStatus::Dead
                    })
                    .count() as u32;
                if live >= group.guardrails.max_agents {
                    let _ = fs::remove_file(&cfg);
                    return Err(format!(
                        "guardrail: {live} live agents already (max {})",
                        group.guardrails.max_agents
                    ));
                }
            }
            agents.insert(agent_id.clone(), entry.clone());
        }
        self.by_token.lock().unwrap().insert(token, agent_id.clone());
        self.persist_agent_record(&entry, "running");
        self.audit(group_id, "loomux", "agent-spawn", json!({
            "agent": agent_id, "role": role, "name": display, "cwd": cwd,
            "model": model, "worktree": use_worktree, "branch": branch_name, "task": task,
            "session": session_id, "resume": resume,
        }));

        let request = SpawnRequest {
            group_id: group_id.to_string(),
            agent_id: agent_id.clone(),
            role,
            name: display,
            cwd: cwd.clone(),
            command,
        };

        let app = self.app.lock().unwrap().clone();
        let Some(app) = app else {
            // Test mode: no frontend. Mark running so guardrail/authz logic
            // can be exercised without panes.
            self.agents.lock().unwrap().get_mut(&agent_id).unwrap().status = AgentStatus::Running;
            return Ok(self.agent(&agent_id).unwrap());
        };

        let (tx, rx) = mpsc::channel::<u32>();
        self.pending_binds.lock().unwrap().insert(agent_id.clone(), tx);
        app.emit("orch-spawn-request", &request).map_err(|e| e.to_string())?;

        match rx.recv_timeout(BIND_TIMEOUT) {
            Ok(pty_id) => {
                {
                    let mut agents = self.agents.lock().unwrap();
                    if let Some(a) = agents.get_mut(&agent_id) {
                        a.status = AgentStatus::Running;
                        a.pty_id = Some(pty_id);
                    }
                }
                self.by_pty.lock().unwrap().insert(pty_id, agent_id.clone());
                self.audit(group_id, "loomux", "agent-bind", json!({ "agent": agent_id, "pty": pty_id }));
                if resume {
                    // Resumed sessions already have their role and history;
                    // deliver only the follow-up (if any) instead of the
                    // full kickoff.
                    if !task.trim().is_empty() {
                        self.deliver_prompt(&agent_id, task, "loomux", true)?;
                    }
                } else {
                    let kickoff =
                        self.kickoff_prompt(&self.agent(&agent_id).unwrap(), &group, &branch_note);
                    self.deliver_prompt(&agent_id, &kickoff, "loomux", true)?;
                }
                Ok(self.agent(&agent_id).unwrap())
            }
            Err(_) => {
                self.pending_binds.lock().unwrap().remove(&agent_id);
                self.mark_dead(&agent_id, None);
                Err("frontend did not open the agent pane in time".into())
            }
        }
    }

    #[doc(hidden)] // pub for integration tests
    pub fn kickoff_prompt(&self, a: &AgentEntry, g: &GroupInfo, branch_note: &str) -> String {
        let instructions = self.group_dir(&g.id).join(a.role.instructions_file());
        match a.role {
            Role::Orchestrator => format!(
                "You are the orchestrator of loomux agent group {gid} for the repository {repo}.\n\
                 First read your role instructions: {ins}\n\
                 Guardrails (enforced by loomux): max {max} live agents, worker model {wm}, reviewer model {rm}.\n\
                 Start by calling get_state, run `gh issue list --label agent-managed --state open`, call list_agents, \
                 reconcile them, then give the human a short status summary and wait for direction.",
                gid = g.id, repo = g.repo, ins = instructions.display(),
                max = g.guardrails.max_agents, wm = g.guardrails.worker_model, rm = g.guardrails.reviewer_model,
            ),
            Role::Worker | Role::Reviewer => {
                let head = format!(
                    "You are \"{name}\" ({id}), a {role} agent in loomux group {gid} for repository {repo}.\n\
                     First read your role instructions: {ins}\n{note}",
                    name = a.name, id = a.id,
                    role = if a.role == Role::Worker { "worker" } else { "reviewer" },
                    gid = g.id, repo = g.repo, ins = instructions.display(), note = branch_note,
                );
                if a.task.trim().is_empty() {
                    format!("{head}\nNo task is assigned yet. After reading the instructions, call report(\"progress\", \"ready\") and wait for prompts.")
                } else {
                    format!("{head}\nYour task:\n{}", a.task)
                }
            }
        }
    }

    /// Type `text` into an agent's CLI: audit, then bracketed paste + Enter
    /// on a background thread (serialized so deliveries never interleave).
    /// `wait_ready` is for freshly spawned CLIs: the paste is held until the
    /// pane's output shows the CLI has painted its UI and gone quiet —
    /// input typed before a CLI's reader attaches is flushed and lost.
    pub fn deliver_prompt(
        &self,
        agent_id: &str,
        text: &str,
        from: &str,
        wait_ready: bool,
    ) -> Result<(), String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        if a.status == AgentStatus::Dead {
            return Err(format!("agent {agent_id} is dead"));
        }
        let pty_id = a.pty_id.ok_or("agent has no terminal yet")?;
        let app = self.app.lock().unwrap().clone().ok_or("no app handle")?;
        self.audit(&a.group, from, "prompt", json!({ "to": agent_id, "text": text }));

        let paste = bracketed_paste(text);
        let lock = self
            .delivery
            .lock()
            .unwrap()
            .entry(pty_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let (root, group, agent) = (self.root.clone(), a.group.clone(), a.id.clone());
        std::thread::spawn(move || {
            let _guard = lock.lock().unwrap();
            let ptys = app.state::<crate::pty::PtyManager>();

            let start = std::time::Instant::now();
            if wait_ready {
                let mut last_len = 0usize;
                let mut last_change = std::time::Instant::now();
                loop {
                    std::thread::sleep(READY_POLL);
                    let Some(out) = ptys.output_tail(pty_id) else {
                        append_audit(&root, &group, "loomux", "prompt-failed",
                            json!({ "to": agent, "reason": "terminal closed while waiting for CLI to become ready" }));
                        return;
                    };
                    if out.len() != last_len {
                        last_len = out.len();
                        last_change = std::time::Instant::now();
                    }
                    if cli_ready(last_len, last_change.elapsed(), start.elapsed()) {
                        break;
                    }
                    if start.elapsed() >= READY_MAX_WAIT {
                        // Paste anyway — better a visible prompt the human
                        // can re-submit than one silently withheld.
                        break;
                    }
                }
            }

            // Echo-verified typing: paste, then require the TUI to emit
            // output (its input box redrawing). No echo means the CLI
            // flushed the paste with its startup stdin buffer — retype.
            let mut echoed = false;
            let mut attempts = 0u32;
            while attempts < ECHO_ATTEMPTS {
                attempts += 1;
                let Some(before) = ptys.output_total(pty_id) else {
                    append_audit(&root, &group, "loomux", "prompt-failed",
                        json!({ "to": agent, "reason": "terminal closed before delivery" }));
                    return;
                };
                if ptys.write_bytes(pty_id, &paste).is_err() {
                    append_audit(&root, &group, "loomux", "prompt-failed",
                        json!({ "to": agent, "reason": "terminal closed before delivery" }));
                    return;
                }
                let echo_deadline = std::time::Instant::now() + ECHO_WINDOW;
                while std::time::Instant::now() < echo_deadline {
                    std::thread::sleep(Duration::from_millis(150));
                    match ptys.output_total(pty_id) {
                        Some(now_total) if now_total >= before + ECHO_MIN_BYTES => {
                            echoed = true;
                            break;
                        }
                        Some(_) => {}
                        None => {
                            append_audit(&root, &group, "loomux", "prompt-failed",
                                json!({ "to": agent, "reason": "terminal closed during delivery" }));
                            return;
                        }
                    }
                }
                if echoed {
                    break;
                }
                std::thread::sleep(ECHO_RETRY_DELAY);
            }
            std::thread::sleep(PASTE_SUBMIT_DELAY);

            // Wait for the pane to go quiet before Enter: a busy CLI
            // (mid-turn) ignores the submit and the prompt would sit in
            // the input box until a human presses Enter.
            let submit_start = std::time::Instant::now();
            let mut last_total = ptys.output_total(pty_id).unwrap_or(0);
            let mut last_change = std::time::Instant::now();
            while submit_start.elapsed() < SUBMIT_MAX_WAIT {
                std::thread::sleep(Duration::from_millis(200));
                match ptys.output_total(pty_id) {
                    Some(t) if t != last_total => {
                        last_total = t;
                        last_change = std::time::Instant::now();
                    }
                    Some(_) => {
                        if last_change.elapsed() >= SUBMIT_QUIET {
                            break;
                        }
                    }
                    None => {
                        append_audit(&root, &group, "loomux", "prompt-failed",
                            json!({ "to": agent, "reason": "terminal closed before submit" }));
                        return;
                    }
                }
            }
            let submit_sent_ms = now_ms();
            let _ = ptys.write_bytes(pty_id, b"\r");
            for delay in SUBMIT_RETRY_DELAYS {
                std::thread::sleep(delay);
                // A human typing in this pane means the box may hold THEIR
                // half-written text — a blind Enter would submit it.
                if ptys.last_user_input_ms(pty_id).unwrap_or(0) > submit_sent_ms {
                    append_audit(&root, &group, "loomux", "submit-retries-skipped",
                        json!({ "to": agent, "reason": "human typing in pane" }));
                    break;
                }
                if ptys.write_bytes(pty_id, b"\r").is_err() {
                    break;
                }
            }
            append_audit(&root, &group, "loomux", "prompt-typed", json!({
                "to": agent,
                "waited_ms": start.elapsed().as_millis() as u64,
                "attempts": attempts,
                "echoed": echoed,
                "submit_waited_ms": submit_start.elapsed().as_millis() as u64,
            }));
        });
        Ok(())
    }

    /// Deliver to the group's orchestrator (worker reports, exit notices).
    pub fn deliver_to_orchestrator(&self, group: &str, text: &str, from: &str) -> Result<(), String> {
        let orch = self
            .agents
            .lock()
            .unwrap()
            .values()
            .find(|a| a.group == group && a.role == Role::Orchestrator && a.status != AgentStatus::Dead)
            .map(|a| a.id.clone())
            .ok_or("no live orchestrator in this group")?;
        self.deliver_prompt(&orch, text, from, false)
    }

    pub fn list_agents(&self, group: &str) -> Value {
        let agents = self.agents.lock().unwrap();
        let mut list: Vec<Value> = agents
            .values()
            .filter(|a| a.group == group)
            .map(|a| json!({
                "id": a.id, "name": a.name, "role": a.role,
                "status": a.status, "task": a.task,
                "session": a.session_id, "cwd": a.cwd,
            }))
            .collect();
        list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        json!(list)
    }

    pub fn agent_output_tail(&self, agent_id: &str, lines: usize) -> Result<String, String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        let pty_id = a.pty_id.ok_or("agent has no terminal")?;
        let app = self.app.lock().unwrap().clone().ok_or("no app handle")?;
        let ptys = app.state::<crate::pty::PtyManager>();
        let raw = ptys.output_tail(pty_id).ok_or("terminal already closed")?;
        let text = strip_ansi(&raw);
        let all: Vec<&str> = text.lines().collect();
        let n = lines.clamp(1, 500);
        let start = all.len().saturating_sub(n);
        Ok(all[start..].join("\n"))
    }

    pub fn kill_agent(&self, agent_id: &str) -> Result<(), String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        if a.role == Role::Orchestrator {
            return Err("refusing to kill the orchestrator; close its pane instead".into());
        }
        let app = self.app.lock().unwrap().clone().ok_or("no app handle")?;
        if let Some(pty) = a.pty_id {
            app.state::<crate::pty::PtyManager>().kill(pty);
        }
        self.audit(&a.group, "loomux", "agent-kill", json!({ "agent": agent_id }));
        Ok(())
    }

    pub fn focus_agent(&self, agent_id: &str) -> Result<(), String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        let app = self.app.lock().unwrap().clone().ok_or("no app handle")?;
        app.emit("orch-focus", json!({ "agent_id": agent_id, "pty_id": a.pty_id }))
            .map_err(|e| e.to_string())
    }

    #[doc(hidden)] // pub for integration tests
    pub fn mark_dead(&self, agent_id: &str, exit_code: Option<u32>) -> Option<AgentEntry> {
        let mut agents = self.agents.lock().unwrap();
        let a = agents.get_mut(agent_id)?;
        if a.status == AgentStatus::Dead {
            return None;
        }
        a.status = AgentStatus::Dead;
        let snapshot = a.clone();
        drop(agents);
        self.by_token.lock().unwrap().remove(&snapshot.token);
        if let Some(p) = snapshot.pty_id {
            self.by_pty.lock().unwrap().remove(&p);
            self.delivery.lock().unwrap().remove(&p);
        }
        let _ = fs::remove_file(
            self.group_dir(&snapshot.group).join("configs").join(format!("{agent_id}.json")),
        );
        self.audit(&snapshot.group, "loomux", "agent-exit",
            json!({ "agent": agent_id, "exit_code": exit_code }));
        self.persist_agent_record(&snapshot, "dead");
        Some(snapshot)
    }

    /// Called from the pty waiter thread when any pty exits. No-op for ptys
    /// that aren't orchestration agents.
    pub fn on_pty_exit(&self, pty_id: u32, exit_code: Option<u32>) {
        let agent_id = match self.by_pty.lock().unwrap().get(&pty_id).cloned() {
            Some(id) => id,
            None => return,
        };
        if let Some(a) = self.mark_dead(&agent_id, exit_code) {
            if a.role != Role::Orchestrator {
                let _ = self.deliver_to_orchestrator(
                    &a.group,
                    &format!(
                        "[loomux] agent {} ({}) exited (code {:?}). Update your plan and state accordingly.",
                        a.name, a.id, exit_code
                    ),
                    "loomux",
                );
            }
        }
    }

    #[doc(hidden)] // pub for integration tests
    pub fn state_root(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn bind(&self, agent_id: &str, pty_id: u32) -> Result<(), String> {
        let tx = self
            .pending_binds
            .lock()
            .unwrap()
            .remove(agent_id)
            .ok_or_else(|| format!("no pending bind for agent {agent_id}"))?;
        tx.send(pty_id).map_err(|_| "spawner is gone (bind timed out)".to_string())
    }
}

// ---------- tauri commands ----------

/// Create (or reattach to) an orchestration group and register its
/// orchestrator. Returns the pane spec the frontend opens directly; initial
/// idle workers are spawned in the background once the orchestrator binds.
#[tauri::command]
pub fn create_orchestration(
    reg: tauri::State<Arc<OrchRegistry>>,
    repo: String,
    initial_workers: u32,
    max_agents: u32,
    agent_cli: String,
    worker_model: String,
    reviewer_model: String,
    orchestrator_model: String,
    auto_ops: bool,
) -> Result<SpawnRequest, String> {
    create_orchestration_group(
        reg.inner(),
        &repo,
        Guardrails {
            max_agents,
            agent_cli,
            worker_model,
            reviewer_model,
            orchestrator_model,
            auto_ops,
        },
        None,
        None,
        initial_workers,
    )
}

/// Create (or reattach to) a group and register its orchestrator, under the
/// creation lock: the group id is picked by liveness, and a group only
/// becomes live once its orchestrator is registered, so id selection and
/// registration must be atomic against concurrent launches.
/// `expect_group` pins restores to their recorded group id.
pub fn create_orchestration_group(
    reg: &Arc<OrchRegistry>,
    repo: &str,
    guardrails: Guardrails,
    resume_session: Option<String>,
    expect_group: Option<&str>,
    initial_workers: u32,
) -> Result<SpawnRequest, String> {
    // Paths are interpolated into a quoted shell line; a quote inside one
    // would escape it. (Windows filesystems forbid `"` in names; this
    // guards the Unix builds and hand-typed paths.)
    if repo.contains('"') {
        return Err("repository path must not contain a quote character".into());
    }
    if !Path::new(repo).is_dir() {
        return Err(format!("repository path does not exist: {repo}"));
    }
    let _creation = reg.creation.lock().unwrap();
    let group = reg.create_group(repo, guardrails)?;
    if let Some(want) = expect_group {
        if group.id != want {
            return Err(format!(
                "group id mismatch (recorded {want}, resolved {}) — another orchestration is live on this repo",
                group.id
            ));
        }
    }
    register_orchestrator_pane(reg, &group, resume_session, initial_workers)
}

/// Register a group's orchestrator and hand back the pane spec the frontend
/// opens. `resume_session` reopens a prior orchestrator conversation (with
/// fresh MCP wiring) instead of starting cold. A background thread waits
/// for the pane bind, types the kickoff/re-sync prompt, and brings up any
/// initial idle workers.
fn register_orchestrator_pane(
    reg: &Arc<OrchRegistry>,
    group: &GroupInfo,
    resume_session: Option<String>,
    initial_workers: u32,
) -> Result<SpawnRequest, String> {
    let model = group.guardrails.orchestrator_model.clone();
    let token = new_token();
    let agent_id = format!("orch-{}", reg.seq.fetch_add(1, Ordering::SeqCst) + 1);
    if group.guardrails.agent_cli == "copilot" {
        pre_trust_copilot_folder(&group.repo);
    }
    let cfg = reg.write_mcp_config(&group.id, &agent_id, &token, &group.guardrails.agent_cli)?;
    let resume = resume_session.is_some();
    let session_id = match resume_session {
        Some(s) => Some(sanitize_session(&s).ok_or("invalid resume session id")?),
        None => (group.guardrails.agent_cli == "claude").then(new_session_uuid),
    };
    let command = reg.build_agent_command(
        &group.guardrails.agent_cli,
        &model,
        group.guardrails.auto_ops,
        &cfg,
        &reg.group_dir(&group.id),
        Path::new(&group.repo),
        session_id.as_deref(),
        resume,
    );
    let entry = AgentEntry {
        id: agent_id.clone(),
        group: group.id.clone(),
        name: "orchestrator".into(),
        role: Role::Orchestrator,
        token: token.clone(),
        status: AgentStatus::Starting,
        pty_id: None,
        task: String::new(),
        session_id,
        cwd: group.repo.clone(),
    };
    reg.agents.lock().unwrap().insert(agent_id.clone(), entry.clone());
    reg.by_token.lock().unwrap().insert(token, agent_id.clone());
    reg.persist_agent_record(&entry, "running");
    reg.audit(&group.id, "loomux", "agent-spawn",
        json!({ "agent": agent_id, "role": "orchestrator", "model": model,
                "session": entry.session_id, "resume": resume }));

    let request = SpawnRequest {
        group_id: group.id.clone(),
        agent_id: agent_id.clone(),
        role: Role::Orchestrator,
        name: "orchestrator".into(),
        cwd: group.repo.clone(),
        command,
    };

    if reg.app.lock().unwrap().is_none() {
        // Test mode: no frontend; mark running without a pane.
        reg.agents.lock().unwrap().get_mut(&agent_id).unwrap().status = AgentStatus::Running;
        return Ok(request);
    }

    // Background: wait for the orchestrator pane to bind, type its kickoff,
    // then bring up the initial idle workers one by one.
    let (tx, rx) = mpsc::channel::<u32>();
    reg.pending_binds.lock().unwrap().insert(agent_id.clone(), tx);
    let reg2 = reg.clone();
    let group2 = group.clone();
    std::thread::spawn(move || {
        let Ok(pty_id) = rx.recv_timeout(BIND_TIMEOUT) else {
            reg2.pending_binds.lock().unwrap().remove(&agent_id);
            reg2.mark_dead(&agent_id, None);
            return;
        };
        {
            let mut agents = reg2.agents.lock().unwrap();
            if let Some(a) = agents.get_mut(&agent_id) {
                a.status = AgentStatus::Running;
                a.pty_id = Some(pty_id);
            }
        }
        reg2.by_pty.lock().unwrap().insert(pty_id, agent_id.clone());
        reg2.audit(&group2.id, "loomux", "agent-bind", json!({ "agent": agent_id, "pty": pty_id }));
        let kickoff = if resume {
            "[loomux] Orchestration restored: your MCP tools, the task board, and the audit log are live again in this session. Re-sync now: list_tasks, list_agents, get_state. Your previous worker panes are gone; resume a task session with spawn_agent(resume_session, cwd) when follow-ups need it. Then give the human a short status summary.".to_string()
        } else {
            reg2.kickoff_prompt(&reg2.agent(&agent_id).unwrap(), &group2, "")
        };
        let _ = reg2.deliver_prompt(&agent_id, &kickoff, "loomux", true);
        for i in 0..initial_workers.min(group2.guardrails.max_agents) {
            if let Err(e) = reg2.spawn_agent(&group2.id, Role::Worker, &format!("worker {}", i + 1), "", false, None)
            {
                reg2.audit(&group2.id, "loomux", "error",
                    json!({ "what": "initial worker spawn failed", "err": e }));
                break;
            }
        }
    });

    Ok(request)
}

/// Restore orchestration for a recorded session id (from the session
/// browser). An orchestrator session of a dead group relaunches the whole
/// control plane — group, MCP identity, task board — resuming that
/// conversation, and returns the pane spec for the frontend to open. A
/// worker/reviewer session rejoins its live group; its pane arrives via the
/// normal orch-spawn-request event (the spawn must not block this IPC
/// thread, which also serves the bind), so `None` is returned.
pub fn resume_recorded_session(
    reg: &Arc<OrchRegistry>,
    session_id: &str,
    hint: Option<(String, String)>, // (group_id, role) from transcript signatures
) -> Result<Option<SpawnRequest>, String> {
    let record = reg
        .session_roles()
        .into_iter()
        .filter(|r| r.session_id == session_id)
        .last()
        .or_else(|| {
            // Sessions from before the roster (and before session-id
            // tracking) are identified by loomux signatures in their own
            // transcript; trust the hint if that group exists on disk.
            let (group_id, role) = hint?;
            if !reg.group_dir(&group_id).join("group.json").is_file() {
                return None;
            }
            let group_live = reg.group_is_live(&group_id);
            Some(SessionRole {
                session_id: session_id.to_string(),
                agent_name: if role == "orchestrator" { "orchestrator".into() } else { "agent".into() },
                group_id,
                role,
                group_live,
            })
        })
        .ok_or("this session is not part of a recorded orchestration")?;

    if record.role == "orchestrator" {
        if record.group_live {
            return Err(format!(
                "group {} already has a live orchestrator — focus its pane instead",
                record.group_id
            ));
        }
        let (repo, guardrails) = reg
            .load_group_file(&record.group_id)
            .ok_or("group.json is missing for this orchestration")?;
        return create_orchestration_group(
            reg,
            &repo,
            guardrails,
            Some(session_id.to_string()),
            Some(&record.group_id),
            0,
        )
        .map(Some);
    }

    // Worker / reviewer: only meaningful inside a live group.
    if !record.group_live {
        return Err(
            "this agent's group is not running — restart its orchestrator session (marked ORCH) first"
                .into(),
        );
    }
    let role = if record.role == "reviewer" { Role::Reviewer } else { Role::Worker };
    let cwd = reg
        .merged_records(&record.group_id)
        .into_iter()
        .find(|r| r.session.as_deref() == Some(session_id))
        .map(|r| r.cwd)
        .filter(|c| Path::new(c).is_dir());
    let reg2 = reg.clone();
    let sid = session_id.to_string();
    let (group_id, name) = (record.group_id.clone(), record.agent_name.clone());
    std::thread::spawn(move || {
        if let Err(e) =
            reg2.spawn_agent_ex(&group_id, role, &name, "", false, None, Some(sid.clone()), cwd)
        {
            reg2.audit(&group_id, "loomux", "error",
                json!({ "what": "session rejoin failed", "session": sid, "err": e.clone() }));
            let _ = reg2.deliver_to_orchestrator(
                &group_id,
                &format!("[loomux] failed to resume session {sid} into this group: {e}"),
                "loomux",
            );
        }
    });
    Ok(None)
}

#[tauri::command]
pub fn bind_agent(reg: tauri::State<Arc<OrchRegistry>>, agent_id: String, pty_id: u32) -> Result<(), String> {
    reg.bind(&agent_id, pty_id)
}

/// Session ↔ orchestration-role mapping for the session browser badges.
#[tauri::command]
pub fn orch_session_roles(reg: tauri::State<Arc<OrchRegistry>>) -> Vec<SessionRole> {
    reg.session_roles()
}

/// Restore a recorded orchestration session (see `resume_recorded_session`).
/// Returns the orchestrator pane spec, or null when the pane will arrive
/// via `orch-spawn-request` (worker/reviewer rejoin).
#[tauri::command]
pub fn resume_orch_session(
    reg: tauri::State<Arc<OrchRegistry>>,
    session_id: String,
    group_hint: Option<String>,
    role_hint: Option<String>,
) -> Result<Option<SpawnRequest>, String> {
    let hint = group_hint.zip(role_hint);
    resume_recorded_session(reg.inner(), &session_id, hint)
}

// ---------- task board (human side) ----------
// The pane overlay edits the same tasks.json the orchestrator manages via
// MCP. Human edits are audited as actor "human" and (except reorders, which
// are too chatty) surface in the orchestrator pane as a typed notice.

#[tauri::command]
pub fn orch_tasks(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Vec<Task> {
    reg.tasks(&group_id)
}

/// Audit-log timeline for the pane's audit-viewer overlay (read-only). Oldest
/// first; the frontend filters, expands prompt texts, and — in follow mode —
/// re-polls this command.
#[tauri::command]
pub fn orch_audit(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Vec<AuditEntry> {
    reg.audit_log(&group_id)
}

#[tauri::command]
pub fn orch_upsert_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: Option<String>,
    title: Option<String>,
    status: Option<String>,
    note: Option<String>,
) -> Result<Task, String> {
    let task = reg.upsert_task(
        &group_id,
        "human",
        id.as_deref(),
        TaskPatch { title, status, note, ..Default::default() },
    )?;
    reg.notify_board_edit(&group_id, &format!("{} \"{}\" is now {}", task.id, task.title, task.status));
    Ok(task)
}

#[tauri::command]
pub fn orch_delete_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
) -> Result<(), String> {
    reg.delete_task(&group_id, "human", &id)?;
    reg.notify_board_edit(&group_id, &format!("deleted task {id}"));
    Ok(())
}

#[tauri::command]
pub fn orch_reorder_tasks(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    ids: Vec<String>,
) -> Result<(), String> {
    // No typed notice: reorders come in bursts; board order is read via
    // list_tasks whenever the orchestrator plans.
    reg.reorder_tasks(&group_id, "human", &ids)
}
