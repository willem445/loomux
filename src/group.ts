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

/** What the group panel says when the roster declares a manager and none is
 *  live (#1433, #1161 M5). `null` — say nothing — for every other combination,
 *  which is nearly every group: most declare no manager at all.
 *
 *  **This is the whole human-facing answer to "the manager is not there", and
 *  it is deliberately a NOTICE rather than a repair.** Nothing in orrerix
 *  reopens a manager pane automatically, because it cannot tell the two reasons
 *  apart: `docs/features/manager.md` promises the human "if you close the
 *  manager pane, the group behaves as it always has", so closing it is a
 *  legitimate act — and a pane that crashed is indistinguishable from one the
 *  human deliberately closed. Auto-reopening would contradict a shipped promise
 *  on a guess. So the app states the fact and names the route back; the human
 *  decides. See `doc/design/manager.md`, "Why nothing reopens a dead manager".
 *
 *  It covers BOTH of #1433's cases with one surface, because from here they are
 *  the same fact: the launch-time open failed, or the pane died later. What the
 *  human needs in either case is to know the pane is not there and how to get
 *  it back — not which of the two happened, which the audit log records anyway.
 *
 *  `live` is a count and not a bool on purpose: it is `roles.manager` straight
 *  off the summary, and a group can never hold two live managers
 *  (`MANAGER_MAX` is 1, enforced at parse), so anything above zero is "the pane
 *  is there". */
export function managerAbsenceNotice(
  declared: boolean,
  live: number
): { text: string; title: string } | null {
  if (!declared || live > 0) return null;
  return {
    text: "manager declared · not open",
    title:
      "This group's workflow file declares a manager pane — your own interface to the " +
      "group — and none is live. Either it could not be opened at launch (the group's " +
      "audit log says why) or it has since been closed or died. Nothing reopens it " +
      "automatically, on purpose: closing that pane is something you are allowed to do, " +
      "and orrerix cannot tell that apart from a crash. Bring it back from the session " +
      "browser; until then the orchestrator takes your input in its own pane, exactly as " +
      "it does for a group with no manager.",
  };
}
