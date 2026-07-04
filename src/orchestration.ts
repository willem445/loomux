// Orchestrator/worker groups, frontend half.
//
// The Rust backend owns the registry, guardrails, MCP server, persistence,
// and audit log; this module owns what only the frontend can do — open a
// visible pane when the backend asks for one (`orch-spawn-request`), report
// the resulting pty id back (`bind_agent`), badge/color panes by group and
// role, and focus panes on request.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Grid } from "./grid";
import type { PaneEvents, PaneBadge } from "./pane";

export type OrchRole = "orchestrator" | "worker" | "reviewer";

/** Backend request to open (or spec to open) an agent pane. */
export interface OrchSpawnRequest {
  group_id: string;
  agent_id: string;
  role: OrchRole;
  name: string;
  cwd: string;
  command: string;
}

/** Launcher-collected group settings; guardrails are enforced backend-side. */
export interface OrchestratorConfig {
  repo: string;
  /** "claude" | "copilot" — which agent CLI the whole group runs. */
  agentCli: string;
  initialWorkers: number;
  maxAgents: number;
  workerModel: string;
  reviewerModel: string;
  orchestratorModel: string;
  autoOps: boolean;
  /** Cost guardrail: auto-kill an idle worker/reviewer after this many
   *  minutes without a task (0 = disabled). */
  idleKillMinutes: number;
  /** Cost guardrail: cap on worker/reviewer spawns per rolling hour
   *  (0 = unlimited). */
  maxSpawnsPerHour: number;
  /** Recovery guardrail: nudge the orchestrator when a working agent goes
   *  silent (no output, no report) this many minutes (0 = disabled). */
  watchdogStallMinutes: number;
}

// Per-group identity: a stable accent color AND a short ordinal tag shown in
// every badge, so with several orchestrations open you can pair each worker
// to its orchestrator at a glance ("ORCH 2" ↔ "W 2") without relying on
// color perception alone. Groups are few; palette wrap collisions are fine
// because the tag still disambiguates.
const GROUP_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e"];
interface GroupMeta {
  color: string;
  tag: number;
}
const groupMeta = new Map<string, GroupMeta>();

export function metaForGroup(groupId: string): GroupMeta {
  let m = groupMeta.get(groupId);
  if (!m) {
    const tag = groupMeta.size + 1;
    m = { tag, color: GROUP_COLORS[(tag - 1) % GROUP_COLORS.length] };
    groupMeta.set(groupId, m);
  }
  return m;
}

const ROLE_LABELS: Record<OrchRole, string> = {
  orchestrator: "ORCH",
  worker: "W",
  reviewer: "REV",
};

export function badgeFor(req: OrchSpawnRequest): PaneBadge {
  const meta = metaForGroup(req.group_id);
  return {
    label: `${ROLE_LABELS[req.role] ?? "AGENT"} ${meta.tag}`,
    color: meta.color,
    title: `${req.role} · ${req.agent_id} · group ${req.group_id}`,
  };
}

/** One pane that needs the human, from the backend attention scan. `reason`
 *  is (most→least urgent) "blocked" | "waiting" | "report" | "gate". */
export interface AttentionItem {
  agent_id: string;
  group: string;
  name: string;
  role: OrchRole;
  pty_id: number | null;
  reason: string;
  detail: string;
}

/** The human focused/handled an attention-badged pane: clear its latched
 *  report backend-side so the badge drops. */
export const ackAttention = (agentId: string): Promise<void> =>
  invoke("orch_ack_attention", { agentId });

/** Whether desktop notifications are enabled for a group. */
export const notifyEnabled = (groupId: string): Promise<boolean> =>
  invoke<boolean>("orch_notify_enabled", { groupId });

/** Enable/disable desktop notifications for a group (durable, per-group). */
export const setNotify = (groupId: string, enabled: boolean): Promise<void> =>
  invoke("orch_set_notify", { groupId, enabled });

async function openAgentPane(
  grid: Grid,
  paneEvents: PaneEvents,
  req: OrchSpawnRequest
): Promise<void> {
  const pane = await grid.openPane(
    {
      name: req.name,
      cwd: req.cwd,
      command: req.command,
      badge: badgeFor(req),
      orchGroup: req.group_id,
      orchRole: req.role,
      orchAgent: req.agent_id,
    },
    paneEvents,
    grid.paneCount >= 2 ? "column" : "row"
  );
  // Report the pty so the backend can unblock the spawner and type the
  // kickoff prompt. A failed spawn (ptyId null) times out backend-side.
  if (pane.ptyId !== null) {
    await invoke("bind_agent", { agentId: req.agent_id, ptyId: pane.ptyId });
  }
}

/** Wire backend→frontend orchestration events. Call once at startup,
 *  before any orchestrator can be launched. */
export function initOrchestration(grid: Grid, paneEvents: PaneEvents): void {
  void listen<OrchSpawnRequest>("orch-spawn-request", ({ payload }) => {
    void openAgentPane(grid, paneEvents, payload);
  });
  void listen<{ agent_id: string; pty_id: number | null }>("orch-focus", ({ payload }) => {
    if (payload.pty_id === null) return;
    const pane = grid.findByPtyId(payload.pty_id);
    if (pane) {
      grid.setActive(pane);
      pane.focus();
    }
  });
  // Attention routing: the backend pushes the full current set of panes that
  // need the human every scan; badge each group pane by its pty (absent =
  // clear). Idempotent per pane, so re-emits every few seconds are cheap.
  void listen<AttentionItem[]>("orch-attention", ({ payload }) => {
    const byPty = new Map<number, AttentionItem>();
    for (const it of payload) if (it.pty_id !== null) byPty.set(it.pty_id, it);
    for (const pane of grid.panes()) {
      if (!pane.orchGroupId || pane.ptyId === null) continue;
      const it = byPty.get(pane.ptyId);
      pane.setAttention(it ? it.reason : null, it?.detail);
    }
  });
  // End-orchestration: the backend has already killed the group's agents, so
  // close their (now-dead) panes rather than leaving a screen of dead
  // terminals — the pane-by-pane ✕-clicking this action exists to replace.
  void listen<{ group_id: string }>("orch-group-ended", ({ payload }) => {
    for (const pane of grid.panes()) {
      if (pane.orchGroupId === payload.group_id) grid.closePane(pane, false);
    }
  });
}

