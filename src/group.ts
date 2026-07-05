// Pure, DOM-free helpers for orchestration group membership. Kept here so the
// selection logic (e.g. which panes to close when a group ends) is unit-testable
// without pulling in the Tauri event/IPC layer that orchestration.ts imports.

/** The subset of panes belonging to `groupId` — the set to close when that
 *  group ends. Operates on anything exposing `orchGroupId`, so it's independent
 *  of whether a pane is visible or minimized (the caller decides the input
 *  set). Panes with a null group, or a different group, are excluded. */
export function panesInGroup<T extends { orchGroupId: string | null }>(
  panes: T[],
  groupId: string
): T[] {
  return panes.filter((p) => p.orchGroupId === groupId);
}

/** A group member as the "minimize/restore whole group" toggle (#46) sees it:
 *  its role and whether it is currently docked (minimized out of the grid). */
export interface GroupPaneState {
  orchGroupId: string | null;
  /** "orchestrator" | "worker" | "reviewer" | null. */
  orchRole: string | null;
  /** True while parked in the dock (out of the split tree). */
  minimized: boolean;
}

export type GroupMinimizeAction = "minimize" | "restore";

/** Plan the group-minimize toggle: which panes to act on and whether to
 *  minimize or restore them. `targets` is already narrowed to exactly the
 *  panes the action applies to. */
export interface GroupMinimizePlan<T> {
  action: GroupMinimizeAction;
  targets: T[];
}

/** Decide what the "minimize/restore whole group" toggle (#46) should do.
 *
 *  The members it operates on are every pane in `groupId` EXCEPT the
 *  orchestrator — the toggle lives on the orchestrator's own pane, which stays
 *  put so the human keeps a foothold on the group. If ANY member is currently
 *  visible, the toggle minimizes all visible members (folding the group down to
 *  just the orchestrator); once they are all docked, it restores them all.
 *
 *  Returns the action plus the exact panes to act on, or `null` when the group
 *  has no worker/reviewer members to act on at all (nothing to toggle). Pure so
 *  the selection/direction decision is unit-testable without the grid/DOM. */
export function planGroupMinimize<T extends GroupPaneState>(
  panes: T[],
  groupId: string
): GroupMinimizePlan<T> | null {
  const members = panesInGroup(panes, groupId).filter(
    (p) => p.orchRole !== "orchestrator"
  );
  if (members.length === 0) return null;
  const visible = members.filter((p) => !p.minimized);
  if (visible.length > 0) return { action: "minimize", targets: visible };
  return { action: "restore", targets: members };
}
