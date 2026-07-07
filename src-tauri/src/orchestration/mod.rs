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
pub mod profiles;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

use crate::obs::LockExt;

const ORCHESTRATOR_TPL: &str = include_str!("templates/orchestrator.md");
const WORKER_TPL: &str = include_str!("templates/worker.md");
const REVIEWER_TPL: &str = include_str!("templates/reviewer.md");
const PLANNER_TPL: &str = include_str!("templates/planner.md");

/// Read-only containment note handed to a planner at spawn time as its kickoff
/// "branch note". The worktree denial (spawn cwd logic) and the CLI-level
/// write/commit denials (`build_agent_command`, `read_only`) enforce most of
/// this structurally; the note communicates the whole contract to the agent.
/// Exposed (doc-hidden) so tests can pin the exact text.
#[doc(hidden)]
pub const PLANNER_READONLY_NOTE: &str = "You explore the codebase read-only to produce an implementation plan. You never create branches, worktrees, commits, or PRs — your deliverable is a plan written as a GitHub issue comment.";

/// Hard ceiling on `max_agents` regardless of what the launcher asks for.
const MAX_AGENTS_CEILING: u32 = 12;

/// One-line notice delivered to the orchestrator when the live-agent cap
/// changes mid-session, so it re-plans against the new ceiling (its kickoff
/// prompt still carries the old, already-rendered {{MAX_AGENTS}}).
pub fn max_agents_notice(from: u32, to: u32) -> String {
    format!("[loomux] max live agents changed {from}→{to} — re-plan accordingly")
}

/// Quiet window a group's cap must fall silent for before its coalesced
/// cap-change notice is delivered. Rapid stepper clicks (#79) each persist,
/// enforce, and audit immediately, but the token-costing orchestrator notice
/// waits out this window and then spans the whole burst (first change's `from`
/// → last change's `to`), so a flurry of clicks is one prompt, not many.
const MAX_NOTICE_DEBOUNCE: Duration = Duration::from_secs(3);
/// How often the flusher loop checks for a debounced cap-change notice whose
/// window has elapsed. Well under `MAX_NOTICE_DEBOUNCE` so the delivered notice
/// lags the last click by at most a tick beyond the debounce.
const MAX_NOTICE_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// A cap-change notice awaiting its debounce window (#79). `from` is the cap
/// before the burst began — preserved across coalesced changes so the notice
/// reads end-to-end; `to` is the latest cap; `due_ms` is the Unix-ms at which,
/// absent any further change, the notice fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingMaxNotice {
    from: u32,
    to: u32,
    due_ms: u64,
}

/// Fold one cap change into the per-group debounce map (#79). A change that
/// lands while a notice is still pending keeps the original `from` (so the
/// coalesced notice spans the whole burst) and only advances `to` and pushes
/// the deadline out; the first change of a burst seeds a fresh entry. Pure, so
/// the coalescing is unit-testable without a clock or a live registry.
fn record_max_notice(
    pending: &mut HashMap<String, PendingMaxNotice>,
    group: &str,
    from: u32,
    to: u32,
    now: u64,
    debounce: Duration,
) {
    let due_ms = now.saturating_add(debounce.as_millis() as u64);
    pending
        .entry(group.to_string())
        .and_modify(|p| {
            p.to = to;
            p.due_ms = due_ms;
        })
        .or_insert(PendingMaxNotice { from, to, due_ms });
}

/// Drain the notices whose debounce window has elapsed (`due_ms <= now`),
/// returning `(group, from, to)` for each that is a real net change. A burst
/// that nets back to where it started (e.g. 4→3→4) is dropped without a notice
/// — no orchestrator tokens spent announcing a no-op. Pure, so the flush
/// decision is unit-testable without sleeping out the debounce.
fn take_due_max_notices(
    pending: &mut HashMap<String, PendingMaxNotice>,
    now: u64,
) -> Vec<(String, u32, u32)> {
    let due: Vec<String> = pending
        .iter()
        .filter(|(_, p)| p.due_ms <= now)
        .map(|(g, _)| g.clone())
        .collect();
    let mut out = Vec::new();
    for g in due {
        if let Some(p) = pending.remove(&g) {
            if p.from != p.to {
                out.push((g, p.from, p.to));
            }
        }
    }
    out
}

/// Upper bound on the idle-worker auto-kill timeout (24h); 0 disables it.
const MAX_IDLE_KILL_MINUTES: u32 = 1440;
/// Upper bound on the spawn-rate guardrail; 0 = unlimited.
const MAX_SPAWNS_PER_HOUR: u32 = 240;
/// Sliding window the spawn-rate guardrail counts spawns over.
const SPAWN_RATE_WINDOW_MS: u64 = 60 * 60 * 1000;
/// How often the idle reaper wakes to look for workers to auto-kill.
const IDLE_REAP_INTERVAL: Duration = Duration::from_secs(30);
/// How often the watchdog wakes to look for stalled working agents.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);
/// Upper bound on the watchdog stall timeout (24h); 0 disables it.
const MAX_WATCHDOG_STALL_MINUTES: u32 = 1440;
/// How often the attention scan recomputes which panes need the human
/// (idle-with-prompt detection; report/gate signals are event-driven and
/// picked up on the next tick).
const ATTENTION_INTERVAL: Duration = Duration::from_secs(3);
/// A pane's terminal output must be stable (unchanged) at least this long
/// before an idle-with-prompt is asserted — the CLI has stopped painting and
/// is genuinely parked on a prompt, not mid-render. Measured across ticks, so
/// it also debounces (needs a couple of consecutive quiet scans).
const ATTENTION_QUIET_MS: u64 = 4000;
/// If the human typed into a pane within this window it does not "need
/// attention" — they are already at the keyboard on it.
const ATTENTION_RECENT_INPUT_MS: u64 = 6000;
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

// Human-typing backstop (#43, option A): even with the loomux compose strip,
// a human can still type directly into the terminal. Before the paste AND
// before the first Enter, hold delivery while the pane has seen recent
// keystrokes so a report can't land in — or submit — the human's half-typed
// line. Capped so a long compose session can't starve reports forever.
/// Treat the human as "still typing" if they hit a key within this window.
const USER_QUIET_HOLD: Duration = Duration::from_secs(4);
/// Deliver anyway once a single hold has waited this long (never starve).
const USER_QUIET_MAX_HOLD: Duration = Duration::from_secs(90);
/// Poll interval while holding for the human to go quiet.
const USER_QUIET_POLL: Duration = Duration::from_millis(250);

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

/// Cap on a single steered image attachment (#72), in decoded bytes. Sized to
/// comfortably hold a full-screen PNG screenshot while bounding the per-group
/// `attachments/` scratch dir. The steering strip enforces the same limit and
/// toasts on overflow; this is the backstop against a hostile/oversize IPC.
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Cap on the base64 payload the save-attachment command will decode. Rejecting
/// oversize *before* decode keeps a giant string from ballooning memory — same
/// discipline as the OSC 52 clipboard path. base64 is 4 bytes per 3 input, plus
/// slack for padding/whitespace.
pub const MAX_ATTACHMENT_B64_LEN: usize = MAX_ATTACHMENT_BYTES / 3 * 4 + 16;

/// Monotonic tiebreaker so two images pasted inside the same millisecond get
/// distinct filenames without pulling in a randomness/uuid crate (the Windows
/// `getrandom` backends are banned here — see the build notes).
static ATTACH_SEQ: AtomicU32 = AtomicU32::new(0);

// Copilot session tracking: unlike Claude, copilot can't be handed a session
// id up front — it mints one and writes `~/.copilot/session-state/<id>/` a
// few seconds into boot. After spawning a copilot pane we poll for the new
// session directory and bind its id to the pane's roster record.
/// How often to poll `session-state` for the pane's new session.
const COPILOT_SESSION_POLL: Duration = Duration::from_millis(1000);
/// Give up watching after this long (copilot never initialized, or crashed).
const COPILOT_SESSION_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Orchestrator,
    Worker,
    Reviewer,
    /// Read-only explorer: investigates the codebase and writes a structured
    /// implementation plan (as a GitHub issue comment), then reports and
    /// exits. A planner NEVER writes code, branches, or PRs. It counts as a
    /// delegate against the live-agent cap, like a worker/reviewer.
    Planner,
}

impl Role {
    fn prefix(self) -> &'static str {
        match self {
            Role::Orchestrator => "orch",
            Role::Worker => "w",
            Role::Reviewer => "rev",
            Role::Planner => "plan",
        }
    }
    fn template(self) -> &'static str {
        match self {
            Role::Orchestrator => ORCHESTRATOR_TPL,
            Role::Worker => WORKER_TPL,
            Role::Reviewer => REVIEWER_TPL,
            Role::Planner => PLANNER_TPL,
        }
    }
    fn instructions_file(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator.md",
            Role::Worker => "worker.md",
            Role::Reviewer => "reviewer.md",
            Role::Planner => "planner.md",
        }
    }
    /// Lowercase wire/label name (matches the `Serialize` rename).
    fn as_str(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator",
            Role::Worker => "worker",
            Role::Reviewer => "reviewer",
            Role::Planner => "planner",
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

/// Who last set an agent's display name — the precedence ladder for the pane
/// title / roster name (#95r). A rename applies only when its source ranks at
/// least as high as whoever set the current name: `Human` > `Orchestrator` >
/// `Default`. So the human's manual rename is never clobbered by the
/// orchestrator's `rename_agent` or the id-derived default, while the
/// orchestrator can still relabel an id-default (or its own earlier name).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NameSource {
    /// Minted from the agent id at spawn ("worker 2" for `w-2`).
    Default,
    /// Chosen by the orchestrator (a `spawn_agent` name, or `rename_agent`).
    Orchestrator,
    /// Typed by the human into the pane title (F2 / double-click).
    Human,
}

impl Default for NameSource {
    /// Legacy roster rows (written before the tier was persisted, #95r) carry a
    /// name but no source. Treat them as orchestrator-chosen: their non-empty
    /// name was picked deliberately, so a later `rename_agent` may still relabel
    /// it, and it never sits *below* an id-default. (Pre-95r human renames were
    /// frontend-only and never reached the roster, so none are being demoted.)
    fn default() -> Self {
        NameSource::Orchestrator
    }
}

impl NameSource {
    fn rank(self) -> u8 {
        match self {
            NameSource::Default => 0,
            NameSource::Orchestrator => 1,
            NameSource::Human => 2,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            NameSource::Default => "default",
            NameSource::Orchestrator => "orchestrator",
            NameSource::Human => "human",
        }
    }
}

/// Which agent CLI a group runs. Each needs an adapter in
/// `build_agent_command` + `write_mcp_config`; anything unknown falls back
/// to Claude (explicitly, in `clamped`, never silently at spawn time).
pub const SUPPORTED_CLIS: [&str; 2] = ["claude", "copilot"];

#[derive(Clone, Debug, Default)]
pub struct Guardrails {
    pub max_agents: u32,
    /// Group-default agent CLI ("claude" | "copilot", see `SUPPORTED_CLIS`).
    /// A per-role CLI below overrides it; an empty per-role CLI inherits this.
    /// Kept as the group default so old group.json (pre per-role CLI) and the
    /// launcher's single-CLI path both keep working (issue #4).
    pub agent_cli: String,
    /// Per-role agent CLI overrides (issue #4, mixed agent types). Empty =
    /// inherit `agent_cli`. Resolved through `cli_for`; validated at spawn.
    pub orchestrator_cli: String,
    pub worker_cli: String,
    pub reviewer_cli: String,
    pub planner_cli: String,
    pub worker_model: String,
    pub reviewer_model: String,
    pub orchestrator_model: String,
    /// Model for the planner role (issue #47). Sanitized like the others.
    pub planner_model: String,
    /// Additionally pre-approve `git`/`gh` shell commands for the group's
    /// agents. Never maps to `--dangerously-skip-permissions`: bypass mode
    /// shows a confirm dialog whose default answer is "exit", which the
    /// kickoff typing would accept, killing the pane.
    pub auto_ops: bool,
    /// Trust this repo's own agent config for local code execution (issue
    /// #51). Default **off**: a repo's `.mcp.json` server entry is an
    /// arbitrary `command` loomux would launch, and under `auto_ops` no human
    /// approves each tool call — so repo MCP servers are only merged into an
    /// agent's config (Claude), and a Copilot persona only engaged via its
    /// native `--agent` (which pulls the file's `mcp-servers`), when the human
    /// has explicitly trusted this repo. Repo role *instructions* always
    /// apply (text, not code); only the MCP/code-exec surface is gated.
    pub trust_repo_mcp: bool,
    /// Manual per-role profile assignment (issue #51): the profile NAME the
    /// human explicitly picked for each role in the launcher, overriding the
    /// filename/frontmatter auto-mapping. Semantics of each string:
    ///   - empty  → auto (append-mode file mapped to the role, if any),
    ///   - `none` → explicit built-in only (no profile, even if one matches),
    ///   - `<name>` → that named profile (any mode, incl. replace).
    /// Additive in group.json; absent = auto (back-compat).
    pub orchestrator_profile: String,
    pub worker_profile: String,
    pub reviewer_profile: String,
    pub planner_profile: String,
    /// Cost guardrail: auto-kill a worker/reviewer that has sat without a
    /// task for this many minutes (the orchestrator is notified so it can
    /// respawn on demand). 0 disables it. See `idle_should_kill`.
    pub idle_kill_minutes: u32,
    /// Cost guardrail: cap on worker/reviewer spawns per rolling hour, a
    /// runaway-orchestrator backstop. 0 = unlimited. See `spawn_rate_exceeded`.
    pub max_spawns_per_hour: u32,
    /// Recovery guardrail: nudge the orchestrator once when a working agent
    /// produces no terminal output and sends no report for this many minutes
    /// (likely stalled or waiting on input). 0 disables it. See
    /// `watchdog_should_notify`.
    pub watchdog_stall_minutes: u32,
}

impl Guardrails {
    #[doc(hidden)] // pub for integration tests (unit tests can't load the UI stack; see tests/smoke.rs)
    pub fn clamped(mut self) -> Self {
        self.max_agents = self.max_agents.clamp(1, MAX_AGENTS_CEILING);
        // The group default CLI is coerced to a supported value (legacy /
        // single-CLI path). Per-role CLIs are validated at spawn instead of
        // coerced here, so a genuinely unknown per-role type is rejected
        // rather than silently downgraded (issue #4).
        if !SUPPORTED_CLIS.contains(&self.agent_cli.as_str()) {
            self.agent_cli = "claude".into();
        }
        // Model fallbacks depend on the role's *effective* CLI: Copilot picks
        // its own best model with "auto"; Claude needs a tier.
        self.orchestrator_model =
            sanitize_model(&self.orchestrator_model, default_model(self.cli_for(Role::Orchestrator), Role::Orchestrator));
        self.worker_model =
            sanitize_model(&self.worker_model, default_model(self.cli_for(Role::Worker), Role::Worker));
        self.reviewer_model =
            sanitize_model(&self.reviewer_model, default_model(self.cli_for(Role::Reviewer), Role::Reviewer));
        self.planner_model =
            sanitize_model(&self.planner_model, default_model(self.cli_for(Role::Planner), Role::Planner));
        self.idle_kill_minutes = self.idle_kill_minutes.min(MAX_IDLE_KILL_MINUTES);
        self.max_spawns_per_hour = self.max_spawns_per_hour.min(MAX_SPAWNS_PER_HOUR);
        self.watchdog_stall_minutes = self.watchdog_stall_minutes.min(MAX_WATCHDOG_STALL_MINUTES);
        self
    }

    /// The agent CLI a role runs: its per-role override when set, else the
    /// group default `agent_cli`. May return an unsupported value (a per-role
    /// CLI is not coerced in `clamped`); `spawn_agent` validates it.
    pub fn cli_for(&self, role: Role) -> &str {
        let per_role = match role {
            Role::Orchestrator => &self.orchestrator_cli,
            Role::Worker => &self.worker_cli,
            Role::Reviewer => &self.reviewer_cli,
            Role::Planner => &self.planner_cli,
        };
        if per_role.trim().is_empty() {
            &self.agent_cli
        } else {
            per_role
        }
    }

    /// The pinned model for a role.
    pub fn model_for(&self, role: Role) -> &str {
        match role {
            Role::Orchestrator => &self.orchestrator_model,
            Role::Worker => &self.worker_model,
            Role::Reviewer => &self.reviewer_model,
            Role::Planner => &self.planner_model,
        }
    }

    /// The human's manual profile choice for a role (issue #51): a profile
    /// name, the literal `none` (force built-in), or empty (auto). See the
    /// field docs and `OrchRegistry::resolve_profile`.
    pub fn profile_for(&self, role: Role) -> &str {
        match role {
            Role::Orchestrator => &self.orchestrator_profile,
            Role::Worker => &self.worker_profile,
            Role::Reviewer => &self.reviewer_profile,
            Role::Planner => &self.planner_profile,
        }
    }
}

/// Default model for a role on a given CLI. Copilot picks its own best model
/// ("auto"); on Claude the reasoning-heavy roles (orchestrator, planner) get
/// the strong tier and the executing roles (worker, reviewer) the mid tier.
fn default_model(cli: &str, role: Role) -> &'static str {
    if cli == "copilot" {
        return "auto";
    }
    match role {
        Role::Orchestrator | Role::Planner => "opus",
        Role::Worker | Role::Reviewer => "sonnet",
    }
}

/// Whether an idle agent has sat long enough to auto-kill. Pure so the
/// threshold logic is testable without threads or wall-clock; the reaper
/// loop lives in `start_idle_reaper`. `idle_since_ms` is `None` for an agent
/// that currently has work (never idle-killed); a `threshold_min` of 0
/// disables the guardrail entirely.
pub fn idle_should_kill(idle_since_ms: Option<u64>, now_ms: u64, threshold_min: u32) -> bool {
    match (threshold_min, idle_since_ms) {
        (0, _) | (_, None) => false,
        (m, Some(t)) => now_ms.saturating_sub(t) >= (m as u64) * 60_000,
    }
}

/// Whether the spawn-rate guardrail should reject the next spawn: true when
/// at least `limit` spawns already fall inside the trailing `window_ms`.
/// Pure so the sliding-window arithmetic is testable; `limit` 0 = unlimited.
pub fn spawn_rate_exceeded(times: &[u64], now: u64, limit: u32, window_ms: u64) -> bool {
    if limit == 0 {
        return false;
    }
    let recent = times.iter().filter(|&&t| now.saturating_sub(t) < window_ms).count();
    recent as u32 >= limit
}

/// Whether a working agent has been silent (no terminal output, no report)
/// long enough to warrant one watchdog nudge to the orchestrator. Pure so the
/// stall arithmetic and the anti-nag rule are testable without threads or a
/// real pty; the scan loop lives in `start_watchdog`. `threshold_min` 0
/// disables the guardrail; `already_notified` enforces at-most-one-notice per
/// stall (the caller clears it when the agent produces output/reports again).
pub fn watchdog_should_notify(
    silent_since_ms: u64,
    now_ms: u64,
    threshold_min: u32,
    already_notified: bool,
) -> bool {
    if threshold_min == 0 || already_notified {
        return false;
    }
    now_ms.saturating_sub(silent_since_ms) >= (threshold_min as u64) * 60_000
}

/// Attention routing (#6): does a pane's ANSI-stripped output tail look like a
/// CLI parked on a prompt only the human can answer — a permission dialog, a
/// yes/no confirmation, or a numbered/selection menu? This is the "last output"
/// half of idle-with-prompt detection; the caller pairs it with an
/// output-quiet check (this alone can't tell a live prompt from the same words
/// scrolled past). So it errs toward recognizable interactive-prompt structure
/// rather than any mention of a question. Case-insensitive.
///
/// Two tiers of signal, by how prose-safe each is (#40 review):
/// - *Structured* signals (numbered y/n menu, explicit y/n tokens, stock
///   permission phrasings) don't occur in ordinary prose, so they're honored
///   across the last ~12 lines.
/// - *Prose-like* signals — a bare selection pointer and the plain-English menu
///   footer ("use arrow keys", "enter to select") — DO appear in finished-turn
///   agent output (agents describe keyboard UIs, paste shell prompts, echo
///   `a › b` breadcrumbs). A *live* menu paints these as the last thing on
///   screen, with its pointer *leading* an option line (after any box frame);
///   prose does neither. So the pointer must lead a de-framed line, and the
///   footer is only read from the last few non-empty lines — once the CLI
///   redraws its idle input box below the prose, the phrase falls out of range.
pub fn prompt_wait_detected(tail: &str) -> bool {
    let lines: Vec<String> = tail
        .lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }
    let recent = &lines[lines.len().saturating_sub(12)..];
    let joined = recent.join("\n");

    // Strip a line's leading box border / bullet / indent so a menu pointer
    // inside a bordered dialog (`│ ❯ Yes`) is seen to *lead* its content.
    fn deframe(l: &str) -> &str {
        l.trim_start_matches(|c: char| {
            c == '│' || c == '┃' || c == '|' || c == '*' || c == '●' || c == '•' || c == '◆'
                || c.is_whitespace()
        })
    }

    // The last few non-empty lines — "the last thing the CLI painted". Both
    // prose-like signals (pointer, footer) are read only from here (#40 review):
    // a live menu paints its pointer/footer last, whereas finished-turn prose
    // that happens to lead a line with `❯`/`›`/`→` (a `❯ npm run dev` shell
    // example, a fenced repro block) is followed by the CLI's redrawn idle input
    // box, which pushes it out of this window.
    let last_painted = &recent[recent.len().saturating_sub(3)..];

    // Selection pointer marking the highlighted choice. A `❯`/`›`/`→` that
    // *leads* a line's content (after any box frame) is menu-shaped; the same
    // glyph mid-line is pervasive in ordinary output — pasted shell prompts
    // (`demo ❯ npm run dev`), UI breadcrumbs (`Home › Prefs`), diff/log arrows.
    // Requiring it to lead rules those out; requiring it in the last painted
    // lines also rules out a *leading* glyph in finished prose above the idle box.
    let has_pointer_option = last_painted.iter().any(|l| {
        let d = deframe(l);
        d.starts_with('❯') || d.starts_with('›') || d.starts_with('→')
    });
    // A numbered yes/no menu even without the pointer glyph.
    let has_numbered_menu = joined.contains("1. yes") || joined.contains("❯ 1.");
    // Explicit yes/no confirmation tokens.
    let has_yes_no = joined.contains("(y/n)")
        || joined.contains("[y/n]")
        || joined.contains("y/n)")
        || joined.contains("[y/n]?")
        || joined.contains("yes/no");
    // Stock permission / trust / continue phrasings from Claude Code & Copilot.
    let has_permission_phrase = joined.contains("do you want to proceed")
        || joined.contains("do you want to make this edit")
        || joined.contains("do you want to create")
        || joined.contains("do you want to run")
        || joined.contains("do you trust")
        || joined.contains("trust the files")
        || joined.contains("allow this")
        || joined.contains("allow command")
        || joined.contains("grant access")
        || joined.contains("press enter to continue")
        || joined.contains("waiting for your");
    // Interactive selection-menu footer (AskUserQuestion / Copilot / inquirer).
    // Claude Code's AskUserQuestion highlights the active option with reverse
    // video (an ANSI attribute stripped before we see it), so no glyph survives
    // and this footer is the only durable signal (#40). Like the pointer it's
    // read only from the last painted lines. NOTE: matched on single lines, so a
    // footer wrapped across rows in a very narrow pane, or a localized / reworded
    // footer, won't match — a known gap (see design doc).
    let footer = last_painted.join("\n");
    let has_menu_footer = footer.contains("enter to select")
        || footer.contains("enter to confirm")
        || footer.contains("use arrow")
        || footer.contains("arrow keys")
        || footer.contains("↑↓")
        || footer.contains("↑/↓");
    has_pointer_option || has_numbered_menu || has_yes_no || has_permission_phrase || has_menu_footer
}

