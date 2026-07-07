// Pure task-board helpers, kept DOM/Tauri-free so they can be unit-tested
// (tasksview.ts wires the DOM + IPC and imports these). See test/taskboard.test.ts.

/** Just the field the board's "delete all done" affordance keys off. */
export interface HasStatus {
  status: string;
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
