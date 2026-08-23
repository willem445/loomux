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

/** The demo-gate statuses (#1091 slice B/C): a board row parked in front of
 *  the human for a LOOK, as opposed to a decision — accepted H5 on #1091.
 *  `PROTOTYPE_STATUS` is the #147 demo gate; `human-testing` is a visible-UI
 *  park. Deliberately NARROWER than `isAwaitingHuman` above, which also
 *  covers `pr` and `blocked`: those are the merge gate and a stall, both
 *  owned by the board, neither a demo to go run.
 *
 *  Owned HERE, not by the NEEDS-YOU panel (`decisions.ts`), because
 *  `decisions.ts` already imports FROM this module (`canApprove`,
 *  `canProceed`) — a copy of this set there, alongside the one the board's
 *  own marker chip needs (#1091 slice G), is exactly the "no new source of
 *  truth" drift the slice-G brief warns against. `decisions.ts` re-exports
 *  both names, so nothing importing them from there has to change. */
export const DEMO_STATUSES = [PROTOTYPE_STATUS, "human-testing"] as const;

export function isDemoGated(status: string): boolean {
  return (DEMO_STATUSES as readonly string[]).includes(status);
}

// ---------------------------------------------------------------------------
// Board marker + deep-link (#1091 slice G): an obvious chip on a row that is
// blocked on a human DECISION or gated on a DEMO, routing through the pane's
// focus hook (embedfocus.ts) to open the NEEDS-YOU panel at that item.
//
// Both signals are DERIVED, never stored: decision-blocked reads the
// pending-questions list the NEEDS-YOU panel already owns, demo-gated reads
// `isDemoGated` above, which the panel's own demo tier already uses. Neither
// adds a field to a task or a second registry.
// ---------------------------------------------------------------------------

/** Just the fields the chip's decision signal needs from a question row.
 *  Structural, like the `Has*` interfaces above, so this module never has to
 *  import `decisions.ts`'s `OrchQuestion` — that module already imports FROM
 *  this one, and a reverse import would cycle. Callers pass only PENDING
 *  questions: this function does not read `status` at all, so `decisions.ts`'s
 *  `isPending` stays the one place "pending" is decided — a second copy of
 *  that rule here is how the two would drift apart. */
export interface PendingQuestionRef {
  id: string;
  task?: string | null;
}

/** t-N → q-N for every row a pending question cites (#1091 slice G).
 *
 *  A blank or absent `task` cites nothing. When more than one pending
 *  question names the same task, the FIRST one in the caller's list order
 *  wins — pending order is ask order, oldest first (`decisions.ts`'s
 *  `projectQuestions` keeps file order for exactly this reason), so a row
 *  links to the oldest still-open ask rather than whichever the caller
 *  happened to iterate last. A question citing a task id that names nothing
 *  on the board still goes in the map — this function never reads the board
 *  — but `boardMarker` below only ever looks a REAL row up by its own id, so
 *  such an entry simply never matches anything and marks nothing. */
export function blockedTaskMap(
  pendingQuestions: readonly PendingQuestionRef[]
): Map<string, string> {
  const map = new Map<string, string>();
  for (const q of pendingQuestions) {
    const t = (q.task ?? "").trim();
    if (!t || map.has(t)) continue;
    map.set(t, q.id);
  }
  return map;
}

/** Which surface the board-marker chip should route the human to — both
 *  route through the same focus hook, just with a different target id. */
export type BoardMarkerKind = "decision" | "demo";

/** What a row's chip is, and what clicking it should focus in the NEEDS-YOU
 *  panel: the citing question's OWN id for a decision marker (its card is
 *  what the human answers), the task's OWN id for a demo marker (there is no
 *  question card for a demo — the demo card's `data-item-id` is the task id
 *  itself, see `decisionsview.ts`'s `demoCard`). */
export interface BoardMarker {
  kind: BoardMarkerKind;
  target: string;
}

/** The chip for one board row, or `null` when nothing is waiting on the human
 *  for it (#1091 slice G).
 *
 *  Decision-blocked wins over demo-gated when a row is somehow both (a
 *  pending question can cite a `human-testing` row while it's also parked for
 *  a demo) — a decision is the more specific, more blocking ask, and the
 *  brief calls for exactly one chip per row, not two competing for the same
 *  corner. */