/** A recorded session's orchestration identity (backend roster). */
export interface SessionRoleInfo {
  session_id: string;
  group_id: string;
  role: string;
  agent_name: string;
  group_live: boolean;
}

export const orchSessionRoles = (): Promise<SessionRoleInfo[]> =>
  invoke<SessionRoleInfo[]>("orch_session_roles");

/** Restore a recorded orchestration session from the session browser.
 *  Orchestrator sessions relaunch the whole group (MCP identity, task
 *  board) and return a pane spec to open; worker/reviewer rejoins arrive
 *  via the normal orch-spawn-request event. */
export async function resumeOrchSession(
  grid: Grid,
  paneEvents: PaneEvents,
  sessionId: string,
  hint?: { group: string; role: string }
): Promise<void> {
  const spec = await invoke<OrchSpawnRequest | null>("resume_orch_session", {
    sessionId,
    groupHint: hint?.group ?? null,
    roleHint: hint?.role ?? null,
  });
  if (spec) await openAgentPane(grid, paneEvents, spec);
}

/** Create/resume the group for `config.repo` and open its orchestrator
 *  pane. The backend spawns the initial idle workers (as spawn-request
 *  events) once the orchestrator binds. */
export async function launchOrchestrator(
  grid: Grid,
  paneEvents: PaneEvents,
  config: OrchestratorConfig
): Promise<void> {
  const spec = await invoke<OrchSpawnRequest>("create_orchestration", {
    repo: config.repo,
    agentCli: config.agentCli,
    initialWorkers: config.initialWorkers,
    maxAgents: config.maxAgents,
    workerModel: config.workerModel,
    reviewerModel: config.reviewerModel,
    orchestratorModel: config.orchestratorModel,
    autoOps: config.autoOps,
    idleKillMinutes: config.idleKillMinutes,
    maxSpawnsPerHour: config.maxSpawnsPerHour,
    watchdogStallMinutes: config.watchdogStallMinutes,
  });
  await openAgentPane(grid, paneEvents, spec);
}

// ---------- cost containment: pause/resume + per-group usage ----------

/** One agent's parsed session cost within a group usage summary. */
export interface AgentUsage {
  id: string;
  name: string;
  role: string;
  /** Dollars parsed from the pane statusline, or null if none was visible. */
  cost_usd: number | null;
}

/** Aggregated per-group cost/usage (backend `orch_group_usage`). */
export interface GroupUsage {
  group: string;
  /** Sum across agents with a visible cost, or null if none had one. */
  total_cost_usd: number | null;
  agents: AgentUsage[];
  note: string;
}

/** Pause a group: loomux stops delivering prompts/kickoffs so its agents
 *  idle out, containing unattended spend. Reversible with `resumeGroup`. */
export const pauseGroup = (groupId: string): Promise<void> =>
  invoke("orch_pause_group", { groupId });

/** Resume a paused group so prompt/kickoff delivery flows again. */
export const resumeGroup = (groupId: string): Promise<void> =>
  invoke("orch_resume_group", { groupId });

/** Whether a group is currently paused (for the pause/resume button state). */
export const groupPaused = (groupId: string): Promise<boolean> =>
  invoke<boolean>("orch_group_paused", { groupId });

/** Aggregate per-pane session cost into one group summary. */
export const groupUsage = (groupId: string): Promise<GroupUsage> =>
  invoke<GroupUsage>("orch_group_usage", { groupId });

// ---------- group lifecycle: summary + end-orchestration (#8) ----------

/** One live agent in a group lifecycle summary. */
export interface AgentSummary {
  id: string;
  name: string;
  role: OrchRole;
  /** Empty for an idle/ready agent. */
  task: string;
  /** Unix-ms this agent last went idle, or null while it has work. */
  idle_since_ms: number | null;
  /** Milliseconds since the agent was spawned. */
  uptime_ms: number;
}

/** At-a-glance lifecycle summary for a group (backend `orch_group_summary`). */
export interface GroupSummary {
  group: string;
  live_agents: number;
  paused: boolean;
  /** Group uptime (from the earliest live agent), or null if none are live. */
  uptime_ms: number | null;
  roles: { orchestrator: number; worker: number; reviewer: number };
  agents: AgentSummary[];
}

/** Result of ending a group (killed agent ids + worktree cleanup outcome). */
export interface EndGroupResult {
  group: string;
  killed: string[];
  worktrees_removed: string[];
  worktree_errors: { path: string; error: string }[];
}

/** Live-agent count, role breakdown, and uptime for the lifecycle panel. */
export const groupSummary = (groupId: string): Promise<GroupSummary> =>
  invoke<GroupSummary>("orch_group_summary", { groupId });

/** End a whole orchestration: kill all its agents and (optionally) remove
 *  their worktrees. Destructive and human-initiated — the caller confirms
 *  first. The backend emits `orch-group-ended` so the panes close. */
export const endGroup = (groupId: string, cleanupWorktrees: boolean): Promise<EndGroupResult> =>
  invoke<EndGroupResult>("orch_end_group", { groupId, cleanupWorktrees });
