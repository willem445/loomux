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
