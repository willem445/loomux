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

use serde::Serialize;
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

#[derive(Clone, Debug)]
pub struct Guardrails {
    pub max_agents: u32,
    pub worker_model: String,
    pub reviewer_model: String,
    pub orchestrator_model: String,
    pub full_auto: bool,
}

impl Guardrails {
    #[doc(hidden)] // pub for integration tests (unit tests can't load the UI stack; see tests/smoke.rs)
    pub fn clamped(mut self) -> Self {
        self.max_agents = self.max_agents.clamp(1, MAX_AGENTS_CEILING);
        self.worker_model = sanitize_model(&self.worker_model, "sonnet");
        self.reviewer_model = sanitize_model(&self.reviewer_model, "sonnet");
        self.orchestrator_model = sanitize_model(&self.orchestrator_model, "opus");
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

    // ---------- groups & agents ----------

    /// Create (or reattach to) the group for `repo`. State and audit history
    /// persist under the repo-derived group id; guardrails are refreshed from
    /// the new launch.
    pub fn create_group(&self, repo: &str, guardrails: Guardrails) -> Result<GroupInfo, String> {
        let guardrails = guardrails.clamped();
        let id = group_id_for_repo(repo);
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
                    "worker_model": info.guardrails.worker_model,
                    "reviewer_model": info.guardrails.reviewer_model,
                    "orchestrator_model": info.guardrails.orchestrator_model,
                    "full_auto": info.guardrails.full_auto,
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

    /// Write the per-agent MCP config the `claude` CLI connects with.
    fn write_mcp_config(&self, group: &str, agent_id: &str, token: &str) -> Result<PathBuf, String> {
        let port = self.port();
        if port == 0 {
            return Err("loomux MCP server is not running".into());
        }
        let cfg = json!({ "mcpServers": { "loomux": {
            "type": "http",
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "headers": { "X-Loomux-Agent": token },
        }}});
        let dir = self.group_dir(group).join("configs");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{agent_id}.json"));
        fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())?;
        Ok(path)
    }

    #[doc(hidden)] // pub for integration tests
    pub fn build_claude_command(&self, model: &str, full_auto: bool, cfg: &Path) -> String {
        let perm = if full_auto {
            "--dangerously-skip-permissions"
        } else {
            "--permission-mode acceptEdits"
        };
        format!(
            "claude --mcp-config \"{}\" --strict-mcp-config --model {model} {perm}",
            cfg.display()
        )
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
        let branch_name = branch
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| format!("agent/{agent_id}"));
        let (cwd, branch_note) = if use_worktree && role != Role::Orchestrator {
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

        let cfg = self.write_mcp_config(group_id, &agent_id, &token)?;
        let command = self.build_claude_command(model, group.guardrails.full_auto, &cfg);

        let entry = AgentEntry {
            id: agent_id.clone(),
            group: group_id.to_string(),
            name: display.clone(),
            role,
            token: token.clone(),
            status: AgentStatus::Starting,
            pty_id: None,
            task: task.to_string(),
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
                let kickoff = self.kickoff_prompt(&self.agent(&agent_id).unwrap(), &group, &branch_note);
                self.deliver_prompt(&agent_id, &kickoff, "loomux", KICKOFF_BOOT_DELAY)?;
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
    worker_model: String,
    reviewer_model: String,
    orchestrator_model: String,
    full_auto: bool,
) -> Result<SpawnRequest, String> {
    if !Path::new(&repo).is_dir() {
        return Err(format!("repository path does not exist: {repo}"));
    }
    let group = reg.create_group(&repo, Guardrails {
        max_agents,
        worker_model,
        reviewer_model,
        orchestrator_model,
        full_auto,
    })?;

    // Register the orchestrator without the spawn round-trip: the frontend
    // is the caller here and opens the pane from the returned spec.
    let model = group.guardrails.orchestrator_model.clone();
    let seq_reg = reg.inner().clone();
    let token = new_token();
    let agent_id = format!("orch-{}", seq_reg.seq.fetch_add(1, Ordering::SeqCst) + 1);
    let cfg = seq_reg.write_mcp_config(&group.id, &agent_id, &token)?;
    let command = seq_reg.build_claude_command(&model, group.guardrails.full_auto, &cfg);
    let entry = AgentEntry {
        id: agent_id.clone(),
        group: group.id.clone(),
        name: "orchestrator".into(),
        role: Role::Orchestrator,
        token: token.clone(),
        status: AgentStatus::Starting,
        pty_id: None,
        task: String::new(),
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
