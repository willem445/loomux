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
import { sessionIdFromCommand } from "./panerestore";
import type { AutonomyState } from "./autonomy";
import type { WorkflowPreview } from "./roster";
import { showToast } from "./toast";
import { showContextMenu } from "./contextmenu";
import { buildPaneMenu, type PaneConnectState, type PaneMenuAction, type PendingConnect } from "./panemenu";
import { reduceConnect, channelBadge, dropIfStale } from "./channel";

export type { AutonomyState };
export type { WorkflowPreview };

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
  /** Extra per-pane env (#83): the gh-shim PATH + `LOOMUX_GROUP_DIR` that enforce
   *  the merge gate on agent panes. `[key, value]` pairs; absent on an older
   *  backend or for panes with nothing extra to inject. */
  env?: [string, string][];
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
  /** Cost guardrail (#83): autonomous-era token budget applied at creation
   *  (0 = no cap). The backend `create_orchestration` command has no budget
   *  parameter, so `launchOrchestrator` applies this via `setAutonomyBudget`
   *  right after the group is created. */
  autonomyBudgetTokens: number;
  /** The advanced-orchestrator toggle (#222). OFF (the default) = the group
   *  ignores `<repo>/.loomux/workflow.yml` entirely and runs the four roles above
   *  — loomux's behavior before workflows existed, unchanged. ON = the repo's
   *  workflow file is loaded and validated, and ITS blocks are the roster (the
   *  per-role picks above then apply only as the CLI a block inherits when it
   *  names none). A launch choice, persisted with the group. */
  advancedOrchestrator: boolean;
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

// ---------- autonomous mode (#83) ----------

/** Enable/disable autonomous idle-tick mode for a group (durable, audited).
 *  Enabling anchors the budget meter at the group's current spend; disabling is
 *  the explicit consent needed to resume after a budget suspension (re-enabling
 *  re-anchors). */
export const setAutonomous = (groupId: string, enabled: boolean): Promise<void> =>
  invoke("orch_set_autonomous", { groupId, enabled });

/** Enable/disable the auto-merge gate for a group (durable, audited). Default
 *  OFF = the human merges; ON lets the orchestrator merge an adequately-tested
 *  PR itself. The human-facing control frames this as its inverse — a "require
 *  approval" checkbox — so callers pass `enabled = auto_merge`, not the checkbox
 *  value (see `autoMergeFromApproval`). */
export const setAutoMerge = (groupId: string, enabled: boolean): Promise<void> =>
  invoke("orch_set_auto_merge", { groupId, enabled });

/** Enable/disable the auto-release gate for a group (#83), independent of
 *  auto-merge. Default OFF = releases/tags need a per-tag human grant; ON lets the
 *  orchestrator publish releases itself while autonomous. Rejects enable unless
 *  autonomous is on (the follow-on UI locks the checkbox accordingly). */
export const setAutoRelease = (groupId: string, enabled: boolean): Promise<void> =>
  invoke("orch_set_auto_release", { groupId, enabled });

/** Enable/disable supervised dangerous mode (#83): the human, present and
 *  supervising, authorizes the orchestrator to merge/release itself WITHOUT
 *  autonomous mode. Mutually exclusive with autonomous — rejects enable while
 *  autonomous is on; enabling autonomous force-clears it. */
export const setDangerousMode = (groupId: string, enabled: boolean): Promise<void> =>
  invoke("orch_set_dangerous_mode", { groupId, enabled });

/** Set a group's autonomous-era token budget (0 = no cap; durable, audited).
 *  Resolves to the applied value. Does not move the enable-time anchor, so
 *  raising the budget after a suspension lets the human resume without losing
 *  already-counted spend. */
export const setAutonomyBudget = (groupId: string, tokens: number): Promise<number> =>
  invoke<number>("orch_set_autonomy_budget", { groupId, tokens });

/** The whole autonomous-mode panel state in one read: toggles, budget, its
 *  enable-time anchor, the spend metered since enable (`null` when off),
 *  `suspended` (budget enforcer turned autonomy off), and the idle-tick
 *  observability (status, countdown, minutes/floor knobs). */
export const autonomyState = (groupId: string): Promise<AutonomyState> =>
  invoke<AutonomyState>("orch_autonomy", { groupId });

/** Set a group's idle-tick window in minutes (0 → backend default 5; clamped
 *  1..1440; durable, audited). Resolves to the applied value. */
