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

/** The board's status vocabulary, in picker order. Mirrors the backend's
 *  TASK_STATUSES (validated there) — the frontend only offers these; the
 *  backend rejects anything else on write. */
export const STATUSES = [
  "queued",
  "in-progress",
  "review",
  "pr",
  "prototype",
  "human-testing",
  "done",
  "blocked",
] as const;

/** The demo-gate status (#147): a prototype awaiting the human's promote/scrap
 *  verdict. Must match the backend's `prototype` status string. */
export const PROTOTYPE_STATUS = "prototype";

/** Whether the board should show the **Proceed** button on an item — only a
 *  prototype can be promoted. The backend enforces the same guard
 *  (`ensure_prototype`), so this just governs whether the affordance appears. */
export function canProceed(status: string): boolean {
  return status === PROTOTYPE_STATUS;
}

/** Statuses only the human can move forward, highlighted on the board so what
 *  is waiting on you stands out (attention routing #6). `prototype` belongs
 *  here — it's parked on the human's demo verdict, like the merge gates and
 *  `blocked`. */
export function isAwaitingHuman(status: string): boolean {
  return (
    status === "pr" ||
    status === "human-testing" ||
    status === "blocked" ||
    status === PROTOTYPE_STATUS
  );
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

/** The row-level accent a task's status gets on the board (#339): `queued`
 *  is deliberately un-accented (nothing to highlight, it's waiting its turn)
 *  and the human-gated statuses already get `isAwaitingHuman`'s amber — this
 *  covers the two left uncovered. `in-progress`/`review` pick up the exact
 *  accent color their own status pill already uses, so the row itself reads
 *  as "currently moving" at a glance instead of only the small pill text;
 *  `done` dims a settled task out of the way so it recedes behind what's
 *  still active. Returns the status verbatim (it doubles as the CSS
 *  modifier the caller appends), or null for no extra treatment. */
export function taskRowAccentClass(status: string): "in-progress" | "review" | "done" | null {
  if (status === "in-progress" || status === "review" || status === DONE_STATUS) return status;
  return null;
}
