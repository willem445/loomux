// Pure task-board helpers, kept DOM/Tauri-free so they can be unit-tested
// (tasksview.ts wires the DOM + IPC and imports these). See test/taskboard.test.ts.

/** Just the field the board's "delete all done" affordance keys off. */
export interface HasStatus {
  status: string;
}

/** Just the field the board's multi-select keys off. */
export interface HasId {
  id: string;
}

/** Prune a multi-select down to the ids that still name a row on the board,
 *  returning a fresh set. Selection is frontend-only, so it can outlive the
 *  rows it points at — the orchestrator (or a completed batch delete) can
 *  remove a task the human had ticked. Run this on every board refresh so the
 *  "delete selected (N)" count reflects what is actually there, and so a stale
 *  id can't be sent to a later delete. */
export function retainExisting(selected: Iterable<string>, tasks: readonly HasId[]): Set<string> {
  const present = new Set(tasks.map((t) => t.id));
  const live = new Set<string>();
  for (const id of selected) if (present.has(id)) live.add(id);
  return live;
}

/** The terminal status whose tasks the "delete all done" action clears. Must
 *  match the backend's `done` status string (validated in orchestration). */
export const DONE_STATUS = "done";

/** How many tasks are in the terminal `done` status. Drives the board's
 *  "delete all done" button: it appears only when this is > 0 and reports the
 *  count. The backend recomputes the actual set at delete time — this is just
 *  the human-facing hint — so the two can't disagree on what gets removed. */
export function doneCount(tasks: readonly HasStatus[]): number {
  return tasks.reduce((n, t) => (t.status === DONE_STATUS ? n + 1 : n), 0);
}