export const setIdleTickMinutes = (groupId: string, minutes: number): Promise<number> =>
  invoke<number>("orch_set_idle_tick_minutes", { groupId, minutes });

/** Set a group's idle-tick activity floor in bytes — output below this per
 *  interval counts as idle, making the quiet clock repaint-tolerant (0 → backend
 *  default 2048; clamped 1..1MiB; durable, audited). Resolves to the applied value. */
export const setIdleActivityFloor = (groupId: string, bytes: number): Promise<number> =>
  invoke<number>("orch_set_idle_activity_floor", { groupId, bytes });

// ---------- human merge / release grants (#83) ----------

/** Approve a merge-gate task: flip it done, write a one-time merge grant for its
 *  PR, and deliver the optional `comment` to the orchestrator with the grant
 *  (null = grant only, no note). Resolves to the updated task (callers that only
 *  need success can ignore it). The grant is single-use and expires after ~30
 *  min — see `grantMerge`. `comment` is optional so pre-existing callers that
 *  approved without a note keep working. */
export const approveTask = (
  groupId: string,
  id: string,
  comment: string | null = null
): Promise<unknown> => invoke("orch_approve_task", { groupId, id, comment });

/** Issue a one-time human merge grant for a PR directly (board-independent path):
 *  authorizes exactly one default-branch merge of that PR, single-use and
 *  expiring after ~30 min. Optional `comment` is delivered to the orchestrator.
 *  Human-only (no MCP tool can write a grant). Resolves to the grant nonce. */
export const grantMerge = (
  groupId: string,
  pr: string,
  comment: string | null = null
): Promise<number> => invoke<number>("orch_grant_merge", { groupId, pr, comment });

/** Issue a one-time human release/tag grant: authorizes exactly one publish of
 *  `tag` (GH release + npm). Releases are NEVER blanket-allowed by autonomous
 *  mode, so this explicit grant is the only path. Single-use, ~30-min TTL.
 *  Optional `comment` is delivered to the orchestrator. Human-only. */
export const grantRelease = (
  groupId: string,
  tag: string,
  comment: string | null = null
): Promise<void> => invoke("orch_grant_release", { groupId, tag, comment });

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
      env: req.env,
      badge: badgeFor(req),
      orchGroup: req.group_id,
      orchRole: req.role,
      orchAgent: req.agent_id,
      // Record the session id the backend embedded in the command (#194.5) so
      // capture() persists it — a group resume then restores exactly the captured
      // members from their own sessions, not the full historical roster.
      sessionId: sessionIdFromCommand(req.command, req.argv ?? null) ?? undefined,
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
      // Rehydrate a channel chip that predates this pane (#271: a rejoin/respawn
      // while its channel is still live in the registry) — see hydratePaneChannel.
      void hydratePaneChannel(pane, req.group_id, req.agent_id);
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

/** Where an orchestration event should act: a grid and the pane-events to open
 *  panes into it. With project tabs (#63) there are N grids (one per tab). */
export interface OrchTarget {
  grid: Grid;
  paneEvents: PaneEvents;
}

/** The tab layer, as the orchestration event router sees it (#63). Its
 *  implementation (main.ts, over TabManager) owns tab creation/switching; this
 *  module owns the backend-event plumbing and calls into it. Keeping the
 *  interface here means orchestration.ts has no dependency on the concrete tabs
 *  module (avoids a cycle) while every event routes to the right tab. */
export interface OrchWiring {
  /** Grid+events a group's spawns open into. Creates and binds a project tab on
   *  first sight of the group (named from the spawn request / repo). */
  targetForGroup(req: OrchSpawnRequest): OrchTarget;
  /** Locate a pane by pty across ALL tabs (rename, cancel sweep). */
  findByPty(ptyId: number): Pane | undefined;
  /** Every grid across every tab (spawn-cancel sweep, group-ended close). */
  allGrids(): Grid[];
  /** Focus the pane for `ptyId`, switching to its tab first (orch-focus). */
  focusPty(ptyId: number): void;
  /** Apply an attention scan across all tabs: badge each pane by its pty AND
   *  badge the tab-bar entry of any tab that owns a needs-attention pty. */
  applyAttention(items: AttentionItem[]): void;
  /** Force a tab-strip re-render (#271): channel membership is derived live from
   *  each pane's state (tabcounts.ts), not tracked in a maintained per-tab map the
   *  way attention is, so there is no setter that already triggers one. Called
   *  after every `orch-channel` event so the tab-strip dot doesn't wait for the
   *  next 4s status poll. */
  refreshTabBar(): void;
}