/// Distinct agent working directories to remove when a group is torn down
/// with worktree cleanup: dedup (case/separator-insensitively), and never the
/// repo root itself — the orchestrator and any repo-mode workers run there, so
/// removing it would delete the user's own checkout. Pure so the path
/// filtering is testable without a real git tree; the actual removal is
/// `git::git_worktree_remove`, which git refuses on a non-worktree anyway.
pub fn worktree_cleanup_targets(repo: &str, cwds: &[String]) -> Vec<String> {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let repo_n = norm(repo);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for c in cwds {
        if c.trim().is_empty() {
            continue;
        }
        let cn = norm(c);
        if cn == repo_n {
            continue; // repo root — the orchestrator's cwd, never a worktree
        }
        if seen.insert(cn) {
            out.push(c.clone());
        }
    }
    out
}

/// Best-effort extraction of a session's dollar cost from a pane's
/// ANSI-stripped terminal tail. Claude Code renders running cost in its
/// in-pane statusline (bottom of the screen), so scan lines bottom-up and
/// return the dollar amount from the lowest line that carries one — that is
/// the freshest statusline render. Thousands separators are tolerated.
/// Returns `None` when no `$<amount>` token is present.
pub fn parse_session_cost(text: &str) -> Option<f64> {
    for line in text.lines().rev() {
        if let Some(cost) = line
            .match_indices('$')
            .find_map(|(i, _)| parse_dollar_amount(&line[i + 1..]))
        {
            return Some(cost);
        }
    }
    None
}

/// Parse a leading `1,234.56`-style number (optionally after the `$` already
/// consumed by the caller), returning `None` if the text does not start with
/// a digit. Commas are dropped; a single decimal point is honored.
fn parse_dollar_amount(after_dollar: &str) -> Option<f64> {
    let mut digits = String::new();
    let mut seen_dot = false;
    for c in after_dollar.chars() {
        match c {
            '0'..='9' => digits.push(c),
            ',' if !seen_dot => {} // thousands separator
            '.' if !seen_dot => {
                seen_dot = true;
                digits.push('.');
            }
            _ => break,
        }
    }
    // Reject a bare "." or empty (a lone `$` or `$.`); require a real digit.
    if digits.is_empty() || digits == "." {
        return None;
    }
    digits.parse::<f64>().ok()
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
    /// Who set `name` — the precedence tier for renames (#95r). See
    /// [`NameSource`] and [`OrchRegistry::rename_agent`].
    pub name_source: NameSource,
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
    /// Unix-ms this worker/reviewer became idle (spawned without a task, or
    /// reported done/blocked); `None` while it has work or for the
    /// orchestrator. The idle reaper (`idle_kill_minutes`) reads this.
    pub idle_since_ms: Option<u64>,
    /// Unix-ms this agent was registered (spawn time). Drives the per-agent
    /// and group uptime shown in the lifecycle summary; unaffected by idle.
    pub started_ms: u64,
    /// Watchdog: Unix-ms of this agent's last observed activity — terminal
    /// output growth or a report/message. Silence is measured from here.
    /// Seeded at spawn and whenever work is (re)assigned. See
    /// `watchdog_should_notify`.
    pub last_progress_ms: u64,
    /// Watchdog: last observed value of the pane's monotonic pty output
    /// counter, so a tick can tell whether the CLI has emitted anything since
    /// the previous one even when the output ring is saturated.
    pub last_output_total: u64,
    /// Watchdog anti-nag latch: set once a stall notice has been delivered for
    /// the current stall, cleared when the agent produces output/reports again.
    pub watchdog_notified: bool,
}

/// One pane that needs the human, pushed to the frontend as an `orch-attention`
/// event (the full current set each scan; the frontend badges panes by
/// `pty_id`). `reason`, most- to least-urgent:
/// - `blocked` — a worker reported it is blocked
/// - `waiting` — the pane is parked on a prompt (idle-with-prompt)
/// - `report`  — a worker reported done (awaiting the human's review/merge)
/// - `gate`    — this agent's task sits at a human merge gate on the board
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AttentionItem {
    /// Empty for a plain (non-orchestration) pane, which is keyed only by
    /// `pty_id` — the human's hand-opened shells have no agent identity (#40).
    pub agent_id: String,
    pub group: String,
    pub name: String,
    /// `None` for a plain pane (no orchestration role).
    pub role: Option<Role>,
    pub pty_id: Option<u32>,
    pub reason: &'static str,
    /// Short human phrase for the badge tooltip and the toast body.
    pub detail: String,
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

/// Statuses where the human's merge-gate actions (approve / request changes)
/// apply: the PR is open and awaiting the human's decision.
pub const MERGE_GATE_STATUSES: [&str; 2] = ["pr", "human-testing"];

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
    /// Precedence tier of `name` (#95r). Persisted so a session rejoin restores
    /// the human's rename AND its "human beats orchestrator" tier, not just the
    /// text. Additive: legacy rows without it deserialize to `Default::default`.
    #[serde(default)]
    pub name_source: NameSource,
    pub session: Option<String>,
    pub cwd: String,
    pub status: String,
    pub updated_ms: u64,
}

/// Durable per-agent usage snapshot (`usage.json` per group). Keyed by the CLI
/// session id when known (so a resumed session updates one row instead of
/// double-counting), else `agent:<id>`. Snapshots survive `kill_agent`/exit —
/// captured in `mark_dead` — so a group's lifetime cost keeps counting
/// recycled panes (issue #42).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageSnapshot {
    /// Stable identity: the CLI session id, or `agent:<id>` when there is none.
    pub key: String,
    pub agent_id: String,
    pub name: String,
    pub role: String,
    /// Where the figures came from: `transcript` (token-derived, exact tokens),
    /// `statusline` (last-resort parse of the CLI's own dollar figure), or
    /// `none` (nothing available yet).
    pub source: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Dollar cost, or `None` when only tokens are known (unknown model, or a
    /// transcript-less agent whose statusline shows nothing).
    pub cost_usd: Option<f64>,
    /// true = dollars estimated from the price table; false = reported by the
    /// CLI's statusline (which reads $0.00 on subscription/Max accounts).
    pub estimated: bool,
    pub model: Option<String>,
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

/// Repo-profile extras threaded into `build_agent_command` (issue #51). A
/// profile-less spawn passes `Default::default()` (all empty/none), which
/// reproduces the pre-#51 command exactly. Kept as one struct so adding a
/// profile knob doesn't grow the already-long positional signature.
#[derive(Default)]
pub struct ProfileInject<'a> {
    /// Extra pre-approved tool patterns from the profile's `allow:`.
    pub extra_allow: &'a [String],
    /// Profile instructions rendered to a file, injected as Claude's appended
    /// system prompt (`--append-system-prompt-file`). Text only — always
    /// applied when a profile is present (not gated by `trust_repo_mcp`).
    pub system_prompt_file: Option<&'a Path>,
    /// Copilot native custom-agent name (`--agent <name>`). Only set when the
    /// group trusts the repo, since `--agent` also pulls the file's
    /// `mcp-servers` (local code execution).
    pub copilot_agent: Option<&'a str>,
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
    /// Groups the human has paused: loomux stops delivering prompts/kickoffs
    /// to them so their agents idle out (see `deliver_prompt`). Mirrored to a
    /// `paused` marker file per group so it survives restarts.
    paused: Mutex<HashSet<String>>,
    /// Per-group spawn timestamps (Unix-ms) for the spawn-rate guardrail;
    /// pruned to the trailing hour on each check.
    spawn_times: Mutex<HashMap<String, Vec<u64>>>,
    /// Weak handle to our own `Arc`, set once at startup (`set_self_arc`), so
    /// `&self` methods can hand an owned registry to background threads (e.g.
    /// the copilot session watcher). `Weak` avoids a self-referential `Arc`
    /// cycle that would leak the registry.
    self_arc: Mutex<Weak<OrchRegistry>>,
    /// Attention routing (#6): latched worker reports awaiting the human's
    /// eyes — agent id → "done" | "blocked". Set by the report tool, cleared on
    /// ack (the human focused the pane) or reassignment.
    attn_reports: Mutex<HashMap<String, &'static str>>,
    /// Attention routing: per-agent output-quiet tracking, agent id → (last pty
    /// output total, Unix-ms that total last changed). Kept separate from the
    /// watchdog's counter so the two features never clobber each other's clocks.
    attn_quiet: Mutex<HashMap<String, (u64, u64)>>,
    /// Attention routing: agents whose live `waiting` badge the human has acked
    /// (focused the pane) while the prompt is still on screen. Unlike
    /// `blocked`/`report`, `waiting` is recomputed every scan, so without this it
    /// would re-light ~3s after focus. Cleared when the pane's output next
    /// changes (the menu was answered / the CLI repainted) so a genuinely new
    /// prompt flags again. See `attention_tick`.
    attn_waiting_ack: Mutex<HashSet<String>>,
    /// Attention routing: the agent → reason set last emitted, so a scan fires a
    /// desktop toast only once per attention onset (the event itself is
    /// re-emitted every tick and the frontend badges idempotently).
    attn_emitted: Mutex<HashMap<String, String>>,
    /// Groups with desktop notifications enabled (durable `notify` marker file).
    notify_groups: Mutex<HashSet<String>>,
    /// Debounced cap-change notices (#79): group → its pending, not-yet-
    /// delivered `PendingMaxNotice`. `set_max_agents` folds rapid stepper
    /// clicks in here (persist/enforce/audit stay per-click); the
    /// `start_max_notice_flusher` loop delivers one coalesced notice per burst
    /// once the group falls quiet.
    pending_max_notice: Mutex<HashMap<String, PendingMaxNotice>>,
    /// Test-only override of the Claude transcript root (`~/.claude/projects`).
    /// `None` in production. Set via `set_claude_projects_dir` so the usage
    /// reader can be pointed at a fixture tree without touching global env —
    /// safe under parallel test execution.
    claude_projects_dir: Mutex<Option<PathBuf>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Map a caller-supplied image extension to a vetted one, rejecting anything
/// outside the allowlist (#72). A pasted image's extension is attacker-influenced
/// (it rides in from the browser clipboard), so we never echo it into a filename
/// verbatim: only these known raster/image types are accepted, which both blocks
/// path-traversal / executable extensions and matches what the agent CLIs open.
/// Pure and `pub` so the mapping is unit-testable.
pub fn sanitize_attachment_ext(ext: &str) -> Option<&'static str> {
    match ext.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        "bmp" => Some("bmp"),
        _ => None,
    }
}

/// Should prompt delivery keep holding for the human to stop typing? (#43,
/// option A). Returns true to keep waiting, false to proceed. Pure so the
/// hold/deadline decision is unit-testable without a live PTY.
///
/// - `last_input_ms` is the pane's last-keystroke time (0 = none recorded).
/// - `held` is how long THIS hold has already waited; once it reaches
///   `max_hold` we deliver anyway so a long compose session can't starve the
///   report queue.
fn should_hold_for_user(
    last_input_ms: u64,
    now_ms: u64,
    held: Duration,
    quiet_window: Duration,
    max_hold: Duration,
) -> bool {
    if held >= max_hold {
        return false; // cap reached — deliver anyway
    }
    if last_input_ms == 0 {
        return false; // nobody has typed in this pane
    }
    let since = now_ms.saturating_sub(last_input_ms);
    since < quiet_window.as_millis() as u64
}

/// Poll-and-hold loop that drives `should_hold_for_user`: block while
/// `last_input_ms()` reports recent keystrokes, until quiet or the hold hits
/// `max_hold`. Returns `Some(held_ms)` when it actually waited (so the caller
/// can audit the held duration), `None` when it was already quiet on entry.
///
/// Generic over the keystroke source and timings so the wiring — that the
/// loop consults the decision every `poll` and honours the starvation cap —
/// is integration-testable without a live PTY (see the #40 twice-bitten
/// lesson: the pure decision alone isn't enough; the loop that calls it must
/// be exercised too).
#[doc(hidden)] // pub for integration tests
pub fn hold_until_quiet<F: Fn() -> u64>(
    last_input_ms: F,
    quiet_window: Duration,
    max_hold: Duration,
    poll: Duration,
) -> Option<u64> {
    let start = std::time::Instant::now();
    let mut held = false;
    while should_hold_for_user(last_input_ms(), now_ms(), start.elapsed(), quiet_window, max_hold) {
        held = true;
        std::thread::sleep(poll);
    }
    held.then(|| start.elapsed().as_millis() as u64)
}

