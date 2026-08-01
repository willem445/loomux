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

/** Just the field a merge grant needs a value in. */
export interface HasPrRef {
  pr?: string | null;
}

/** How many of an approvable selection will actually be authorized — the rows
 *  carrying a PR reference (#507). An item at the gate with no PR is approved
 *  and marked done, but nothing is granted for it, so any count the board
 *  states as *grants* must be this one and not the selection size: promising N
 *  grants and issuing fewer is a claim the code doesn't honor.
 *
 *  "Linked", not "grantable": only the backend resolves a ref to the PR NUMBER
 *  a grant is keyed on, so a ref that parses to nothing (`"TBD"`) is counted
 *  here and lands in the notice's no-PR-number sentence there. That gap is why
 *  the board says "one per **linked** PR" — the property it can actually see —
 *  rather than duplicating the backend's parser to guess at the other one. */
export function grantableCount(tasks: readonly HasPrRef[]): number {
  return tasks.filter((t) => !!t.pr && t.pr.trim() !== "").length;
}

/** The status a task must reach before anything depending on it can start
 *  (#582) — mirrors the backend's `dep_satisfied` (mod.rs). Merged/accepted is
 *  the bar deliberately: a dep sitting at `pr` or `human-testing` is work the
 *  human hasn't signed off yet, so a dependent starting on it would be
 *  building on something that can still come back. */
const DEP_SATISFIED_STATUS = DONE_STATUS;

/** The one status readiness is defined over (#582). Must match the backend's
 *  `queued` — nothing else is ever "startable", however unblocked it is. */
export const QUEUED_STATUS = "queued";

/** A board row as far as the dependency chips care (#582).
 *
 *  Both link fields are OPTIONAL on the wire, not merely possibly-empty: the
 *  backend serializes them with `skip_serializing_if = "Vec::is_empty"`, so a
 *  link-free task arrives over `orch_tasks` with **no `deps` key at all** —
 *  which is exactly what every pre-#582 board looks like. Missing and empty
 *  therefore mean the same thing to every helper below. */
export interface HasLinks extends HasId, HasStatus {
  deps?: readonly string[] | null;
  related?: readonly string[] | null;
}

/** What a single dependency edge is doing to the task that declares it:
 *  `met` (the dep reached `done`), `unmet` (it exists but hasn't), or
 *  `missing` (no task on the board carries that id). */
export type DepState = "met" | "unmet" | "missing";

/** Classify one dependency id against the board (#582).
 *
 *  `missing` is rendered as its own chip state rather than being folded into
 *  `unmet`, because the two have different causes and different fixes: an
 *  unmet dep is work still to do, a missing one is a link naming nothing —
 *  only reachable by hand-editing `tasks.json`, since the backend validates
 *  ids on write and strips them from survivors on delete. It still counts as
 *  NOT satisfied (see `unmetDeps`); the distinction is for the human's eye,
 *  never for readiness. */
export function depState(id: string, board: readonly HasLinks[]): DepState {
  const dep = board.find((t) => t.id === id);
  if (!dep) return "missing";
  return dep.status === DEP_SATISFIED_STATUS ? "met" : "unmet";
}

/** The task's dependency ids that are not satisfied yet, in its own link
 *  order — the board-side mirror of the backend's `unmet_deps` (mod.rs).
 *
 *  An id naming no live task counts as unmet, never as satisfied: reading a
 *  typo as "satisfied" would silently unblock work, which is the failure
 *  direction that matters. Kept a straight scan (quadratic in the worst case)
 *  because a board is 10–100 rows and this runs once per render, same argument
 *  as `board_summaries` backend-side. */
export function unmetDeps(task: HasLinks, board: readonly HasLinks[]): string[] {
  return (task.deps ?? []).filter((id) => depState(id, board) !== "met");
}

/** Derived readiness (#582): `queued` AND every dep `done`. The board mirrors
 *  the backend's `task_ready` rather than reading a `ready` flag off the wire,
 *  because the human's board reads full `Task`s via `orch_tasks` — `ready` is
 *  a `TaskSummary` field, and `TaskSummary` is the MCP `list_tasks` row the
 *  orchestrator gets, not this path. The rules are duplicated on purpose and
 *  pinned by tests on both sides; the alternative (a second derived field on
 *  the human command) would be a new wire shape for something the board can
 *  compute exactly from data it already has.
 *
 *  Like the backend, this is a read-time projection only: nothing here ever
 *  writes a status, so a wrong link can never wedge a task. `related` never
 *  participates. */
export function isReady(task: HasLinks, board: readonly HasLinks[]): boolean {
  return task.status === QUEUED_STATUS && unmetDeps(task, board).length === 0;
}

/** Whether this board uses dependencies at all (#582) — the gate for the
 *  "ready" affordance.
 *
 *  Every queued task on a dep-free board is trivially ready, so marking them
 *  would put a badge on every queued row of every existing board and mean
 *  nothing: the mark exists to separate "startable now" from "waiting on
 *  something", a distinction that only exists once some task declares a dep.
 *  Gated on the WHOLE board rather than per-task on purpose: once any task has
 *  deps, a plain queued row genuinely is startable and should say so — telling
 *  the human only about the linked rows would leave the rest ambiguous. */
export function boardUsesDeps(board: readonly HasLinks[]): boolean {
  return board.some((t) => (t.deps?.length ?? 0) > 0);
}

/** The tasks the "add a dependency" picker offers for a row: every other task
 *  on the board, minus itself and the ones it already depends on, in board
 *  (priority) order so the list reads like the board above it.
 *
 *  Deliberately does NOT pre-filter choices that would close a cycle. The
 *  backend rejects those inside its lock with an error naming the path, and
 *  that error surfaces through the board's existing `mutate()` toast — a
 *  frontend cycle walk would be a second copy of a rule that has to stay
 *  authoritative in one place, and it could only ever disagree. */
export function depCandidates<T extends HasLinks>(task: T, board: readonly T[]): T[] {
  const already = new Set(task.deps ?? []);
  return board.filter((t) => t.id !== task.id && !already.has(t.id));
}

/** The full `deps` array to send after the human adds one (#582). Every board
 *  dep edit sends the WHOLE array — the backend's array args are
 *  replace-or-untouched, never a delta — so these two helpers are what
 *  "adding" and "removing" actually mean on this path. Adding an id that is
 *  already there is a no-op rather than a duplicate (the backend dedups too,
 *  first-wins; agreeing here keeps the sent array identical to the stored
 *  one). */
export function withDep(deps: readonly string[] | null | undefined, id: string): string[] {
  const current = [...(deps ?? [])];
  return current.includes(id) ? current : [...current, id];
}

/** The full `deps` array to send after the human removes one — see `withDep`.
 *  Removing the last dep sends `[]`, which the backend reads as "clear", not
 *  as "leave untouched" (that is what omitting the argument means). */
export function withoutDep(deps: readonly string[] | null | undefined, id: string): string[] {
  return [...(deps ?? [])].filter((d) => d !== id);
}