/** Wire backend→frontend orchestration events. Call once at startup,
 *  before any orchestrator can be launched. */
export function initOrchestration(wiring: OrchWiring): void {
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
    // Route the spawn to the group's own tab (creating one on first sight).
    // Background open so it doesn't steal focus from where the human is typing
    // (#117). Focus/attention/rename later locate this pane by scanning live
    // panes across tabs (findByPty), so there's no per-pty binding to maintain.
    const { grid, paneEvents } = wiring.targetForGroup(payload);
    void openAgentPane(grid, paneEvents, payload, true);
  });
  // The backend's bind wait for a spawn timed out (#106): it cleaned up the
  // minted config and pending bind. Remember the agent so an in-flight
  // openAgentPane drops it before binding, and close any pane already opened
  // for it (in whichever tab) so a live frontend doesn't leave a zombie.
  void listen<{ group_id: string; agent_id: string }>(
    "orch-spawn-cancelled",
    ({ payload }) => {
      cancelledSpawns.add(payload.agent_id);
      for (const grid of wiring.allGrids()) {
        for (const pane of grid.allPanes()) {
          if (pane.orchAgentId === payload.agent_id) discardStalePane(grid, pane);
        }
      }
    }
  );
  // Focus: switch to the pane's TAB first, then focus the pane (#63).
  void listen<{ agent_id: string; pty_id: number | null }>("orch-focus", ({ payload }) => {
    if (payload.pty_id === null) return;
    wiring.focusPty(payload.pty_id);
  });
  // The orchestrator (or a human rename echoed back) renamed an agent pane
  // (#95r): retitle it in whichever tab it lives. The backend only emits renames
  // it accepted under the precedence ladder, so a human-owned title never
  // arrives back as an orchestrator override — no frontend guard. Idempotent.
  void listen<{ agent_id: string; pty_id: number | null; name: string }>(
    "orch-rename",
    ({ payload }) => {
      if (payload.pty_id === null) return;
      wiring.findByPty(payload.pty_id)?.setName(payload.name);
    }
  );
  // Attention routing: the backend pushes the full current set of panes that
  // need the human every scan. Applied across ALL tabs — a hidden tab's blocked
  // agent must still badge its tab strip entry (#63) — reusing the same
  // attention.ts mapping the pane header and dock chip use. Also covers plain
  // panes keyed only by pty (#40), not just orchestration agents.
  void listen<AttentionItem[]>("orch-attention", ({ payload }) => {
    wiring.applyAttention(payload);
  });
  // End-orchestration: the backend has already killed the group's agents, so
  // close their (now-dead) panes across every tab rather than leaving a screen
  // of dead terminals — the pane-by-pane ✕-clicking this action replaces.
  void listen<{ group_id: string }>("orch-group-ended", ({ payload }) => {
    let kept = 0;
    for (const grid of wiring.allGrids()) {
      // allPanes(): a minimized group pane must be closed too, or it would
      // linger in the dock (with a live agent) after its group ends.
      for (const pane of panesInGroup(grid.allPanes(), payload.group_id)) {
        // …unless the human left unsaved edits in that pane's Alt+F editor (#219).
        // Ending a group is a deliberate, confirmed act — but what it is deliberately
        // destroying is AGENTS, not the human's own half-written file, and the two got
        // conflated because both live in the same pane. The agent is already dead, so
        // keeping the pane costs nothing; disposing it costs work nobody agreed to lose.
        // The pane stays with its exit banner (which says the editor is unsaved), and
        // closing it later asks like any human close. Same rule the PTY-exit reaper
        // follows — automatic teardown never destroys a buffer.
        if (pane.hasUnsavedWork()) {
          kept++;
          continue;
        }
        grid.closePane(pane, false);
      }
    }
    if (kept > 0) {
      showToast(
        kept === 1
          ? "Group ended. One pane stayed open — it has unsaved edits in its file editor."
          : `Group ended. ${kept} panes stayed open — they have unsaved edits in their file editors.`,
        "info"
      );
    }
  });
  // Cross-workspace channel membership (#271): the backend pushes the current
  // membership on every connect/join/disconnect/teardown. Matched across ALL
  // tabs by agent id — the SAME cross-tab match `orch-spawn-cancelled` above
  // uses — since agent ids are globally unique in the registry (orchbadge.ts).
  void listen<OrchChannelEvent>("orch-channel", ({ payload }) => {
    applyChannelEvent(payload, wiring);
  });
}