/// Production wrapper: hold delivery to `pty_id` while its human is typing,
/// using the shipped window/cap/poll timings.
fn wait_for_user_quiet(ptys: &crate::pty::PtyManager, pty_id: u32) -> Option<u64> {
    hold_until_quiet(
        || ptys.last_user_input_ms(pty_id).unwrap_or(0),
        USER_QUIET_HOLD,
        USER_QUIET_MAX_HOLD,
        USER_QUIET_POLL,
    )
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

/// Normalize a caller-supplied pane name (#95r): trim, drop control characters
/// (so a pasted name can't smuggle newlines/escape codes into the pane title or
/// the roster JSON), and cap the length. Not a security boundary — the title is
/// rendered via `textContent`, never HTML — just hygiene. May return empty (an
/// all-control/whitespace name); callers decide what an empty result means.
fn sanitize_agent_name(name: &str) -> String {
    name.trim().chars().filter(|c| !c.is_control()).take(40).collect()
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

/// The **non-overridable loomux mechanics core** for a role (issue #51).
///
/// A repo profile in `mode: replace` swaps only the role's *personality/policy*
/// body — it must NOT be able to strip the functional contract that makes the
/// app work (the loomux MCP tools, the task board, `report()` discipline, the
/// spawn/review/plan flow, the branch→PR git discipline). loomux always injects
/// this core so those guarantees hold no matter what a replace-mode file omits.
/// In `append` mode (and profile-less spawns) the full built-in `<role>.md`
/// template already carries these mechanics, so the core is delivered only when
/// a replace-mode persona has dropped the built-in body.
///
/// This is the extracted, always-on subset of the built-in templates; a fuller
/// split of every template into `mechanics + body` files is follow-up work.
fn mechanics_core(role: Role) -> String {
    // Shared spine for every delegate; the orchestrator gets its own.
    let common = "\
These loomux mechanics are guaranteed by the app and are NOT optional, whatever your \
persona says:\n\
- You act through the loomux MCP tools. `report(status, summary)` (status: progress | \
done | blocked) is your channel to the orchestrator — report `progress` on start, \
`blocked` when stuck (say what you need), and `done` with the PR URL. \
`message_orchestrator(text)` is for questions; `list_agents()` / `get_state()` are \
read-only context. These tools never need approval; use them, don't ask the human to.\n\
- Git discipline: work only in your assigned workspace; create your branch off the \
default branch before changing anything; never commit to the default branch; open a PR \
with `gh` linking the issue. NEVER merge — the human gates merges.\n\
- One task per session. Follow-ups and review fixes for your own task are yours; a \
different task means asking for a fresh agent.";
    match role {
        Role::Orchestrator => "\
These loomux mechanics are guaranteed by the app and are NOT optional, whatever your \
persona says:\n\
- You drive the group through the loomux MCP tools: `spawn_agent` (worker | reviewer | \
planner, with an optional `profile`), `send_prompt`, `get_output`, `kill_agent`, \
`focus_agent`, `rename_agent`; the shared task board via `list_tasks` / `upsert_task` / \
`remove_task`; and durable state via `get_state` / `set_state`. Guardrails (live-agent \
cap, per-role CLI + model) are enforced by loomux.\n\
- Maintain the task board: it is the human's view of the work. Record each agent's \
`session` id on its task so finished work can be resumed for follow-ups instead of \
cold-started. Never disturb a busy worker with a new task.\n\
- Drive the flow: plan → spawn workers/reviewers/planners → branch → PR → review → human \
merge gate. You never merge; you surface work at the gate for the human.\n\
- Use `report`/`message_orchestrator` semantics from your delegates as their status \
channel; keep the human oriented with short summaries."
            .to_string(),
        Role::Worker => format!(
            "{common}\n- Deliverable: a branch → commit → PR with the project's tests green. \
             Add tests that would fail if the feature regressed."
        ),
        Role::Reviewer => format!(
            "{common}\n- You review PRs via `gh` (checking out the PR branch locally is fine); \
             you do NOT create branches or push. Report findings via `report`/`message_orchestrator`."
        ),
        Role::Planner => format!(
            "{common}\n- You explore the codebase READ-ONLY and write an implementation plan as a \
             GitHub issue comment, then `report` and exit. You never write code, branches, \
             worktrees, or PRs (loomux also denies those at the CLI level)."
        ),
    }
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
            paused: Mutex::new(HashSet::new()),
            spawn_times: Mutex::new(HashMap::new()),
            self_arc: Mutex::new(Weak::new()),
            attn_reports: Mutex::new(HashMap::new()),
            attn_quiet: Mutex::new(HashMap::new()),
            attn_waiting_ack: Mutex::new(HashSet::new()),
            attn_emitted: Mutex::new(HashMap::new()),
            notify_groups: Mutex::new(HashSet::new()),
            pending_max_notice: Mutex::new(HashMap::new()),
            claude_projects_dir: Mutex::new(None),
        }
    }

    /// Point the usage reader at a specific Claude transcript root, instead of
    /// `~/.claude/projects`. Test-only seam (see `claude_projects_dir`).
    #[doc(hidden)]
    pub fn set_claude_projects_dir(&self, dir: PathBuf) {
        *self.claude_projects_dir.lock_safe() = Some(dir);
    }

    /// Record the `Arc` the registry is stored behind so `&self` methods can
    /// spawn background work that outlives the current call. Call once, right
    /// after wrapping the registry in an `Arc`.
    pub fn set_self_arc(self: &Arc<Self>) {
        *self.self_arc.lock_safe() = Arc::downgrade(self);
    }

    /// Upgrade the stored weak self-handle. `None` in unit tests that build a
    /// bare registry without calling `set_self_arc` — background helpers then
    /// simply don't run.
    fn arc(&self) -> Option<Arc<Self>> {
        self.self_arc.lock_safe().upgrade()
    }

    /// Default persistent root: `<user data dir>/loomux/orchestration`.
    pub fn default_root() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("loomux")
            .join("orchestration")
    }

    pub fn set_app(&self, app: AppHandle) {
        *self.app.lock_safe() = Some(app);
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

    /// Scratch dir holding images pasted/attached into the steering strip (#72).
    /// A subdir of the group state dir, so it's naturally per-group and swept
    /// on group end alongside the worktrees.
    fn attachments_dir(&self, group: &str) -> PathBuf {
        self.group_dir(group).join("attachments")
    }

    /// The CLI the group's orchestrator runs (`claude`/`copilot`/…), resolving
    /// per-role overrides through `cli_for`. Used to format image references the
    /// way that CLI consumes them (#72). Falls back to the default `claude`
    /// wording if the group isn't loaded (a save always follows a live steer, so
    /// this is just a safety net).
    pub fn orchestrator_cli(&self, group: &str) -> String {
        self.group(group)
            .map(|g| g.guardrails.cli_for(Role::Orchestrator).to_string())
            .unwrap_or_else(|| "claude".into())
    }

    /// Persist a steered image to the group's `attachments/` scratch dir and
    /// return its absolute path (#72). The steering strip can't hand binary to
    /// a CLI prompt, but Claude Code and Copilot both *read image files from
    /// paths* — so a pasted screenshot is written here and the steer text gains
    /// an "Attached image: <path>" line pointing at it. Bytes are written
    /// verbatim: the image arrives as a browser Blob and we never decode it
    /// (no image crate, no `getrandom` deps) — only size and extension are
    /// validated. Files are reclaimed when the group ends (see `end_group`).
    pub fn save_attachment(&self, group: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        // Membership guard: only ever write under a real, known group id (#72
        // review). The dir is `root.join(group)`, so without this a caller could
        // steer `group` to a traversal component; requiring the group to exist
        // pins it to a generated group token. Cheap hardening on top of the
        // pre-existing trusted-webview model (see the orch-command notes).
        if self.group(group).is_none() {
            return Err("unknown group".into());
        }
        if bytes.is_empty() {
            return Err("empty attachment".into());
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "attachment too large ({} bytes, max {MAX_ATTACHMENT_BYTES})",
                bytes.len()
            ));
        }
        let ext = sanitize_attachment_ext(ext)
            .ok_or_else(|| format!("unsupported attachment type: {ext:?}"))?;
        let dir = self.attachments_dir(group);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // `<ms>-<seq>.<ext>`: wall-clock time keeps names sortable/legible while
        // the process-local sequence disambiguates a same-millisecond burst.
        let name = format!("{}-{}.{ext}", now_ms(), ATTACH_SEQ.fetch_add(1, Ordering::Relaxed));
        let path = dir.join(name);
        fs::write(&path, bytes).map_err(|e| e.to_string())?;
        self.audit(group, "human", "attachment-save",
            json!({ "path": path.display().to_string(), "bytes": bytes.len() }));
        Ok(path)
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
        if let Some(app) = self.app.lock_safe().clone() {
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
        let _guard = self.tasks_lock.lock_safe();
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
        let _guard = self.tasks_lock.lock_safe();
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
        let _guard = self.tasks_lock.lock_safe();
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

    /// Guard the merge-gate actions to items actually at the gate. The UI only
    /// shows the buttons on `pr`/`human-testing` items, but the command surface
    /// is callable directly, so enforce it backend-side too — approving a
    /// `queued` item or requesting changes on a `done` one is meaningless.
    fn ensure_at_merge_gate(&self, group: &str, id: &str) -> Result<(), String> {
        let status = self
            .tasks(group)
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("unknown task: {id}"))?
            .status;
        if MERGE_GATE_STATUSES.contains(&status.as_str()) {
            Ok(())
        } else {
            Err(format!(
                "task {id} is {status:?}, not at the merge gate — this action only applies to {}",
                MERGE_GATE_STATUSES.join(" | ")
            ))
        }
    }

    /// Merge-gate approve: mark the item done and tell the orchestrator to
    /// merge. The status change is the human's direct sign-off, applied here;
    /// the notice is best-effort (the board is the source of truth).
    pub fn approve_task(&self, group: &str, id: &str) -> Result<Task, String> {
        self.ensure_at_merge_gate(group, id)?;
        let task = self.upsert_task(
            group,
            "human",
            Some(id),
            TaskPatch {
                status: Some("done".into()),
                note: Some("Approved at the merge gate.".into()),
                ..Default::default()
            },
        )?;
        let pr = task.pr.as_deref().unwrap_or("(no PR ref)");
        let _ = self.deliver_to_orchestrator(
            group,
            &format!(
                "[loomux] the human APPROVED {} \"{}\" ({}) at the merge gate and marked it done. \
                 Merge the PR and close out the work item.",
                task.id, task.title, pr
            ),
            "human",
        );
        Ok(task)
    }

    /// Merge-gate request-changes: record the findings as a note and deliver
    /// them to the orchestrator to route back to a worker. Status is left for
    /// the orchestrator to manage as it re-dispatches.
    pub fn request_changes(&self, group: &str, id: &str, findings: &str) -> Result<Task, String> {
        let findings = findings.trim();
        if findings.is_empty() {
            return Err("request changes needs a note describing what to fix".into());
        }
        self.ensure_at_merge_gate(group, id)?;
        let task = self.upsert_task(
            group,
            "human",
            Some(id),
            TaskPatch { note: Some(format!("Requested changes: {findings}")), ..Default::default() },
        )?;
        let pr = task.pr.as_deref().unwrap_or("(no PR ref)");
        let _ = self.deliver_to_orchestrator(
            group,
            &format!(
                "[loomux] the human REQUESTED CHANGES on {} \"{}\" ({}) at the merge gate. \
                 Findings: {findings}. Route it back to a worker to address, then re-request review.",
                task.id, task.title, pr
            ),
            "human",
        );
        Ok(task)
    }

    /// Guard the start action to items that are actually queued. The UI only
    /// shows the button on `queued` items, but the command surface is callable
    /// directly, so enforce it backend-side too — starting an in-progress or
    /// done item is meaningless.
    fn ensure_queued(&self, group: &str, id: &str) -> Result<(), String> {
        let status = self
            .tasks(group)
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("unknown task: {id}"))?
            .status;
        if status == "queued" {
            Ok(())
        } else {
            Err(format!("task {id} is {status:?}, not queued — only a queued task can be started"))
        }
    }

    /// Start a queued item: record a human-attributed note and tell the
    /// orchestrator to begin work on it now. Deliberately does NOT flip the
    /// status — the orchestrator moves it to `in-progress` when it actually
    /// assigns a worker, so the board reflects real assignment rather than
    /// intent. The notice is best-effort (the board is the source of truth).
    ///
    /// A paused group is rejected up front (mirroring `steer_orchestrator`):
    /// its delivery is silently suppressed and queued prompts aren't replayed
    /// on resume, so without this guard the nudge would vanish — with a note
    /// left behind implying it landed. Reject before any mutation so no note is
    /// appended, and let the human resume first.
    pub fn start_task(&self, group: &str, id: &str) -> Result<Task, String> {
        self.ensure_queued(group, id)?;
        if self.is_paused(group) {
            return Err("group is paused — resume before starting tasks".into());
        }
        let task = self.upsert_task(
            group,
            "human",
            Some(id),
            TaskPatch {
                note: Some("Started by the human — asked the orchestrator to begin work.".into()),
                ..Default::default()
            },
        )?;
        let _ = self.deliver_to_orchestrator(
            group,
            &format!(
                "[loomux] the human started task {} (\"{}\") — begin work on it now.",
                task.id, task.title
            ),
            "human",
        );
        Ok(task)
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
        let _guard = self.tasks_lock.lock_safe();
        let path = self.group_dir(&entry.group).join("agents.json");
        let mut list: Vec<AgentRecord> = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let record = AgentRecord {
            id: entry.id.clone(),
            role: entry.role.as_str().into(),
            name: entry.name.clone(),
            name_source: entry.name_source,
            session: entry.session_id.clone(),
            cwd: entry.cwd.clone(),
            status: status.to_string(),
            updated_ms: now_ms(),
        };
        // Match by (id, session): agent ids restart at 1 every app run, so
        // a bare-id match would overwrite a previous run's record and lose
        // that session's identity. A session-bearing record also supersedes
        // this run's placeholder for the same id — copilot writes an entry
        // with no session at spawn, then upgrades it once its session id is
        // discovered (only placeholders have session == None).
        match list.iter_mut().find(|r| {
            r.id == record.id && (r.session == record.session || r.session.is_none())
        }) {
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

    /// Poll `~/.copilot/session-state` for the session the just-spawned
    /// copilot pane created (the one absent from `baseline`) and bind its id
    /// to the pane. Runs on its own thread — copilot writes the session a few
    /// seconds into boot. Gives up after `COPILOT_SESSION_TIMEOUT`.
    fn spawn_copilot_session_watcher(
        self: Arc<Self>,
        agent_id: String,
        group_id: String,
        cwd: String,
        baseline: HashSet<String>,
    ) {
        let Some(root) = crate::sessions::copilot_session_state_root() else {
            return;
        };
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + COPILOT_SESSION_TIMEOUT;
            loop {
                std::thread::sleep(COPILOT_SESSION_POLL);
                // Stop if the pane died or was already associated (a resume
                // re-spawn, or a manual edit) — nothing left to track.
                match self.agent(&agent_id) {
                    Some(a) if a.status == AgentStatus::Dead => return,
                    Some(a) if a.session_id.is_some() => return,
                    Some(_) => {}
                    None => return,
                }
                if let Some(sid) =
                    crate::sessions::newest_new_copilot_session(&root, &baseline, &cwd)
                {
                    self.associate_copilot_session(&group_id, &agent_id, &sid);
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    self.audit(&group_id, "loomux", "copilot-session-untracked",
                        json!({ "agent": agent_id, "reason": "no new session-state appeared before timeout" }));
                    return;
                }
            }
        });
    }

    /// Bind a discovered copilot session id to a live pane: update the agent
    /// map, the durable roster (`agents.json`), and any task board item this
    /// agent owns — the same session trail Claude gets at spawn. Best-effort.
    /// Public for the session watcher and its tests; a no-op if the pane is
    /// gone or already carries a session id.
    pub fn associate_copilot_session(&self, group_id: &str, agent_id: &str, session_id: &str) {
        let entry = {
            let mut agents = self.agents.lock_safe();
            let Some(a) = agents.get_mut(agent_id) else { return };
            // Don't clobber an id set in the meantime (e.g. a resume).
            if a.session_id.is_some() {
                return;
            }
            a.session_id = Some(session_id.to_string());
            a.clone()
        };
        let status = match entry.status {
            AgentStatus::Dead => "dead",
            _ => "running",
        };
        self.persist_agent_record(&entry, status);
        // Mirror onto the task board: any item this agent owns (by id or
        // display name) that lacks a session gets it, so the orchestrator can
        // resume the task later without hunting the id out of list_agents.
        {
            let _guard = self.tasks_lock.lock_safe();
            let mut tasks = self.tasks(group_id);
            let mut changed = false;
            for t in tasks.iter_mut() {
                let owner = t.assignee.as_deref().unwrap_or("");
                if t.session.is_none() && (owner == entry.id || owner == entry.name) {
                    t.session = Some(session_id.to_string());
                    t.updated_ms = now_ms();
                    changed = true;
                }
            }
            if changed {
                let _ = self.write_tasks(group_id, &tasks);
            }
        }
        self.audit(group_id, "loomux", "copilot-session",
            json!({ "agent": agent_id, "session": session_id }));
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
                // The spawn audit predates the name-tier field; backfilled
                // sessions restore at the default tier (#95r).
                name_source: NameSource::default(),
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
                // Per-role CLIs are additive (issue #4): absent in older
                // group.json → empty → inherit `agent_cli`.
                orchestrator_cli: s("orchestrator_cli", ""),
                worker_cli: s("worker_cli", ""),
                reviewer_cli: s("reviewer_cli", ""),
                planner_cli: s("planner_cli", ""),
                worker_model: s("worker_model", ""),
                reviewer_model: s("reviewer_model", ""),
                orchestrator_model: s("orchestrator_model", ""),
                planner_model: s("planner_model", ""),
                auto_ops: g["auto_ops"].as_bool().unwrap_or(true),
                // Additive (issue #51): absent in older group.json → false
                // (repo MCP stays gated off until the human opts in).
                trust_repo_mcp: g["trust_repo_mcp"].as_bool().unwrap_or(false),
                // Manual per-role profile assignment (issue #51); absent = auto.
                orchestrator_profile: s("orchestrator_profile", ""),
                worker_profile: s("worker_profile", ""),
                reviewer_profile: s("reviewer_profile", ""),
                planner_profile: s("planner_profile", ""),
                idle_kill_minutes: g["idle_kill_minutes"].as_u64().unwrap_or(0) as u32,
                max_spawns_per_hour: g["max_spawns_per_hour"].as_u64().unwrap_or(0) as u32,
                watchdog_stall_minutes: g["watchdog_stall_minutes"].as_u64().unwrap_or(0) as u32,
            },
        ))
    }

    // ---------- groups & agents ----------

    /// Create (or reattach to) the group for `repo`. State and audit history
    /// persist under the repo-derived group id; guardrails are refreshed from
    /// the new launch.
    pub fn create_group(&self, repo: &str, guardrails: Guardrails) -> Result<GroupInfo, String> {
        let mut guardrails = guardrails.clamped();
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
        // The live-agent cap is adjustable mid-session (`set_max_agents`) and
        // persisted, so it's a durable human choice — like the pause/notify
        // markers re-seeded below. On resume, prefer the persisted cap over the
        // caller's param: the launcher hardcodes its default (4) and can't
        // pre-fill from group.json, so without this a relaunch would silently
        // revert an on-the-fly adjustment. Other guardrails still refresh from
        // the launch (only the cap is live-adjustable). Read before the write
        // below overwrites the file.
        if resumed {
            if let Some((_, persisted)) = self.load_group_file(&id) {
                guardrails.max_agents = persisted.max_agents.clamp(1, MAX_AGENTS_CEILING);
            }
        }
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
                    "orchestrator_cli": info.guardrails.orchestrator_cli,
                    "worker_cli": info.guardrails.worker_cli,
                    "reviewer_cli": info.guardrails.reviewer_cli,
                    "planner_cli": info.guardrails.planner_cli,
                    "worker_model": info.guardrails.worker_model,
                    "reviewer_model": info.guardrails.reviewer_model,
                    "orchestrator_model": info.guardrails.orchestrator_model,
                    "planner_model": info.guardrails.planner_model,
                    "auto_ops": info.guardrails.auto_ops,
                    "trust_repo_mcp": info.guardrails.trust_repo_mcp,
                    "orchestrator_profile": info.guardrails.orchestrator_profile,
                    "worker_profile": info.guardrails.worker_profile,
                    "reviewer_profile": info.guardrails.reviewer_profile,
                    "planner_profile": info.guardrails.planner_profile,
                    "idle_kill_minutes": info.guardrails.idle_kill_minutes,
                    "max_spawns_per_hour": info.guardrails.max_spawns_per_hour,
                    "watchdog_stall_minutes": info.guardrails.watchdog_stall_minutes,
                },
            }))
            .unwrap(),
        )
        .map_err(|e| e.to_string())?;
        self.write_instruction_files(&info)?;
        // A pause is a durable human safety action: re-seed it from the
        // marker file so a resumed group stays paused across restarts.
        if dir.join("paused").is_file() {
            self.paused.lock_safe().insert(id.clone());
        }
        // Desktop-notification opt-in is likewise a durable per-group choice.
        if dir.join("notify").is_file() {
            self.notify_groups.lock_safe().insert(id.clone());
        }
        self.groups.lock_safe().insert(id.clone(), info.clone());
        self.audit(&id, "loomux", if resumed { "group-resume" } else { "group-create" },
            json!({ "repo": repo, "max_agents": info.guardrails.max_agents,
                    "worker_model": info.guardrails.worker_model }));
        Ok(info)
    }

    pub fn group(&self, id: &str) -> Option<GroupInfo> {
        self.groups.lock_safe().get(id).cloned()
    }

    /// A group is live while any of its agents is not dead.
    fn group_is_live(&self, id: &str) -> bool {
        self.agents
            .lock_safe()
            .values()
            .any(|a| a.group == id && a.status != AgentStatus::Dead)
    }

    // ---------- cost containment: pause, idle-kill, spawn-rate, usage ----------

    /// Whether a group is currently paused (prompts/kickoffs suppressed).
    pub fn is_paused(&self, group: &str) -> bool {
        self.paused.lock_safe().contains(group)
    }

    /// Pause a group: loomux stops delivering prompts and kickoffs to its
    /// agents, so they finish their current turn and idle out (containing
    /// unattended spend) without being killed. Durable via a marker file.
    pub fn pause_group(&self, group: &str) -> Result<(), String> {
        let newly = self.paused.lock_safe().insert(group.to_string());
        if newly {
            let dir = self.group_dir(group);
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let _ = fs::write(dir.join("paused"), b"");
            self.audit(group, "human", "group-pause", json!({}));
        }
        Ok(())
    }

    /// Resume a paused group: prompt/kickoff delivery flows again. Queued
    /// prompts are not replayed — agents resync from the board/state on the
    /// next prompt, which is the point of idling out.
    pub fn resume_group(&self, group: &str) -> Result<(), String> {
        let was = self.paused.lock_safe().remove(group);
        if was {
            let _ = fs::remove_file(self.group_dir(group).join("paused"));
            self.audit(group, "human", "group-resume", json!({}));
        }
        Ok(())
    }

    /// Flip a worker/reviewer between idle (awaiting/finished a task) and
    /// active. `idle` true stamps `idle_since_ms = now`; false clears it.
    /// No-op for the orchestrator, which is never idle-reaped.
    fn set_agent_idle(&self, agent_id: &str, idle: bool) {
        let mut agents = self.agents.lock_safe();
        if let Some(a) = agents.get_mut(agent_id) {
            if a.role == Role::Orchestrator {
                return;
            }
            a.idle_since_ms = idle.then(now_ms);
            if !idle {
                // (Re)assigned work: restart the watchdog's silence clock and
                // clear its anti-nag latch so the fresh stall gets a nudge.
                a.last_progress_ms = now_ms();
                a.watchdog_notified = false;
                // New work supersedes a prior done/blocked report — drop its
                // attention latch so a stale badge doesn't linger.
                self.attn_reports.lock_safe().remove(agent_id);
            }
        }
    }

    /// Ids of workers/reviewers whose idle time has crossed their group's
    /// `idle_kill_minutes`. Pure selection (no killing) so the reaper policy
    /// is testable at a chosen `now`.
    pub fn idle_reap_candidates(&self, now: u64) -> Vec<String> {
        let thresholds: HashMap<String, u32> = self
            .groups
            .lock_safe()
            .iter()
            .map(|(id, g)| (id.clone(), g.guardrails.idle_kill_minutes))
            .collect();
        self.agents
            .lock_safe()
            .values()
            .filter(|a| a.role != Role::Orchestrator && a.status == AgentStatus::Running)
            .filter(|a| {
                let t = thresholds.get(&a.group).copied().unwrap_or(0);
                idle_should_kill(a.idle_since_ms, now, t)
            })
            .map(|a| a.id.clone())
            .collect()
    }

    /// Kill every idle worker/reviewer past its group's timeout, notifying
    /// each group's orchestrator so it can respawn on demand. Returns the
    /// killed agent ids. Called on a timer by `start_idle_reaper`.
    pub fn reap_idle_agents(&self, now: u64) -> Vec<String> {
        let mut killed = Vec::new();
        for id in self.idle_reap_candidates(now) {
            let Some(a) = self.agent(&id) else { continue };
            let mins = self
                .group(&a.group)
                .map(|g| g.guardrails.idle_kill_minutes)
                .unwrap_or(0);
            // Re-check against the agent's *current* idle state: selection and
            // kill happen under separate locks, so a worker prompted in that
            // window (idle clock cleared) must not be killed.
            if !idle_should_kill(a.idle_since_ms, now, mins) {
                continue;
            }
            self.audit(&a.group, "loomux", "idle-kill",
                json!({ "agent": id, "name": a.name, "idle_minutes": mins }));
            let _ = self.deliver_to_orchestrator(
                &a.group,
                &format!(
                    "[loomux] idle-kill guardrail: agent {} ({}) sat without a task for {mins}+ min and was terminated to contain cost. Respawn a worker when you have work for it.",
                    a.name, a.id
                ),
                "loomux",
            );
            let _ = self.kill_agent(&id);
            killed.push(id);
        }
        killed
    }

    // ---------- watchdog: stalled-agent detection ----------

    /// Record that an agent just did something loomux can see (reported,
    /// messaged the orchestrator): reset its watchdog silence clock and clear
    /// the anti-nag latch so a *later* stall still earns a fresh nudge. No-op
    /// for the orchestrator (never watchdogged). Output-driven activity is
    /// handled separately in `watchdog_tick` via the pty counter.
    pub fn note_agent_activity(&self, agent_id: &str) {
        let mut agents = self.agents.lock_safe();
        if let Some(a) = agents.get_mut(agent_id) {
            if a.role == Role::Orchestrator {
                return;
            }
            a.last_progress_ms = now_ms();
            a.watchdog_notified = false;
        }
    }

    /// Snapshot every agent's monotonic pty output counter. Needs the app's
    /// `PtyManager`, so it yields an empty map without an app handle (unit
    /// tests drive `watchdog_tick` with synthetic counters instead).
    fn agent_output_totals(&self) -> HashMap<String, u64> {
        let Some(app) = self.app.lock_safe().clone() else {
            return HashMap::new();
        };
        let ptys = app.state::<crate::pty::PtyManager>();
        self.agents
            .lock_safe()
            .values()
            .filter_map(|a| Some((a.id.clone(), ptys.output_total(a.pty_id?)?)))
            .collect()
    }

    /// One watchdog pass. For each *working* agent (running worker/reviewer
    /// with a task assigned — idle clock clear), fold in the latest pty output
    /// counter from `outputs`: any growth is activity that resets the silence
    /// clock and the anti-nag latch. An agent silent (no output, no report)
    /// past its group's `watchdog_stall_minutes` earns exactly one audited
    /// `[loomux]` nudge to the orchestrator suggesting get_output + re-send.
    /// Paused groups are skipped entirely — delivery is suppressed there
    /// anyway, so we must not spend the one-notice budget while paused.
    /// Returns the notified agent ids. Split from the pty read
    /// (`agent_output_totals`) so the stall / anti-nag / pause logic is
    /// testable with synthetic counters and no threads.
    pub fn watchdog_tick(&self, now: u64, outputs: &HashMap<String, u64>) -> Vec<String> {
        let thresholds: HashMap<String, u32> = self
            .groups
            .lock_safe()
            .iter()
            .map(|(id, g)| (id.clone(), g.guardrails.watchdog_stall_minutes))
            .collect();
        let paused = self.paused.lock_safe().clone();

        // First pass under the agents lock: refresh counters and pick who to
        // nudge. Delivery (which types into a pane and can block) happens after
        // the lock is released.
        let mut to_notify: Vec<(String, String, String, u32)> = Vec::new();
        {
            let mut agents = self.agents.lock_safe();
            for a in agents.values_mut() {
                // Only agents actively working: running, not the orchestrator,
                // and currently assigned (idle_since_ms clear). This excludes
                // idle, done/blocked, dead, and reaped agents by construction.
                if a.role == Role::Orchestrator
                    || a.status != AgentStatus::Running
                    || a.idle_since_ms.is_some()
                {
                    continue;
                }
                // Output growth = activity: reset the clock and the latch, and
                // this tick can't also flag the agent as stalled.
                if let Some(&cur) = outputs.get(&a.id) {
                    if cur > a.last_output_total {
                        a.last_output_total = cur;
                        a.last_progress_ms = now;
                        a.watchdog_notified = false;
                        continue;
                    }
                }
                // A paused group's agents idle out on purpose; never nudge and
                // never burn their one-notice budget while paused.
                if paused.contains(&a.group) {
                    continue;
                }
                let threshold = thresholds.get(&a.group).copied().unwrap_or(0);
                if watchdog_should_notify(a.last_progress_ms, now, threshold, a.watchdog_notified) {
                    a.watchdog_notified = true;
                    let minutes = (now.saturating_sub(a.last_progress_ms) / 60_000) as u32;
                    to_notify.push((a.id.clone(), a.group.clone(), a.name.clone(), minutes));
                }
            }
        }

        let mut notified = Vec::new();
        for (id, group, name, minutes) in to_notify {
            self.audit(&group, "loomux", "watchdog-stall",
                json!({ "agent": id, "name": name, "silent_minutes": minutes }));
            let _ = self.deliver_to_orchestrator(
                &group,
                &format!(
                    "[loomux] watchdog: agent {name} ({id}) has produced no terminal output and sent no report for {minutes}+ min — it may be stalled or waiting on input. Inspect it with get_output(\"{id}\"); if its kickoff was lost or it is stuck, re-send the task with send_prompt. You will get this notice at most once per stall."
                ),
                "loomux",
            );
            notified.push(id);
        }
        notified
    }

    /// One full watchdog cycle: read pty counters, then tick. Called on a
    /// timer by `start_watchdog`.
    pub fn run_watchdog(&self, now: u64) -> Vec<String> {
        let outputs = self.agent_output_totals();
        self.watchdog_tick(now, &outputs)
    }

    // ---------- attention routing: surface which pane needs the human ----------

    /// Latch (or clear) a worker's report as an attention signal. `done` and
    /// `blocked` badge the pane and can fire a toast until the human acks or the
    /// agent is reassigned; `progress` (the agent is working again) clears it.
    /// No-op for the orchestrator, which never reports.
    pub fn note_report_attention(&self, agent_id: &str, status: &str) {
        let mut m = self.attn_reports.lock_safe();
        match status {
            "done" => {
                m.insert(agent_id.to_string(), "done");
            }
            "blocked" => {
                m.insert(agent_id.to_string(), "blocked");
            }
            _ => {
                m.remove(agent_id);
            }
        }
    }

    /// The human focused/handled a pane: drop any latched report so its badge
    /// clears, and suppress the live `waiting` badge so focusing a pane whose
    /// menu is still on screen makes the ack *stick* — otherwise the next 3s scan
    /// re-emits `waiting` and re-lights the pane the human is already on (#40
    /// review). The suppression self-clears once the pane's output changes (the
    /// menu was answered / the CLI repainted), so a genuinely new prompt on the
    /// same pane flags again. The `gate` reason is board state, cleared by moving
    /// the task, so it needs no ack.
    pub fn ack_attention(&self, agent_id: &str) {
        self.attn_reports.lock_safe().remove(agent_id);
        self.attn_waiting_ack.lock_safe().insert(agent_id.to_string());
    }

    /// The human turned to a *plain* pane (#40): make its `waiting` ack stick the
    /// same way `ack_attention` does for agents, keyed by the pane's pty id. The
    /// suppression lifts when the pane's output next changes (see
    /// `plain_pane_attention`).
    pub fn ack_attention_pty(&self, pty_id: u32) {
        self.attn_waiting_ack.lock_safe().insert(format!("pty:{pty_id}"));
    }

    /// Whether desktop notifications are enabled for a group.
    pub fn notify_enabled(&self, group: &str) -> bool {
        self.notify_groups.lock_safe().contains(group)
    }

    /// Enable/disable desktop notifications for a group, durably (a `notify`
    /// marker file, mirroring the pause marker) so the choice survives restarts.
    pub fn set_notify(&self, group: &str, on: bool) -> Result<(), String> {
        let dir = self.group_dir(group);
        let mut set = self.notify_groups.lock_safe();
        if on {
            if set.insert(group.to_string()) {
                fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let _ = fs::write(dir.join("notify"), b"");
                self.audit(group, "human", "notify-on", json!({}));
            }
        } else if set.remove(group) {
            let _ = fs::remove_file(dir.join("notify"));
            self.audit(group, "human", "notify-off", json!({}));
        }
        Ok(())
    }

    /// Adjust a live group's max live-agent cap on the fly. Bounds are the
    /// launcher's `1..=MAX_AGENTS_CEILING`. The new value is written to the
    /// in-memory guardrail (which `spawn_agent` reads fresh on every spawn, so
    /// it takes effect immediately — nothing caches the creation-time number)
    /// and persisted to group.json so a restart keeps it, then the change is
    /// audited (per-click) and the orchestrator notice is *debounced* — a burst
    /// of stepper clicks coalesces into one re-plan prompt (#79). Lowering the cap below
    /// the current live count kills nobody: new spawns are simply refused until
    /// attrition brings the count back under the cap. Returns the new value.
    /// A no-op change (`n` already the current cap) short-circuits without a
    /// second write, audit, or notice. `actor` records who made the change.
    pub fn set_max_agents(&self, group: &str, n: u32, actor: &str) -> Result<u32, String> {
        if !(1..=MAX_AGENTS_CEILING).contains(&n) {
            return Err(format!("max agents must be between 1 and {MAX_AGENTS_CEILING}"));
        }
        let old = self.group(group).ok_or("unknown group")?.guardrails.max_agents;
        if n == old {
            return Ok(n);
        }
        // Persist first: a failed disk write must leave the in-memory cap (the
        // value enforcement reads) unchanged, so the two never disagree.
        self.persist_max_agents(group, n)?;
        self.groups
            .lock_safe()
            .get_mut(group)
            .ok_or("unknown group")?
            .guardrails
            .max_agents = n;
        self.audit(group, actor, "max-agents-set", json!({ "from": old, "to": n }));
        // The orchestrator's kickoff prompt already rendered the old
        // {{MAX_AGENTS}} into static text; it needs the new ceiling to re-plan.
        // But rapid-clicking the stepper (4→3→2) would otherwise fire a notice
        // per click, each a real prompt that burns orchestrator tokens/time
        // (#79). So debounce: record the change here (carrying the burst's
        // original `from`) and let `flush_due_max_notices` deliver ONE notice —
        // 4→2, not 4→3 then 3→2 — once the clicks stop. Enforcement/persist
        // above and the audit are per-click and immediate; only the notice waits.
        record_max_notice(
            &mut self.pending_max_notice.lock_safe(),
            group,
            old,
            n,
            now_ms(),
            MAX_NOTICE_DEBOUNCE,
        );
        Ok(n)
    }

    /// Rewrite only `guardrails.max_agents` in group.json, preserving every
    /// other stored field (created_ms, the other guardrails, and anything a
    /// later feature adds). Patching the parsed JSON in place — rather than
    /// reserializing a full GroupInfo — keeps this additive and rebase-clean.
    fn persist_max_agents(&self, group: &str, n: u32) -> Result<(), String> {
        let dir = self.group_dir(group);
        let path = dir.join("group.json");
        let mut v: Value = serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        // Guard the indexing so a corrupt-but-valid-JSON file (e.g. a `null`
        // root) fails soft instead of panicking on assignment.
        let obj = v.as_object_mut().ok_or("group.json root is not a JSON object")?;
        match obj.get_mut("guardrails").and_then(Value::as_object_mut) {
            Some(guard) => {
                guard.insert("max_agents".into(), json!(n));
            }
            None => {
                obj.insert("guardrails".into(), json!({ "max_agents": n }));
            }
        }
        // Crash-safe write: serialize to a temp file, then atomically rename
        // over group.json (same pattern as usage.json). group.json is
        // identity-critical — a half-written file breaks the rejoin path
        // ("group.json is missing") — so never expose a truncated version.
        let body = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        let tmp = dir.join("group.json.tmp");
        fs::write(&tmp, &body).map_err(|e| e.to_string())?;
        if fs::rename(&tmp, &path).is_err() {
            // Rename can fail if the destination exists on some platforms; fall
            // back to a direct write so the update isn't lost.
            fs::write(&path, &body).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&tmp);
        }
        Ok(())
    }

    /// Deliver any debounced cap-change notice whose quiet window has elapsed
    /// (#79). Called on a timer by `start_max_notice_flusher`; `now` is injected
    /// so tests drive the coalescing deterministically without sleeping out the
    /// debounce. A burst that netted to a no-op is dropped inside
    /// `take_due_max_notices` and never reaches the orchestrator.
    #[doc(hidden)] // pub for integration tests
    pub fn flush_due_max_notices(&self, now: u64) {
        let due = take_due_max_notices(&mut self.pending_max_notice.lock_safe(), now);
        for (group, from, to) in due {
            // Best-effort, like the exit notice: a dead/paused orchestrator
            // just misses it. Delivery is intentionally outside the lock.
            let _ = self.deliver_to_orchestrator(&group, &max_agents_notice(from, to), "loomux");
        }
    }

    /// Read every agent pane's output counter, last-lines tail, and last human
    /// keystroke time — the raw inputs `attention_tick` needs. Empty without an
    /// app handle (unit tests drive `attention_tick` with synthetic maps).
    fn attention_inputs(&self) -> (HashMap<String, u64>, HashMap<String, String>, HashMap<String, u64>) {
        let mut outs = HashMap::new();
        let mut tails = HashMap::new();
        let mut ins = HashMap::new();
        let Some(app) = self.app.lock_safe().clone() else {
            return (outs, tails, ins);
        };
        let ptys = app.state::<crate::pty::PtyManager>();
        for a in self.agents.lock_safe().values() {
            let Some(pid) = a.pty_id else { continue };
            if let Some(t) = ptys.output_total(pid) {
                outs.insert(a.id.clone(), t);
            }
            if let Some(raw) = ptys.output_tail(pid) {
                // A prompt is at the very end; strip only the last few KB so
                // the scan stays cheap against a saturated 256 KB ring.
                let start = raw.len().saturating_sub(4096);
                tails.insert(a.id.clone(), strip_ansi(&raw[start..]));
            }
            if let Some(u) = ptys.last_user_input_ms(pid) {
                ins.insert(a.id.clone(), u);
            }
        }
        (outs, tails, ins)
    }

    /// Pty snapshots for every live pane that is NOT a registered agent, keyed
    /// by pty id, plus the agent-pty set. Feeds `plain_pane_attention` so the
    /// scan reaches plain shells the human opened by hand (#40). Empty without an
    /// app handle (unit tests drive the pure core `pane_attention_inputs_from`).
    #[allow(clippy::type_complexity)]
    fn pane_attention_inputs(
        &self,
    ) -> (HashMap<u32, u64>, HashMap<u32, String>, HashMap<u32, u64>, HashSet<u32>) {
        let mut agent_ptys = HashSet::new();
        let Some(app) = self.app.lock_safe().clone() else {
            return (HashMap::new(), HashMap::new(), HashMap::new(), agent_ptys);
        };
        for a in self.agents.lock_safe().values() {
            if let Some(pid) = a.pty_id {
                agent_ptys.insert(pid);
            }
        }
        let ptys = app.state::<crate::pty::PtyManager>();
        let mut live = Vec::new();
        for pid in ptys.live_ids() {
            // Skip agent ptys *before* touching the ring: `attention_tick`
            // already covers them, and `attention_inputs` already cloned their
            // (up-to-256 KB) output ring this tick — cloning it a second time
            // here would be pure waste (#40 review).
            if agent_ptys.contains(&pid) {
                continue;
            }
            let Some(total) = ptys.output_total(pid) else { continue };
            let raw = ptys.output_tail(pid).unwrap_or_default();
            let input = ptys.last_user_input_ms(pid).unwrap_or(0);
            live.push((pid, total, raw, input));
        }
        let (outs, tails, ins) = self.pane_attention_inputs_from(&live, &agent_ptys);
        (outs, tails, ins, agent_ptys)
    }

    /// Pure core of `pane_attention_inputs`: build the pty-keyed snapshot maps
    /// `plain_pane_attention` consumes from a list of live pane snapshots
    /// `(pty_id, output_total, raw_tail, last_input_ms)`, ANSI-stripping only the
    /// trailing few KB of each tail (a prompt is at the end). Agent ptys are
    /// skipped. Pure w.r.t. the pty, so run_attention's gather wiring is testable
    /// with a fake live-ids source (#40 review).
    #[allow(clippy::type_complexity)]
    pub fn pane_attention_inputs_from(
        &self,
        live: &[(u32, u64, Vec<u8>, u64)],
        agent_ptys: &HashSet<u32>,
    ) -> (HashMap<u32, u64>, HashMap<u32, String>, HashMap<u32, u64>) {
        let mut outs = HashMap::new();
        let mut tails = HashMap::new();
        let mut ins = HashMap::new();
        for (pid, total, raw, input) in live {
            if agent_ptys.contains(pid) {
                continue;
            }
            outs.insert(*pid, *total);
            let start = raw.len().saturating_sub(4096);
            tails.insert(*pid, strip_ansi(&raw[start..]));
            ins.insert(*pid, *input);
        }
        (outs, tails, ins)
    }

    /// One attention pass: compute the current set of panes that need the human
    /// from live agent state plus the supplied pty snapshots. Reasons, in
    /// priority order, are `blocked` (reported), `waiting` (parked on a prompt:
    /// output quiet past `ATTENTION_QUIET_MS`, a prompt-shaped tail, and no
    /// recent human keystroke), `report` (reported done), and `gate` (this
    /// agent's board task sits at a `pr`/`human-testing`/`blocked` merge gate).
    /// Pure w.r.t. the OS/pty — the pty reads live in `attention_inputs` — so
    /// the whole policy is testable with synthetic maps and no real CLI.
    pub fn attention_tick(
        &self,
        now: u64,
        outputs: &HashMap<String, u64>,
        tails: &HashMap<String, String>,
        last_inputs: &HashMap<String, u64>,
    ) -> Vec<AttentionItem> {
        // Board-derived gate map: agent id → gate status, across every live
        // group. Read once per group (a small fs read) rather than per agent.
        let groups: HashSet<String> =
            self.agents.lock_safe().values().map(|a| a.group.clone()).collect();
        let mut gate_of: HashMap<String, String> = HashMap::new();
        for g in &groups {
            for t in self.tasks(g) {
                let is_gate = MERGE_GATE_STATUSES.contains(&t.status.as_str()) || t.status == "blocked";
                if is_gate {
                    if let Some(assignee) = t.assignee.filter(|s| !s.trim().is_empty()) {
                        gate_of.insert(assignee, t.status);
                    }
                }
            }
        }

        let reports = self.attn_reports.lock_safe().clone();
        let mut quiet = self.attn_quiet.lock_safe();
        let mut waiting_ack = self.attn_waiting_ack.lock_safe();
        let agents = self.agents.lock_safe();
        let mut out = Vec::new();
        for a in agents.values() {
            if a.status != AgentStatus::Running {
                quiet.remove(&a.id);
                waiting_ack.remove(&a.id);
                continue;
            }
            // Track how long the pane's output has been stable.
            let cur = outputs.get(&a.id).copied().unwrap_or(0);
            let entry = quiet.entry(a.id.clone()).or_insert((cur, now));
            let output_changed = cur != entry.0;
            if output_changed {
                *entry = (cur, now);
                // The pane repainted — the acked menu was answered or replaced,
                // so re-arm: a fresh prompt on this pane flags again.
                waiting_ack.remove(&a.id);
            }
            let quiet_for = now.saturating_sub(entry.1);
            let recently_typed = last_inputs
                .get(&a.id)
                .map(|&t| t != 0 && now.saturating_sub(t) < ATTENTION_RECENT_INPUT_MS)
                .unwrap_or(false);
            let waiting = !recently_typed
                && !waiting_ack.contains(&a.id)
                && quiet_for >= ATTENTION_QUIET_MS
                && tails.get(&a.id).map(|t| prompt_wait_detected(t)).unwrap_or(false);

            let report = reports.get(a.id.as_str()).copied();
            let (reason, detail): (&'static str, String) = if report == Some("blocked") {
                ("blocked", format!("{} reported blocked — it needs you", a.name))
            } else if waiting {
                ("waiting", format!("{} is waiting on a prompt", a.name))
            } else if report == Some("done") {
                ("report", format!("{} reported done — review & merge", a.name))
            } else if let Some(st) = gate_of.get(a.id.as_str()) {
                ("gate", format!("task is {st} — awaiting your call"))
            } else {
                continue;
            };
            out.push(AttentionItem {
                agent_id: a.id.clone(),
                group: a.group.clone(),
                name: a.name.clone(),
                role: Some(a.role),
                pty_id: a.pty_id,
                reason,
                detail,
            });
        }
        out.sort_by(|x, y| x.agent_id.cmp(&y.agent_id));
        out
    }

    /// Attention scan for *plain* panes (#40): any pane with a live pty that is
    /// **not** a registered agent — the shells the human opens by hand to run a
    /// CLI. It only ever raises `waiting` (parked on an interactive prompt): the
    /// agent-only reasons (`blocked`/`report`/`gate`) require a roster identity a
    /// plain pane doesn't have. Same quiet + no-keystroke + prompt-tail gate and
    /// the same sticky-ack semantics as the agent path, keyed by a synthetic
    /// `pty:<id>` id in the shared `attn_quiet`/`attn_waiting_ack` maps (agent
    /// ids never collide — they're group-scoped uuids). Pure w.r.t. the pty (the
    /// pty reads live in `pane_attention_inputs`), so it's fixture-testable.
    /// `agent_ptys` are the ptys already handled by `attention_tick`, skipped here.
    pub fn plain_pane_attention(
        &self,
        now: u64,
        outputs: &HashMap<u32, u64>,
        tails: &HashMap<u32, String>,
        last_inputs: &HashMap<u32, u64>,
        agent_ptys: &HashSet<u32>,
    ) -> Vec<AttentionItem> {
        let mut quiet = self.attn_quiet.lock_safe();
        let mut waiting_ack = self.attn_waiting_ack.lock_safe();
        let mut out = Vec::new();
        for (&pty, &cur) in outputs {
            if agent_ptys.contains(&pty) {
                continue;
            }
            let key = format!("pty:{pty}");
            let entry = quiet.entry(key.clone()).or_insert((cur, now));
            if cur != entry.0 {
                *entry = (cur, now);
                waiting_ack.remove(&key); // repainted → re-arm (menu answered)
            }
            let quiet_for = now.saturating_sub(entry.1);
            let recently_typed = last_inputs
                .get(&pty)
                .map(|&t| t != 0 && now.saturating_sub(t) < ATTENTION_RECENT_INPUT_MS)
                .unwrap_or(false);
            let waiting = !recently_typed
                && !waiting_ack.contains(&key)
                && quiet_for >= ATTENTION_QUIET_MS
                && tails.get(&pty).map(|t| prompt_wait_detected(t)).unwrap_or(false);
            if waiting {
                out.push(AttentionItem {
                    agent_id: String::new(),
                    group: String::new(),
                    name: String::new(),
                    role: None,
                    pty_id: Some(pty),
                    reason: "waiting",
                    detail: "This pane is waiting on your input".to_string(),
                });
            }
        }
        // Prune bookkeeping for ptys that have gone away (pane closed), so the
        // shared maps don't grow unbounded with `pty:` keys.
        quiet.retain(|k, _| !k.starts_with("pty:") || k[4..].parse::<u32>().map(|p| outputs.contains_key(&p)).unwrap_or(false));
        waiting_ack.retain(|k| !k.starts_with("pty:") || k[4..].parse::<u32>().map(|p| outputs.contains_key(&p)).unwrap_or(false));
        out.sort_by_key(|i| i.pty_id);
        out
    }

    /// Decide which current attention items warrant a fresh desktop toast:
    /// their group opted in, the reason is an event (not the persistent `gate`
    /// board state, which the board highlight already surfaces), and this
    /// (agent, reason) hasn't been toasted yet. Records only what actually
    /// fires — so enabling notifications surfaces already-pending attention —
    /// and prunes cleared/changed entries so a fresh onset toasts again.
    /// Returns the agent ids to toast; pure w.r.t. the OS, so the policy is
    /// testable without firing a real notification.
    pub fn attention_toast_targets(&self, items: &[AttentionItem]) -> Vec<String> {
        let notify = self.notify_groups.lock_safe().clone();
        let mut toasted = self.attn_emitted.lock_safe();
        let mut fire = Vec::new();
        for i in items {
            let already = toasted.get(&i.agent_id).map(|p| p == i.reason).unwrap_or(false);
            if !already && i.reason != "gate" && notify.contains(&i.group) {
                fire.push(i.agent_id.clone());
                toasted.insert(i.agent_id.clone(), i.reason.to_string());
            }
        }
        // Drop ledger entries whose attention cleared or whose reason changed,
        // so the same pane can toast again on a genuinely new onset.
        let current: HashMap<&str, &str> =
            items.iter().map(|i| (i.agent_id.as_str(), i.reason)).collect();
        toasted.retain(|id, reason| current.get(id.as_str()) == Some(&reason.as_str()));
        fire
    }

    /// One full attention cycle: read pty snapshots, compute the attention set,
    /// fire toasts for newly-attention panes in opted-in groups, and push the
    /// whole set to the frontend. Called on a timer by `start_attention`.
    /// The full attention set: the roster scan (`attention_tick`, all reasons)
    /// merged with the plain-pane scan (`plain_pane_attention`, `waiting` only).
    /// This is run_attention's core, factored out so the merge wiring — plain
    /// panes surface, an agent's pty is never double-covered — is testable
    /// without a real PtyManager (#40 review).
    #[allow(clippy::too_many_arguments)]
    pub fn attention_scan(
        &self,
        now: u64,
        agent_outputs: &HashMap<String, u64>,
        agent_tails: &HashMap<String, String>,
        agent_inputs: &HashMap<String, u64>,
        pane_outputs: &HashMap<u32, u64>,
        pane_tails: &HashMap<u32, String>,
        pane_inputs: &HashMap<u32, u64>,
        agent_ptys: &HashSet<u32>,
    ) -> Vec<AttentionItem> {
        let mut items = self.attention_tick(now, agent_outputs, agent_tails, agent_inputs);
        items.extend(self.plain_pane_attention(now, pane_outputs, pane_tails, pane_inputs, agent_ptys));
        items
    }

    pub fn run_attention(&self, now: u64) {
        let (outputs, tails, last_inputs) = self.attention_inputs();
        // Also scan plain (non-agent) panes for an interactive prompt (#40).
        let (p_out, p_tails, p_ins, agent_ptys) = self.pane_attention_inputs();
        let items = self.attention_scan(
            now, &outputs, &tails, &last_inputs, &p_out, &p_tails, &p_ins, &agent_ptys,
        );
        for id in self.attention_toast_targets(&items) {
            if let Some(i) = items.iter().find(|i| i.agent_id == id) {
                self.audit(&i.group, "loomux", "attention-toast",
                    json!({ "agent": i.agent_id, "reason": i.reason }));
                notify_desktop(&format!("loomux · {}", i.name), &i.detail);
            }
        }
        if let Some(app) = self.app.lock_safe().clone() {
            let _ = app.emit("orch-attention", &items);
        }
    }

    /// Record a spawn against the group's rolling-hour window and report
    /// whether the spawn-rate guardrail is now exceeded. Checks and records
    /// under one lock so concurrent spawns can't both slip past the cap.
    fn check_and_record_spawn(&self, group: &str, limit: u32) -> Result<(), String> {
        let now = now_ms();
        let mut all = self.spawn_times.lock_safe();
        let times = all.entry(group.to_string()).or_default();
        times.retain(|&t| now.saturating_sub(t) < SPAWN_RATE_WINDOW_MS);
        if spawn_rate_exceeded(times, now, limit, SPAWN_RATE_WINDOW_MS) {
            return Err(format!(
                "guardrail: spawn-rate limit reached ({limit} spawns/hour). Wait, or reuse an idle agent instead of spawning a new one."
            ));
        }
        times.push(now);
        Ok(())
    }

    /// Compute an agent's current usage from the best available source, in
    /// preference order: the CLI's own session transcript (token records —
    /// exact, and readable even after the pane is gone) → a last-resort parse
    /// of the dollar figure the CLI prints in its statusline. Returns a
    /// snapshot keyed for durable accumulation (issue #42).
    fn compute_usage_snapshot(&self, entry: &AgentEntry, cli: &str) -> UsageSnapshot {
        let key = entry
            .session_id
            .clone()
            .unwrap_or_else(|| format!("agent:{}", entry.id));
        let role = entry.role.as_str();
        let mut snap = UsageSnapshot {
            key,
            agent_id: entry.id.clone(),
            name: entry.name.clone(),
            role: role.to_string(),
            source: "none".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cost_usd: None,
            estimated: false,
            model: None,
            updated_ms: now_ms(),
        };

        // Primary source: per-session token usage from the transcript. Claude
        // Code writes it; Copilot has no readable token record today (see the
        // `usage` module's design note), so it falls through to the statusline.
        if cli == "claude" {
            if let Some(sid) = entry.session_id.as_deref() {
                // Use the test override when set, else the default ~/.claude root.
                let root = self
                    .claude_projects_dir
                    .lock_safe()
                    .clone()
                    .or_else(crate::usage::default_claude_projects_root);
                if let Some(u) = root
                    .as_deref()
                    .and_then(|r| crate::usage::claude_session_usage_in(r, sid))
                {
                    if u.tokens.total() > 0 {
                        snap.source = "transcript".to_string();
                        snap.input_tokens = u.tokens.input_tokens;
                        snap.output_tokens = u.tokens.output_tokens;
                        snap.cache_creation_tokens = u.tokens.cache_creation_tokens;
                        snap.cache_read_tokens = u.tokens.cache_read_tokens;
                        snap.cost_usd = u.cost_usd;
                        snap.estimated = true; // token-derived dollar estimate
                        snap.model = u.model;
                        return snap;
                    }
                }
            }
        }

        // Last resort: the dollar figure the CLI renders in its own statusline.
        // Unreliable (empty on subscription/Max accounts; gone once the pane is
        // killed), so it only runs when no transcript usage was found.
        if let Some(app) = self.app.lock_safe().clone() {
            if let Some(pty) = entry.pty_id {
                let ptys = app.state::<crate::pty::PtyManager>();
                if let Some(raw) = ptys.output_tail(pty) {
                    if let Some(c) = parse_session_cost(&strip_ansi(&raw)) {
                        snap.source = "statusline".to_string();
                        snap.cost_usd = Some(c); // reported by the CLI, not estimated
                    }
                }
            }
        }
        snap
    }

    fn load_usage_snapshots(&self, group: &str) -> Vec<UsageSnapshot> {
        let path = self.group_dir(group).join("usage.json");
        let Ok(text) = fs::read_to_string(&path) else {
            return Vec::new(); // absent is normal (no usage yet)
        };
        match serde_json::from_str(&text) {
            Ok(list) => list,
            Err(e) => {
                // The file exists but is corrupt (interrupted write, manual
                // edit). Silently treating it as empty would wipe all
                // killed-agent history, so preserve it for inspection and
                // start fresh rather than overwrite it on the next upsert.
                let bad = path.with_extension("json.bad");
                let _ = fs::rename(&path, &bad);
                self.audit(group, "loomux", "usage-corrupt",
                    json!({ "error": e.to_string(), "preserved": bad.to_string_lossy() }));
                Vec::new()
            }
        }
    }

    /// Upsert one agent's snapshot into the group's durable `usage.json`,
    /// matched by `key`. Shares the task-board file lock. Public for the
    /// kill-snapshot accumulation test.
    #[doc(hidden)]
    pub fn upsert_usage_snapshot(&self, group: &str, snap: UsageSnapshot) {
        let _guard = self.tasks_lock.lock_safe();
        let mut list = self.load_usage_snapshots(group);
        match list.iter_mut().find(|s| s.key == snap.key) {
            Some(existing) => {
                // A transcript only ever grows, so a read that comes back empty
                // (e.g. transient failure, or the pane died before Copilot wrote
                // a token record) must not clobber usage we already captured —
                // otherwise a kill could zero a session's spend. Refresh the
                // identity fields but keep the richer usage.
                let new_empty = snap.source == "none"
                    && snap.cost_usd.is_none()
                    && snap.input_tokens
                        + snap.output_tokens
                        + snap.cache_creation_tokens
                        + snap.cache_read_tokens
                        == 0;
                let old_has_data = existing.source != "none"
                    || existing.cost_usd.is_some()
                    || existing.input_tokens
                        + existing.output_tokens
                        + existing.cache_creation_tokens
                        + existing.cache_read_tokens
                        > 0;
                if new_empty && old_has_data {
                    existing.agent_id = snap.agent_id;
                    existing.name = snap.name;
                    existing.role = snap.role;
                    existing.updated_ms = snap.updated_ms;
                } else {
                    *existing = snap;
                }
            }
            None => list.push(snap),
        }
        let dir = self.group_dir(group);
        let _ = fs::create_dir_all(&dir);
        // Crash-safe write: serialize to a temp file, then atomically rename
        // over usage.json. A crash mid-write leaves the old (valid) file or
        // the temp file behind, never a half-written usage.json.
        let path = dir.join("usage.json");
        let tmp = dir.join("usage.json.tmp");
        let body = serde_json::to_string_pretty(&list).unwrap();
        if fs::write(&tmp, &body).is_ok() {
            if fs::rename(&tmp, &path).is_err() {
                // Rename can fail if the destination exists on some platforms;
                // fall back to a direct write so we don't lose the update.
                let _ = fs::write(&path, &body);
                let _ = fs::remove_file(&tmp);
            }
        }
    }

    /// Aggregate the group's usage into one summary with a **live vs lifetime**
    /// split. Live agents' snapshots are refreshed from their transcripts on
    /// each call; killed/recycled agents keep the snapshot captured when they
    /// exited, so the lifetime total never forgets historical spend. Tokens are
    /// exact; dollar figures are estimates (labelled per agent).
    pub fn group_usage(&self, group: &str) -> Value {
        let live_agents: Vec<AgentEntry> = self
            .agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group && a.status != AgentStatus::Dead)
            .cloned()
            .collect();
        // Each agent's CLI is per-role (issue #4), so resolve it per agent.
        // The group-level `cli` in the summary is the group default (workers/
        // reviewers/planners may each run a different one).
        let rails = self.group(group).map(|g| g.guardrails);
        let cli = rails
            .as_ref()
            .map(|g| g.agent_cli.clone())
            .unwrap_or_else(|| "claude".to_string());

        // Refresh each live agent's durable snapshot from its current usage.
        let mut live_keys: HashSet<String> = HashSet::new();
        for a in &live_agents {
            let cli = rails.as_ref().map(|g| g.cli_for(a.role)).unwrap_or("claude");
            let snap = self.compute_usage_snapshot(a, cli);
            live_keys.insert(snap.key.clone());
            self.upsert_usage_snapshot(group, snap);
        }

        // The store now holds live + historical (killed) snapshots.
        let snaps = {
            let _guard = self.tasks_lock.lock_safe();
            self.load_usage_snapshots(group)
        };

        let (mut live_cost, mut lifetime_cost) = (0.0f64, 0.0f64);
        let (mut live_cost_known, mut lifetime_cost_known) = (false, false);
        let (mut live_tokens, mut lifetime_tokens) = (0u64, 0u64);
        // Track whether each total mixes token-estimated and CLI-reported
        // dollars, so we never blend them under one honest label.
        let (mut live_est, mut live_rep) = (false, false);
        let (mut lifetime_est, mut lifetime_rep) = (false, false);
        let mut rows: Vec<Value> = Vec::new();

        for s in &snaps {
            let tokens = s.input_tokens
                + s.output_tokens
                + s.cache_creation_tokens
                + s.cache_read_tokens;
            let live = live_keys.contains(&s.key);
            lifetime_tokens += tokens;
            if let Some(c) = s.cost_usd {
                lifetime_cost += c;
                lifetime_cost_known = true;
                if s.estimated {
                    lifetime_est = true;
                } else {
                    lifetime_rep = true;
                }
            }
            if live {
                live_tokens += tokens;
                if let Some(c) = s.cost_usd {
                    live_cost += c;
                    live_cost_known = true;
                    if s.estimated {
                        live_est = true;
                    } else {
                        live_rep = true;
                    }
                }
            }
            rows.push(json!({
                "id": s.agent_id,
                "name": s.name,
                "role": s.role,
                "live": live,
                "source": s.source,
                "model": s.model,
                "cost_usd": s.cost_usd,
                "estimated": s.estimated,
                "tokens": {
                    "input": s.input_tokens,
                    "output": s.output_tokens,
                    "cache_creation": s.cache_creation_tokens,
                    "cache_read": s.cache_read_tokens,
                    "total": tokens,
                },
            }));
        }
        rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));

        // How to label each dollar total: all token-estimated, all
        // CLI-reported, or a mix. `null` when there is no cost figure.
        let basis = |est: bool, rep: bool| -> Option<&'static str> {
            match (est, rep) {
                (true, true) => Some("mixed"),
                (true, false) => Some("estimated"),
                (false, true) => Some("reported"),
                (false, false) => None,
            }
        };

        json!({
            "group": group,
            "cli": cli,
            "live_cost_usd": live_cost_known.then_some(live_cost),
            "lifetime_cost_usd": lifetime_cost_known.then_some(lifetime_cost),
            "live_cost_basis": basis(live_est, live_rep),
            "lifetime_cost_basis": basis(lifetime_est, lifetime_rep),
            "live_tokens": live_tokens,
            "lifetime_tokens": lifetime_tokens,
            "agents": rows,
            "note": "Tokens come from each agent's session transcript and are exact; dollar figures are estimated from a dated model price table. Subscription/Max accounts have no marginal dollar cost (the CLI statusline shows $0.00), so tokens are the reliable metric. Killed/recycled agents stay in the lifetime total; statusline-parsed dollars are a last-resort fallback.",
        })
    }

    // ---------- lifecycle: group summary & end-orchestration ----------

    /// A one-glance summary of a group's live agents for the lifecycle panel:
    /// how many are up, the role breakdown, and uptime (per agent and for the
    /// group as a whole, measured from the earliest-started live agent — the
    /// orchestrator in practice). Also reports the paused flag so the panel can
    /// compose pause and end-orchestration sanely.
    pub fn group_summary(&self, group: &str) -> Value {
        let now = now_ms();
        let live: Vec<AgentEntry> = self
            .agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group && a.status != AgentStatus::Dead)
            .cloned()
            .collect();
        let (mut orch, mut worker, mut reviewer, mut planner) = (0u32, 0u32, 0u32, 0u32);
        let mut earliest: Option<u64> = None;
        let mut list: Vec<Value> = live
            .iter()
            .map(|a| {
                match a.role {
                    Role::Orchestrator => orch += 1,
                    Role::Worker => worker += 1,
                    Role::Reviewer => reviewer += 1,
                    Role::Planner => planner += 1,
                }
                earliest = Some(earliest.map_or(a.started_ms, |e| e.min(a.started_ms)));
                json!({
                    "id": a.id, "name": a.name, "role": a.role,
                    "task": a.task, "idle_since_ms": a.idle_since_ms,
                    "uptime_ms": now.saturating_sub(a.started_ms),
                })
            })
            .collect();
        list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        json!({
            "group": group,
            "live_agents": live.len(),
            // The current adjustable cap and how many delegates count against
            // it, so the UI can show the stepper's value and warn when a lower
            // cap would (harmlessly) block spawns until attrition. Must match
            // `live_delegate_count` (the value enforcement actually reads):
            // planners count too (#47), so a cap-below-live warning stays honest.
            "max_agents": self.group(group).map(|g| g.guardrails.max_agents),
            "live_delegates": worker + reviewer + planner,
            "paused": self.is_paused(group),
            "uptime_ms": earliest.map(|e| now.saturating_sub(e)),
            "roles": { "orchestrator": orch, "worker": worker, "reviewer": reviewer, "planner": planner },
            "agents": list,
        })
    }

    /// End a whole orchestration: kill every one of the group's agents (the
    /// orchestrator included — unlike `kill_agent`, which protects it), and,
    /// when asked, remove the agents' worktrees. Human-initiated and
    /// destructive: it is a Tauri command only (never an MCP tool an agent
    /// could call on itself), audited as actor `human`, and the frontend
    /// confirms before invoking. Composes with a paused group: killing works
    /// regardless of pause, and the pause marker is cleared so the teardown is
    /// total — a later relaunch on the same repo won't inherit a stale pause.
    pub fn end_group(&self, group: &str, cleanup_worktrees: bool) -> Result<Value, String> {
        // Snapshot every member (all statuses): already-dead workers may still
        // have a worktree on disk that cleanup should reclaim.
        let members: Vec<AgentEntry> = self
            .agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group)
            .cloned()
            .collect();
        if members.is_empty() {
            return Err("no such group (no agents ever registered here)".into());
        }
        let app = self.app.lock_safe().clone();

        // Kill the live ones. Kill the pty (best-effort) then mark the entry
        // dead directly — mark_dead is idempotent against the async pty-exit,
        // and going straight through it avoids the orchestrator-notification
        // path in on_pty_exit (there is no orchestrator left to tell).
        let mut killed = Vec::new();
        for a in &members {
            if a.status == AgentStatus::Dead {
                continue;
            }
            if let (Some(app), Some(pty)) = (app.as_ref(), a.pty_id) {
                app.state::<crate::pty::PtyManager>().kill(pty);
            }
            self.mark_dead(&a.id, None);
            killed.push(a.id.clone());
        }

        // Optionally reclaim the worktrees. Resolve the repo (from memory or
        // group.json) so `git worktree remove` runs from the main checkout.
        let mut worktrees_removed = Vec::new();
        let mut worktree_errors = Vec::new();
        if cleanup_worktrees {
            let repo = self
                .group(group)
                .map(|g| g.repo)
                .or_else(|| self.load_group_file(group).map(|(r, _)| r));
            if let Some(repo) = repo {
                let cwds: Vec<String> = members.iter().map(|a| a.cwd.clone()).collect();
                for path in worktree_cleanup_targets(&repo, &cwds) {
                    match crate::git::git_worktree_remove(&repo, &path) {
                        Ok(()) => worktrees_removed.push(path),
                        Err(e) => worktree_errors.push(json!({ "path": path, "error": e })),
                    }
                }
            }
        }

        // Sweep the steering-strip image attachments (#72): they're only useful
        // while the group's agents are live, so teardown reclaims the scratch
        // dir alongside the worktrees. Best-effort — a leftover screenshot must
        // never block a group from ending. This includes any that were queued
        // but never sent (removed chips / abandoned drafts), so the cheap
        // policy is simply "cleaned up on group end", no per-image bookkeeping.
        let _ = fs::remove_dir_all(self.attachments_dir(group));

        // Total teardown: drop any pause (in-memory + marker) so a future
        // relaunch on this repo starts clean rather than silently paused.
        if self.paused.lock_safe().remove(group) {
            let _ = fs::remove_file(self.group_dir(group).join("paused"));
        }

        self.audit(group, "human", "group-end", json!({
            "killed": killed,
            "cleanup_worktrees": cleanup_worktrees,
            "worktrees_removed": worktrees_removed,
            "worktree_errors": worktree_errors,
        }));

        // Tell the frontend to close the group's (now-dead) panes so the human
        // isn't left ✕-clicking a screen of dead terminals — the very chore
        // this action exists to remove.
        if let Some(app) = app.as_ref() {
            let _ = app.emit("orch-group-ended", json!({ "group_id": group }));
        }

        Ok(json!({
            "group": group,
            "killed": killed,
            "worktrees_removed": worktrees_removed,
            "worktree_errors": worktree_errors,
        }))
    }

    /// Resolve the repo profile that shapes an agent of `role` (issue #51),
    /// applying the precedence: explicit `spawn_agent(profile:)` name > manual
    /// per-role assignment (persisted in group.json) > auto role-mapping
    /// (append-mode files only) > built-in. Re-reads `.github/agents/*.md`
    /// fresh so edits apply to the next spawn.
    fn resolve_profile(
        &self,
        group: &GroupInfo,
        role: Role,
        explicit: Option<&str>,
    ) -> Result<Option<profiles::AgentProfile>, String> {
        let all = profiles::discover_profiles(&group.repo);
        // 1. Explicit named profile — the orchestrator's deliberate choice.
        if let Some(name) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
            return match profiles::find_named(&all, name) {
                Some(p) => Ok(Some(p.clone())),
                None => {
                    let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
                    Err(format!(
                        "unknown profile {name:?}. Available profiles: {}",
                        if names.is_empty() {
                            "none — define <repo>/.github/agents/<name>.md".to_string()
                        } else {
                            names.join(", ")
                        }
                    ))
                }
            };
        }
        // 2. Manual per-role assignment from the launcher.
        match group.guardrails.profile_for(role).trim() {
            "" => {} // fall through to auto
            "none" | "built-in" => return Ok(None),
            name => {
                if let Some(p) = profiles::find_named(&all, name) {
                    return Ok(Some(p.clone()));
                }
                // The pinned file was removed; fall back to auto rather than
                // failing the spawn, and note it.
                self.audit(&group.id, "loomux", "warning", json!({
                    "what": "assigned profile not found — falling back to auto/built-in",
                    "role": role.as_str(), "profile": name,
                }));
            }
        }
        // 3. Auto role-mapping — append-mode files only (replace never auto-applies).
        Ok(profiles::profile_for_role(&all, role).cloned())
    }

    /// Render a profile's persona to a per-agent brief and return its path. The
    /// file name encodes the mode (`<id>.replace.md` vs `<id>.md`) so
    /// `kickoff_prompt` honors replace without threading state through the bind
    /// thread. For a replace-mode profile, loomux also (idempotently) writes the
    /// role's non-overridable **mechanics core** — the persona body is replaced,
    /// but the functional contract (MCP tools, board, report(), git flow) is
    /// still guaranteed by loomux, not the repo file (issue #51).
    fn write_profile_brief(
        &self,
        group_id: &str,
        agent_id: &str,
        repo: &str,
        p: &profiles::AgentProfile,
    ) -> Result<PathBuf, String> {
        let dir = self.group_dir(group_id).join("profiles");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let vars = [("REPO", repo), ("GROUP_ID", group_id)];
        let fname = match p.mode {
            profiles::ProfileMode::Replace => format!("{agent_id}.replace.md"),
            profiles::ProfileMode::Append => format!("{agent_id}.md"),
        };
        let path = dir.join(fname);
        fs::write(&path, render_template(&p.instructions, &vars)).map_err(|e| e.to_string())?;
        if p.mode == profiles::ProfileMode::Replace {
            let mech = self.group_dir(group_id).join(format!("{}.mechanics.md", p.role.as_str()));
            fs::write(&mech, mechanics_core(p.role)).map_err(|e| e.to_string())?;
        }
        Ok(path)
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
            ("PLANNER_MODEL", g.guardrails.planner_model.as_str()),
        ];
        let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, *v)).collect();
        let dir = self.group_dir(&g.id);
        for role in [Role::Orchestrator, Role::Worker, Role::Reviewer, Role::Planner] {
            fs::write(dir.join(role.instructions_file()), render_template(role.template(), &vars))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn resolve_token(&self, token: &str) -> Option<Caller> {
        let id = self.by_token.lock_safe().get(token).cloned()?;
        let agents = self.agents.lock_safe();
        let a = agents.get(&id)?;
        if a.status == AgentStatus::Dead {
            return None;
        }
        Some(Caller { agent_id: a.id.clone(), group: a.group.clone(), role: a.role })
    }

    pub fn agent(&self, id: &str) -> Option<AgentEntry> {
        self.agents.lock_safe().get(id).cloned()
    }

    fn live_delegate_count(&self, group: &str) -> u32 {
        self.agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group && a.role != Role::Orchestrator && a.status != AgentStatus::Dead)
            .count() as u32
    }

    /// Servers from the repo's standard `.mcp.json` (the agent's workdir /
    /// checkout first, then the group repo), for merging into a **Claude**
    /// agent's per-agent config: `--strict-mcp-config` (required for group
    /// isolation) suppresses Claude's native `.mcp.json` loading, so without
    /// this a repo's declared MCP servers would never reach the agent. A
    /// malformed or unreadable file is audited and skipped rather than
    /// blocking the spawn.
    ///
    /// SECURITY (issue #51): an `.mcp.json` `stdio` entry is an arbitrary
    /// `command` + `args` that loomux would launch — local code execution on
    /// the operator's machine, with no per-call human approval under
    /// `auto_ops`. Callers therefore only invoke this when the group's
    /// `trust_repo_mcp` toggle is on. The reserved `loomux` identity entry is
    /// stripped here as well, so a repo server named `loomux` can never shadow
    /// it (defence in depth alongside `write_mcp_config`).
    fn repo_mcp_servers(
        &self,
        group: &str,
        workdir: &str,
        repo: &str,
    ) -> Option<serde_json::Map<String, Value>> {
        let path = [workdir, repo]
            .iter()
            .map(|d| Path::new(d).join(".mcp.json"))
            .find(|p| p.is_file())?;
        let parsed = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("mcpServers").and_then(Value::as_object).cloned());
        match parsed {
            Some(mut servers) => {
                servers.remove("loomux");
                Some(servers)
            }
            None => {
                self.audit(group, "loomux", "warning", json!({
                    "what": ".mcp.json unreadable — repo MCP servers skipped",
                    "path": path.display().to_string(),
                }));
                None
            }
        }
    }

    /// Write the per-agent MCP config the agent CLI connects with. Claude
    /// and Copilot share the same core schema; Copilot additionally expects
    /// a `tools` allowlist inside the server entry. `extra_servers` (repo
    /// `.mcp.json`, only when the group trusts it — see `repo_mcp_servers`)
    /// ride in the same config; the reserved `loomux` identity entry always
    /// wins and can never be shadowed.
    fn write_mcp_config(
        &self,
        group: &str,
        agent_id: &str,
        token: &str,
        cli: &str,
        extra_servers: Option<&serde_json::Map<String, Value>>,
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
        let mut servers = serde_json::Map::new();
        if let Some(extra) = extra_servers {
            for (k, v) in extra {
                if k != "loomux" {
                    servers.insert(k.clone(), v.clone());
                }
            }
        }
        // Inserted last so the loomux identity entry always wins on a clash.
        servers.insert("loomux".into(), server);
        let cfg = json!({ "mcpServers": servers });
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
    /// everything else still asks the human. A `read_only` planner is
    /// *always* treated as unattended (Auto perms + git/gh allowlist)
    /// regardless of `auto_ops`: it never mutates and has no human in its
    /// pane, so gating it would only deadlock it (see below).
    ///
    /// `read_only` hardens the planner contract at the CLI level (#47): where
    /// the CLI supports tool denial, the file-editing tools and the git
    /// mutation subcommands (`commit`/`push`) are denied outright, so a planner
    /// cannot write code or create branches/commits/pushes even under Auto
    /// perms — while `gh` stays available so it can still post its plan as an
    /// issue comment. Deny rules take precedence over the allow list on both
    /// CLIs. NOTE: this is a real, structural denial for the write/commit/push
    /// surface; it is deliberately NOT a full sandbox (e.g. `gh pr create` is
    /// left reachable so the plan comment works), so the *complete* read-only
    /// contract still rests partly on the planner's instructions.
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
        read_only: bool,
        inject: &ProfileInject,
    ) -> String {
        // A planner never mutates and has no human in its pane, so there is
        // nothing for `auto_ops` to gate: it must explore, post its plan
        // comment, and report with zero prompts, or it would stall waiting on
        // an approval no one is there to give. So a planner (`read_only`)
        // always runs unattended on BOTH CLIs; workers/reviewers follow the
        // group's `auto_ops`. (This is also why claude's `plan` permission
        // mode / copilot's `--plan` can't be used here — both hold the plan
        // for interactive human sign-off.)
        let unattended = auto_ops || read_only;
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
                if unattended {
                    // Copilot's own unattended mode: autopilot + all tools
                    // + no path-verification prompts. A planner (read_only)
                    // always takes this path even in a non-auto_ops group —
                    // interactive mode would stall it on a human that isn't
                    // there; the deny rules below keep it read-only, and deny
                    // takes precedence over --allow-all-tools in Copilot.
                    cmd.push_str(" --autopilot --allow-all-tools --allow-all-paths");
                } else {
                    cmd.push_str(" --allow-tool \"shell(git:*)\" --allow-tool \"shell(gh:*)\"");
                }
                if read_only {
                    // Deny file writes and git mutations even under
                    // --allow-all-tools (deny takes precedence in Copilot).
                    // `write`/`edit` are Copilot's file-modification tools;
                    // `gh` is left allowed so the plan comment can be posted.
                    cmd.push_str(
                        " --deny-tool \"write\" --deny-tool \"edit\" \
                         --deny-tool \"shell(git commit)\" --deny-tool \"shell(git push)\"",
                    );
                }
                // Repo persona: engage Copilot's native custom agent (issue
                // #51). `--agent <name>` resolves the same .github/agents file
                // and applies its instructions AND its `mcp-servers` scopes —
                // which is why the caller only sets `copilot_agent` when the
                // group trusts the repo (those `mcp-servers` are code exec).
                // When untrusted, the persona still reaches the agent as text
                // via the kickoff-referenced brief; only the native --agent is
                // withheld.
                if let Some(agent) = inject.copilot_agent {
                    cmd.push_str(&format!(" --agent {agent}"));
                }
                for entry in inject.extra_allow {
                    cmd.push_str(&format!(" --allow-tool \"{entry}\""));
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
                // (what the human uses interactively); otherwise acceptEdits.
                // A planner (`read_only`) is always `unattended` (see above),
                // so it runs under Auto perms even in a non-auto_ops group.
                let perm = if unattended { "auto" } else { "acceptEdits" };
                let mut cmd = format!(
                    "claude {session_flag}--mcp-config \"{}\" --strict-mcp-config --model {model} \
                     --permission-mode {perm} --add-dir \"{}\" --allowedTools mcp__loomux",
                    cfg.display(),
                    group_dir.display()
                );
                if unattended {
                    // Pre-approve git + gh so the unattended flow runs without
                    // prompts (workers: branch→commit→PR; planners: read-only
                    // explore + `gh issue comment` for the plan). `Bash(git *)`
                    // matches every git subcommand; a planner's denials below
                    // carve commit/push back out.
                    cmd.push_str(" \"Bash(git *)\" \"Bash(gh *)\"");
                }
                // Profile-declared extra tool patterns join the SAME
                // --allowedTools list (issue #51): a second --allowedTools flag
                // would replace the first and silently drop the loomux MCP
                // approval. These land before --disallowedTools, which still
                // overrides them for a read-only planner.
                for entry in inject.extra_allow {
                    cmd.push_str(&format!(" \"{entry}\""));
                }
                if read_only {
                    // Deny the file-editing tools and the git mutation
                    // subcommands outright (--disallowedTools overrides the
                    // permission mode AND the allow list in Claude Code), so a
                    // planner can't write code or commit/push. `gh` (incl.
                    // `gh issue comment`) stays reachable for the plan comment.
                    //
                    // Spelling matters. `:*` is a valid wildcard only as a
                    // TRAILING suffix (`Bash(gh:*)` is fine); a colon in the
                    // MIDDLE of the command (`Bash(git commit:*)`) is not —
                    // Claude Code discards that rule as malformed AND prints a
                    // startup warning, the "auto deny rule" flash a human
                    // caught on planner boot. So the enforcing denial rests on
                    // the space form `Bash(git commit *)`: it is the canonical
                    // spelling and actually blocks commit/push, with no
                    // warning. (An earlier draft passed both spellings; the
                    // colon-mid one added nothing but the warning.)
                    cmd.push_str(
                        " --disallowedTools Edit Write MultiEdit NotebookEdit \
                         \"Bash(git commit *)\" \"Bash(git push *)\"",
                    );
                }
                // Repo persona: inject the profile's instructions as the
                // agent's appended system prompt (issue #51). Claude has no
                // native `.github/agents` persona-by-name, so this is how the
                // repo addendum reaches it — stronger than a kickoff mention,
                // and it *appends to* Claude's built-in system prompt (base +
                // repo additions). Text only, so it's not gated by trust.
                if let Some(f) = inject.system_prompt_file {
                    cmd.push_str(&format!(" --append-system-prompt-file \"{}\"", f.display()));
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
        self.spawn_agent_ex(group_id, role, name, task, use_worktree, branch, None, None, None, None)
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
        profile: Option<String>,
        resume_session: Option<String>,
        cwd_override: Option<String>,
        restore_name_source: Option<NameSource>,
    ) -> Result<AgentEntry, String> {
        let group = self.group(group_id).ok_or("unknown group")?;

        // Resolve the repo profile that shapes this agent (issue #51): explicit
        // spawn_agent(profile:) name > manual per-role assignment > auto
        // role-mapping (append-only) > built-in. Re-read fresh per spawn.
        let profile = self.resolve_profile(&group, role, profile.as_deref())?;
        // A named/assigned profile may retarget the role (its `role`/`kind`
        // mapping); an auto role-addendum already matches `role` by construction.
        let role = profile.as_ref().map(|p| p.role).unwrap_or(role);

        // Guardrail: live delegate cap (the orchestrator itself is exempt).
        if role != Role::Orchestrator {
            let live = self.live_delegate_count(group_id);
            if live >= group.guardrails.max_agents {
                return Err(format!(
                    "guardrail: {live} live agents already (max {}). Reuse an idle agent or kill one first.",
                    group.guardrails.max_agents
                ));
            }
            // Guardrail: spawn-rate backstop against a runaway orchestrator.
            // Checked (and the timestamp recorded only when the check passes)
            // before any pane/worktree work so a burst fast-fails. A refused
            // spawn is not counted; one admitted here but later aborted
            // (worktree/bind failure) still counts toward the hour.
            self.check_and_record_spawn(group_id, group.guardrails.max_spawns_per_hour)?;
        }

        // Guardrail: the CLI and model are pinned per role at group creation
        // (issue #4). Reject an unknown per-role CLI at spawn rather than
        // silently downgrading it — the launcher only offers supported CLIs,
        // so an unsupported one here means a hand-edited group.json.
        let cli = group.guardrails.cli_for(role);
        if !SUPPORTED_CLIS.contains(&cli) {
            return Err(format!(
                "guardrail: unsupported agent CLI {cli:?} for role {} — supported: {}",
                role.as_str(), SUPPORTED_CLIS.join(", ")
            ));
        }
        let cli = cli.to_string();
        // The model is pinned per role (issue #4); a profile may override it
        // (issue #51), sanitized like everything shell-bound. `profile_model`
        // is owned so it outlives this borrow.
        let profile_model = profile
            .as_ref()
            .and_then(|p| p.model.as_deref())
            .map(|m| sanitize_model(m, group.guardrails.model_for(role)));
        let model = profile_model.as_deref().unwrap_or(group.guardrails.model_for(role));

        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let agent_id = format!("{}-{seq}", role.prefix());
        let token = new_token();
        // Name precedence (#95r): a caller-supplied name is the orchestrator's
        // choice; an empty one means "no meaningful name", so we derive the
        // default from the minted id — "worker 2" for `w-2` — which agrees with
        // the pane's "W 2" badge (#75) and the roster id instead of the old
        // per-launch "worker N" counter that drifted from the seq.
        let (display, derived_source) = {
            let cleaned = sanitize_agent_name(name);
            if cleaned.is_empty() {
                (format!("{} {seq}", role.as_str()), NameSource::Default)
            } else {
                (cleaned, NameSource::Orchestrator)
            }
        };
        // A session rejoin re-spawns with the roster name (non-empty, so the
        // derived tier would be `Orchestrator`); `restore_name_source` carries
        // the persisted tier instead, so a human-renamed pane comes back at the
        // `Human` tier and a later `rename_agent` still cannot clobber it.
        let name_source = restore_name_source.unwrap_or(derived_source);

        // Workspace: dedicated worktree (branch of the same name) or the repo
        // itself, where the worker is instructed to branch before touching
        // anything.
        // Session identity: resumes reuse the given id; fresh Claude agents
        // get a pre-assigned UUID so their session is resumable later.
        let resume = resume_session.is_some();
        let session_id = match resume_session {
            Some(s) => Some(sanitize_session(&s).ok_or("invalid resume session id")?),
            None => (cli == "claude").then(new_session_uuid),
        };

        // Copilot mints its own session id after boot (no `--session-id`), so
        // snapshot the sessions that already exist now, before this pane's
        // copilot starts — the watcher then identifies the newly appeared one.
        let copilot_baseline = (!resume && cli == "copilot")
            .then(|| {
                crate::sessions::copilot_session_state_root()
                    .map(|root| crate::sessions::copilot_session_ids(&root))
                    .unwrap_or_default()
            });

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
        } else if use_worktree && role != Role::Orchestrator && role != Role::Planner {
            let wt = crate::git::git_worktree_add(group.repo.clone(), branch_name.clone())?;
            (wt.clone(), format!(
                "Your working directory is a dedicated git worktree at {wt} already checked out on branch '{branch_name}'."
            ))
        } else if role == Role::Orchestrator {
            (group.repo.clone(), String::new())
        } else if role == Role::Reviewer {
            (group.repo.clone(), "You review; you do not create branches or push. Inspect PRs via gh (checking out the PR branch locally is fine).".to_string())
        } else if role == Role::Planner {
            // Planners explore read-only in the repo itself and never branch,
            // worktree, commit, or PR — so a worktree is never created for
            // them even if `use_worktree` was set (the CLI-level write/commit
            // denials in `build_agent_command` back this note structurally).
            (group.repo.clone(), PLANNER_READONLY_NOTE.to_string())
        } else {
            (group.repo.clone(), format!(
                "Work in the repo itself; create branch '{branch_name}' off the default branch before changing anything. Never commit to the default branch."
            ))
        };

        if cli == "copilot" {
            pre_trust_copilot_folder(&cwd);
        }

        // Repo-profile extras (issue #51). The rendered instructions brief is
        // written per-agent (keyed by agent id) so it survives concurrent
        // spawns and the kickoff can point at it; on Claude it becomes the
        // appended system prompt. Repo MCP servers + the Copilot native
        // `--agent` are BOTH gated on `trust_repo_mcp` — they are the local
        // code-execution surface.
        let trust = group.guardrails.trust_repo_mcp;
        let profile_brief: Option<PathBuf> = match &profile {
            Some(p) => Some(self.write_profile_brief(group_id, &agent_id, &group.repo, p)?),
            None => None,
        };
        // Claude needs the repo's `.mcp.json` merged (strict mode suppresses
        // native loading); Copilot reads its own config, so no merge — and
        // only when the repo is trusted.
        let extra_servers = if cli == "claude" && trust {
            self.repo_mcp_servers(group_id, &cwd, &group.repo)
        } else {
            None
        };
        let cfg = self.write_mcp_config(group_id, &agent_id, &token, &cli, extra_servers.as_ref())?;

        let empty_allow: Vec<String> = Vec::new();
        let command = self.build_agent_command(
            &cli,
            model,
            group.guardrails.auto_ops,
            &cfg,
            &self.group_dir(group_id),
            Path::new(&cwd),
            session_id.as_deref(),
            resume,
            role == Role::Planner, // read_only: deny writes/commits at the CLI level
            &ProfileInject {
                extra_allow: profile.as_ref().map(|p| p.allow.as_slice()).unwrap_or(&empty_allow),
                system_prompt_file: profile_brief.as_deref(),
                // Native Copilot persona (pulls the file's mcp-servers) only
                // when trusted; untrusted, the brief still reaches it as text.
                copilot_agent: profile
                    .as_ref()
                    .filter(|_| trust)
                    .and_then(|p| p.copilot_agent.as_deref()),
            },
        );

        let entry = AgentEntry {
            id: agent_id.clone(),
            group: group_id.to_string(),
            name: display.clone(),
            name_source,
            role,
            token: token.clone(),
            status: AgentStatus::Starting,
            pty_id: None,
            task: task.to_string(),
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            // An agent spawned without a task starts the idle clock; one
            // given work does not (the orchestrator is exempt regardless).
            idle_since_ms: (role != Role::Orchestrator && task.trim().is_empty()).then(now_ms),
            started_ms: now_ms(),
            last_progress_ms: now_ms(),
            last_output_total: 0,
            watchdog_notified: false,
        };
        {
            // Re-check the cap under the same lock as the insert: the early
            // check above fast-fails before worktree creation, but only this
            // one is race-free against concurrent spawns.
            let mut agents = self.agents.lock_safe();
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
        self.by_token.lock_safe().insert(token, agent_id.clone());
        self.persist_agent_record(&entry, "running");
        self.audit(group_id, "loomux", "agent-spawn", json!({
            "agent": agent_id, "role": role, "name": display, "cwd": cwd,
            "cli": cli, "model": model, "worktree": use_worktree, "branch": branch_name, "task": task,
            "session": session_id, "resume": resume,
            // issue #51: which repo profile shaped this agent, its mode
            // (append|replace), and whether the repo's MCP/code-exec surface
            // was trusted for it.
            "profile": profile.as_ref().map(|p| p.name.clone()),
            "profile_mode": profile.as_ref().map(|p| p.mode.as_str()),
            "trust_repo_mcp": group.guardrails.trust_repo_mcp,
        }));
        // Breadcrumb (no prompt/task text): ids + role only.
        crate::obs::breadcrumb(
            "agent-spawn",
            &format!("group={group_id} agent={agent_id} role={role:?} worktree={use_worktree}"),
        );

        let request = SpawnRequest {
            group_id: group_id.to_string(),
            agent_id: agent_id.clone(),
            role,
            name: display,
            cwd: cwd.clone(),
            command,
        };

        let app = self.app.lock_safe().clone();
        let Some(app) = app else {
            // Test mode: no frontend. Mark running so guardrail/authz logic
            // can be exercised without panes. Handle a vanished entry (a
            // concurrent reap between insert and here) instead of unwrapping —
            // a panic here would fire while holding the agents lock.
            if let Some(a) = self.agents.lock_safe().get_mut(&agent_id) {
                a.status = AgentStatus::Running;
            }
            return self
                .agent(&agent_id)
                .ok_or_else(|| "agent vanished during spawn".to_string());
        };

        let (tx, rx) = mpsc::channel::<u32>();
        self.pending_binds.lock_safe().insert(agent_id.clone(), tx);
        app.emit("orch-spawn-request", &request).map_err(|e| e.to_string())?;

        match rx.recv_timeout(BIND_TIMEOUT) {
            Ok(pty_id) => {
                {
                    let mut agents = self.agents.lock_safe();
                    if let Some(a) = agents.get_mut(&agent_id) {
                        a.status = AgentStatus::Running;
                        a.pty_id = Some(pty_id);
                    }
                }
                self.by_pty.lock_safe().insert(pty_id, agent_id.clone());
                self.audit(group_id, "loomux", "agent-bind", json!({ "agent": agent_id, "pty": pty_id }));
                crate::obs::breadcrumb("agent-bind", &format!("agent={agent_id} pty={pty_id}"));
                if resume {
                    // Resumed sessions already have their role and history;
                    // deliver only the follow-up (if any) instead of the
                    // full kickoff.
                    if !task.trim().is_empty() {
                        self.deliver_prompt(&agent_id, task, "loomux", true)?;
                    }
                } else {
                    let a = self
                        .agent(&agent_id)
                        .ok_or("agent vanished during spawn")?;
                    let kickoff = self.kickoff_prompt(&a, &group, &branch_note);
                    self.deliver_prompt(&agent_id, &kickoff, "loomux", true)?;
                }
                // Copilot minted a session as it booted; watch for it and bind
                // its id to this pane's roster record so the session becomes
                // resumable and shows in the session browser. Needs an owned
                // registry (background thread) — a no-op in unit tests, which
                // don't set the self-arc.
                if let Some(baseline) = copilot_baseline {
                    if let Some(reg) = self.arc() {
                        reg.spawn_copilot_session_watcher(
                            agent_id.clone(),
                            group_id.to_string(),
                            cwd.clone(),
                            baseline,
                        );
                    }
                }
                self.agent(&agent_id)
                    .ok_or_else(|| "agent vanished during spawn".into())
            }
            Err(_) => {
                self.pending_binds.lock_safe().remove(&agent_id);
                self.mark_dead(&agent_id, None);
                Err("frontend did not open the agent pane in time".into())
            }
        }
    }

    #[doc(hidden)] // pub for integration tests
    pub fn kickoff_prompt(&self, a: &AgentEntry, g: &GroupInfo, branch_note: &str) -> String {
        // Which file the agent reads as its ROLE INSTRUCTIONS, and any note
        // about a profile (issue #51). A per-agent brief written at spawn
        // encodes the mode in its name: `<id>.replace.md` (replace) vs
        // `<id>.md` (append).
        //   - replace: the persona body IS the role instructions; loomux's
        //     non-overridable mechanics core is delivered alongside, so the
        //     functional contract (MCP tools, board, report(), git/PR flow)
        //     holds even though the built-in role body is gone.
        //   - append: the built-in `<role>.md` is the base; the brief is an
        //     addendum read on top of it.
        let profiles_dir = self.group_dir(&g.id).join("profiles");
        let replace_brief = profiles_dir.join(format!("{}.replace.md", a.id));
        let append_brief = profiles_dir.join(format!("{}.md", a.id));
        let (instructions, brief_note) = if replace_brief.is_file() {
            let mechanics = self.group_dir(&g.id).join(format!("{}.mechanics.md", a.role.as_str()));
            (
                replace_brief.clone(),
                format!(
                    "\nThat file is this repo's REPLACE-mode role instructions — your \
                     personality/policy fully replaces loomux's built-in role body. loomux's \
                     functional mechanics are NON-overridable and are specified separately; read \
                     and obey them too: {}",
                    mechanics.display()
                ),
            )
        } else {
            let built_in = self.group_dir(&g.id).join(a.role.instructions_file());
            let note = if append_brief.is_file() {
                format!(
                    "\nThis repo also defines a custom profile for your role — read it and treat \
                     it as an addendum to (not a replacement for) your role instructions above: {}",
                    append_brief.display()
                )
            } else {
                String::new()
            };
            (built_in, note)
        };
        match a.role {
            Role::Orchestrator => {
                // Surface the repo's available profiles so the orchestrator can
                // spawn a named persona with spawn_agent(profile: "<name>").
                let repo_profiles = profiles::discover_profiles(&g.repo);
                let profile_note = if repo_profiles.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\nCustom agent profiles defined in this repo (.github/agents), spawn a named one with \
                         spawn_agent(profile: \"<name>\"); a role's addendum also auto-applies to plain spawns of that role: {}.",
                        repo_profiles
                            .iter()
                            .map(|p| format!(
                                "{} [{}] — {}",
                                p.name,
                                p.role.as_str(),
                                if p.description.is_empty() { "(no description)" } else { &p.description }
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                };
                format!(
                    "You are the orchestrator of loomux agent group {gid} for the repository {repo}.\n\
                     First read your role instructions: {ins}{brief_note}\n\
                     Guardrails (enforced by loomux): max {max} live agents, worker model {wm}, reviewer model {rm}, planner model {pm}.{profile_note}\n\
                     Start by calling get_state, run `gh issue list --label agent-managed --state open`, call list_agents, \
                     reconcile them, then give the human a short status summary and wait for direction.",
                    gid = g.id, repo = g.repo, ins = instructions.display(),
                    max = g.guardrails.max_agents, wm = g.guardrails.worker_model,
                    rm = g.guardrails.reviewer_model, pm = g.guardrails.planner_model,
                )
            }
            Role::Worker | Role::Reviewer | Role::Planner => {
                let head = format!(
                    "You are \"{name}\" ({id}), a {role} agent in loomux group {gid} for repository {repo}.\n\
                     First read your role instructions: {ins}{brief_note}\n{note}",
                    name = a.name, id = a.id, role = a.role.as_str(),
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
        // Pause guardrail: while a group is paused, loomux delivers nothing
        // to its panes so agents finish their turn and idle out. The attempt
        // is audited (nothing is silently lost from the record) and reported
        // as success so callers don't error or retry.
        if self.is_paused(&a.group) {
            self.audit(&a.group, from, "prompt-suppressed-paused", json!({ "to": agent_id, "text": text }));
            return Ok(());
        }
        let pty_id = a.pty_id.ok_or("agent has no terminal yet")?;
        let app = self.app.lock_safe().clone().ok_or("no app handle")?;
        self.audit(&a.group, from, "prompt", json!({ "to": agent_id, "text": text }));

        let paste = bracketed_paste(text);
        let lock = self
            .delivery
            .lock_safe()
            .entry(pty_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let (root, group, agent) = (self.root.clone(), a.group.clone(), a.id.clone());
        std::thread::spawn(move || {
            let _guard = lock.lock_safe();
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

            // Human-typing backstop (#43, option A): if a human is typing
            // directly in this pane, hold the paste until they go quiet so a
            // report can't land inside their half-typed line. Capped so a long
            // compose session can't starve the queue.
            if let Some(held_ms) = wait_for_user_quiet(&ptys, pty_id) {
                append_audit(&root, &group, "loomux", "delivery-held-for-user", json!({
                    "to": agent, "stage": "pre-paste", "held_ms": held_ms,
                    "capped": held_ms >= USER_QUIET_MAX_HOLD.as_millis() as u64,
                }));
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
            // Re-check right before the first Enter: the human may have
            // started typing during the quiet-wait above, and a blind Enter
            // would submit their line. Hold again until they're quiet (#43).
            if let Some(held_ms) = wait_for_user_quiet(&ptys, pty_id) {
                append_audit(&root, &group, "loomux", "delivery-held-for-user", json!({
                    "to": agent, "stage": "pre-enter", "held_ms": held_ms,
                    "capped": held_ms >= USER_QUIET_MAX_HOLD.as_millis() as u64,
                }));
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
            // Delivery outcome breadcrumb — timing + flags only, never the text.
            crate::obs::breadcrumb(
                "delivery",
                &format!(
                    "agent={agent} pty={pty_id} outcome=typed echoed={echoed} attempts={attempts} waited_ms={}",
                    start.elapsed().as_millis() as u64
                ),
            );
        });
        Ok(())
    }

    /// Human steering from the loomux compose strip (#43, option C): enqueue
    /// `text` to the group's orchestrator through the SAME per-pane serialized
    /// delivery path worker reports use. Rejects empty text and a paused group
    /// up front so the strip can tell the human why nothing was sent — a paused
    /// group's delivery is silently suppressed, so without this guard a steered
    /// message would vanish with no feedback. A dead/absent orchestrator
    /// surfaces as the "no live orchestrator" error from delivery.
    #[doc(hidden)] // pub for integration tests
    pub fn steer_orchestrator(&self, group: &str, text: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("empty steering message".into());
        }
        if self.is_paused(group) {
            return Err("group is paused — resume it before steering".into());
        }
        self.deliver_to_orchestrator(group, text, "human")
    }

    /// Deliver to the group's orchestrator (worker reports, exit notices).
    pub fn deliver_to_orchestrator(&self, group: &str, text: &str, from: &str) -> Result<(), String> {
        let orch = self
            .agents
            .lock_safe()
            .values()
            .find(|a| a.group == group && a.role == Role::Orchestrator && a.status != AgentStatus::Dead)
            .map(|a| a.id.clone())
            .ok_or("no live orchestrator in this group")?;
        self.deliver_prompt(&orch, text, from, false)
    }

    pub fn list_agents(&self, group: &str) -> Value {
        let agents = self.agents.lock_safe();
        let mut list: Vec<Value> = agents
            .values()
            .filter(|a| a.group == group)
            .map(|a| json!({
                "id": a.id, "name": a.name, "role": a.role,
                "status": a.status, "task": a.task,
                "session": a.session_id, "cwd": a.cwd,
                "idle_since_ms": a.idle_since_ms,
            }))
            .collect();
        list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        json!(list)
    }

    pub fn agent_output_tail(&self, agent_id: &str, lines: usize) -> Result<String, String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        let pty_id = a.pty_id.ok_or("agent has no terminal")?;
        let app = self.app.lock_safe().clone().ok_or("no app handle")?;
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
        let app = self.app.lock_safe().clone().ok_or("no app handle")?;
        if let Some(pty) = a.pty_id {
            app.state::<crate::pty::PtyManager>().kill(pty);
        }
        self.audit(&a.group, "loomux", "agent-kill", json!({ "agent": agent_id }));
        Ok(())
    }

    pub fn focus_agent(&self, agent_id: &str) -> Result<(), String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        let app = self.app.lock_safe().clone().ok_or("no app handle")?;
        app.emit("orch-focus", json!({ "agent_id": agent_id, "pty_id": a.pty_id }))
            .map_err(|e| e.to_string())
    }

    /// Rename an agent's pane title and durable roster entry, respecting the
    /// name-source precedence ladder (#95r): the rename applies only when
    /// `source` ranks at least as high as whoever set the current name, so a
    /// human rename (highest) is never overwritten by the orchestrator's
    /// `rename_agent` (middle) or the id-derived default (lowest); the
    /// orchestrator can still relabel an id-default or its own earlier name.
    /// Rejects a dead/unknown target. On success the pane title follows via an
    /// `orch-rename` event, the roster is updated, the change is audited, and
    /// the applied (trimmed/truncated) name is returned. Caller scopes the
    /// target to its group (see the MCP `rename_agent` tool).
    pub fn rename_agent(&self, agent_id: &str, name: &str, source: NameSource) -> Result<String, String> {
        let name = sanitize_agent_name(name);
        if name.is_empty() {
            return Err("name must not be empty".into());
        }
        let entry = {
            let mut agents = self.agents.lock_safe();
            let a = agents.get_mut(agent_id).ok_or("unknown agent")?;
            if a.status == AgentStatus::Dead {
                return Err("agent is not alive".into());
            }
            if source.rank() < a.name_source.rank() {
                // Only the orchestrator-vs-human case reaches here in practice.
                return Err(format!(
                    "not overriding {agent_id}: its name \"{}\" was set by the human and takes precedence",
                    a.name
                ));
            }
            a.name = name.clone();
            a.name_source = source;
            a.clone()
        };
        self.persist_agent_record(&entry, "running");
        if let Some(app) = self.app.lock_safe().clone() {
            let _ = app.emit(
                "orch-rename",
                json!({ "agent_id": entry.id, "pty_id": entry.pty_id, "name": name }),
            );
        }
        self.audit(&entry.group, "loomux", "agent-rename",
            json!({ "agent": agent_id, "name": name, "source": source.as_str() }));
        Ok(name)
    }

    #[doc(hidden)] // pub for integration tests
    pub fn mark_dead(&self, agent_id: &str, exit_code: Option<u32>) -> Option<AgentEntry> {
        let mut agents = self.agents.lock_safe();
        let a = agents.get_mut(agent_id)?;
        if a.status == AgentStatus::Dead {
            return None;
        }
        a.status = AgentStatus::Dead;
        let snapshot = a.clone();
        drop(agents);
        self.by_token.lock_safe().remove(&snapshot.token);
        if let Some(p) = snapshot.pty_id {
            self.by_pty.lock_safe().remove(&p);
            self.delivery.lock_safe().remove(&p);
        }
        // Attention bookkeeping is per-live-agent; drop this one's entries.
        self.attn_reports.lock_safe().remove(agent_id);
        self.attn_quiet.lock_safe().remove(agent_id);
        self.attn_waiting_ack.lock_safe().remove(agent_id);
        self.attn_emitted.lock_safe().remove(agent_id);
        let _ = fs::remove_file(
            self.group_dir(&snapshot.group).join("configs").join(format!("{agent_id}.json")),
        );
        self.audit(&snapshot.group, "loomux", "agent-exit",
            json!({ "agent": agent_id, "exit_code": exit_code }));
        crate::obs::breadcrumb(
            "agent-dead",
            &format!("agent={agent_id} pty={:?} code={exit_code:?}", snapshot.pty_id),
        );
        self.persist_agent_record(&snapshot, "dead");
        // Durably capture final usage before the pane is fully torn down, so a
        // recycled/killed agent still counts toward the group's lifetime total
        // (issue #42). The transcript remains readable after exit; the
        // statusline does not, but token usage is the source we rely on.
        let cli = self
            .group(&snapshot.group)
            .map(|g| g.guardrails.cli_for(snapshot.role).to_string())
            .unwrap_or_else(|| "claude".to_string());
        let usage = self.compute_usage_snapshot(&snapshot, &cli);
        self.upsert_usage_snapshot(&snapshot.group, usage);
        Some(snapshot)
    }

    /// Called from the pty waiter thread when any pty exits. No-op for ptys
    /// that aren't orchestration agents.
    pub fn on_pty_exit(&self, pty_id: u32, exit_code: Option<u32>) {
        let agent_id = match self.by_pty.lock_safe().get(&pty_id).cloned() {
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
            .lock_safe()
            .remove(agent_id)
            .ok_or_else(|| format!("no pending bind for agent {agent_id}"))?;
        tx.send(pty_id).map_err(|_| "spawner is gone (bind timed out)".to_string())
    }
}

/// Background loop that enforces the idle-worker auto-kill guardrail: every
/// `IDLE_REAP_INTERVAL` it kills any worker/reviewer whose idle time has
/// crossed its group's `idle_kill_minutes` (groups with the guardrail off
/// are skipped inside `reap_idle_agents`). Started once at app setup.
pub fn start_idle_reaper(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(IDLE_REAP_INTERVAL);
        reg.reap_idle_agents(now_ms());
    });
}

/// Background loop for the stalled-agent watchdog: every `WATCHDOG_INTERVAL`
/// it nudges the orchestrator (once per stall) about any working agent that
/// has gone silent — no terminal output, no report — past its group's
/// `watchdog_stall_minutes`. Groups with the guardrail off and paused groups
/// are skipped inside `run_watchdog`. Started once at app setup.
pub fn start_watchdog(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(WATCHDOG_INTERVAL);
        reg.run_watchdog(now_ms());
    });
}

/// Background loop for the debounced cap-change notice (#79): every
/// `MAX_NOTICE_FLUSH_INTERVAL` it delivers any coalesced max-agents notice
/// whose quiet window has elapsed, so a burst of stepper clicks reaches the
/// orchestrator as one re-plan prompt instead of one per click. Started once
/// at app setup.
pub fn start_max_notice_flusher(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(MAX_NOTICE_FLUSH_INTERVAL);
        reg.flush_due_max_notices(now_ms());
    });
}

/// Background loop for attention routing (#6): every `ATTENTION_INTERVAL` it
/// recomputes which panes need the human (idle-with-prompt, worker reports,
/// human merge gates), pushes the set to the frontend for pane badges, and
/// toasts newly-attention panes in notification-enabled groups. Started once at
/// app setup.
pub fn start_attention(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(ATTENTION_INTERVAL);
        reg.run_attention(now_ms());
    });
}

