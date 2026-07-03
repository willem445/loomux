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
/// Delay before typing a kickoff prompt into a freshly spawned CLI, so the
/// agent's input box exists by the time the paste arrives.
const KICKOFF_BOOT_DELAY: Duration = Duration::from_millis(4000);
/// Gap between the bracketed paste and the Enter that submits it.
const PASTE_SUBMIT_DELAY: Duration = Duration::from_millis(300);
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

/// Identity resolved from an MCP request's token header.
#[derive(Clone, Debug)]
pub struct Caller {
    pub agent_id: String,
    pub group: String,
    pub role: Role,
}

/// Payload asking the frontend to open a pane for an agent. Also the return
/// value of `create_orchestration` (the orchestrator's own pane).
#[derive(Clone, Serialize)]
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
    /// Serializes typed deliveries so two prompts can't interleave keystrokes.
    delivery: Arc<Mutex<()>>,
    /// Serializes task-board read-modify-write cycles (MCP threads and the
    /// human UI mutate the same tasks.json).
    tasks_lock: Mutex<()>,
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

/// Stable, filesystem-safe group id for a repo path, so relaunching an
/// orchestrator on the same repo reattaches to the same state directory.
fn group_id_for_repo(repo: &str) -> String {
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
            delivery: Arc::new(Mutex::new(())),
            tasks_lock: Mutex::new(()),
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
        let line = json!({ "ts_ms": now_ms(), "actor": actor, "action": action, "detail": detail });
        let path = self.group_dir(group).join("audit.jsonl");
        let _ = fs::create_dir_all(self.group_dir(group));
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
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
    #[doc(hidden)] // pub for integration tests
    pub fn build_agent_command(
        &self,
        cli: &str,
        model: &str,
        auto_ops: bool,
        cfg: &Path,
        group_dir: &Path,
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
                let mut cmd = format!(
                    "copilot {resume_flag}--additional-mcp-config @\"{}\" --model {model} \
                     --add-dir \"{}\" --allow-tool loomux",
                    cfg.display(),
                    group_dir.display()
                );
                if auto_ops {
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
        } else {
            (group.repo.clone(), format!(
                "Work in the repo itself; create branch '{branch_name}' off the default branch before changing anything. Never commit to the default branch."
            ))
        };

        let cfg = self.write_mcp_config(group_id, &agent_id, &token, &group.guardrails.agent_cli)?;
        let command = self.build_agent_command(
            &group.guardrails.agent_cli,
            model,
            group.guardrails.auto_ops,
            &cfg,
            &self.group_dir(group_id),
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
                        self.deliver_prompt(&agent_id, task, "loomux", KICKOFF_BOOT_DELAY)?;
                    }
                } else {
                    let kickoff =
                        self.kickoff_prompt(&self.agent(&agent_id).unwrap(), &group, &branch_note);
                    self.deliver_prompt(&agent_id, &kickoff, "loomux", KICKOFF_BOOT_DELAY)?;
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
    pub fn deliver_prompt(
        &self,
        agent_id: &str,
        text: &str,
        from: &str,
        boot_delay: Duration,
    ) -> Result<(), String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        if a.status == AgentStatus::Dead {
            return Err(format!("agent {agent_id} is dead"));
        }
        let pty_id = a.pty_id.ok_or("agent has no terminal yet")?;
        let app = self.app.lock().unwrap().clone().ok_or("no app handle")?;
        self.audit(&a.group, from, "prompt", json!({ "to": agent_id, "text": text }));

        let paste = bracketed_paste(text);
        let lock = self.delivery.clone();
        std::thread::spawn(move || {
            let _guard = lock.lock().unwrap();
            std::thread::sleep(boot_delay);
            let ptys = app.state::<crate::pty::PtyManager>();
            if ptys.write_bytes(pty_id, &paste).is_err() {
                return; // pane died between audit and delivery
            }
            std::thread::sleep(PASTE_SUBMIT_DELAY);
            let _ = ptys.write_bytes(pty_id, b"\r");
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
        self.deliver_prompt(&orch, text, from, Duration::ZERO)
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
        }
        let _ = fs::remove_file(
            self.group_dir(&snapshot.group).join("configs").join(format!("{agent_id}.json")),
        );
        self.audit(&snapshot.group, "loomux", "agent-exit",
            json!({ "agent": agent_id, "exit_code": exit_code }));
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
    if !Path::new(&repo).is_dir() {
        return Err(format!("repository path does not exist: {repo}"));
    }
    let group = reg.create_group(&repo, Guardrails {
        max_agents,
        agent_cli,
        worker_model,
        reviewer_model,
        orchestrator_model,
        auto_ops,
    })?;

    // Register the orchestrator without the spawn round-trip: the frontend
    // is the caller here and opens the pane from the returned spec.
    let model = group.guardrails.orchestrator_model.clone();
    let seq_reg = reg.inner().clone();
    let token = new_token();
    let agent_id = format!("orch-{}", seq_reg.seq.fetch_add(1, Ordering::SeqCst) + 1);
    let cfg = seq_reg.write_mcp_config(&group.id, &agent_id, &token, &group.guardrails.agent_cli)?;
    let session_id = (group.guardrails.agent_cli == "claude").then(new_session_uuid);
    let command = seq_reg.build_agent_command(
        &group.guardrails.agent_cli,
        &model,
        group.guardrails.auto_ops,
        &cfg,
        &seq_reg.group_dir(&group.id),
        session_id.as_deref(),
        false,
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
    seq_reg.agents.lock().unwrap().insert(agent_id.clone(), entry);
    seq_reg.by_token.lock().unwrap().insert(token, agent_id.clone());
    seq_reg.audit(&group.id, "loomux", "agent-spawn",
        json!({ "agent": agent_id, "role": "orchestrator", "model": model }));

    let request = SpawnRequest {
        group_id: group.id.clone(),
        agent_id: agent_id.clone(),
        role: Role::Orchestrator,
        name: "orchestrator".into(),
        cwd: group.repo.clone(),
        command,
    };

    // Background: wait for the orchestrator pane to bind, type its kickoff,
    // then bring up the initial idle workers one by one.
    let (tx, rx) = mpsc::channel::<u32>();
    seq_reg.pending_binds.lock().unwrap().insert(agent_id.clone(), tx);
    let reg2 = seq_reg.clone();
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
        let kickoff = reg2.kickoff_prompt(&reg2.agent(&agent_id).unwrap(), &group2, "");
        let _ = reg2.deliver_prompt(&agent_id, &kickoff, "loomux", KICKOFF_BOOT_DELAY);
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

#[tauri::command]
pub fn bind_agent(reg: tauri::State<Arc<OrchRegistry>>, agent_id: String, pty_id: u32) -> Result<(), String> {
    reg.bind(&agent_id, pty_id)
}

// ---------- task board (human side) ----------
// The pane overlay edits the same tasks.json the orchestrator manages via
// MCP. Human edits are audited as actor "human" and (except reorders, which
// are too chatty) surface in the orchestrator pane as a typed notice.

#[tauri::command]
pub fn orch_tasks(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Vec<Task> {
    reg.tasks(&group_id)
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