/** Apply one `orch-channel` event across every open pane in every tab.
 *
 *  - `connected`: `members` is the channel's FULL current membership (a fresh
 *    2-party channel, or a third pane joining an existing one) — set/refresh the
 *    chip on every matching pane.
 *  - `disconnected`: `members` is who's left (still active) — refresh their
 *    chips (a departed peer changes the tooltip) and clear the pane named in
 *    `agent`.
 *  - `closed`: membership dropped below 2, so the backend tore the WHOLE channel
 *    down — `members` (0 or 1 stranded leftover) plus `agent` (the disconnector)
 *    are both cleared; there is no "still active" set. */
function applyChannelEvent(payload: OrchChannelEvent, wiring: OrchWiring): void {
  const activeIds = payload.kind === "closed" ? new Set<string>() : new Set(payload.members.map((m) => m.agent_id));
  const clearIds = new Set<string>();
  if (payload.agent) clearIds.add(payload.agent);
  if (payload.kind === "closed") for (const m of payload.members) clearIds.add(m.agent_id);

  for (const grid of wiring.allGrids()) {
    for (const pane of grid.allPanes()) {
      // #271 W3 addendum: a standalone pane's channel agent id lives on the
      // dedicated `channelAgent` carrier, not `orchAgentId` — check both.
      const agentId = pane.orchAgentId ?? pane.channelAgentAgentId;
      if (!agentId) continue;
      if (activeIds.has(agentId)) {
        pane.setConnected(channelBadge(payload.channel_id, payload.members, agentId));
      } else if (clearIds.has(agentId)) {
        pane.setConnected(null);
      }
    }
  }
  if (payload.kind === "closed" && payload.agent) {
    showToast(`Channel ${payload.channel_id} closed — a peer disconnected.`, "info");
  } else if (payload.kind === "updated") {
    showToast(`Channel ${payload.channel_id}'s sender changed to ${payload.sender ?? "?"}.`, "info");
  }
  wiring.refreshTabBar();
}

// ---------- pane connect-menu wiring (#271, human-only OPT-IN gesture) ----------
//
// There is at most one armed connect source live at a time, globally, across every
// tab — module-level state, mirroring `cancelledSpawns` above (also DOM/backend
// glue state that doesn't belong in a pure module). `reduceConnect` (channel.ts) is
// the pure state-transition function; this is just the DOM/backend shell around it:
// build the menu, dispatch the fired action, make the backend call, toast errors.

let pendingConnect: PendingConnect | null = null;
let pendingPane: Pane | null = null;

function paneConnectState(pane: Pane): PaneConnectState {
  // #271 W3 addendum: an orchestration-group pane's identity always wins when
  // present; a standalone pane's channel identity lives on the SEPARATE
  // `channelAgent` carrier (never `orchGroup`/`orchAgent` — that would light
  // up the full orchestration chrome for a plain standalone pane). Every
  // orchestration-group agent (orchestrator/worker/reviewer/planner) is
  // minted with a token at spawn, so it can always be the sender; a
  // standalone pane's `canSend` reflects whatever `orch_solo_prepare`/
  // `orch_solo_adopt` actually gave it.
  const group = pane.orchGroupId ?? pane.channelAgentGroupId;
  const agentId = pane.orchAgentId ?? pane.channelAgentAgentId;
  const role = pane.orchRole ?? pane.channelAgentRole;
  const canSend = pane.orchGroupId !== null ? true : pane.channelAgentCanSend;
  const badge = pane.channelBadge;
  return {
    group,
    agentId,
    name: pane.name,
    role,
    channelId: pane.channelId,
    canSend,
    senderId: badge?.senderId ?? null,
    senderName: badge?.senderName ?? null,
  };
}

function setPending(next: PendingConnect | null, source: Pane | null): void {
  pendingPane?.setPendingConnect(false);
  pendingConnect = next;
  pendingPane = next ? source : null;
  pendingPane?.setPendingConnect(true);
}