// ---------- tauri commands ----------

/// Create (or reattach to) an orchestration group and register its
/// orchestrator. Returns the pane spec the frontend opens directly; initial
/// idle workers are spawned in the background once the orchestrator binds.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // launcher-collected guardrails, one field each
pub fn create_orchestration(
    reg: tauri::State<Arc<OrchRegistry>>,
    repo: String,
    initial_workers: u32,
    max_agents: u32,
    agent_cli: String,
    // Per-role CLI overrides (issue #4). Empty inherits `agent_cli`; the
    // launcher sends the picked CLI for each role.
    orchestrator_cli: String,
    worker_cli: String,
    reviewer_cli: String,
    planner_cli: String,
    worker_model: String,
    reviewer_model: String,
    orchestrator_model: String,
    planner_model: String,
    auto_ops: bool,
    // Trust this repo's agent config for local code execution (issue #51).
    // Default off; the launcher sends the toggle state.
    trust_repo_mcp: bool,
    // Manual per-role profile assignment (issue #51): the profile name picked
    // per role in the launcher ("" = auto, "none" = built-in, else a name).
    orchestrator_profile: String,
    worker_profile: String,
    reviewer_profile: String,
    planner_profile: String,
    idle_kill_minutes: u32,
    max_spawns_per_hour: u32,
    watchdog_stall_minutes: u32,
) -> Result<SpawnRequest, String> {
    create_orchestration_group(
        reg.inner(),
        &repo,
        Guardrails {
            max_agents,
            agent_cli,
            orchestrator_cli,
            worker_cli,
            reviewer_cli,
            planner_cli,
            worker_model,
            reviewer_model,
            orchestrator_model,
            planner_model,
            auto_ops,
            trust_repo_mcp,
            orchestrator_profile,
            worker_profile,
            reviewer_profile,
            planner_profile,
            idle_kill_minutes,
            max_spawns_per_hour,
            watchdog_stall_minutes,
        },
        None,
        None,
        initial_workers,
    )
}

