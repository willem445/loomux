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

/** A board row as far as readiness cares: its links, plus the container chain
 *  readiness climbs (#958 slice R). Both halves are optional on the wire, so a
 *  pre-#582 or pre-#958 row satisfies this exactly as it stands. */
export type ReadinessRow = HasLinks & HasParent;

/** Derived readiness (#582, extended by #958 slice R): `queued`, every dep
 *  `done`, AND every container above it clear too (`blockingAncestor`). The
 *  board mirrors the backend's `task_ready` rather than reading a `ready` flag
 *  off the wire, because the human's board reads full `Task`s via `orch_tasks`
 *  — `ready` is a `TaskSummary` field, and `TaskSummary` is the MCP
 *  `list_tasks` row the orchestrator gets, not this path. The rules are
 *  duplicated on purpose and pinned by tests on both sides; the alternative (a
 *  second derived field on the human command) would be a new wire shape for
 *  something the board can compute exactly from data it already has.
 *
 *  Like the backend, this is a read-time projection only: nothing here ever
 *  writes a status, so a wrong link — or a hand-edited container — can never
 *  wedge a task. `related` never participates, at any level. */
export function isReady(task: ReadinessRow, board: readonly ReadinessRow[]): boolean {
  return (
    task.status === QUEUED_STATUS &&
    unmetDeps(task, board).length === 0 &&
    // Defined with the rest of the hierarchy helpers below, since that is where
    // the one ancestor walk lives — this call reaches it by hoisting.
    blockingAncestor(task, board) === null
  );
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

// ---------------------------------------------------------------------------
// Board hierarchy (#958): containment, not ordering.
//
// `parent` names the row this one sits inside; `deps` still names what must
// finish first, and the two are orthogonal (a dep may cross subtrees). The
// board array stays FLAT and its order stays the priority order — every tree
// below is derived from `parent` at render time, exactly like `isReady` is
// derived rather than read off the wire.
// ---------------------------------------------------------------------------

/** The advisory Agile levels, in the backend's `TASK_KINDS` order (#958).
 *  Advisory means advisory: a story directly inside an epic is legal, and the
 *  board only ever *labels* a row with this — nothing here gates anything. */
export const KINDS = ["epic", "feature", "story", "task"] as const;

/** Which of a row's pickers is open: the dependency one (ordering), the
 *  container one (nesting), or the Agile-level one (#958 slice K). */
export type PickerField = "dep" | "parent" | "kind";

/** The board's single open picker, if any — one at a time across every row and
 *  every field. */
export interface PickerTarget {
  id: string;
  field: PickerField;
}

/** What the picker state becomes when a picker button is clicked: open it, or
 *  close it if that exact picker was already open. Clicking a DIFFERENT picker
 *  — other row, or a different field on this row — replaces the open one. */
export function nextPicker(
  open: PickerTarget | null,
  id: string,
  field: PickerField
): PickerTarget | null {
  const same = open !== null && open.id === id && open.field === field;
  return same ? null : { id, field };
}

/** Whether a picker's own deferred close still owns the open picker.
 *
 *  A picker takes focus when it opens, and its `blur` schedules the close on a
 *  timeout so the click that caused the blur lands first. That means the close
 *  runs AFTER whatever the click did — so it has to re-ask whether the picker
 *  it belongs to is still the open one, by BOTH signals. Reading only the row
 *  id swallows a click exactly the width of the missing signal: with the nest
 *  picker open on a row, clicking that same row's dependency button opens the
 *  dep picker and then lets the queued close tear it down in the same tick, so
 *  the click reads as having done nothing at all. The button that decides to
 *  open reads two signals, so the close that can undo it reads the same two. */
export function pickerIsOpen(
  open: PickerTarget | null,
  id: string,
  field: PickerField
): boolean {
  return open !== null && open.id === id && open.field === field;
}

/** Whether this board uses containment at all (#958) — the gate for the
 *  nesting chrome, exactly as `boardUsesDeps` gates the readiness mark.
 *
 *  On a board where nothing is nested, the collapse column is 13px of empty
 *  gutter in front of every row and the indent rail can never appear, so the
 *  affordances are suppressed and such a board keeps precisely the shape it
 *  has today. A single nested row turns the column on for the whole board:
 *  once nesting exists, a row sitting at the top level is saying something,
 *  and it needs the same left edge as the rows that aren't. */
export function boardUsesHierarchy<T extends HasParent>(board: readonly T[]): boolean {
  return board.some((t) => !!t.parent);
}

/** A board row as far as the hierarchy helpers care. Both fields are optional
 *  on the wire for the same reason `deps` is: the backend skips them when
 *  absent, so every pre-#958 board arrives with no keys at all. */
export interface HasParent extends HasId {
  parent?: string | null;
  kind?: string | null;
}

/** The derived tree: which rows sit at top level, and each row's direct
 *  children, both in board (priority) order. */
export interface TaskTree<T extends HasParent> {
  roots: T[];
  children: Map<string, T[]>;
}

/** One rendered board line: the row, how deep it sits, and whether it has
 *  children / is currently collapsed. */
export interface BoardRow<T extends HasParent> {
  task: T;
  depth: number;
  hasChildren: boolean;
  collapsed: boolean;
}

/** How many indent steps the board actually draws. The backend caps writes at
 *  `MAX_TASK_DEPTH` (4), but a hand-edited `tasks.json` can be deeper, and an
 *  unbounded indent would walk such a row off the right edge of the overlay. */
export const MAX_INDENT_DEPTH = 4;

/** Clamp a tree depth to the indent the stylesheet actually draws. */
export function indentLevel(depth: number): number {
  return Math.max(0, Math.min(depth, MAX_INDENT_DEPTH));
}

/** The container chain above a row — its parent, then that row's parent, up to
 *  a root — NEAREST FIRST, with the row itself never included, and only rows
 *  that actually exist on the board (a `parent` naming nothing ends the chain,
 *  which is exactly where `buildTree` puts such a row: the top level).
 *
 *  Terminates on any board, the one thing a walk over hand-editable data must
 *  guarantee: a cycle stops at the first repeat, having listed each member
 *  once, and reports that it did — `cyclic` is this same walk's other question.
 *  Mirrors the backend's `find_parent_cycle` (mod.rs), which both of that
 *  side's callers share for the same reason. */
function ancestorChain<T extends HasParent>(
  task: T,
  byId: ReadonlyMap<string, T>
): { chain: T[]; cyclic: boolean } {
  const chain: T[] = [];
  const seen = new Set([task.id]);
  let cur = task.parent ?? null;
  while (cur) {
    if (seen.has(cur)) return { chain, cyclic: true };
    seen.add(cur);
    const row = byId.get(cur);
    if (!row) break;
    chain.push(row);
    cur = row.parent ?? null;
  }
  return { chain, cyclic: false };
}

/** The nearest container above this row whose OWN deps aren't all met (#958
 *  slice R), or `null` when the whole chain is clear — the board-side mirror of
 *  the backend's `blocking_ancestor` (mod.rs), and the id is returned rather
 *  than a boolean so a caller can name the row that is holding this one.
 *
 *  Only an ancestor's `deps` are read, never its `status`: a container sitting
 *  at `in-progress` (or `blocked`, or `pr`) is the normal state while the work
 *  inside it runs, so gating on status would make a subtask's readiness a
 *  function of how promptly the container row is maintained. Containment still
 *  isn't ordering — what climbs the chain here is the ordering primitive
 *  itself, applied to every container: a feature waiting on something outside
 *  it is waiting on it for every slice inside it.
 *
 *  Tolerant like every other hierarchy helper: an orphan container ends the
 *  chain and blocks nothing, and a cycle terminates with each member checked
 *  once. */
export function blockingAncestor(task: ReadinessRow, board: readonly ReadinessRow[]): string | null {
  const byId = new Map(board.map((t) => [t.id, t]));
  for (const anc of ancestorChain(task, byId).chain) {
    if (unmetDeps(anc, board).length > 0) return anc.id;
  }
  return null;
}

/** Build the containment tree from the flat board (#958).
 *
 *  Tolerant by construction, because `tasks.json` is hand-editable and the
 *  backend deliberately runs no repair pass over it: a row whose `parent`
 *  names nothing, names itself, or sits in a cycle is treated as a ROOT rather
 *  than being dropped from the board. Same philosophy as the `missing` dep
 *  chip — a broken link must be visible, never invisible. */
export function buildTree<T extends HasParent>(board: readonly T[]): TaskTree<T> {
  const byId = new Map(board.map((t) => [t.id, t]));
  /** Does walking up from this row ever come back to something it already
   *  passed? The same walk `blockingAncestor` needs, asked its other question,
   *  so the board keeps ONE ancestor walk rather than two that could disagree
   *  about where a hand-edited chain ends. */
  const cyclic = (t: T): boolean => ancestorChain(t, byId).cyclic;
  const roots: T[] = [];
  const children = new Map<string, T[]>();
  for (const t of board) {
    const parent = t.parent ?? null;
    // Registered under its container whenever that container exists at all —
    // including for a cyclic row, which is ALSO listed as a root below. That
    // double listing is what lets `visibleRows` show both halves of a cycle
    // once each: whichever it reaches first renders, and the other is skipped
    // as already seen rather than recursed into.
    if (parent && parent !== t.id && byId.has(parent)) {
      const kids = children.get(parent);
      if (kids) kids.push(t);
      else children.set(parent, [t]);
    }
    if (!parent || parent === t.id || !byId.has(parent) || cyclic(t)) roots.push(t);
  }
  return { roots, children };
}

/** The rows to render, in display order: roots in board order, each followed
 *  by its own subtree (recursively, in board order), with the depth each row
 *  should be indented to.
 *
 *  `collapsed` hides a row's whole subtree, not just its direct children — a
 *  collapsed epic must not leave its grandchildren stranded at the top level.
 *  Every row on the board appears EXACTLY ONCE, whatever `parent` says: that
 *  is the invariant a hand-edited cycle would otherwise break, in either
 *  direction (an infinite render, or a row that silently vanishes). */
export function visibleRows<T extends HasParent>(
  board: readonly T[],
  collapsed: Iterable<string> = []
): BoardRow<T>[] {
  const hidden = new Set(collapsed);
  const { roots, children } = buildTree(board);
  const rows: BoardRow<T>[] = [];
  const seen = new Set<string>();
  // `visible` is threaded down rather than returning early on a collapsed row:
  // the hidden subtree still has to be WALKED, so its rows are marked seen and
  // can't be picked up again by the fallback below as stray top-level rows.
  const walk = (task: T, depth: number, visible: boolean): void => {
    if (seen.has(task.id)) return;
    seen.add(task.id);
    const kids = children.get(task.id) ?? [];
    const isCollapsed = kids.length > 0 && hidden.has(task.id);
    if (visible) rows.push({ task, depth, hasChildren: kids.length > 0, collapsed: isCollapsed });
    for (const k of kids) walk(k, depth + 1, visible && !isCollapsed);
  };
  for (const r of roots) walk(r, 0, true);
  // Nothing should reach this — `buildTree` makes every unreachable row a root
  // — but a row that fell out of the board would be work made invisible, so it
  // renders at top level instead of being trusted away.
  for (const t of board) walk(t, 0, true);
  return rows;
}

/** Direct-child counts for a container row: how many children it has and how
 *  many of those are `done`.
 *
 *  DIRECT children only, deliberately: these are the same two numbers the
 *  backend puts on a `TaskSummary` (`children` / `children_done`), and the
 *  human's board and the orchestrator's `list_tasks` rows disagreeing about a
 *  count they both display would be a defect, not a nuance. */
export function childCounts<T extends HasParent & HasStatus>(
  id: string,
  board: readonly T[]
): { total: number; done: number } {
  const kids = buildTree(board).children.get(id) ?? [];
  return {
    total: kids.length,
    done: kids.reduce((n, k) => (k.status === DONE_STATUS ? n + 1 : n), 0),
  };
}

/** Whether EVERY task under this one — the whole subtree, not just the direct
 *  children — is `done` (#958). Says nothing about the container's OWN status,
 *  which it never reads: a `done` container with a `done` subtree is `true`
 *  here.
 *
 *  Drives a nudge chip only, and the caller pairs it with the container's own
 *  status to make the point the chip actually makes ("finished inside, but this
 *  row's status lags"). It never writes a status (the auto-status rollup was
 *  rejected outright: status has two authors and a derived write-back is
 *  exactly the wedge `ready` avoids by staying derived).
 *
 *  Whole subtree, unlike `childCounts` above, because this one makes a CLAIM
 *  ("everything under here is finished") — direct-children-only would let it
 *  say that with an open grandchild, which is simply false. A row with no
 *  children never qualifies: there is nothing under it to have finished. */
export function subtreeAllDone<T extends HasParent & HasStatus>(
  id: string,
  board: readonly T[]
): boolean {
  const { children } = buildTree(board);
  const seen = new Set([id]); // visited set: a hand-edited cycle must terminate
  const queue = [...(children.get(id) ?? [])];
  let any = false;
  while (queue.length > 0) {
    const t = queue.shift() as T;
    if (seen.has(t.id)) continue;
    seen.add(t.id);
    any = true;
    if (t.status !== DONE_STATUS) return false;
    queue.push(...(children.get(t.id) ?? []));
  }
  return any;
}

/** Where a row sits among its SIBLINGS (not on the board): drives whether the
 *  up/down buttons are disabled. Reordering is sibling-scoped — the first
 *  child of a container has nowhere higher to go, even though the board array
 *  has rows above it. `{ index: -1, count: 0 }` for an id that isn't on the
 *  board. */
export function siblingPosition<T extends HasParent>(
  board: readonly T[],
  id: string
): { index: number; count: number } {
  const siblings = siblingIds(buildTree(board), id);
  const index = siblings.indexOf(id);
  return index < 0 ? { index: -1, count: 0 } : { index, count: siblings.length };
}

/** The id list this row is ordered within (a copy): its container's children,
 *  or the top-level rows.
 *
 *  A row in a hand-edited cycle is listed BOTH ways by `buildTree`, and this
 *  resolves it to the root list. That matches where `visibleRows` renders the
 *  cycle's FIRST member and not the others: a 3-cycle renders three levels
 *  deep, so its second and third members are ordered among the roots while
 *  displayed nested. Their up/down buttons then act on the root list. Left as
 *  is deliberately — the move is still a valid permutation and every row still
 *  renders exactly once (§5 of doc/design/task-hierarchy.md stakes out
 *  tolerate-and-show for cycles), and only a hand-edited `tasks.json` can
 *  produce one at all. */
function siblingIds<T extends HasParent>(tree: TaskTree<T>, id: string): string[] {
  const root = tree.roots.find((t) => t.id === id);
  if (root) return tree.roots.map((t) => t.id);
  for (const kids of tree.children.values()) {
    if (kids.some((k) => k.id === id)) return kids.map((k) => k.id);
  }
  return [];
}

/** The full flattened id order to send to `orch_reorder_tasks` after moving a
 *  row one step among its siblings (#958).
 *
 *  A container moves WITH its subtree: reordering is about priority between
 *  siblings, and a parent that left its children behind would silently
 *  re-home them (the array is flat, so "left behind" means "now sits under
 *  whoever is above them"). Out-of-range moves are a no-op that still returns
 *  the current order, so a caller can send it unconditionally.
 *
 *  Always a permutation of the board — every id present exactly once — since
 *  `reorder_tasks` appends whatever the caller omitted, and an omission would
 *  therefore be a silent priority change nobody asked for. */
export function reorderWithSubtree<T extends HasParent>(
  board: readonly T[],
  id: string,
  delta: number
): string[] {
  const tree = buildTree(board);
  const rootIds = tree.roots.map((t) => t.id);
  const childIds = new Map<string, string[]>();
  for (const [parent, kids] of tree.children) childIds.set(parent, kids.map((k) => k.id));

  /** Depth-first over the (possibly just-reordered) sibling lists: a container
   *  is immediately followed by its own subtree, which is what makes the move
   *  carry the children with it. Visited-guarded for the cyclic board. */
  const flatten = (): string[] => {
    const out: string[] = [];
    const seen = new Set<string>();
    const walk = (rowId: string): void => {
      if (seen.has(rowId)) return;
      seen.add(rowId);
      out.push(rowId);
      for (const k of childIds.get(rowId) ?? []) walk(k);
    };
    for (const r of rootIds) walk(r);
    for (const t of board) walk(t.id); // never omit a row — see the doc above
    return out;
  };

  // The live sibling array (inside rootIds/childIds, so splicing it is what
  // `flatten` then reads) — reordering is scoped to it and to nothing else.
  const siblings = rootIds.includes(id)
    ? rootIds
    : [...childIds.values()].find((kids) => kids.includes(id));
  if (!siblings) return flatten();
  const i = siblings.indexOf(id);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= siblings.length) return flatten();
  siblings.splice(i, 1);
  siblings.splice(j, 0, id);
  return flatten();
}