/** Drop a stale armed source (review finding #286-1) before it can render a
 *  menu label naming a pane that's gone. `channel.ts`'s `dropIfStale` is the
 *  pure decision (unit-tested: alive → unchanged, dead → null); this is just
 *  the DOM shell supplying the one fact it can't observe itself
 *  (`Pane.isDisposed`) and applying the result. */
function dropStalePending(): void {
  if (pendingPane && dropIfStale(pendingConnect, !pendingPane.isDisposed) === null) {
    setPending(null, null);
  }
}

/** The reserved standalone pseudo-group id — mirrors mod.rs's `SOLO_GROUP`
 *  constant (#271 W3 addendum, part A1). */
export const SOLO_GROUP = "__solo__";

/** Adopt-on-connect (#271 W3 addendum, part A3): on the FIRST Connect gesture
 *  against an agent pane with no channel identity yet — launched before this
 *  feature, or on a CLI the launcher didn't eagerly mint one for — register it
 *  as a delivery-only member so it stops hitting `NOT_CAPABLE_REASON`. A no-op
 *  for a pane that already has an identity (orchestration OR channelAgent), and
 *  for a shell/content pane (`!pane.isAgentPane`) — those stay not-capable, per
 *  the addendum's Worker-split note. Best-effort: a failed adopt just leaves the
 *  menu showing `NOT_CAPABLE_REASON` this time, retried on the next right-click. */
async function adoptIfEligible(pane: Pane): Promise<void> {
  if (pane.orchGroupId || pane.channelAgentAgentId) return;
  if (!pane.isAgentPane || pane.ptyId === null) return;
  try {
    const { agent_id } = await soloAdopt(pane.ptyId, pane.name, pane.workdir ?? "");
    // Adopted panes are ALWAYS delivery-only (soloAdopt mints no token) — see
    // `OrchRegistry::solo_adopt`.
    pane.setChannelAgent({ group: SOLO_GROUP, agentId: agent_id, role: "solo", canSend: false });
  } catch {
    /* best-effort — falls back to NOT_CAPABLE this time */
  }
}

/** Right-click on a pane header (#271): show the Connect/Disconnect menu built
 *  from this pane's current state and the (global, cross-tab) armed connect
 *  source. Wired from `PaneEvents.onPaneContextMenu`. */
export async function showPaneConnectMenu(pane: Pane, x: number, y: number): Promise<void> {
  dropStalePending();
  await adoptIfEligible(pane);
  const items = buildPaneMenu(paneConnectState(pane), pendingConnect);
  showContextMenu(x, y, items, (action) => void handlePaneMenuAction(action, pane));
}

/** The pane's own channel chip was clicked (#271's one-click "easy close").
 *  Wired from `PaneEvents.onDisconnectChannel`. */
export function disconnectPaneChannel(pane: Pane): void {
  const group = pane.orchGroupId ?? pane.channelAgentGroupId;
  const agentId = pane.orchAgentId ?? pane.channelAgentAgentId;
  if (!group || !agentId) return;
  const state = paneConnectState(pane);
  void handlePaneMenuAction(
    { kind: "disconnect", pane: { group, agentId, name: pane.name, canSend: state.canSend, senderId: state.senderId, senderName: state.senderName, channelId: state.channelId } },
    pane
  );
}

/** Esc cancels an in-progress connect gesture from anywhere — a no-op if
 *  nothing is armed. Wired from main.ts's global keydown handler. */
export function cancelPendingConnect(): void {
  if (!pendingConnect) return;
  setPending(null, null);
}