export function boardMarker(
  task: HasId & HasStatus,
  blocked: ReadonlyMap<string, string>
): BoardMarker | null {
  const q = blocked.get(task.id);
  if (q) return { kind: "decision", target: q };
  if (isDemoGated(task.status)) return { kind: "demo", target: task.id };
  return null;
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
// finish first, and the two are orthogonal (a dep may cross subtrees).
// Orthogonal is not independent: readiness reads BOTH (`blockingAncestor`),
// because a slice inside a waiting feature is waiting too — what never happens
// is one becoming the other. The board array stays FLAT and its order stays the
// priority order — every tree below is derived from `parent` at render time,
// exactly like `isReady` is derived rather than read off the wire.
// ---------------------------------------------------------------------------

/** The Agile levels, in the backend's `TASK_KINDS` order (#958).
 *
 *  ENFORCED since #1156: the ladder below is the backend's `ladder_rule`,
 *  mirrored here so the pickers can offer only what a write would accept. The
 *  backend stays the authority — every refusal it makes surfaces in this view's
 *  toast — and this copy is a convenience, which is a claim #958 explicitly
 *  refused to make for the cycle and depth-cap rules (§9 of
 *  doc/design/task-hierarchy.md). The difference is what the rule reads: a
 *  cycle or a depth bust is a property of the whole mutable tree, re-derived
 *  per candidate, where the ladder is a fixed table over two closed
 *  vocabularies that both sides enumerate in full. What makes a divergence
 *  REDDEN rather than be discovered by a refused write is one specific test —
 *  `the board's ladder table is the backend's, read out of the Rust source`
 *  (test/taskboard.test.ts), which reads `ladder_rule`'s arms out of the Rust
 *  source and compares them to the table below. The two per-side table tests
 *  do NOT buy that: each asserts its own side against its own literals, so
 *  editing one language's rule and its own test leaves the other green. */
export const KINDS = ["epic", "feature", "story", "task"] as const;

/** Where a row of a given kind may sit (#1156) — the mirror of the backend's
 *  `LadderRule` enum, discriminated the same way. */
export type LadderRule =
  | { rule: "exempt" }
  | { rule: "top-level-only" }
  | { rule: "inside"; container: string };

/** The strict Agile ladder (#1156), and the one place this side knows it.
 *
 *  A kind-less row is `exempt` — legal anywhere, permanently. That is not a
 *  migration allowance that ages out: a flat board of plain rows is the right
 *  shape for work with no hierarchy worth describing, and loomux is a generic
 *  tool that must not require a methodology (CLAUDE.md constraint 8). Any
 *  out-of-vocabulary kind (hand-edited `tasks.json` only) is exempt too, for
 *  the same reason the backend exempts it: no rule can name where a fifth
 *  level belongs. */
export function ladderRule(kind: string | null | undefined): LadderRule {
  switch (kind) {
    case "epic":
      return { rule: "top-level-only" };
    case "feature":
      return { rule: "inside", container: "epic" };
    case "story":
      return { rule: "inside", container: "feature" };
    case "task":
      return { rule: "inside", container: "story" };
    default:
      return { rule: "exempt" };
  }
}

/** Whether a row levelled `childKind` may sit directly inside one levelled
 *  `containerKind`. Both sides may be null/absent — an unlevelled container
 *  holds only unlevelled rows, since "inside an epic" is a claim about the
 *  container that a row carrying no level does not make. */
export function mayContain(
  containerKind: string | null | undefined,
  childKind: string | null | undefined
): boolean {
  const rule = ladderRule(childKind);
  if (rule.rule === "exempt") return true;
  if (rule.rule === "top-level-only") return false;
  return containerKind === rule.container;
}

/** Whether a row levelled `kind` may sit at the top level — i.e. whether
 *  "promote out of its container" is a move the backend would accept. */
export function mayBeTopLevel(kind: string | null | undefined): boolean {
  return ladderRule(kind).rule !== "inside";
}

/** The ladder rule for one level, as a phrase for a tooltip. Derived from
 *  `ladderRule` rather than written out per level, so the badge and the
 *  picker button cannot end up describing a rule the board no longer enforces
 *  — the failure mode #1156 is itself correcting on the word "advisory". */
export function levelRuleText(kind: string | null | undefined): string {
  const rule = ladderRule(kind);
  if (rule.rule === "top-level-only") return "it sits at the top level and inside nothing";
  if (rule.rule === "inside") return `it must sit inside ${rule.container === "epic" ? "an" : "a"} ${rule.container}`;
  return "no level, so it may sit anywhere";
}

/** Which of a row's pickers is open: the dependency one (ordering), the
 *  container one (nesting), or the Agile-level one (#958 slice K). */
export type PickerField = "dep" | "parent" | "kind" | "sprint";

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

/** One rendered board line: the row, how deep it sits, whether it has children
 *  / is currently collapsed, and which side of the relevant-first split it
 *  landed on (#1152 — see `settledIds` / `isCleared`). */
export interface BoardRow<T extends HasParent> {
  task: T;
  depth: number;
  hasChildren: boolean;
  collapsed: boolean;
  /** This row and everything under it is `done`, so the board sinks it below
   *  the live work and its position is DERIVED (most recently updated first), not
   *  the human's manual priority order. */
  settled: boolean;
  /** The human archived this row from their working view — this row's OWN
   *  stamp (`isCleared`), which is not the same question as whether the board
   *  is hiding it.
   *
   *  Hiding is decided by `clearedIds`, the whole-subtree closure, so a
   *  **cleared container still holding live work renders with "show cleared"
   *  OFF** and wears this flag: it never left the list, because hiding it would
   *  have taken the live child with it. That is the case `clearedIds` exists to
   *  create, so it is normal, not a leak — the 📥 chip is what explains the
   *  dimming, and the row's own ↩ is the way back. Consequence worth knowing at
   *  the header: `👁 show cleared (N)` and `↩ restore all (N)` both count
   *  `clearedIds`, so both can read 0 and be hidden entirely while such a row
   *  is on screen. */
  cleared: boolean;
  /** This row is on screen only because a match sits somewhere under it
   *  (#1270): it did NOT satisfy the active filter itself. Always `false`
   *  while no filter is active, so a board with the strip untouched is
   *  unchanged. The view renders these as scaffolding — dimmed, not as hits —
   *  which is what keeps a filtered board readable AS a tree instead of as a
   *  flat list of matches that lost their containment. */
  context: boolean;
  /** How many of this row's direct children the current projection actually
   *  rendered. Below `childCounts(id).total` whenever something is being
   *  withheld — the row is collapsed, or the filter cut its children — and the
   *  view shows the `done/total` badge exactly then. Collapsing a container
   *  used to hide its contents AND the only evidence of how much it hid; this
   *  is the number that fixes that, for both causes at once rather than for
   *  collapse alone. */
  shownKids: number;
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

// ---------------------------------------------------------------------------
// Relevant-first order and the cleared archive (#1152).
//
// A long-lived group's board is mostly history: 400+ rows, nearly all `done`,
// with the handful that anyone can act on scattered among them. Two separate
// mechanisms answer that, and keeping them separate is the point:
//
//  1. **Sinking** is automatic and derived. A finished subtree drops below the
//     live work of its own sibling group, most recently updated first. Nothing is
//     stored and nothing is written — it is a projection, exactly like
//     `isReady`.
//  2. **Clearing** is the human's explicit archive action, and it IS stored
//     (`cleared_ms` on the task, stamped by the board's own command). It hides
//     the row from the working view until they ask for it back. It never
//     deletes: the row, its notes and its links stay in `tasks.json`, and the
//     audit log records the action.
//
// What sinking must never do is override the human's manual priority order
// among rows that are still live — board order IS priority order, and "top =
// next" is a contract the orchestrator reads. So the split is a STABLE
// partition of each sibling list: within the live half, relative order is
// untouched.
// ---------------------------------------------------------------------------

/** The archive stamp (#1152): when the human cleared this row out of their
 *  working view. Optional on the wire like every other additive field — the
 *  backend skips the key when absent, so every pre-#1152 board arrives without
 *  it, and absent means "not cleared". */
export interface HasCleared {
  cleared_ms?: number | null;
}

/** Just the field the settled half is ordered by. Optional because the pure
 *  helpers here are exercised with minimal rows; a row without one sorts as
 *  `0`, i.e. oldest, and ties keep board order. */
export interface HasUpdated {
  updated_ms?: number;
}

/** Everything the board's display projection reads off a row (#1152). */
export type OrderedRow = HasParent & HasStatus & HasUpdated & HasCleared;

/** Whether a row is currently archived out of the human's working view.
 *
 *  Read-time, deliberately: the stamp alone is not enough, the row must STILL
 *  be `done`. Reopening a cleared task (the orchestrator moving it back to
 *  `in-progress`, or a human status edit) therefore brings it straight back
 *  into view without anything having to remember to wipe the stamp — the same
 *  no-repair-pass discipline `isReady` and the dep chips follow. Clear it
 *  again and the stamp is refreshed. */
export function isCleared(task: HasStatus & HasCleared): boolean {
  return !!task.cleared_ms && task.status === DONE_STATUS;
}

/** How many rows the board's **clear done** action would archive: `done` rows
 *  not already cleared. Drives the button's count and whether it shows at all.
 *  Deliberately NOT `doneCount` — that one counts every `done` row and drives
 *  the destructive *delete* button, which still offers to remove the archive
 *  too. */
export function clearableCount(tasks: readonly (HasStatus & HasCleared)[]): number {
  return tasks.reduce((n, t) => (t.status === DONE_STATUS && !isCleared(t) ? n + 1 : n), 0);
}

/** The ids whose row AND whole subtree satisfy `pred` — the rows the board may
 *  treat as one unit, because acting on such a row can never strand a
 *  descendant that doesn't satisfy it.
 *
 *  This is the guard that makes both projections below safe on a nested board:
 *  a `done` epic with one `queued` slice inside it is NOT finished, and sinking
 *  or hiding it would take that live slice down with it. Terminates on a
 *  hand-edited containment cycle and fails SAFE there — re-entering a row
 *  mid-walk answers `false`, so a cyclic row is never sunk or hidden on the
 *  strength of a walk that hasn't finished. */
function closedSubtrees<T extends HasParent>(
  board: readonly T[],
  children: ReadonlyMap<string, T[]>,
  pred: (t: T) => boolean
): Set<string> {
  const memo = new Map<string, boolean>();
  const visiting = new Set<string>();
  const holds = (t: T): boolean => {
    const cached = memo.get(t.id);
    if (cached !== undefined) return cached;
    if (visiting.has(t.id)) return false;
    visiting.add(t.id);
    const ok = pred(t) && (children.get(t.id) ?? []).every(holds);
    visiting.delete(t.id);
    memo.set(t.id, ok);
    return ok;
  };
  const out = new Set<string>();
  for (const t of board) if (holds(t)) out.add(t.id);
  return out;
}

/** The rows the board sinks below the live work: `done`, with nothing
 *  unfinished under them (#1152). Their displayed position is derived, so the
 *  manual reorder buttons are off on them — see `siblingPosition`. */
export function settledIds<T extends HasParent & HasStatus>(
  board: readonly T[]
): Set<string> {
  return closedSubtrees(board, buildTree(board).children, (t) => t.status === DONE_STATUS);
}

/** The rows the board actually hides while "show cleared" is off (#1152):
 *  cleared, with nothing un-cleared under them. A cleared container holding a
 *  live child stays on the board — it is the only thing on screen that says
 *  where that child lives. */
export function clearedIds<T extends OrderedRow>(board: readonly T[]): Set<string> {
  return closedSubtrees(board, buildTree(board).children, isCleared);
}

/** Why a focus request (a NEEDS-YOU deep link, #1091 slice C) found no rendered
 *  row for its target — the three cases the board must tell apart:
 *
 *  - `gone` — the id names nothing on the board. The task really can be deleted
 *    between the request being parked and the render draining it, so this stays
 *    a SILENT no-op: an error here would fire on an ordinary race.
 *  - `cleared` — it is on the board and archived, so 👁 is what brings it back.
 *  - `hidden` — on the board, not archived, and still not rendered: it sits
 *    inside a collapsed container (#958). Pre-dates #1152; the board simply
 *    could not say so before.
 *
 *  The distinction is the whole point of the finding this answers: the request
 *  is CONSUMED either way, so without it a deep link onto an off-screen row is
 *  indistinguishable from one onto a deleted row — both look like a dead click. */
export type FocusMiss = "gone" | "cleared" | "hidden";

export function focusMiss<T extends OrderedRow>(
  target: string,
  board: readonly T[]
): FocusMiss {
  const row = board.find((t) => t.id === target);
  if (!row) return "gone";
  return isCleared(row) ? "cleared" : "hidden";
}

/** One sibling list, split into the two halves the board shows in order (#1152):
 *  `manual` — still-live rows in the human's own priority order, untouched —
 *  then the settled ones, most-recently-updated first.
 *
 *  **The key is `updated_ms`, which is "last TOUCHED", not "finished".** No
 *  `done_ms` exists, and inventing one to make this sentence exact would be a
 *  new stored field on every row to order a list the human is looking at
 *  because they are done with it. So the approximation is deliberate and it is
 *  stated rather than papered over: a note added to a six-week-old `done` row
 *  lifts it above work that actually finished yesterday. Every surface that
 *  describes this order says "most recently updated" for that reason (#1152
 *  review round 1, finding 3).
 *
 *  `ordered` is what renders; `manual` is what the ▲/▼ buttons step through,
 *  and the two being derived here together is what keeps a click meaning
 *  exactly one displayed position. Stable: `sort` has been stable since ES2019,
 *  so settled rows with equal (or absent) `updated_ms` keep board order. */
export function orderSiblings<T extends OrderedRow>(
  siblings: readonly T[],
  settled: ReadonlySet<string>
): { manual: T[]; ordered: T[] } {
  const manual: T[] = [];
  const finished: T[] = [];
  for (const t of siblings) (settled.has(t.id) ? finished : manual).push(t);
  finished.sort((a, b) => (b.updated_ms ?? 0) - (a.updated_ms ?? 0));
  return { manual, ordered: [...manual, ...finished] };
}

/** Where an id sits in a manually-ordered list. A row that isn't in it — a
 *  settled row, or an id naming nothing — reports `{ index: -1, count: 0 }`,
 *  which is what disables both reorder buttons. */
export function positionAmong<T extends HasId>(
  manual: readonly T[],
  id: string
): { index: number; count: number } {
  const index = manual.findIndex((t) => t.id === id);
  return index < 0 ? { index: -1, count: 0 } : { index, count: manual.length };
}

// ---------------------------------------------------------------------------
// Tree-view filtering (#1270).
//
// The board is a tree, so a filter cannot simply be `board.filter(pred)`: a
// flat list of matches throws away the containment the whole #1156 hierarchy
// model exists to show. Three rules, all here, all pinned:
//
//  1. **A match keeps its ancestor chain.** Matches render, and so does every
//     container above them, marked `context` so the view can render them as
//     scaffolding rather than as hits. Descendants of a match are NOT pulled in
//     — an epic matching `kind=epic` renders alone, and `shownKids` beside
//     `childCounts` is what tells the human what is inside it.
//  2. **An active filter overrides collapse, and never mutates it.** Standard
//     tree-view behaviour: a search that finds a row and then hides it inside a
//     collapsed container is worse than no search. The stored collapsed set is
//     untouched, so clearing the filter restores the exact shape the human
//     left. Note this is not merely a preference — under rule 1 a kept
//     container either has a kept descendant (so it MUST expand) or is a match
//     whose whole subtree was filtered out (so collapsing it changes nothing).
//     Collapse has no observable effect while a filter is active, and the view
//     renders the chevron inert to say so rather than offering a dead click.
//  3. **AND across families, OR within one.** An empty family constrains
//     nothing; it never means "match nothing".
// ---------------------------------------------------------------------------

/** The kind-filter chip standing for a row with no `kind` at all — legal and
 *  permanently exempt from the ladder (#1156), so it needs a chip of its own or
 *  it would be the one class of row the kind filter cannot name. A sentinel,
 *  never a stored value: `KINDS` are plain words, so no real kind collides with
 *  it (the same trick `CLEAR_KIND_CHOICE` plays in tasksview.ts). */
export const UNLABELLED_KIND = "unlabelled";

/** The `sprint` family's chip for the rows carrying no sprint at all — the
 *  backlog (#1272). Exactly [`UNLABELLED_KIND`]'s argument, applied to the
 *  other optional field: without it the backlog would be the one class of row
 *  the sprint filter cannot name, and "everything that is not yet batched" is
 *  the question a sprint board is most often asked.
 *
 *  A sentinel that no real value can collide with, because every real one is a
 *  decimal number rendered by [`sprintFilterValue`] and `backlog` is not a
 *  decimal number. Never stored on a row — the absence of `sprint` IS the
 *  backlog, and 0 is the backend's CLEAR, not a sprint. */
export const BACKLOG_SPRINT = "backlog";

/** Which sprint chip a row answers to. The whole mapping from the wire's
 *  `sprint?: number | null` onto the family's string vocabulary, in one place
 *  so the chip list and the predicate cannot spell it differently.
 *
 *  Absent, null and a non-number all read as the backlog — the same
 *  tolerate-and-show posture the rest of this module takes toward a
 *  hand-edited `tasks.json`, and the reading `HasSprint`'s own doc requires:
 *  absent is the backlog, never sprint 0. */
export function sprintFilterValue<T extends HasSprint>(task: T): string {
  return typeof task.sprint === "number" ? String(task.sprint) : BACKLOG_SPRINT;
}

/** The sprint chips to offer: every sprint number the board actually carries,
 *  ascending, then [`BACKLOG_SPRINT`].
 *
 *  Derived from the board rather than from a range, for the reason §5 of
 *  `board-sprints-and-links.md` gives for deriving `current_sprint`: sprint
 *  numbers need not be contiguous and need not start at 1, so `1..max` would
 *  offer chips that match nothing and hide nothing. Ascending and not board
 *  order because a sprint number IS an ordering — chips reading `1 2 3` are
 *  the sequence the board is worked in, where board order would shuffle them
 *  by whichever row happens to sit on top.
 *
 *  The backlog chip comes last, and unconditionally: it is the "not batched"
 *  bucket, which exists on every board whether or not a row is in it right
 *  now, exactly as `unlabelled` does for kinds. */
export function sprintFilterChoices<T extends HasSprint>(board: readonly T[]): string[] {
  const seen = new Set<number>();
  for (const t of board) if (typeof t.sprint === "number") seen.add(t.sprint);
  return [...[...seen].sort((a, b) => a - b).map(String), BACKLOG_SPRINT];
}

/** The title the text filter reads. Optional like every other additive board
 *  field, so the pure helpers stay exercisable with minimal rows; absent reads
 *  as an empty title, never as "matches everything". */
export interface HasTitle {
  title?: string;
}

/** The board's active view filter (#1270). Five families, evaluated as AND
 *  across families and OR within one.
 *
 *  Deliberately a flat bag of families rather than a predicate: it is the thing
 *  that gets PERSISTED (`src/boardprefs.ts`), so it has to survive a JSON round
 *  trip, and a future family — #1273's typed links — is then a new key here
 *  plus one clause in `matchesFilter`, not a migration. #1272's `sprint` was
 *  the first to arrive that way and cost exactly that. */
export interface BoardFilter {
  /** Members of `kindFilterChoices` — `KINDS` entries plus `UNLABELLED_KIND`,
   *  plus any out-of-vocabulary kind the board actually carries. */
  kind: readonly string[];
  /** Members of `STATUSES`. */
  status: readonly string[];
  /** Members of `sprintFilterChoices` — the decimal sprint numbers this board
   *  actually carries, as strings, plus [`BACKLOG_SPRINT`] for the rows that
   *  carry none (#1272).
   *
   *  Strings and not numbers so the family is shaped exactly like `kind` and
   *  `status`: one array per family, persisted through `boardprefs.ts`'s
   *  `stringList`, rendered by one `familyChip`. A number array would need its
   *  own decoder and its own chip builder for no gain, and would still have to
   *  invent a value for "no sprint". */
  sprint: readonly string[];
  /** Case-insensitive substring over a row's id and title. Blank = no
   *  constraint. */
  text: string;
  /** Only rows the view says are waiting on the human (decision-blocked or
   *  demo-gated — the ids behind the board's ❓/👀 marker chips). */
  attention: boolean;
}

/** The filter that constrains nothing — the board as it renders with the strip
 *  untouched. Frozen because it is a shared default: a caller mutating it would
 *  silently arm a filter on every other board. */
export const NO_FILTER: BoardFilter = Object.freeze({
  kind: Object.freeze([]) as readonly string[],
  status: Object.freeze([]) as readonly string[],
  sprint: Object.freeze([]) as readonly string[],
  text: "",
  attention: false,
});

/** No attention ids known — the default when a caller filters without
 *  supplying the set. With `attention: true` this correctly matches nothing:
 *  the toggle asks for rows the view flagged, and a view that flagged none has
 *  none to show. */
const NO_ATTENTION: ReadonlySet<string> = new Set<string>();

/** Is any family constraining anything? Drives whether the board filters at
 *  all — and, in the view, whether the collapse affordances go inert (rule 2)
 *  and the "showing N of M" hint appears. */
export function filterActive(filter: BoardFilter): boolean {
  return (
    filter.kind.length > 0 ||
    filter.status.length > 0 ||
    filter.sprint.length > 0 ||
    filter.text.trim() !== "" ||
    filter.attention
  );
}

/** The kind chips to offer: the four ladder levels, then `unlabelled`, then any
 *  OTHER kind the board actually carries.
 *
 *  That tail is not decoration. `ladderRule` exempts an out-of-vocabulary kind
 *  on purpose (CLAUDE.md constraint 8 — Orrerix must not require a
 *  methodology), so a hand-edited `tasks.json` may legitimately carry `saga`.
 *  Without a chip for it, such a row would match NO kind selection — neither a
 *  ladder level nor `unlabelled`, since it is labelled — and the only way to
 *  see it again would be to clear the kind filter entirely. Deriving the tail
 *  from the board keeps every row reachable. Deduped, and stable: ladder order
 *  first, then the extras in the order the board presents them. */
export function kindFilterChoices<T extends HasParent>(board: readonly T[]): string[] {
  const out: string[] = [...KINDS, UNLABELLED_KIND];
  const known = new Set(out);
  for (const t of board) {
    const k = t.kind || UNLABELLED_KIND;
    if (!known.has(k)) {
      known.add(k);
      out.push(k);
    }
  }
  return out;
}

/** Does this ONE row satisfy the filter? Containment is not consulted here —
 *  `visibleRows` is what adds a match's ancestors back as context.
 *
 *  `attention` is an opaque id set the view derives (`blockedTaskMap` +
 *  `boardMarker`) and threads in, so this module never learns what a question
 *  or a demo gate is. */
export function matchesFilter<T extends HasParent & HasStatus & HasTitle & HasSprint>(
  task: T,
  filter: BoardFilter,
  attention: ReadonlySet<string> = NO_ATTENTION
): boolean {
  // `||`, not `??`: a hand-edited empty-string kind is "no level", the same as
  // the key being absent, and must land on the `unlabelled` chip rather than
  // becoming an invisible fifth class.
  if (filter.kind.length > 0 && !filter.kind.includes(task.kind || UNLABELLED_KIND)) return false;
  if (filter.status.length > 0 && !filter.status.includes(task.status)) return false;
  // #1272. `sprintFilterValue`, not a second reading of the field here: the
  // chip list is built from the same function, so a row can never answer to a
  // chip the strip does not offer, nor fall through every chip it does.
  if (filter.sprint.length > 0 && !filter.sprint.includes(sprintFilterValue(task))) return false;
  if (filter.attention && !attention.has(task.id)) return false;
  const needle = filter.text.trim().toLowerCase();
  if (needle) {
    const inId = task.id.toLowerCase().includes(needle);
    // Id and title are tested SEPARATELY rather than as one joined haystack, so
    // a needle can never match across the seam between them ("1 fix" hitting
    // `t-1` + "fix the thing").
    const inTitle = (task.title ?? "").toLowerCase().includes(needle);
    if (!inId && !inTitle) return false;
  }
  return true;
}

/** Which rows survive the filter, and which of those are actual hits. `keep` is
 *  the matches plus every container above them; `matched` is the hits alone. */
interface FilterSieve {
  keep: ReadonlySet<string>;
  matched: ReadonlySet<string>;
}

/** Build the sieve for one render.
 *
 *  `archived` is passed IN rather than re-derived, and that is the point: this
 *  and `visibleRows` must read "is this row hidden by the archive" by one rule,
 *  or the two disagree exactly where they differ (CLAUDE.md's one-rule guard
 *  convention). It is why there is no exported `filterSieve(board, filter,
 *  attention, showCleared)` for the view to call — two callers passing
 *  `showCleared` to two places is precisely the asymmetry.
 *
 *  An archived row cannot MATCH. **That line is defence in depth, and has no
 *  observable effect today** — stated rather than left to look load-bearing:
 *  `visibleRows` refuses to render an archived row anyway, and `clearedIds`
 *  only archives a row whose whole subtree is cleared too, so an archived
 *  container can never sit above an un-archived match whose ancestors this
 *  would otherwise pull in. It is kept because `matched` and `keep` should mean
 *  what they say to any future reader of them, not because a test can redden it
 *  — removing it reddens nothing, and the PR says so. What DOES pin the
 *  property is `visibleRows`' own archive check (#1152). */
function buildSieve<T extends OrderedRow>(
  board: readonly T[],
  filter: BoardFilter,
  attention: ReadonlySet<string>,
  archived: ReadonlySet<string>
): FilterSieve {
  const byId = new Map(board.map((t) => [t.id, t]));
  const matched = new Set<string>();
  for (const t of board) {
    if (archived.has(t.id)) continue;
    if (matchesFilter(t, filter, attention)) matched.add(t.id);
  }
  const keep = new Set(matched);
  for (const id of matched) {
    const t = byId.get(id);
    if (!t) continue;
    // The same ancestor walk `blockingAncestor` and `buildTree` use, so a
    // hand-edited cycle terminates here for free rather than needing its own
    // guard.
    for (const anc of ancestorChain(t, byId).chain) keep.add(anc.id);
  }
  return { keep, matched };
}

/** The view-level inputs `visibleRows` needs beyond the board itself (#1270).
 *  Optional as a whole, so every pre-#1270 call site renders exactly what it
 *  rendered before. */
export interface BoardView {
  filter?: BoardFilter;
  /** Ids the `attention` family matches — the view derives these from the
   *  pending questions and demo gates it already reads, and threads them in. */
  attention?: ReadonlySet<string>;
}

/** Every row that contains something — what **collapse all** collapses (#1270).
 *  Leaves are omitted: a collapsed id that names no container is inert, but
 *  storing hundreds of them would persist noise `retainExisting` cannot prune
 *  (they name live rows) and make the saved blob grow with the board rather
 *  than with the tree.
 *
 *  Board order, so the persisted set is stable across renders that changed
 *  nothing. **Expand all** is the empty set, which needs no helper. */
export function containerIds<T extends HasParent>(board: readonly T[]): string[] {
  const { children } = buildTree(board);
  return board.filter((t) => (children.get(t.id)?.length ?? 0) > 0).map((t) => t.id);
}

/** The rows to render, in display order: roots first, each followed by its own
 *  subtree (recursively), with the depth each row should be indented to.
 *
 *  Within every sibling group the order is `orderSiblings`' — the human's
 *  manual priority order for live work, then finished subtrees most recently updated first
 *  (#1152). `collapsed` hides a row's whole subtree, not just its direct
 *  children — a collapsed epic must not leave its grandchildren stranded at the
 *  top level — and `showCleared` brings the archived rows back.
 *
 *  Every row on the board appears AT MOST ONCE and, unless it is archived,
 *  exactly once: that is the invariant a hand-edited cycle would otherwise
 *  break, in either direction (an infinite render, or a row that silently
 *  vanishes). */
export function visibleRows<T extends OrderedRow>(
  board: readonly T[],
  collapsed: Iterable<string> = [],
  showCleared = false,
  view: BoardView = {}
): BoardRow<T>[] {
  const hidden = new Set(collapsed);
  const settled = settledIds(board);
  const archived = showCleared ? new Set<string>() : clearedIds(board);
  // The sieve is built HERE, off the one `archived` value above, rather than
  // being handed in by the caller — see `buildSieve`'s doc for why splitting
  // `showCleared` across two call sites is the bug this shape refuses to allow.
  const filter = view.filter ?? NO_FILTER;
  const sieve = filterActive(filter)
    ? buildSieve(board, filter, view.attention ?? NO_ATTENTION, archived)
    : null;
  const { roots, children } = buildTree(board);
  const rows: BoardRow<T>[] = [];
  const seen = new Set<string>();
  // `visible` is threaded down rather than returning early on a collapsed (or
  // archived) row: the hidden subtree still has to be WALKED, so its rows are
  // marked seen and can't be picked up again by the fallback below as stray
  // top-level rows.
  const walk = (task: T, depth: number, visible: boolean): void => {
    if (seen.has(task.id)) return;
    seen.add(task.id);
    const kids = orderSiblings(children.get(task.id) ?? [], settled).ordered;
    // Rule 2: an active filter overrides collapse without touching the stored
    // set, so clearing the filter restores the shape the human left.
    const isCollapsed = kids.length > 0 && hidden.has(task.id) && !sieve;
    const show = visible && !archived.has(task.id) && (!sieve || sieve.keep.has(task.id));
    if (show) {
      rows.push({
        task,
        depth,
        hasChildren: kids.length > 0,
        collapsed: isCollapsed,
        settled: settled.has(task.id),
        cleared: isCleared(task),
        context: !!sieve && !sieve.matched.has(task.id),
        shownKids: 0, // filled in below, once the whole projection is known
      });
    }
    for (const k of kids) walk(k, depth + 1, show && !isCollapsed);
  };
  for (const r of orderSiblings(roots, settled).ordered) walk(r, 0, true);
  // Nothing should reach this — `buildTree` makes every unreachable row a root
  // — but a row that fell out of the board would be work made invisible, so it
  // renders at top level instead of being trusted away.
  for (const t of board) walk(t, 0, true);
  // `shownKids` can only be counted once every row's fate is settled — a
  // child renders or not depending on collapse, the archive AND the filter, and
  // this is the one place that knows all three. Counted off the rendered set
  // rather than re-deriving those three rules, so it cannot disagree with what
  // is actually on screen.
  const shown = new Set(rows.map((r) => r.task.id));
  for (const r of rows) {
    r.shownKids = (children.get(r.task.id) ?? []).reduce(
      (n, k) => (shown.has(k.id) ? n + 1 : n),
      0
    );
  }
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

/** Where a row sits among the siblings it is MANUALLY ordered within: drives
 *  whether the up/down buttons are disabled. Reordering is sibling-scoped — the
 *  first child of a container has nowhere higher to go, even though the board
 *  array has rows above it.
 *
 *  `{ index: -1, count: 0 }` — both buttons off — for an id that isn't on the
 *  board, and (since #1152) for a **settled** row: a finished subtree's
 *  position is derived, most recently updated first, so a manual step would either do
 *  nothing visible or contradict the order the board just told the human it was
 *  using. Reopen the row and it rejoins the manual list. */
export function siblingPosition<T extends OrderedRow>(
  board: readonly T[],
  id: string,
  // The board's settled set, when the caller already has it. A render asks this
  // once per ROW, and deriving the set here would walk the whole tree again
  // each time — so the view computes it once and threads it, exactly as it
  // already does with `blocked` and the two board-level `usesX` flags.
  settled: ReadonlySet<string> = settledIds(board)
): { index: number; count: number } {
  const { manual } = orderSiblings(siblingRows(buildTree(board), id), settled);
  return positionAmong(manual, id);
}

/** The rows this one is ordered within: its container's children, or the
 *  top-level rows. The tree's own arrays, so callers must not mutate them —
 *  `orderSiblings` copies.
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
function siblingRows<T extends HasParent>(tree: TaskTree<T>, id: string): T[] {
  if (tree.roots.some((t) => t.id === id)) return tree.roots;
  for (const kids of tree.children.values()) {
    if (kids.some((k) => k.id === id)) return kids;
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
 *  One step is one step **on screen** (#1152). The board sinks finished
 *  subtrees below the live work, so between two live rows the stored array can
 *  hold any number of settled ones — and a move computed against the array
 *  would step onto one of those and change nothing the human can see. So the
 *  step is taken against `orderSiblings`' manual list and then applied to the
 *  stored order as a minimal splice: the row lands immediately beside its
 *  displayed neighbour and every other row, settled ones included, keeps the
 *  array position the human or the orchestrator gave it. The display rule stays
 *  a projection — it never rewrites priority data as a side effect of a click.
 *
 *  Always a permutation of the board — every id present exactly once — since
 *  `reorder_tasks` appends whatever the caller omitted, and an omission would
 *  therefore be a silent priority change nobody asked for. */
export function reorderWithSubtree<T extends OrderedRow>(
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

  // One step against what is on screen: the manual list is this row's sibling
  // group minus the finished subtrees the board has sunk to the bottom of it.
  // A zero delta is a no-op rather than a move onto itself — the splice below
  // reads the target's position AFTER lifting the row out, and "beside myself"
  // has no such position.
  if (delta === 0) return flatten();
  const { manual } = orderSiblings(siblingRows(tree, id), settledIds(board));
  const i = manual.findIndex((t) => t.id === id);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= manual.length) return flatten();
  const target = manual[j].id;

  // The live sibling array (inside rootIds/childIds, so splicing it is what
  // `flatten` then reads) — reordering is scoped to it and to nothing else.
  const siblings = rootIds.includes(id)
    ? rootIds
    : [...childIds.values()].find((kids) => kids.includes(id));
  if (!siblings) return flatten();
  siblings.splice(siblings.indexOf(id), 1);
  // `target` is a sibling and is not `id`, so it is still in the array here.
  const at = siblings.indexOf(target);
  siblings.splice(delta < 0 ? at : at + 1, 0, id);
  return flatten();
}

/** The rows the "nest under…" picker offers: every other row on the board,
 *  minus the one it is already inside (picking that is a no-op write), minus
 *  every row the strict ladder would refuse as this row's container (#1156).
 *
 *  Still deliberately does NOT filter out choices that would close a hierarchy
 *  cycle or bust the depth cap — the exact call `depCandidates` makes, for the
 *  exact reason: those are properties of the whole tree, re-derived per
 *  candidate, and a second copy of them here could only ever disagree with the
 *  one inside the backend's lock. The ladder is filtered because it is not that
 *  kind of rule (see `KINDS`) — and because an unfiltered nest picker on a
 *  levelled row is mostly illegal choices, which teaches the ladder by toast. */
export function parentCandidates<T extends HasParent>(task: T, board: readonly T[]): T[] {
  return board.filter(
    (t) => t.id !== task.id && t.id !== task.parent && mayContain(t.kind, task.kind)
  );
}

/** Everything the nest picker needs, decided in one place: the containers it
 *  may offer, whether the "↥ top level" escape is one of them, and what to say
 *  instead when it can offer nothing at all (#1156).
 *
 *  The empty case is worth a real sentence rather than a generic one: on a
 *  levelled row it is the ladder talking, and the reader needs to know WHICH
 *  level is missing from the board — otherwise "no task to nest under" reads as
 *  a bug on a board that is visibly full of tasks. */
export interface ParentPickerChoices<T> {
  candidates: T[];
  topLevel: boolean;
  emptyLabel: string;
}

export function parentPickerChoices<T extends HasParent>(
  task: T,
  board: readonly T[]
): ParentPickerChoices<T> {
  const candidates = parentCandidates(task, board);
  const rule = ladderRule(task.kind);
  const emptyLabel =
    rule.rule === "inside"
      ? `no ${rule.container} to nest this ${task.kind} under`
      : rule.rule === "top-level-only"
        ? `an ${task.kind} is top-level only`
        : "no other task to nest under";
  return { candidates, topLevel: !!task.parent && mayBeTopLevel(task.kind), emptyLabel };
}

/** Whether a row could carry `kind` (null = no level) as it currently sits —
 *  its own container must accept the new level, and so must every row already
 *  inside it. The backend judges exactly these two directions on a `kind`
 *  write; this is the picker's copy of that question.
 *
 *  A row whose `parent` names nothing on the board (hand-edited only) can carry
 *  no level at all: the backend refuses a level write against a container it
 *  cannot resolve, so the only level offered there is none. */
export function kindFits<T extends HasParent>(
  task: T,
  kind: string | null,
  board: readonly T[]
): boolean {
  const container = task.parent ? board.find((t) => t.id === task.parent) : undefined;
  // Own link. The unresolvable-container case is a THIRD branch, not an early
  // return: a row whose `parent` names nothing can carry no level, but that says
  // nothing about the rows inside it, and returning here skipped the children
  // walk below — offering a clear on a row holding levelled children, which the
  // backend then refused (rev round 1, N1). Every branch must fall through to
  // the same second question, because the backend asks both every time.
  const ownOk = task.parent
    ? container
      ? mayContain(container.kind, kind)
      : kind === null
    : mayBeTopLevel(kind);
  if (!ownOk) return false;
  // Children: this row's new level must still hold everything already inside it.
  return board.every((c) => c.parent !== task.id || mayContain(kind, c.kind));
}

/** What the "set kind…" picker offers (#958 slice K, narrowed by #1156): the
 *  levels this row does not already carry AND could legally take where it sits,
 *  plus whether the clear is one of them.
 *
 *  The clear is NOT unconditional. Clearing the level of a row that holds
 *  levelled children would leave them inside an unlevelled container, which the
 *  backend refuses — so on such a row the picker offers nothing and the row's
 *  contents have to be dealt with first. Unlike before #1156 this list can be
 *  EMPTY (a row whose container is unlevelled has no legal level at all), which
 *  is why the caller must handle an empty picker rather than assuming four
 *  minus one. */
export interface KindPickerChoices {
  candidates: string[];
  clear: boolean;
}

export function kindPickerChoices<T extends HasParent>(
  task: T,
  board: readonly T[]
): KindPickerChoices {
  return {
    candidates: KINDS.filter((k) => k !== task.kind && kindFits(task, k, board)),
    clear: !!task.kind && kindFits(task, null, board),
  };
}

/** Whether this row's `parent` names no task on the board — only reachable by
 *  hand-editing `tasks.json` (the backend validates on write and re-homes
 *  survivors on delete), so the board says so on the row rather than rendering
 *  it at top level with no explanation. */
export function hasMissingParent<T extends HasParent>(task: T, board: readonly T[]): boolean {
  return !!task.parent && !board.some((t) => t.id === task.parent);
}


// ---------------------------------------------------------------------------
// Sprints (#1272) and grounding links (#1273) — the board's mirror of the
// backend model. Every rule here is the backend's (mod.rs `current_sprint`,
// `normalize_task_links`); these exist because the human's board reads full
// `Task`s via `orch_tasks` and derives its own view, exactly as it already does
// for readiness. The two sides are pinned against each other by tests.
// ---------------------------------------------------------------------------

/** A row as far as sprints care. Optional on the wire like every other additive
 *  field — the backend skips it when absent, so a sprintless board's rows
 *  simply have no key, and absent must read as "backlog", never as sprint 0. */
export interface HasSprint {
  sprint?: number | null;
}

/** The board's CURRENT sprint — the lowest sprint on any row that is not
 *  `done`, or `null` when no open row carries one.
 *
 *  Mirrors the backend's `current_sprint` exactly, including the part that
 *  looks like an omission and is not: a `blocked` row still HOLDS its sprint.
 *  Only `done` stops counting, so a sprint is finished only when its last open
 *  row leaves it — never because the remaining work looked stuck. Roll-over is
 *  always an explicit, audited row write. */
export function currentSprint<T extends HasStatus & HasSprint>(
  board: readonly T[]
): number | null {
  let lowest: number | null = null;
  for (const t of board) {
    if (t.status === DONE_STATUS) continue;
    const s = t.sprint;
    if (typeof s !== "number") continue;
    if (lowest === null || s < lowest) lowest = s;
  }
  return lowest;
}

/** How far one sprint has got: `done` of `total` rows carrying that number.
 *
 *  Counts EVERY row in the sprint including done ones — the denominator is the
 *  sprint's whole scope, which is what makes "3/7 done" mean anything. A sprint
 *  nobody has assigned reads `{ done: 0, total: 0 }`, and a caller rendering a
 *  header is expected to check `total` rather than divide by it. */
export function sprintProgress<T extends HasStatus & HasSprint>(
  board: readonly T[],
  sprint: number
): { done: number; total: number } {
  let done = 0;
  let total = 0;
  for (const t of board) {
    if (t.sprint !== sprint) continue;
    total++;
    if (t.status === DONE_STATUS) done++;
  }
  return { done, total };
}

/** The rows that would MOVE if the human advanced past `sprint` — every row in
 *  it that is not `done`.
 *
 *  This is the never-silent surface: #1272 requires that rolling work forward
 *  is explicit, so the board shows exactly this list before it writes anything,
 *  and then performs one audited `upsert_task` per row. `blocked` rows are
 *  INCLUDED deliberately — they are precisely the ones a silent roll-over would
 *  sweep up, and the ones the human most needs to see named.
 *
 *  Board order is preserved, so the confirm list reads in the same order as the
 *  board the human is looking at. */
export function rollOverSet<T extends HasStatus & HasSprint>(
  board: readonly T[],
  sprint: number
): T[] {
  return board.filter((t) => t.sprint === sprint && t.status !== DONE_STATUS);
}

/** The largest sprint the backend can store: `Task::sprint` is a `u32`, and
 *  both wire parsers decode into one (`arg_sprint`'s `as_u64` bound in mcp.rs,
 *  serde's own `u32` decode on `orch_upsert_task`). A board sitting on it has
 *  no next sprint to roll into, and [`sprintAdvance`] says so rather than
 *  composing a write the backend will refuse. */
export const MAX_SPRINT = 0xffff_ffff;

/** What advancing past `from` would do: which rows move, and where they land.
 *
 *  ONE function, because the confirm dialog and the writes must not be able to
 *  disagree: the list the human is shown is the list that is written, and the
 *  number in the sentence is the number in the patch. A view deriving `to` on
 *  its own — `from + 1` inline beside a `rollOverSet` call — is the shape the
 *  one-rule convention names, and it fails silently in the worst direction:
 *  the human confirms one thing and the board records another.
 *
 *  `to` is `from + 1` and not "the next sprint number already in use". Gaps in
 *  the numbering are deliberate (§5 of `board-sprints-and-links.md`: numbers
 *  need not be contiguous, so a human can park planned work in sprint 5), and
 *  landing rolled-over rows in an existing later batch would silently redefine
 *  that batch's scope. `from + 1` moves the work one step and nowhere else,
 *  and the dialog names the number, so nothing about where it lands is
 *  inferred.
 *
 *  `to: null` is the refusal, and [`MAX_SPRINT`] is the ONLY thing that
 *  triggers it on any board a caller can actually reach: there is no sprint
 *  after the last one a `u32` holds, so the affordance must go inert rather
 *  than offer a write that cannot land. `rows` is still reported, so a caller
 *  can say what is stuck.
 *
 *  `from` of `0` is accepted, deliberately. `0` is not a sprint anything can
 *  be moved INTO (it is the CLEAR — see [`sprintPickerChoices`]), but a
 *  hand-edited `tasks.json` can put a row in it, `currentSprint` will then
 *  report `0`, and refusing here would leave that board with a dead advance
 *  button and a tooltip claiming it had run out of numbers. Rolling those rows
 *  into sprint 1 is exactly what the affordance is for. */
export function sprintAdvance<T extends HasStatus & HasSprint>(
  board: readonly T[],
  from: number
): { to: number | null; rows: T[] } {
  return {
    to: Number.isInteger(from) && from >= 0 && from < MAX_SPRINT ? from + 1 : null,
    rows: rollOverSet(board, from),
  };
}

/** What the row's sprint picker offers: the sprints this board already runs,
 *  plus the next unused number, plus whether a clear is on the menu.
 *
 *  Derived from the board for `sprintFilterChoices`' reason — a `1..max` range
 *  would offer numbers nobody uses — with ONE addition the filter chips
 *  deliberately do not make: the next number after the highest, so a board can
 *  start a sprint at all. Without it the first sprint could only ever be set
 *  through the orchestrator, and a board whose sprints were all finished could
 *  never open another.
 *
 *  The row's OWN sprint is excluded: picking it is a write that changes
 *  nothing, and every other picker in this module (`depCandidates`,
 *  `parentPickerChoices`, `kindPickerChoices`) omits the no-op choice too.
 *
 *  `clear` is whether the row carries a sprint at all — the backlog is the
 *  absence of one, so there is nothing to clear on a row already in it. The
 *  caller sends `0`, the backend's numeric CLEAR (§8 of the design note), and
 *  `0` is therefore filtered OUT of `options` even when a hand-edited board
 *  carries a row in it: offering it would be a menu entry reading "sprint 0"
 *  that silently performs the clear instead. The row keeps its `0` badge and
 *  its filter chip — nothing becomes unreachable — it just is not a sprint
 *  anything can be moved INTO.
 *
 *  Nothing here is a legality check. A sprint gates nothing and validates
 *  against nothing but its type, so unlike the parent picker there is no
 *  backend rule this could disagree with — the only refusal that exists is
 *  `MAX_SPRINT`, which is why the "next" entry is the one thing bounded. */
export function sprintPickerChoices<T extends HasSprint>(
  task: T,
  board: readonly T[]
): { options: number[]; clear: boolean } {
  const seen = new Set<number>();
  for (const t of board) if (typeof t.sprint === "number") seen.add(t.sprint);
  const highest = seen.size > 0 ? Math.max(...seen) : 0;
  if (highest < MAX_SPRINT) seen.add(highest + 1);
  const mine = typeof task.sprint === "number" ? task.sprint : null;
  return {
    options: [...seen].filter((s) => s >= 1 && s !== mine).sort((a, b) => a - b),
    clear: mine !== null,
  };
}

/** The grounding-link types, mirroring the backend's `TASK_LINK_TYPES`. Pinned
 *  against the Rust source by a test, the way `KINDS` is. */
export const LINK_TYPES = [
  "requirement",
  "spec",
  "design-note",
  "test-case",
  "doc",
  "link",
] as const;

export type LinkType = (typeof LINK_TYPES)[number];

/** One grounding link as it arrives over `orch_tasks`.
 *
 *  Named `TaskArtifactLink`, not `TaskLink`, because `HasLinks` in this module
 *  already means the #582 `deps`/`related` arrays — two unrelated senses of
 *  "link" live in this file and the names have to keep them apart. */
export interface TaskArtifactLink {
  type: string;
  target: string;
  label?: string | null;
}

/** A row as far as grounding links care. */
export interface HasArtifactLinks {
  links?: readonly TaskArtifactLink[] | null;
}

/** What a link target points at, for choosing how to OPEN it.
 *
 *  Display-side only: the backend validates shape and never resolves a target,
 *  so this classification decides an icon and a click action, never whether the
 *  link is valid. An unrecognised shape is `"other"` and still renders — the
 *  same tolerate-and-show posture as the missing-dep chip. */
export type LinkTargetKind = "issue" | "url" | "path" | "other";

/** Classify a link target. Order matters: an issue ref is checked before a URL
 *  because `#123` cannot be one, and a URL before a path because a URL contains
 *  slashes too and would otherwise be misread as a repo path. */
export function linkTargetKind(target: string): LinkTargetKind {
  const t = (target ?? "").trim();
  if (!t) return "other";
  // `#123` — an issue or PR ref on the repo this group is for. Digits only, so
  // `#foo` (a fragment, an anchor) is not mistaken for one.
  if (/^#\d+$/.test(t)) return "issue";
  // Only http(s). A bare `example.com` is deliberately NOT a URL here: it is
  // indistinguishable from a file name, and guessing wrong sends a click to the
  // browser instead of the editor.
  if (/^https?:\/\//i.test(t)) return "url";
  // A repo-relative path. Must contain a separator or a file extension —
  // otherwise a bare word like `README` would claim every unclassified target.
  // A leading slash or a drive letter is NOT a repo path: those are absolute,
  // and this classification exists to open things inside the repo.
  if (/^[A-Za-z]:[\/]/.test(t) || t.startsWith("/") || t.startsWith("\\")) return "other";
  if (t.includes("/") || /\.[A-Za-z0-9]{1,10}$/.test(t)) return "path";
  return "other";
}

/** Does any row on the board carry grounding links? Drives whether the board
 *  shows link affordances at all — the same pay-for-what-you-use rule
 *  `boardUsesDeps`/`boardUsesHierarchy` already apply. */
export function boardUsesLinks<T extends HasArtifactLinks>(board: readonly T[]): boolean {
  return board.some((t) => (t.links?.length ?? 0) > 0);
}

/** Does any row carry a sprint? Same rule: a board that runs no sprints shows
 *  no sprint chrome, so the feature costs nothing to a group not using it. */
export function boardUsesSprints<T extends HasSprint>(board: readonly T[]): boolean {
  return board.some((t) => typeof t.sprint === "number");
}
