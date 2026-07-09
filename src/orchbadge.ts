// Pure orchestration identity/badge derivation — no Tauri or DOM imports, so
// it's unit-testable under `node --test` (mirrors attention.ts / group.ts).
// orchestration.ts (which talks to the backend and opens panes) imports from
// here. The one job of this module: turn a backend spawn/rejoin request into
// the pane's role chip, so the chip always shows the SAME registry id the task
// board and roster show (issue #75) — never a per-group ordinal.

import type { PaneBadge } from "./pane";

export type OrchRole = "orchestrator" | "worker" | "reviewer" | "planner";

// Per-group identity: a stable accent color keyed off the order groups first
// appear. Color is the group-pairing cue ("this orchestrator ↔ its workers");
// the per-agent id (below) is the cross-reference cue. Groups are few; palette
// wrap collisions are fine because the id still disambiguates every agent.
const GROUP_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e"];

interface GroupMeta {
  color: string;
  /** 1-based order this group first appeared — indexes the color palette. */
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

/** Reset the per-group color assignment. Test-only seam. */
export function resetGroupMeta(): void {
  groupMeta.clear();
}

const ROLE_LABELS: Record<OrchRole, string> = {
  orchestrator: "ORCH",
  worker: "W",
  reviewer: "REV",
  planner: "PLAN",
};

/** The static chip a tab shows when it OWNS an orchestration group (#177). Keyed
 *  on the tab→group binding (TabManager.groupForWorkspace), NOT on live status —
 *  so a restored-but-dormant orchestrator tab is identifiable before its group
 *  is resumed, unlike the live count+cost chip which only shows for a running
 *  group. Returns the label, or null for a plain tab. The glyph is the same
 *  "ORCH" role tag the pane badge uses, so the tab and its panes read as one. */
export function orchTabLabel(groupId: string | null | undefined): string | null {
  return groupId ? ROLE_LABELS.orchestrator : null;
}

/** The minimal identity a badge needs. `OrchSpawnRequest` is a structural
 *  superset, so spawn AND rejoin requests both satisfy it. */
export interface BadgeAgent {
  group_id: string;
  agent_id: string;
  role: OrchRole;
}

/** The registry id is `${prefix}-${seq}` (e.g. `w-7`, `rev-5`), with `seq`
 *  globally unique across the whole registry. The badge shows that seq so the
 *  chip ("W 7") cross-references 1:1 to the task board / roster id ("w-7").
 *  Falls back to the whole id if it isn't in the expected shape. */
export function agentSeq(agentId: string): string {
  const dash = agentId.lastIndexOf("-");
  const seq = dash >= 0 ? agentId.slice(dash + 1) : "";
  return /^\d+$/.test(seq) ? seq : agentId;
}

/** Build the pane's role chip for an orchestration agent. The label is the
 *  real registry id (role tag + minted seq) — NOT a per-group ordinal — so a
 *  pane badge and the task-board/roster row for the same agent always match
 *  (issue #75). This is derived fresh from the backend request every open,
 *  including session restore/rejoin, so a restored agent shows whatever id the
 *  registry actually assigned it. The human-facing pane title is separate and
 *  stays renameable; the badge here never overwrites it. */
export function badgeFor(req: BadgeAgent): PaneBadge {
  const meta = metaForGroup(req.group_id);
  return {
    label: `${ROLE_LABELS[req.role] ?? "AGENT"} ${agentSeq(req.agent_id)}`,
    color: meta.color,
    title: `${req.role} · ${req.agent_id} · group ${req.group_id}`,
  };
}