async function handlePaneMenuAction(action: PaneMenuAction, pane: Pane): Promise<void> {
  const { pending, effect } = reduceConnect(action, pendingConnect);
  // Only "connect-arm" legitimately introduces a NEW pending source pane — every
  // other action either leaves `pending` exactly as it was (a disconnect of some
  // UNRELATED pane while a different one is armed elsewhere: `pane` here is the
  // disconnect target, not the armed pane, so it must never become `pendingPane`)
  // or clears it to null (cancel, complete, or a disconnect that WAS the armed
  // source). Passing `pane` in the unchanged case would move the pulsing "armed"
  // outline onto the wrong pane.
  if (action.kind === "connect-arm") setPending(pending, pane);
  else if (pending === null) setPending(null, null);
  switch (effect.kind) {
    case "none":
      if (action.kind === "connect-arm") {
        showToast(`Connecting ${action.source.name}… right-click another pane to complete, Esc to cancel.`, "info");
      } else if (action.kind === "connect-cancel") {
        showToast("Connect cancelled.", "info");
      }
      return;
    case "connect":
      try {
        await channelConnect(effect.from.group, effect.from.agentId, effect.to.group, effect.to.agentId, effect.senderAgent);
      } catch (err) {
        showToast(`Connect failed: ${String(err)}`, "error");
      }
      return;
    case "disconnect":
      try {
        await channelDisconnect(effect.group, effect.agentId);
      } catch (err) {
        showToast(`Disconnect failed: ${String(err)}`, "error");
      }
      return;
    case "set-sender":
      try {
        await channelSetSender(effect.channelId, effect.newSenderAgent);
      } catch (err) {
        showToast(`Making this pane the sender failed: ${String(err)}`, "error");
      }
      return;
  }
}

/** Best-effort rehydration of a pane's channel chip right after it (re)opens
 *  (#271): `orch-channel` only fires on live mutations, but the channel itself
 *  can predate this pane's open — a mid-run rejoin/respawn while its channel is
 *  still live in the registry. A failed read is not worth surfacing to the
 *  human; the chip simply stays off until the next mutation event. */
async function hydratePaneChannel(pane: Pane, group: string, agentId: string): Promise<void> {
  try {
    const ch = await channelForPane(group, agentId);
    if (ch) pane.setConnected(channelBadge(ch.id, ch.members, agentId));
  } catch {
    /* best-effort */
  }
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
): Promise<{ groupId: string } | null> {
  const spec = await invoke<OrchSpawnRequest | null>("resume_orch_session", {
    sessionId,
    groupHint: hint?.group ?? null,
    roleHint: hint?.role ?? null,
  });
  if (!spec) return null;
  // Human clicked a recorded session in the browser — focus the restored pane.
  // Return the group so the caller can bind it to the tab (#63); the pane itself
  // is located later by scanning live panes (findByPty), so it isn't returned.
  await openAgentPane(grid, paneEvents, spec, false);
  return { groupId: spec.group_id };
}

/** Create/resume the group for `config.repo` and open its orchestrator
 *  pane. The backend spawns the initial idle workers (as spawn-request
 *  events) once the orchestrator binds. */
export async function launchOrchestrator(
  grid: Grid,
  paneEvents: PaneEvents,
  config: OrchestratorConfig
): Promise<{ groupId: string }> {
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
    advancedOrchestrator: config.advancedOrchestrator,
  });
  // #83: create_orchestration has no budget parameter (W1's frozen contract), so
  // apply any launcher-collected autonomous budget via the setter now the group
  // exists. Best-effort: a failed budget write must not sink the launch — the
  // group is already up and the human can set it live from the panel. 0 = no cap,
  // which is the backend default anyway, so skip the round-trip.
  if (config.autonomyBudgetTokens > 0) {
    try {
      await setAutonomyBudget(spec.group_id, config.autonomyBudgetTokens);
    } catch (err) {
      showToast(`autonomy budget not applied: ${String(err)}`, "info");
    }
  }
  // Human launched the orchestrator from the UI — focus its pane. Return the
  // group so the caller binds it to the tab (#63); the pane is located later by
  // scanning live panes (findByPty), so it isn't returned.
  await openAgentPane(grid, paneEvents, spec, false);
  return { groupId: spec.group_id };
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

// ---------- CI watches (#243/#248): the group view's "⏳ waiting on …" indicator ----------

/** One live `notify_when` watch, as surfaced across a whole group's agents —
 *  the same registry state the `notify_when`/`list_notifications` MCP tools
 *  read, not a second store. Unlike `list_notifications` (self-scoped by
 *  design, MCP-callable by an agent), this is read group-wide because it's a
 *  Tauri command reached only from the trusted webview. */
export interface GroupWatch {
  id: string;
  /** The watching agent's id — lets groupview.ts group rows by agent. */
  agent: string;
  /** "pr_checks" | "workflow_run". */
  kind: string;
  /** Human label, e.g. "PR #241 checks" / "run 17812". */
  target: string;
  /** The agent's own note, verbatim (may be empty). */
  note: string;
  /** Absolute Unix-ms deadline. */
  expires_ms: number;
}

/** Every live watch for a group's agents, for the group view's per-agent
 *  indicator. */