/** The rows the "nest under…" picker offers: every other row on the board,
 *  minus the one it is already inside (picking that is a no-op write).
 *
 *  Deliberately does NOT filter out choices that would close a hierarchy cycle
 *  or bust the depth cap — the exact call `depCandidates` makes, for the exact
 *  reason: the backend rejects those inside its lock with an error naming the
 *  path, that error surfaces through this view's toast, and a second copy of
 *  the rule here could only ever disagree with the authoritative one. */
export function parentCandidates<T extends HasParent>(task: T, board: readonly T[]): T[] {
  return board.filter((t) => t.id !== task.id && t.id !== task.parent);
}

/** Whether this row's `parent` names no task on the board — only reachable by
 *  hand-editing `tasks.json` (the backend validates on write and re-homes
 *  survivors on delete), so the board says so on the row rather than rendering
 *  it at top level with no explanation. */
export function hasMissingParent<T extends HasParent>(task: T, board: readonly T[]): boolean {
  return !!task.parent && !board.some((t) => t.id === task.parent);
}

/** The rows the "set kind…" picker offers (#958 slice K): every level in
 *  `KINDS` other than this task's current one — picking the level it already
 *  has would be a no-op write, the same reasoning `parentCandidates` uses to
 *  exclude the current container. Unlike `parentCandidates`, this can never
 *  come back empty: `KINDS` has four entries and at most one is excluded.
 *
 *  A task carrying an out-of-vocabulary `kind` (only reachable by hand-editing
 *  `tasks.json` — the backend refuses it on write, same as an invalid
 *  `status`) matches none of `KINDS`, so nothing is excluded and all four
 *  levels are offered; picking one is how the board fixes it back onto the
 *  known vocabulary. */
export function kindCandidates<T extends HasParent>(task: T): readonly string[] {
  return KINDS.filter((k) => k !== task.kind);
}
