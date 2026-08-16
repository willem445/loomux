// Pure orchestration identity/badge derivation — no Tauri or DOM imports, so
// it's unit-testable under `node --test` (mirrors attention.ts / group.ts).
// orchestration.ts (which talks to the backend and opens panes) imports from
// here. The one job of this module: turn a backend spawn/rejoin request into
// the pane's role chip, so the chip always shows the SAME registry id the task
// board and roster show (issue #75) — never a per-group ordinal.

import type { PaneBadge } from "./pane";
import { IDENTITY } from "./theme.ts";

/** A capability class, as the backend's `Role` serializes it. `manager` (#1161)
 *  is declarable only in a repo's `.loomux/workflow.yml` — it is never part of
 *  the built-in roster, so widening this union deliberately does NOT widen
 *  `ORCH_ROLES` (roster.ts), which is the launcher's per-role form AND the
 *  built-in roster itself. */
export type OrchRole = "orchestrator" | "worker" | "reviewer" | "planner" | "manager";

// Per-group identity: a stable accent color keyed off the order groups first
// appear. Color is the group-pairing cue ("this orchestrator ↔ its workers");
// the per-agent id (below) is the cross-reference cue. Groups are few; palette
// wrap collisions are fine because the id still disambiguates every agent.
// Identity, not state: which GROUP this pane belongs to (#879 slice B). Six of the eight
// hues, in wheel order — rose and amber lead the warm arc, and `lime`/`orchid` are held back
// so the set stays the one the tab bar and the channel chips also draw from.
const GROUP_COLORS = [
  IDENTITY.azure,
  IDENTITY.jade,
  IDENTITY.amber,
  IDENTITY.violet,
  IDENTITY.cyan,
  IDENTITY.rose,
];

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
  // #1161. `MGR`, matching the agent-id prefix (`mgr-3`) the backend mints, so
  // the chip and the id it cross-references read as the same word.
  manager: "MGR",
};

/** The short chip text for a role ("REV"). The one source for it: the pane badge
 *  and the group panel's roster row both read this, so a pane and its row can
 *  never label the same agent differently. (`groupview.ts` kept its own copy for a
 *  while, and it silently missed `planner` — every planner showed as "AGENT".)
 *  Unknown roles — a payload from a newer backend — degrade to "AGENT" rather
 *  than to an empty chip. */
export function roleLabel(role: string): string {
  return ROLE_LABELS[role as OrchRole] ?? "AGENT";
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
    label: `${roleLabel(req.role)} ${agentSeq(req.agent_id)}`,
    color: meta.color,
    title: `${req.role} · ${req.agent_id} · group ${req.group_id}`,
  };
}
