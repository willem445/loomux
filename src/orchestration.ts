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
  initialWorkers: number;
  maxAgents: number;
  workerModel: string;
  reviewerModel: string;
  orchestratorModel: string;
  fullAuto: boolean;
}

// Stable per-group accent colors so all panes of one orchestration read as
// a set. Groups are few; collisions after the palette wraps are fine.
const GROUP_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e"];
const groupColors = new Map<string, string>();

export function colorForGroup(groupId: string): string {
  let c = groupColors.get(groupId);
  if (!c) {
    c = GROUP_COLORS[groupColors.size % GROUP_COLORS.length];
    groupColors.set(groupId, c);
  }
  return c;
}

const ROLE_LABELS: Record<OrchRole, string> = {
  orchestrator: "ORCH",
  worker: "W",
  reviewer: "REV",
};

export function badgeFor(req: OrchSpawnRequest): PaneBadge {
  return {
    label: ROLE_LABELS[req.role] ?? "AGENT",
    color: colorForGroup(req.group_id),
    title: `${req.role} · ${req.agent_id} · group ${req.group_id}`,
  };
}

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
    initialWorkers: config.initialWorkers,
    maxAgents: config.maxAgents,
    workerModel: config.workerModel,
    reviewerModel: config.reviewerModel,
    orchestratorModel: config.orchestratorModel,
    fullAuto: config.fullAuto,
  });
  await openAgentPane(grid, paneEvents, spec);
}
