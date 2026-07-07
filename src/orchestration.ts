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
import type { Pane, PaneEvents } from "./pane";
import { panesInGroup } from "./group";
import { badgeFor, type OrchRole } from "./orchbadge";
import { isSpawnRequestExpired } from "./spawnexpiry";
import { showToast } from "./toast";

export type { OrchRole };
export { badgeFor, metaForGroup } from "./orchbadge";

/** Backend request to open (or spec to open) an agent pane. */
export interface OrchSpawnRequest {
  group_id: string;
  agent_id: string;
  role: OrchRole;
  name: string;
  cwd: string;
  command: string;
  /** Wall-clock Unix-ms after which a still-queued request must be dropped
   *  unserviced (#106): the backend's own bind wait has elapsed, so opening a
   *  pane now would spawn a zombie CLI against a torn-down config. See
   *  `isSpawnRequestExpired`. 0/absent from a legacy backend = never expires. */
  deadline_ms: number;
  /** Structured invocation for direct-CLI spawn (issue #78); the backend spawns
   *  the agent executable directly when it resolves, else falls back to
   *  `command`. Absent on payloads from an older backend. */
  argv?: string[];
}

/** Launcher-collected group settings; guardrails are enforced backend-side. */
export interface OrchestratorConfig {
  repo: string;
  /** "claude" | "copilot" — the group's default agent CLI, used as the
   *  fallback for any role whose per-role CLI is left blank. */
  agentCli: string;
  /** Per-role agent CLI (issue #4, mixed agent types). Each is a supported
   *  CLI id; the backend inherits `agentCli` when one is empty. */
  orchestratorCli: string;
  workerCli: string;
  reviewerCli: string;
  plannerCli: string;
  initialWorkers: number;
  maxAgents: number;
  workerModel: string;
  reviewerModel: string;
  orchestratorModel: string;
  /** Model for the planner role (issue #47). */
  plannerModel: string;
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

/** One pane that needs the human, from the backend attention scan. `reason`
 *  is (most→least urgent) "blocked" | "waiting" | "report" | "gate". */
export interface AttentionItem {
  /** Empty for a plain (non-orchestration) pane, which is keyed only by pty_id. */
  agent_id: string;
  group: string;
  name: string;
  /** null for a plain pane (no orchestration role). */
  role: OrchRole | null;
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

/** Change a group's max live-agent cap on the fly (bounds-checked backend-side,
 *  durable, audited). Resolves to the applied value; rejects with the backend
 *  error string on an out-of-range value or unknown group. Lowering below the
 *  current live count blocks new spawns until attrition — it kills no one. */
export const setMaxAgents = (groupId: string, maxAgents: number): Promise<number> =>
  invoke<number>("orch_set_max_agents", { groupId, maxAgents });

/** Agent ids the backend cancelled via `orch-spawn-cancelled` (its bind wait
 *  timed out) whose pane may still be mid-open (#106). Consulted in
 *  `openAgentPane` right before binding so a live-but-slow frontend drops a
 *  request the bind-timeout already tore down, instead of leaving a zombie. */
const cancelledSpawns = new Set<string>();

/** Close a pane we opened for a spawn that turned out to be stale, killing the
 *  CLI it booted against a now-deleted config, and tell the human briefly.
 *  Idempotent: when both the cancel event and the late-bind rejection fire for
 *  the same spawn, the second call finds the pane already gone and does nothing
 *  — no double close, no duplicate "discarded" toast (#106 rev-49). Killing the
 *  pty reaps its CLI descendants via the pane's Job Object (#107), so the stray
 *  agent process tree is fully torn down, not just the visible pane. */
function discardStalePane(grid: Grid, pane: Pane): void {
  if (!grid.allPanes().includes(pane)) return;
  grid.closePane(pane, true);
  showToast("stale spawn request discarded", "info");
}

async function openAgentPane(
  grid: Grid,
  paneEvents: PaneEvents,
  req: OrchSpawnRequest,
  // Orchestrator-driven spawns open in the background so they don't yank focus
  // from the pane the human is typing in (#117). Human-initiated paths (session
  // restore, launching an orchestrator) pass false to focus the new pane.
  background: boolean
): Promise<void> {
  const pane = await grid.openPane(
    {
      name: req.name,
      cwd: req.cwd,
      command: req.command,
      argv: req.argv,
      badge: badgeFor(req),
      orchGroup: req.group_id,
      orchRole: req.role,
      orchAgent: req.agent_id,
      background,
    },
    paneEvents,
    grid.paneCount >= 2 ? "column" : "row"
  );
  try {
    // A failed spawn (ptyId null) has no pty to bind; it times out backend-side.
    if (pane.ptyId === null) return;
    // A cancel that landed while the pane was opening (#106): the backend bind
    // already timed out and cleaned up, so don't bind — discard the fresh pane.
    if (cancelledSpawns.has(req.agent_id)) {
      discardStalePane(grid, pane);
      return;
    }
    // Report the pty so the backend can unblock the spawner and type the kickoff.
    try {
      await invoke("bind_agent", { agentId: req.agent_id, ptyId: pane.ptyId });
    } catch {
      // Late bind (#106): the backend's bind wait timed out and removed the
      // pending bind, so this rejects ("no pending bind for agent …"). Handle it
      // rather than leaking an unhandled-rejection toast — the pane is a zombie
      // (its CLI booted against a deleted config), so close it with a brief
      // notice. Belt-and-braces behind the deadline drop and the cancel event.
      discardStalePane(grid, pane);
    }
  } finally {
    // Whichever path we took, this request is now resolved — clear any cancel
    // note so `cancelledSpawns` can't accumulate stale ids across a run, even
    // on the race where the cancel arrives mid-bind (#106 rev-49).
    cancelledSpawns.delete(req.agent_id);
  }
}

/** Wire backend→frontend orchestration events. Call once at startup,
 *  before any orchestrator can be launched. */
export function initOrchestration(grid: Grid, paneEvents: PaneEvents): void {
  void listen<OrchSpawnRequest>("orch-spawn-request", ({ payload }) => {
    // Drop a request whose backend bind wait already elapsed while this
    // frontend was stalled (#106): servicing it now would open a zombie pane
    // against a torn-down config. Breadcrumb-visible console line, no toast —
    // the human never asked for this pane directly, so a toast would be noise.
    if (isSpawnRequestExpired(payload.deadline_ms ?? 0, Date.now())) {
      console.warn(
        `[loomux] dropped expired spawn request agent=${payload.agent_id} ` +
          `group=${payload.group_id} deadline_ms=${payload.deadline_ms}`
      );
      return;
    }
    // An MCP spawn_agent request: open the pane in the background so it doesn't
    // steal focus from where the human is typing (#117).
    void openAgentPane(grid, paneEvents, payload, true);
  });
  // The backend's bind wait for a spawn timed out (#106): it cleaned up the
  // minted config and pending bind. Remember the agent so an in-flight
  // openAgentPane drops it before binding, and close any pane already opened
  // for it so a live frontend doesn't leave a zombie against the dead config.
  void listen<{ group_id: string; agent_id: string }>(
    "orch-spawn-cancelled",
    ({ payload }) => {
      // Note it for an openAgentPane still mid-open (pane not yet created), which
      // reads the set right after its pane opens; the in-flight call's `finally`
      // clears the note. Also close any pane already opened for this agent so a
      // live frontend doesn't leave a zombie against the now-deleted config.
      cancelledSpawns.add(payload.agent_id);
      for (const pane of grid.allPanes()) {
        if (pane.orchAgentId === payload.agent_id) discardStalePane(grid, pane);
      }
    }
  );
  void listen<{ agent_id: string; pty_id: number | null }>("orch-focus", ({ payload }) => {
    if (payload.pty_id === null) return;
    const pane = grid.findByPtyId(payload.pty_id);
    if (pane) {
      grid.setActive(pane);
      pane.focus();
    }
  });
  // The orchestrator (or a human rename echoed back) renamed an agent pane
  // (#95r): retitle it. The backend only emits renames it accepted under the
  // precedence ladder, so a human-owned title never arrives back as an
  // orchestrator override — no frontend guard needed. setName is idempotent.
  void listen<{ agent_id: string; pty_id: number | null; name: string }>(
    "orch-rename",
    ({ payload }) => {
      if (payload.pty_id === null) return;
      const pane = grid.findByPtyId(payload.pty_id);
      if (pane) pane.setName(payload.name);
    }
  );
  // Attention routing: the backend pushes the full current set of panes that
  // need the human every scan; badge each pane by its pty (absent = clear).
  // Idempotent per pane, so re-emits every few seconds are cheap. Applies to
  // ALL panes with a pty, not just orchestration agents — a plain shell the
  // human opened to run a CLI that's now blocked on a prompt must light up too
  // (#40); the backend emits `waiting` items for those, keyed only by pty_id.
  void listen<AttentionItem[]>("orch-attention", ({ payload }) => {
    const byPty = new Map<number, AttentionItem>();
    for (const it of payload) if (it.pty_id !== null) byPty.set(it.pty_id, it);
    // allPanes(), not panes(): a minimized pane still needs the human, and its
    // dock chip mirrors the state (see Grid.renderDock / #6).
    for (const pane of grid.allPanes()) {
      if (pane.ptyId === null) continue;
      const it = byPty.get(pane.ptyId);
      pane.setAttention(it ? it.reason : null, it?.detail);
    }
  });
  // End-orchestration: the backend has already killed the group's agents, so
  // close their (now-dead) panes rather than leaving a screen of dead
  // terminals — the pane-by-pane ✕-clicking this action exists to replace.
  void listen<{ group_id: string }>("orch-group-ended", ({ payload }) => {
    // allPanes(), not panes(): a minimized group pane must be closed too, or
    // it would linger in the dock (with a live agent) after its group ends.
    for (const pane of panesInGroup(grid.allPanes(), payload.group_id)) {
      grid.closePane(pane, false);
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
  // Human clicked a recorded session in the browser — focus the restored pane.
  if (spec) await openAgentPane(grid, paneEvents, spec, false);
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
    orchestratorCli: config.orchestratorCli,
    workerCli: config.workerCli,
    reviewerCli: config.reviewerCli,
    plannerCli: config.plannerCli,
    initialWorkers: config.initialWorkers,
    maxAgents: config.maxAgents,
    workerModel: config.workerModel,
    reviewerModel: config.reviewerModel,
    orchestratorModel: config.orchestratorModel,
    plannerModel: config.plannerModel,
    autoOps: config.autoOps,
    idleKillMinutes: config.idleKillMinutes,
    maxSpawnsPerHour: config.maxSpawnsPerHour,
    watchdogStallMinutes: config.watchdogStallMinutes,
  });
  // Human launched the orchestrator from the UI — focus its pane.
  await openAgentPane(grid, paneEvents, spec, false);
}

// ---------- cost containment: pause/resume + per-group usage ----------

/** Token counts for one agent (exact, from its session transcript). */
export interface UsageTokens {
  input: number;
  output: number;
  cache_creation: number;
  cache_read: number;
  total: number;
}

/** One agent's usage within a group summary. */
export interface AgentUsage {
  id: string;
  name: string;
  role: string;
  /** Whether this agent is currently live (vs a recycled/killed one that still
   *  counts toward the lifetime total). */
  live: boolean;
  /** `transcript` (token-derived), `statusline` (last-resort CLI parse), or
   *  `none` (nothing available yet). */
  source: "transcript" | "statusline" | "none";
  /** Model the cost was priced against, or null. */
  model: string | null;
  /** Dollar cost, or null when only tokens are known (unknown model / no data). */
  cost_usd: number | null;
  /** true = dollars estimated from the price table; false = reported by the CLI. */
  estimated: boolean;
  tokens: UsageTokens;
}

/** Aggregated per-group cost/usage (backend `orch_group_usage`), with a live
 *  vs lifetime split so killed panes still count. */
export interface GroupUsage {
  group: string;
  cli: string;
  /** Cost across currently-live agents, or null if none has a figure. */
  live_cost_usd: number | null;
  /** Cost across all agents ever in this group (survives kills), or null. */
  lifetime_cost_usd: number | null;
  /** How to read the live total: token-`estimated`, CLI-`reported`, or a
   *  `mixed` blend of both; null when there is no figure. */
  live_cost_basis: "estimated" | "reported" | "mixed" | null;
  /** Same, for the lifetime total. */
  lifetime_cost_basis: "estimated" | "reported" | "mixed" | null;
  live_tokens: number;
  lifetime_tokens: number;
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
  /** Current adjustable live-agent cap (guardrail), or null if the group is
   *  unknown to the registry. Drives the GroupView stepper. */
  max_agents: number | null;
  /** Live delegates — workers + reviewers + planners (what counts against
   *  `max_agents`; the orchestrator is exempt). Lowering the cap below this
   *  blocks new spawns. */
  live_delegates: number;
  paused: boolean;
  /** Group uptime (from the earliest live agent), or null if none are live. */
  uptime_ms: number | null;
  roles: { orchestrator: number; worker: number; reviewer: number; planner: number };
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
