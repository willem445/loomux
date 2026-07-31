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

/** Statuses where an assignee is actually doing something right now, as
 *  opposed to `queued` (nothing assigned yet) or the human-gated statuses
 *  `isAwaitingHuman` already covers. */
const WORKING_STATUSES = new Set(["in-progress", "review"]);

/** The board's single source of truth for "is this task actually being
 *  worked on right now" (#339 refinement). The first cut of this highlight
 *  keyed off status alone, and a human live-testing it found the exact gap
 *  that leaves: an old assignee chip left over from a killed, resumed, or
 *  reassigned session read as indistinguishable from a live agent currently
 *  at the keyboard. So a working-status task is `"active"` ONLY when its
 *  assignee is in the caller's live-agent set (from the group's own agent
 *  roster, e.g. `groupSummary()`'s `agents` list) — otherwise it's `"idle"`:
 *  assigned, working-status, but nobody is actually there, which must never
 *  be visually confused with real active work. `done` always reads as
 *  settled regardless of assignee/liveness. Everything else (queued, and the
 *  human-gated statuses `isAwaitingHuman` covers) is untouched here — `null`. */
export type TaskActivity = "active" | "idle" | "done" | null;
export function taskActivityState(
  status: string,
  assignee: string | null | undefined,
  liveAgentIds: ReadonlySet<string>
): TaskActivity {
  if (status === DONE_STATUS) return "done";
  if (WORKING_STATUSES.has(status)) {
    return assignee && liveAgentIds.has(assignee) ? "active" : "idle";
  }
  return null;
}

/** The status a task moves to when changes are requested on it (#339
 *  refinement): back to a working state, never left sitting at `pr`/
 *  `human-testing` where the merge-gate Approve button would still show
 *  despite a human having just asked for changes — the board must not imply
 *  reopened work is ready. Reusing `in-progress` (rather than inventing a
 *  distinct sub-state) keeps this a plain status transition: `canApprove`
 *  below already excludes it, so nothing else has to remember to hide the
 *  button separately. */
export const REQUEST_CHANGES_STATUS = "in-progress";

/** Whether the board's merge-gate actions (Approve & allow merge / Changes)
 *  should show for a task's current status (#339 refinement — pins what was
 *  previously an inline condition in tasksview.ts). Only `pr`/`human-testing`
 *  are the human's actual decision points; once a status moves off either
 *  (whether via `REQUEST_CHANGES_STATUS` above or a fresh review cycle
 *  landing), the gate closes on its own. */
export function canApprove(status: string): boolean {
  return status === "pr" || status === "human-testing";
}

/** The rows a bulk **Approve selected** can act on: the ticked ones that are
 *  actually at the merge gate, in board order (#507).
 *
 *  The board has ONE selection, shared with "delete selected" — the human can
 *  tick a `queued` row and an at-the-gate row in the same pass. Approve is an
 *  authority action, so it narrows to what `canApprove` allows rather than
 *  approving whatever was ticked: the count on the button, the rows listed in
 *  the confirm dialog, and the ids actually sent are all THIS list, so what
 *  the human is told they are authorizing is exactly what gets a grant. The
 *  backend re-checks the gate on every id and refuses the whole batch if one
 *  moved off it, so a board that shifted between render and click can't
 *  produce a half-approved batch.
 *
 *  Board order (not tick order) so the dialog reads like the board above it. */
export function approvableSelection<T extends HasId & HasStatus>(
  selected: Iterable<string>,
  tasks: readonly T[]
): T[] {
  const ticked = new Set(selected);
  return tasks.filter((t) => ticked.has(t.id) && canApprove(t.status));
}