/// Preview a repo's discovered agent profiles + `.mcp.json` server names for
/// the launcher (issue #51), so the human sees what a repo would contribute
/// *before* launching — and, for MCP, before deciding whether to trust it. A
/// non-repo path or missing files yield empty lists (never an error).
#[tauri::command]
pub fn orch_discover_repo_config(repo: String) -> Value {
    let profiles = profiles::discover_profiles(&repo);
    let profile_json: Vec<Value> = profiles
        .iter()
        .map(|p| json!({
            "name": p.name,
            "role": p.role.as_str(),
            "mode": p.mode.as_str(),
            "description": p.description,
            "model": p.model,
            "allow": p.allow,
        }))
        .collect();
    // Repo MCP server names only (not their commands) — enough for the human
    // to recognise what would run, without dumping arbitrary command lines.
    let mcp_servers: Vec<String> = [".mcp.json"]
        .iter()
        .map(|f| Path::new(&repo).join(f))
        .find(|p| p.is_file())
        .and_then(|p| fs::read_to_string(&p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("mcpServers").and_then(Value::as_object).cloned())
        .map(|m| m.keys().filter(|k| *k != "loomux").cloned().collect())
        .unwrap_or_default();
    json!({ "profiles": profile_json, "mcp_servers": mcp_servers })
}

/// Pause a group: loomux stops delivering prompts/kickoffs so its agents
/// idle out (cost containment). Human action from the pane UI.
#[tauri::command]
pub fn orch_pause_group(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Result<(), String> {
    reg.pause_group(&group_id)
}

/// Resume a paused group: prompt/kickoff delivery flows again.
#[tauri::command]
pub fn orch_resume_group(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Result<(), String> {
    reg.resume_group(&group_id)
}

/// Whether a group is currently paused (drives the pause/resume button state).
#[tauri::command]
pub fn orch_group_paused(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> bool {
    reg.is_paused(&group_id)
}

// ---------- attention routing (human side) ----------

/// The human focused/handled an attention-badged pane: drop its latched report
/// so the badge clears. Live reasons (waiting/gate) are recomputed each scan.
#[tauri::command]
pub fn orch_ack_attention(reg: tauri::State<Arc<OrchRegistry>>, agent_id: String) {
    reg.ack_attention(&agent_id);
}

/// The human turned to a plain (non-agent) pane flagged `waiting` (#40): ack it
/// by pty id, since it has no agent identity to key on.
#[tauri::command]
pub fn orch_ack_attention_pty(reg: tauri::State<Arc<OrchRegistry>>, pty_id: u32) {
    reg.ack_attention_pty(pty_id);
}

/// Whether desktop notifications are enabled for a group (toggle button state).
#[tauri::command]
pub fn orch_notify_enabled(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> bool {
    reg.notify_enabled(&group_id)
}

/// Enable/disable desktop notifications for a group (durable, per-group).
#[tauri::command]
pub fn orch_set_notify(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    enabled: bool,
) -> Result<(), String> {
    reg.set_notify(&group_id, enabled)
}

/// Change a live group's max live-agent cap (durable, bounds-checked, audited).
/// Takes effect on the next spawn; lowering it below the current live count
/// blocks new spawns until attrition rather than killing anyone. Returns the
/// applied value. Human action from the GroupView overlay.
#[tauri::command]
pub fn orch_set_max_agents(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    max_agents: u32,
) -> Result<u32, String> {
    reg.set_max_agents(&group_id, max_agents, "human")
}

/// Aggregate per-pane session cost/usage into one group summary for the UI.
#[tauri::command]
pub fn orch_group_usage(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Value {
    reg.group_usage(&group_id)
}

/// Live-agent count, role breakdown, and uptime for the lifecycle panel.
#[tauri::command]
pub fn orch_group_summary(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Value {
    reg.group_summary(&group_id)
}

/// End a whole orchestration: kill all its agents and (optionally) remove
/// their worktrees. Human-initiated, destructive, audited — the frontend
/// confirms before calling this.
#[tauri::command]
pub fn orch_end_group(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    cleanup_worktrees: bool,
) -> Result<Value, String> {
    reg.end_group(&group_id, cleanup_worktrees)
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
    let _creation = reg.creation.lock_safe();
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
    // The orchestrator's CLI is per-role too (issue #4). It must be a supported
    // CLI; the launcher only offers supported ones, so an unknown value here
    // is a hand-edited group.json.
    let cli = group.guardrails.cli_for(Role::Orchestrator);
    if !SUPPORTED_CLIS.contains(&cli) {
        return Err(format!(
            "unsupported orchestrator CLI {cli:?} — supported: {}",
            SUPPORTED_CLIS.join(", ")
        ));
    }
    let cli = cli.to_string();
    let token = new_token();
    let agent_id = format!("orch-{}", reg.seq.fetch_add(1, Ordering::SeqCst) + 1);
    if cli == "copilot" {
        pre_trust_copilot_folder(&group.repo);
    }

    // Repo orchestrator profile (issue #51): the human's "always branch + PR,
    // never push to main"-style secondary prompt, or a full replace-mode
    // personality. loomux always injects the orchestrator mechanics core, so
    // even a replace profile keeps the board/spawn/report contract. Resolved
    // via the same precedence as a delegate (manual assignment > auto).
    let trust = group.guardrails.trust_repo_mcp;
    let orch_profile = reg.resolve_profile(group, Role::Orchestrator, None)?;
    let profile_brief: Option<PathBuf> = match &orch_profile {
        Some(p) => Some(reg.write_profile_brief(&group.id, &agent_id, &group.repo, p)?),
        None => None,
    };
    let extra_servers = if cli == "claude" && trust {
        reg.repo_mcp_servers(&group.id, &group.repo, &group.repo)
    } else {
        None
    };
    let cfg = reg.write_mcp_config(&group.id, &agent_id, &token, &cli, extra_servers.as_ref())?;
    let resume = resume_session.is_some();
    let session_id = match resume_session {
        Some(s) => Some(sanitize_session(&s).ok_or("invalid resume session id")?),
        None => (cli == "claude").then(new_session_uuid),
    };
    // Copilot mints its own id on boot; snapshot existing sessions now so the
    // orchestrator's newly created one can be tracked (this is what gives a
    // copilot orchestration its ORCH chip and restore).
    let copilot_baseline = (!resume && cli == "copilot")
        .then(|| {
            crate::sessions::copilot_session_state_root()
                .map(|root| crate::sessions::copilot_session_ids(&root))
                .unwrap_or_default()
        });
    let empty_allow: Vec<String> = Vec::new();
    let command = reg.build_agent_command(
        &cli,
        &model,
        group.guardrails.auto_ops,
        &cfg,
        &reg.group_dir(&group.id),
        Path::new(&group.repo),
        session_id.as_deref(),
        resume,
        false, // the orchestrator is never read-only
        &ProfileInject {
            extra_allow: orch_profile.as_ref().map(|p| p.allow.as_slice()).unwrap_or(&empty_allow),
            system_prompt_file: profile_brief.as_deref(),
            copilot_agent: orch_profile
                .as_ref()
                .filter(|_| trust)
                .and_then(|p| p.copilot_agent.as_deref()),
        },
    );
    let entry = AgentEntry {
        id: agent_id.clone(),
        group: group.id.clone(),
        name: "orchestrator".into(),
        // A stable, meaningful single-orchestrator label — treated as the
        // id-default tier so it never blocks anything (the rename tool targets
        // worker/reviewer panes, not the orchestrator).
        name_source: NameSource::Default,
        role: Role::Orchestrator,
        token: token.clone(),
        status: AgentStatus::Starting,
        pty_id: None,
        task: String::new(),
        session_id,
        cwd: group.repo.clone(),
        idle_since_ms: None, // the orchestrator is never idle-reaped
        started_ms: now_ms(),
        last_progress_ms: now_ms(), // unused: the orchestrator is never watchdogged
        last_output_total: 0,
        watchdog_notified: false,
    };
    reg.agents.lock_safe().insert(agent_id.clone(), entry.clone());
    reg.by_token.lock_safe().insert(token, agent_id.clone());
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

    crate::obs::breadcrumb(
        "agent-spawn",
        &format!("group={} agent={agent_id} role=Orchestrator resume={resume}", group.id),
    );

    if reg.app.lock_safe().is_none() {
        // Test mode: no frontend; mark running without a pane. Tolerate a
        // vanished entry rather than unwrapping under the agents lock.
        if let Some(a) = reg.agents.lock_safe().get_mut(&agent_id) {
            a.status = AgentStatus::Running;
        }
        return Ok(request);
    }

    // Background: wait for the orchestrator pane to bind, type its kickoff,
    // then bring up the initial idle workers one by one.
    let (tx, rx) = mpsc::channel::<u32>();
    reg.pending_binds.lock_safe().insert(agent_id.clone(), tx);
    let reg2 = reg.clone();
    let group2 = group.clone();
    std::thread::spawn(move || {
        let Ok(pty_id) = rx.recv_timeout(BIND_TIMEOUT) else {
            reg2.pending_binds.lock_safe().remove(&agent_id);
            reg2.mark_dead(&agent_id, None);
            return;
        };
        {
            let mut agents = reg2.agents.lock_safe();
            if let Some(a) = agents.get_mut(&agent_id) {
                a.status = AgentStatus::Running;
                a.pty_id = Some(pty_id);
            }
        }
        reg2.by_pty.lock_safe().insert(pty_id, agent_id.clone());
        reg2.audit(&group2.id, "loomux", "agent-bind", json!({ "agent": agent_id, "pty": pty_id }));
        crate::obs::breadcrumb("agent-bind", &format!("agent={agent_id} pty={pty_id} role=Orchestrator"));
        let kickoff = if resume {
            "[loomux] Orchestration restored: your MCP tools, the task board, and the audit log are live again in this session. Re-sync now: list_tasks, list_agents, get_state. Your previous worker panes are gone; resume a task session with spawn_agent(resume_session, cwd) when follow-ups need it. Then give the human a short status summary.".to_string()
        } else {
            match reg2.agent(&agent_id) {
                Some(a) => reg2.kickoff_prompt(&a, &group2, ""),
                None => return, // agent reaped before bind; nothing to kick off
            }
        };
        let _ = reg2.deliver_prompt(&agent_id, &kickoff, "loomux", true);
        // Track the copilot session this orchestrator just minted.
        if let Some(baseline) = copilot_baseline {
            reg2.clone().spawn_copilot_session_watcher(
                agent_id.clone(),
                group2.id.clone(),
                group2.repo.clone(),
                baseline,
            );
        }
        for _ in 0..initial_workers.min(group2.guardrails.max_agents) {
            // Empty name → derived from the minted id ("worker 2" for `w-2`),
            // so the pane title agrees with its "W 2" badge instead of the old
            // per-launch counter that drifted from the seq (#95r).
            if let Err(e) = reg2.spawn_agent(&group2.id, Role::Worker, "", "", false, None)
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
    let role = match record.role.as_str() {
        "reviewer" => Role::Reviewer,
        "planner" => Role::Planner,
        _ => Role::Worker,
    };
    // Pull the durable roster row for this session: its cwd (where the work
    // happened) and its name tier — so a human-renamed pane rejoins at the
    // `Human` tier and stays un-clobberable, not silently demoted to
    // orchestrator (#95r). Absent (hint-restored, pre-roster) → `None`, and
    // spawn derives the tier from the name as usual.
    let matched = reg
        .merged_records(&record.group_id)
        .into_iter()
        .find(|r| r.session.as_deref() == Some(session_id));
    let cwd = matched.as_ref().map(|r| r.cwd.clone()).filter(|c| Path::new(c).is_dir());
    let restore_source = matched.as_ref().map(|r| r.name_source);
    let reg2 = reg.clone();
    let sid = session_id.to_string();
    let (group_id, name) = (record.group_id.clone(), record.agent_name.clone());
    std::thread::spawn(move || {
        if let Err(e) = reg2.spawn_agent_ex(
            // profile None on resume: the resumed session already carries its
            // persona; re-injecting would double the addendum.
            &group_id, role, &name, "", false, None, None, Some(sid.clone()), cwd, restore_source,
        ) {
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

/// The human renamed an agent pane in-place (F2 / double-click). Sync the
/// backend so the roster name matches the pane title AND the rename is
/// recorded at the highest precedence tier — an orchestrator `rename_agent`
/// afterwards will not override it (#95r). Best-effort: the pane already shows
/// the new name locally, so a stale/unknown id just fails silently here.
#[tauri::command]
pub fn orch_agent_renamed(
    reg: tauri::State<Arc<OrchRegistry>>,
    agent_id: String,
    name: String,
) -> Result<(), String> {
    reg.rename_agent(&agent_id, &name, NameSource::Human).map(|_| ())
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

// ---------- merge-gate link resolution ----------
// The board stores issue/PR references as the orchestrator typed them
// (`#12`, a bare number, or a full URL). To make the chips clickable we
// resolve those to a web URL against the repo's `origin` remote.

/// Normalize a git remote URL (`git@`, `ssh://`, `https://`, with or without
/// a trailing `.git`) into its browsable web base, e.g.
/// `https://github.com/owner/repo`. None for anything that doesn't look like
/// a host/path we can turn into a link.
#[doc(hidden)] // pub for integration tests
pub fn normalize_remote_web_base(url: &str) -> Option<String> {
    let u = url.trim();
    if u.is_empty() {
        return None;
    }
    // Split into host and path, covering the three shapes git emits.
    let (host, path) = if let Some(rest) = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .or_else(|| u.strip_prefix("ssh://"))
    {
        // scheme://[user@]host[:port]/owner/repo
        let rest = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        // Drop any :port from the host part (ssh URLs may carry one).
        let host = host.split(':').next().unwrap_or(host);
        (host.to_string(), path.to_string())
    } else if let Some(rest) = u.strip_prefix("git@") {
        // scp-like: git@host:owner/repo.git
        let (host, path) = rest.split_once(':')?;
        (host.to_string(), path.to_string())
    } else {
        return None;
    };
    let host = host.trim().trim_end_matches('/');
    let path = path.trim().trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() || !host.contains('.') {
        return None;
    }
    Some(format!("https://{host}/{path}"))
}

/// Web base for a repo's `origin` remote (falling back to any remote), or
/// None when the repo has no usable remote.
fn git_remote_web_base(repo: &str) -> Option<String> {
    if !Path::new(repo).is_dir() {
        return None;
    }
    let run = |args: &[&str]| -> Option<String> {
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(repo).args(args).env("GIT_TERMINAL_PROMPT", "0");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let url = run(&["remote", "get-url", "origin"]).or_else(|| {
        // No `origin` — take the first remote git lists, if any.
        let name = run(&["remote"])?.lines().next()?.trim().to_string();
        (!name.is_empty()).then_some(name).and_then(|n| run(&["remote", "get-url", &n]))
    })?;
    normalize_remote_web_base(&url)
}

/// Resolve a stored issue/PR reference to a URL. `value` may already be a
/// full URL (used verbatim); otherwise it's a `#N`/`N` reference resolved
/// against `base`. `kind` is `"issue"` or `"pr"`. None when there's nothing
/// clickable (no number, or a bare number with no known remote).
#[doc(hidden)] // pub for integration tests
pub fn resolve_ref_url(base: Option<&str>, kind: &str, value: &str) -> Option<String> {
    let v = value.trim();
    if v.starts_with("https://") || v.starts_with("http://") {
        return Some(v.to_string());
    }
    // Pull the first run of digits out of `#12`, `12`, `GH-12`, etc.
    let num: String = v
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    if num.is_empty() {
        return None;
    }
    // GitHub redirects /issues/N <-> /pull/N, so a kind mismatch still lands.
    let seg = if kind == "issue" { "issues" } else { "pull" };
    Some(format!("{}/{seg}/{num}", base?.trim_end_matches('/')))
}

/// WinRT toast script (see `notify_desktop`). Title/body come in via
/// environment variables — never interpolated into the script — so agent/board
/// text can't inject PowerShell. XML-escaped before templating. The AppUserModel
/// id is the stock PowerShell shortcut, which lets an unpackaged process raise a
/// toast on Windows 10; it renders attributed to PowerShell, which is fine for
/// an optional signal.
#[cfg(target_os = "windows")]
const TOAST_PS1: &str = r#"
$ErrorActionPreference='SilentlyContinue'
[void][Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]
[void][Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom,ContentType=WindowsRuntime]
$t=[System.Security.SecurityElement]::Escape($env:LOOMUX_TOAST_TITLE)
$b=[System.Security.SecurityElement]::Escape($env:LOOMUX_TOAST_BODY)
$xml="<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$b</text></binding></visual></toast>"
$doc=New-Object Windows.Data.Xml.Dom.XmlDocument
$doc.LoadXml($xml)
$toast=New-Object Windows.UI.Notifications.ToastNotification $doc
$app='{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show($toast)
"#;

/// Best-effort OS desktop notification (attention routing #6). On Windows this
/// spawns a hidden PowerShell that raises a WinRT toast, passing the title/body
/// as environment variables (injection-proof — see `TOAST_PS1`). Deliberately
/// no notification crate: those pull getrandom, which this project's Windows 10
/// baseline can't load (0xc0000139 — see the Cargo.toml note). Silently a no-op
/// on failure and on non-Windows; the pane badges and board highlight are the
/// primary signal regardless.
#[cfg(target_os = "windows")]
fn notify_desktop(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", TOAST_PS1])
        .env("LOOMUX_TOAST_TITLE", title)
        .env("LOOMUX_TOAST_BODY", body)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

#[cfg(not(target_os = "windows"))]
fn notify_desktop(_title: &str, _body: &str) {}

/// Open an http(s) URL in the user's default browser. The URL is passed to
/// the OS handler as a single process argument (never a shell line), and is
/// validated first so a crafted board reference can't smuggle anything.
fn open_external_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("refusing to open a non-http(s) URL".into());
    }
    if url.len() > 2048 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("unsafe URL".into());
    }
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // rundll32 takes the URL as one argument, sidestepping cmd.exe's
        // `start` metacharacter handling.
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| format!("could not open browser: {e}"))
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

/// Human steering from the loomux compose strip (#43, option C): enqueue
/// `text` to the group's orchestrator through the SAME per-pane serialized
/// delivery path worker reports use, so loomux is the single writer to the
/// pane's stdin and messages land whole (never interleaved; relative order of
/// near-simultaneous sends is best-effort — the per-pty delivery mutex is not
/// FIFO). Empty text, a paused group, and a dead orchestrator all surface as
/// errors the strip shows the human.
#[tauri::command]
pub fn orch_steer(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    text: String,
) -> Result<(), String> {
    reg.steer_orchestrator(&group_id, &text)
}

/// Result of saving a steering-strip attachment: the absolute file path plus
/// the resolved orchestrator CLI, so the frontend can format the in-prompt
/// reference the way that CLI consumes it (Claude reads a plain path; Copilot
/// documents an `@<path>` mention — #72 review note 3).
#[derive(serde::Serialize)]
pub struct SavedAttachment {
    pub path: String,
    pub cli: String,
}

/// Save an image pasted/attached into the steering strip (#72). The image rides
/// over IPC as base64 (`data_b64`) — same wire form as the OSC 52 clipboard
/// bridge — so it survives any webview that won't hand raw bytes through
/// `invoke`. Returns the saved path and the group's orchestrator CLI; the
/// frontend turns those into the per-CLI "Attached image" reference line before
/// sending through `orch_steer`.
#[tauri::command]
pub fn orch_save_attachment(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    ext: String,
    data_b64: String,
) -> Result<SavedAttachment, String> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    // Reject an oversize payload before decoding — see MAX_ATTACHMENT_B64_LEN.
    if data_b64.len() > MAX_ATTACHMENT_B64_LEN {
        return Err(format!(
            "attachment too large (max {MAX_ATTACHMENT_BYTES} bytes)"
        ));
    }
    let bytes = B64
        .decode(data_b64.as_bytes())
        .map_err(|e| format!("invalid attachment encoding: {e}"))?;
    let path = reg.save_attachment(&group_id, &ext, &bytes)?;
    Ok(SavedAttachment {
        path: path.to_string_lossy().to_string(),
        cli: reg.orchestrator_cli(&group_id),
    })
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

// ---------- merge-gate actions (human side) ----------
// The human's gatekeeping touchpoints on `pr` / `human-testing` items. Each
// records on the board (audited, actor "human") and delivers a purpose-built
// typed notice into the orchestrator's CLI so it can act on the decision.

/// Open a task's issue or PR reference in the default browser. `kind` is
/// `"issue"` or `"pr"`; `value` is the stored reference (`#12`, `12`, or a
/// full URL).
#[tauri::command]
pub fn orch_open_ref(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    kind: String,
    value: String,
) -> Result<(), String> {
    let repo = reg
        .group(&group_id)
        .map(|g| g.repo)
        .or_else(|| reg.load_group_file(&group_id).map(|(repo, _)| repo))
        .ok_or("unknown group")?;
    let base = git_remote_web_base(&repo);
    let url = resolve_ref_url(base.as_deref(), &kind, &value)
        .ok_or("no URL for this reference — the repo may have no GitHub remote")?;
    reg.audit(&group_id, "human", "open-ref", json!({ "kind": kind, "url": url }));
    open_external_url(&url)
}

/// Approve a merge-gate item: mark it done and notify the orchestrator to
/// merge. The human's direct sign-off, so the status change is applied here.
#[tauri::command]
pub fn orch_approve_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
) -> Result<Task, String> {
    reg.approve_task(&group_id, &id)
}

/// Request changes on a merge-gate item: record the findings and deliver them
/// to the orchestrator to route back to a worker.
#[tauri::command]
pub fn orch_request_changes(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
    findings: String,
) -> Result<Task, String> {
    reg.request_changes(&group_id, &id, &findings)
}

/// Start a queued item: record a human-attributed note and tell the
/// orchestrator to begin work. Does not flip the status — the orchestrator
/// moves it to `in-progress` when it actually assigns a worker.
#[tauri::command]
pub fn orch_start_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
) -> Result<Task, String> {
    reg.start_task(&group_id, &id)
}

#[cfg(test)]
mod hold_tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(4);
    const CAP: Duration = Duration::from_secs(90);

    #[test]
    fn holds_while_human_typed_recently() {
        // Typed 1s ago (< 4s window), well under the cap: keep holding.
        assert!(should_hold_for_user(9_000, 10_000, Duration::from_secs(5), WINDOW, CAP));
    }

    #[test]
    fn proceeds_once_human_is_quiet() {
        // Last keystroke was 5s ago (> 4s window): deliver.
        assert!(!should_hold_for_user(5_000, 10_000, Duration::from_secs(2), WINDOW, CAP));
    }

    #[test]
    fn proceeds_when_nobody_typed() {
        // 0 == no keystroke ever recorded for this pane.
        assert!(!should_hold_for_user(0, 10_000, Duration::ZERO, WINDOW, CAP));
    }

    #[test]
    fn cap_forces_delivery_even_if_still_typing() {
        // Human is still typing (0ms ago) but the hold hit the 90s cap:
        // deliver anyway so reports aren't starved forever.
        assert!(!should_hold_for_user(10_000, 10_000, CAP, WINDOW, CAP));
        // One tick over the cap also delivers.
        assert!(!should_hold_for_user(10_000, 10_000, CAP + Duration::from_millis(1), WINDOW, CAP));
    }

    #[test]
    fn boundary_at_exactly_the_window_proceeds() {
        // `since == window` is not "< window", so it proceeds (quiet enough).
        assert!(!should_hold_for_user(6_000, 10_000, Duration::from_secs(1), WINDOW, CAP));
    }

    #[test]
    fn future_timestamp_does_not_underflow() {
        // A clock skew where last_input is "after" now must not panic or wrap;
        // saturating_sub yields 0 → within window → hold.
        assert!(should_hold_for_user(11_000, 10_000, Duration::from_secs(1), WINDOW, CAP));
    }
}

#[cfg(test)]
mod max_notice_tests {
    use super::*;

    const DEB: Duration = Duration::from_secs(3);

    #[test]
    fn burst_coalesces_to_one_span() {
        // Three rapid clicks 4→3, 3→2, 2→1 inside the window: one pending entry
        // spanning the whole burst, its deadline riding the LAST click.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "g", 4, 3, 1_000, DEB);
        record_max_notice(&mut p, "g", 3, 2, 1_500, DEB);
        record_max_notice(&mut p, "g", 2, 1, 2_000, DEB);
        assert_eq!(p.len(), 1, "a burst stays one pending notice");
        // Not yet due (last click at 2_000 → due 5_000): nothing flushes.
        assert!(take_due_max_notices(&mut p, 4_999).is_empty());
        // Past the window: exactly one notice, from the burst's first value to
        // its last — 4→1, never the intermediate 4→3 / 3→2.
        assert_eq!(take_due_max_notices(&mut p, 5_000), vec![("g".to_string(), 4, 1)]);
        assert!(p.is_empty(), "delivered notices are drained");
    }

    #[test]
    fn each_click_pushes_the_deadline_out() {
        // A click landing before the prior one's window elapses must reset the
        // deadline, or a long slow drag would fire mid-burst.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "g", 4, 3, 1_000, DEB); // due 4_000
        record_max_notice(&mut p, "g", 3, 2, 3_900, DEB); // due 6_900
        // At 4_000 the first click's deadline has passed, but the second reset
        // it — so nothing is due yet.
        assert!(take_due_max_notices(&mut p, 4_000).is_empty());
        assert_eq!(take_due_max_notices(&mut p, 6_900), vec![("g".to_string(), 4, 2)]);
    }

    #[test]
    fn spaced_changes_deliver_separately() {
        // Two changes far enough apart that the first flushes before the second
        // arrives: two distinct notices, each its own span.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "g", 4, 3, 1_000, DEB);
        assert_eq!(take_due_max_notices(&mut p, 4_000), vec![("g".to_string(), 4, 3)]);
        record_max_notice(&mut p, "g", 3, 2, 10_000, DEB);
        assert_eq!(take_due_max_notices(&mut p, 13_000), vec![("g".to_string(), 3, 2)]);
    }

    #[test]
    fn net_noop_burst_delivers_nothing() {
        // 4→3→4 nets to no change: no orchestrator tokens spent on a no-op.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "g", 4, 3, 1_000, DEB);
        record_max_notice(&mut p, "g", 3, 4, 1_500, DEB);
        assert!(take_due_max_notices(&mut p, 5_000).is_empty());
        assert!(p.is_empty(), "the netted-out entry is still drained, not left pending");
    }

    #[test]
    fn groups_debounce_independently() {
        // Two groups clicking at once don't share a deadline or a span.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "a", 4, 2, 1_000, DEB); // due 4_000
        record_max_notice(&mut p, "b", 5, 6, 3_000, DEB); // due 6_000
        // Only group a is due at 4_000.
        assert_eq!(take_due_max_notices(&mut p, 4_000), vec![("a".to_string(), 4, 2)]);
        assert!(p.contains_key("b"), "b keeps waiting out its own window");
        assert_eq!(take_due_max_notices(&mut p, 6_000), vec![("b".to_string(), 5, 6)]);
    }
}