export const groupWatches = (groupId: string): Promise<GroupWatch[]> =>
  invoke<GroupWatch[]>("orch_group_watches", { groupId });

// ---------- group lifecycle: summary + end-orchestration (#8) ----------

/** One live agent in a group lifecycle summary. */
export interface AgentSummary {
  id: string;
  name: string;
  role: OrchRole;
  /** The workflow block this agent IS (#222) — equal to `role` for the built-in
   *  roster, a declared block id (`rev-security`) for a workflow group. Absent on
   *  a payload from a backend that predates blocks. */
  block?: string;
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

// ---------- the advanced orchestrator (#222) ----------

/** What turning the advanced orchestrator ON for `repo` would run: the resolved
 *  roster from `<repo>/.loomux/workflow.yml`, or every validation finding if the
 *  file is broken (in which case the group still launches, on the built-in
 *  roster). Read by the launcher before the human hits Create, so they see the
 *  blocks — and the repo-authored personas — they are enabling.
 *
 *  The backend resolves this through the same load-and-clamp path `create_group`
 *  uses, so the preview cannot drift from the launch. It never rejects: a missing
 *  or broken file is a described outcome, not an error. `agentCli` is the group's
 *  default CLI, which a block with no `cli:` of its own inherits. */
export const workflowPreview = (repo: string, agentCli: string): Promise<WorkflowPreview> =>
  invoke<WorkflowPreview>("orch_workflow_preview", { repo, agentCli });

// ---------- cross-workspace channels (#271): human-only connect/disconnect ----------
//
// A channel is a human-connected set of two-or-more agent panes, possibly in
// different orchestration groups/tabs. The connect/disconnect gesture and
// every membership mutation are Tauri commands only — there is deliberately
// no MCP tool an agent can call to open/close/join one (CLAUDE.md constraint
// 5/6). An agent's only surface is the `channel_send`/`channel_status` MCP
// tools, which broadcast/read against the membership graph a human built.
//
// This module exposes the typed command wrappers and the `orch-channel`
// event payload shape; wiring them into pane chrome (the connect menu, the
// header chip, the `orch-channel` listener) is the UI slice's job.

/** One member of a channel, as the backend resolves it — cached name/role so
 *  a rendered chip/roster doesn't need a second agent lookup. `direction`/
 *  `can_send`/`delivery_only` are the #271 W3 addendum's directional fields
 *  (part B7/A4). */
export interface ChannelMember {
  group: string;
  agent_id: string;
  name: string;
  role: OrchRole | "solo";
  direction: "sender" | "receiver";
  can_send: boolean;
  delivery_only: boolean;
}

/** A live cross-workspace channel (backend `Channel`). */
export interface OrchChannel {
  id: string;
  created_ms?: number;
  sender: string;
  members: ChannelMember[];
}

/** Connect two agent panes (possibly in different groups) into a channel.
 *  Human-only. Per the backend's join rules: both free mints a new channel;
 *  one free + one already-connected joins the free pane into that channel
 *  (multi-party); both already connected to different channels is rejected.
 *
 *  `senderAgent` (#271 W3 addendum, part B) means something different for a
 *  MINT than a JOIN (review round 2, B1 — this ambiguity was the bug):
 *  - **Fresh mint** (neither pane connected): `senderAgent` DESIGNATES the
 *    new channel's sender — must be `fromAgent` or `toAgent`, and that pane
 *    must hold a channel token (a delivery-only pane can never be the
 *    sender).
 *  - **Join** (either pane already connected): the channel's sender already
 *    exists; `senderAgent` only CONFIRMS who that is, and is very often
 *    neither `fromAgent` nor `toAgent` — the completion gesture can land on
 *    ANY existing member (the sender, or a plain receiver), and the true
 *    sender may be a third pane entirely. Pass the target channel's actual
 *    current sender (`PaneIdentity.senderId`), not either connect-call
 *    argument.
 *
 *  Returns the resulting channel. */
export const channelConnect = (
  fromGroup: string,
  fromAgent: string,
  toGroup: string,
  toAgent: string,
  senderAgent: string,
): Promise<OrchChannel> =>
  invoke<OrchChannel>("orch_channel_connect", { fromGroup, fromAgent, toGroup, toAgent, senderAgent });

/** Result of disconnecting one pane from its channel. */
export interface ChannelDisconnectResult {
  channel_id: string;
  /** True if membership dropped below 2 — OR the disconnected pane was the
   *  channel's sender (#271 W3 addendum, part B: a star topology has exactly
   *  one hub) — and the whole channel was torn down. */
  closed: boolean;
  remaining: number;
}

/** Disconnect one agent pane from its channel. Human-only; tears the channel
 *  down (and strands/notifies any remaining member) if this drops it below
 *  2 members, or if the disconnected pane was the sender. */
export const channelDisconnect = (group: string, agent: string): Promise<ChannelDisconnectResult> =>
  invoke<ChannelDisconnectResult>("orch_channel_disconnect", { group, agent });

/** Every live channel, for cross-tab indicators on tab switch. */
export const channelList = (): Promise<OrchChannel[]> => invoke<OrchChannel[]>("orch_channel_list", {});

/** The channel one pane belongs to, or null — for a single pane's header
 *  chip on tab switch / reconnect. */
export const channelForPane = (group: string, agent: string): Promise<OrchChannel | null> =>
  invoke<OrchChannel | null>("orch_channel_for_pane", { group, agent });

/** Reassign a channel's sender without reconnecting (#271 W3 addendum, part
 *  B5). Human-only; `newSenderAgent` must already be a member and hold a
 *  token. Clears every member's reply credit and notifies both roles. */
export const channelSetSender = (channelId: string, newSenderAgent: string): Promise<OrchChannel> =>
  invoke<OrchChannel>("orch_channel_set_sender", { channelId, newSenderAgent });

/** Payload of the `orch-channel` event, emitted by the backend on every
 *  connect/disconnect/teardown/sender-swap so cross-tab UI (chips, dock
 *  mirror, tab-strip dot) can update without polling. */
export interface OrchChannelEvent {
  kind: "connected" | "disconnected" | "closed" | "updated";
  channel_id: string;
  /** Present on disconnected/closed: the pane that left. */
  agent?: string;
  /** Present on connected/updated: the channel's current sender. */
  sender?: string;
  /** Current membership after the change (empty on `closed`). */
  members: ChannelMember[];
}

// ---------- standalone panes (#271 W3 addendum, part A) ----------
//
// A standalone (launcher) pane has no orchestration group. These mint/bind/
// adopt a channel-scoped MCP identity for it — human-only Tauri commands,
// reached from the launcher's agent-pane spawn path (`solo_prepare`/
// `solo_bind`) or the pane-menu Connect gesture against a pane with none yet
// (`solo_adopt`). See `OrchRegistry::solo_prepare`/`solo_bind`/`solo_adopt`.

/** What `orch_solo_prepare` returns: the minted agent id, the exact per-CLI
 *  flag string to append to the launched command line (empty for a
 *  delivery-only CLI), and whether this pane ended up delivery-only (no
 *  config seam for its CLI — codex/gemini/opencode/custom today). */
export interface SoloPrepared {
  agent_id: string;
  mcp_args: string;
  delivery_only: boolean;
}

/** Mint a channel-scoped identity for a newly-launching standalone pane
 *  BEFORE it boots, so `mcp_args` can be appended to its command line. Call
 *  once per new agent pane, from the launcher's spawn path — never for
 *  terminal/content panes (#271 W3 addendum: "gate eager solo-prepare to
 *  agent panes only"). */
export const soloPrepare = (cli: string, cwd: string, name: string): Promise<SoloPrepared> =>
  invoke<SoloPrepared>("orch_solo_prepare", { cli, cwd, name });

/** Bind a just-spawned solo pane's pty to the `AgentEntry` `soloPrepare`
 *  created. Call right after `spawnPty` resolves, mirroring the
 *  orchestration group's `bind_agent` round trip. */
export const soloBind = (agentId: string, ptyId: number): Promise<void> =>
  invoke("orch_solo_bind", { agentId, ptyId });

/** Adopt an already-running pane (no channel identity yet — launched before
 *  this feature, or on a CLI the human didn't opt into channel tools for) as
 *  a delivery-only member, on its first Connect gesture. Idempotent by pty:
 *  re-adopting an already-adopted pty returns its existing agent id. */
export const soloAdopt = (ptyId: number, name: string, cwd: string): Promise<{ agent_id: string }> =>
  invoke<{ agent_id: string }>("orch_solo_adopt", { ptyId, name, cwd });
