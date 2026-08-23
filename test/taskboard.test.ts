// Unit tests for the task-board "delete all done" selection hint (issue #120).
// The board shows a batch-delete button only when there are done tasks and
// reports how many will go; doneCount is the pure logic behind that. Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  approvableSelection,
  blockedTaskMap,
  blockingAncestor,
  boardMarker,
  boardUsesDeps,
  boardUsesHierarchy,
  buildTree,
  canApprove,
  canProceed,
  childCounts,
  containerIds,
  clearableCount,
  clearedIds,
  DEMO_STATUSES,
  depCandidates,
  depState,
  doneCount,
  filterActive,
  focusMiss,
  isCleared,
  kindFilterChoices,
  matchesFilter,
  NO_FILTER,
  orderSiblings,
  positionAmong,
  UNLABELLED_KIND,
  type BoardFilter,
  settledIds,
  grantableCount,
  hasMissingParent,
  indentLevel,
  isAwaitingHuman,
  isDemoGated,
  isReady,
  ladderRule,
  KINDS,
  kindFits,
  kindPickerChoices,
  levelRuleText,
  mayBeTopLevel,
  mayContain,
  MAX_INDENT_DEPTH,
  nextPicker,
  parentCandidates,
  parentPickerChoices,
  pickerIsOpen,
  reorderWithSubtree,
  siblingPosition,
  subtreeAllDone,
  visibleRows,
  PROTOTYPE_STATUS,
  REQUEST_CHANGES_STATUS,
  retainExisting,
  STATUSES,
  taskActivityState,
  unmetDeps,
  withDep,
  withoutDep,
  boardUsesLinks,
  boardUsesSprints,
  currentSprint,
  BACKLOG_SPRINT,
  LINK_TYPES,
  linkTargetKind,
  MAX_SPRINT,
  rollOverSet,
  sprintAdvance,
  sprintFilterChoices,
  sprintProgress,
} from "../src/taskboard.ts";

test("counts only tasks in the exact `done` status", () => {
  const tasks = [
    { status: "queued" },
    { status: "done" },
    { status: "in-progress" },
    { status: "done" },
    { status: "human-testing" },
    { status: "done" },
  ];
  assert.equal(doneCount(tasks), 3);
});

test("is zero when nothing is done (button stays hidden)", () => {
  assert.equal(doneCount([{ status: "queued" }, { status: "review" }]), 0);
  assert.equal(doneCount([]), 0);
});

test("does not match statuses that merely contain 'done'", () => {
  // Guards against a substring match sweeping up look-alike statuses.
  assert.equal(doneCount([{ status: "done-ish" }, { status: "predone" }]), 0);
});

// --- multi-select pruning (delete-selected, #120 follow-up) ---

test("retainExisting keeps only selected ids that still name a row", () => {
  const tasks = [{ id: "t-1" }, { id: "t-2" }, { id: "t-3" }];
  const live = retainExisting(["t-1", "t-3"], tasks);
  assert.deepEqual([...live].sort(), ["t-1", "t-3"]);
});

test("retainExisting drops ids whose rows vanished from the board", () => {
  // The human ticked t-2, then the orchestrator deleted it out from under them.
  const live = retainExisting(new Set(["t-1", "t-2"]), [{ id: "t-1" }]);
  assert.deepEqual([...live], ["t-1"]);
  // Count drives the "delete selected (N)" button — it must not outlive the row.
  assert.equal(live.size, 1);
});

test("retainExisting on an empty selection or empty board yields nothing", () => {
  assert.equal(retainExisting([], [{ id: "t-1" }]).size, 0);
  assert.equal(retainExisting(["t-1"], []).size, 0);
});

test("retainExisting returns a fresh set, not the input", () => {
  const selected = new Set(["t-1"]);
  const live = retainExisting(selected, [{ id: "t-1" }]);
  assert.notEqual(live, selected);
});

// --- prototype status + proceed workflow (#147) ---

test("prototype is offered in the status picker", () => {
  // The picker must expose the status or the human can never park a demo item.
  assert.ok(STATUSES.includes(PROTOTYPE_STATUS));
});

test("only a prototype is proceed-eligible (Proceed button gate)", () => {
  assert.equal(canProceed("prototype"), true);
  // Every other status the board knows about must NOT show Proceed.
  for (const s of STATUSES) {
    if (s === "prototype") continue;
    assert.equal(canProceed(s), false, `${s} must not be proceed-eligible`);
  }
});

test("proceed-eligibility does not match look-alike statuses", () => {
  // Guards against a substring/loose match sweeping up near-misses.
  assert.equal(canProceed("prototyped"), false);
  assert.equal(canProceed("proto"), false);
  assert.equal(canProceed(""), false);
});

test("a prototype is highlighted as awaiting the human", () => {
  // Prototype joins the merge gates and blocked as human-gated attention.
  assert.equal(isAwaitingHuman("prototype"), true);
  assert.equal(isAwaitingHuman("pr"), true);
  assert.equal(isAwaitingHuman("human-testing"), true);
  assert.equal(isAwaitingHuman("blocked"), true);
  // Statuses the human doesn't gate stay un-highlighted.
  assert.equal(isAwaitingHuman("queued"), false);
  assert.equal(isAwaitingHuman("in-progress"), false);
  assert.equal(isAwaitingHuman("review"), false);
  assert.equal(isAwaitingHuman("done"), false);
});

// --- task activity state (#339, refined: active requires a LIVE agent) ---
//
// The first cut of this highlight keyed off status alone. A human live-
// testing it found the exact gap that leaves: an assignee chip left over
// from a killed/resumed/reassigned session read as indistinguishable from a
// live agent actually at the keyboard. taskActivityState is the single
// source of truth this pins: a working-status task is ACTIVE only when its
// assignee is in the live-agent set, otherwise it's IDLE — assigned, working
// status, but nobody is actually there.

test("in-progress/review with a LIVE assignee is active", () => {
  const live = new Set(["w-2"]);
  assert.equal(taskActivityState("in-progress", "w-2", live), "active");
  assert.equal(taskActivityState("review", "rev-1", new Set(["rev-1"])), "active");
});

test("in-progress/review with an assignee that is NOT live is idle, not active", () => {
  // The human's exact complaint: an old assignee on a reopened/stalled task
  // must never masquerade as active work.
  const live = new Set(["w-9"]);
  assert.equal(taskActivityState("in-progress", "w-2", live), "idle");
  assert.equal(taskActivityState("review", "rev-1", new Set()), "idle");
});

test("in-progress/review with no assignee at all is idle, not active", () => {
  assert.equal(taskActivityState("in-progress", null, new Set(["w-2"])), "idle");
  assert.equal(taskActivityState("review", undefined, new Set(["w-2"])), "idle");
  assert.equal(taskActivityState("in-progress", "", new Set(["w-2"])), "idle");
});

test("done is always done, regardless of assignee or liveness", () => {
  assert.equal(taskActivityState("done", "w-2", new Set(["w-2"])), "done");
  assert.equal(taskActivityState("done", null, new Set()), "done");
});

test("queued and the human-gated statuses get no activity state", () => {
  // queued has nothing to highlight yet; pr/human-testing/blocked/prototype
  // already get isAwaitingHuman's own amber treatment, so this stays null
  // rather than layering a second, competing treatment on the same row.
  for (const s of ["queued", "pr", "human-testing", "blocked", "prototype"]) {
    assert.equal(taskActivityState(s, "w-2", new Set(["w-2"])), null, `${s} must get no activity state`);
  }
});

test("activity state does not match look-alike statuses", () => {
  const live = new Set(["w-2"]);
  assert.equal(taskActivityState("in-progress-ish", "w-2", live), null);
  assert.equal(taskActivityState("predone", "w-2", live), null);
  assert.equal(taskActivityState("", "w-2", live), null);
});

// --- merge-gate Approve visibility + request-changes reopening (#339) ---

test("Approve shows only for pr/human-testing", () => {
  assert.equal(canApprove("pr"), true);
  assert.equal(canApprove("human-testing"), true);
  for (const s of STATUSES) {
    if (s === "pr" || s === "human-testing") continue;
    assert.equal(canApprove(s), false, `${s} must not show Approve`);
  }
});

test("Approve gate does not match look-alike statuses", () => {
  assert.equal(canApprove("pr-ish"), false);
  assert.equal(canApprove("human-testing-done"), false);
  assert.equal(canApprove(""), false);
});

test("request-changes reopens to a status Approve does not show for", () => {
  // The state-honesty guarantee, pinned directly: whatever status a
  // request-changes reopen lands on, Approve must not show for it — a
  // reopened task can never keep displaying a stale Approve button.
  assert.equal(canApprove(REQUEST_CHANGES_STATUS), false);
  // And it's a real, pickable status, not a made-up one the picker lacks.
  assert.ok(STATUSES.includes(REQUEST_CHANGES_STATUS as (typeof STATUSES)[number]));
});

// --- bulk merge-gate approve: what a selection actually authorizes (#507) ---

const board = [
  { id: "t-1", status: "queued" },
  { id: "t-2", status: "pr" },
  { id: "t-3", status: "in-progress" },
  { id: "t-4", status: "human-testing" },
  { id: "t-5", status: "pr" },
];

test("bulk approve narrows a mixed selection to the rows at the merge gate", () => {
  // The board has ONE selection, shared with delete-selected, so a human can
  // tick a queued row and a PR row in the same pass. What Approve acts on —
  // and what the button counts — must be only the gate rows, or the count
  // promises grants it will not (and must not) issue.
  const picked = approvableSelection(["t-1", "t-2", "t-3", "t-4"], board);
  assert.deepEqual(
    picked.map((t) => t.id),
    ["t-2", "t-4"]
  );
});

test("bulk approve returns board order, not tick order", () => {
  // The confirm dialog lists these rows; reading top-to-bottom must match the
  // board above it, whatever order the human clicked the checkboxes in.
  const picked = approvableSelection(["t-5", "t-2", "t-4"], board);
  assert.deepEqual(
    picked.map((t) => t.id),
    ["t-2", "t-4", "t-5"]
  );
});

test("bulk approve ignores ticked ids that no longer name a row", () => {
  // Selection is frontend-only and can outlive its rows (the orchestrator
  // edits the board under it). A vanished id must never be sent for approval.
  const picked = approvableSelection(["t-2", "t-99"], board);
  assert.deepEqual(
    picked.map((t) => t.id),
    ["t-2"]
  );
});

test("bulk approve is empty when nothing ticked is at the gate", () => {
  // Drives the button's hidden state: no gate rows ticked, no affordance.
  assert.deepEqual(approvableSelection(["t-1", "t-3"], board), []);
  assert.deepEqual(approvableSelection([], board), []);
});

test("bulk approve accepts exactly the statuses a single Approve does", () => {
  // Bulk must never widen the gate: whatever canApprove admits for one row is
  // exactly what a selection admits, status for status.
  for (const s of STATUSES) {
    const rows = [{ id: "t-x", status: s }];
    assert.equal(
      approvableSelection(["t-x"], rows).length === 1,
      canApprove(s),
      `${s}: bulk selection and single Approve must agree`
    );
  }
});

test("the grant count is the linked-PR count, not the selection size", () => {
  // #507 review N2: the board's tooltip states how many one-time merge grants
  // are about to be issued. A gate row with no PR is approved but never
  // granted, so counting the selection would promise authority the backend
  // will not issue — a claim the code doesn't honor.
  const picked = [
    { id: "t-2", status: "pr", pr: "#7" },
    { id: "t-4", status: "human-testing", pr: null },
    { id: "t-5", status: "pr", pr: "https://github.com/o/r/pull/9" },
  ];
  assert.equal(picked.length, 3);
  assert.equal(grantableCount(picked), 2);
});

test("a blank PR ref counts as no PR", () => {
  // Whitespace is not a link. Keeps the count honest against a field someone
  // cleared by selecting the text rather than deleting the row's value.
  assert.equal(grantableCount([{ pr: "" }, { pr: "   " }, { pr: undefined }, {}]), 0);
  assert.equal(grantableCount([]), 0);
});

// ---------------------------------------------------------------------------
// Dependency links (#582, slice B — the board's side of the graph).
// The backend owns the rules (mod.rs `dep_satisfied`/`unmet_deps`/`task_ready`);
// these pin that the board's chips say the SAME thing, since the human's board
// reads full Tasks via orch_tasks and derives readiness itself.
// ---------------------------------------------------------------------------

/** A board where t-2 depends on t-1, plus an unrelated row. `deps` is omitted
 *  (not `[]`) on link-free rows, exactly as the backend serializes them. */
const linkedBoard = (depStatus: string) => [
  { id: "t-1", status: depStatus },
  { id: "t-2", status: "queued", deps: ["t-1"] },
  { id: "t-3", status: "queued" },
];

test("only a `done` dep is satisfied — pr/human-testing still block", () => {
  // The bar is merged/accepted, matching the backend's dep_satisfied: a dep at
  // `pr` is work the human hasn't signed off, so a dependent starting on it
  // would be building on something that can still come back.
  for (const blocking of ["queued", "in-progress", "review", "pr", "human-testing", "blocked"]) {
    const board = linkedBoard(blocking);
    assert.deepEqual(unmetDeps(board[1], board), ["t-1"], `dep at ${blocking} must block`);
    assert.equal(isReady(board[1], board), false, `dep at ${blocking} must not be ready`);
    assert.equal(depState("t-1", board), "unmet");
  }
  const done = linkedBoard("done");
  assert.deepEqual(unmetDeps(done[1], done), []);
  assert.equal(isReady(done[1], done), true);
  assert.equal(depState("t-1", done), "met");
});

test("a dep naming no task reads as missing and still counts as unmet", () => {
  // Only reachable by hand-editing tasks.json (the backend validates ids on
  // write and strips them from survivors on delete). Reading a typo as
  // "satisfied" would silently unblock work — the failure direction that
  // matters — so it blocks, and gets its own chip state for the human.
  const board = [{ id: "t-9", status: "queued", deps: ["t-gone"] }];
  assert.equal(depState("t-gone", board), "missing");
  assert.deepEqual(unmetDeps(board[0], board), ["t-gone"]);
  assert.equal(isReady(board[0], board), false);
});

test("unmet deps come back in the task's own link order", () => {
  // The chips render in this order, so it is the order the human reads "what
  // is holding this" in — not board order, and not sorted.
  const board = [
    { id: "t-1", status: "done" },
    { id: "t-2", status: "in-progress" },
    { id: "t-3", status: "review" },
    { id: "t-4", status: "queued", deps: ["t-3", "t-1", "t-2"] },
  ];
  assert.deepEqual(unmetDeps(board[3], board), ["t-3", "t-2"]);
});

test("only a queued task is ever ready, however satisfied its deps are", () => {
  // Readiness answers "can this START now", so anything already past queued —
  // including `done` itself — is not ready. Mirrors task_ready's first clause.
  const board = [
    { id: "t-1", status: "done" },
    { id: "t-2", status: "in-progress", deps: ["t-1"] },
    { id: "t-3", status: "done", deps: ["t-1"] },
    { id: "t-4", status: "queued", deps: ["t-1"] },
  ];
  assert.equal(isReady(board[1], board), false);
  assert.equal(isReady(board[2], board), false);
  assert.equal(isReady(board[3], board), true);
});

test("a queued task with no deps at all is ready", () => {
  // The pre-#582 board: no `deps` key on the wire (skip_serializing_if), so
  // missing must behave exactly like empty.
  const board = [{ id: "t-1", status: "queued" }, { id: "t-2", status: "queued", deps: [] }];
  assert.equal(isReady(board[0], board), true);
  assert.equal(isReady(board[1], board), true);
});

test("`related` never affects readiness", () => {
  // It is an annotation, not an edge — a see-also pointing at unfinished work
  // must not make a task read as blocked.
  const board = [
    { id: "t-1", status: "in-progress" },
    { id: "t-2", status: "queued", related: ["t-1"] },
  ];
  assert.deepEqual(unmetDeps(board[1], board), []);
  assert.equal(isReady(board[1], board), true);
});

// ---------------------------------------------------------------------------
// #958 slice R: readiness climbs the container chain. Mirrors the backend's
// `blocking_ancestor` / `task_ready` (mod.rs), which these tests are the board
// side of — the two derivations are duplicated on purpose (see `isReady`) and
// each is pinned where it lives.
// ---------------------------------------------------------------------------

/** A feature waiting on `t-1`, with one slice inside it whose own deps are
 *  clear. The board's shape for "can this slice start?". */
const nestedBoard = (blockerStatus: string) => [
  { id: "t-1", status: blockerStatus },
  { id: "t-2", status: "queued", deps: ["t-1"] },
  { id: "t-3", status: "queued", parent: "t-2" },
];

test("a slice is not ready while a container above it is still waiting", () => {
  // The point of the slice: t-3's own deps are empty, so pre-#958-R it read as
  // startable — while the feature it belongs to could not start at all.
  const board = nestedBoard("in-progress");
  assert.deepEqual(unmetDeps(board[2], board), [], "a container is not a dependency");
  assert.equal(blockingAncestor(board[2], board), "t-2", "the container is what is holding it");
  assert.equal(isReady(board[2], board), false);
  // And the container itself is blocked the ordinary way, by its own dep.
  assert.equal(isReady(board[1], board), false);

  // Clear the container's dep and the slice becomes startable in the same read.
  const clear = nestedBoard("done");
  assert.equal(blockingAncestor(clear[2], clear), null);
  assert.equal(isReady(clear[2], clear), true, "nothing above it is waiting any more");
  assert.equal(isReady(clear[1], clear), true);
});

test("readiness climbs the WHOLE chain, and names the nearest blocker", () => {
  // A grandparent's dep blocks a grandchild — one level of walking would miss
  // it, which is the failure this test exists to catch.
  const board = [
    { id: "t-1", status: "queued" },
    { id: "t-2", status: "queued" },
    { id: "epic", status: "queued", deps: ["t-1"] },
    { id: "feat", status: "queued", parent: "epic", deps: ["t-2"] },
    { id: "slice", status: "queued", parent: "feat" },
  ];
  assert.equal(blockingAncestor(board[4], board), "feat", "NEAREST first, not the top of the chain");
  assert.equal(isReady(board[4], board), false);

  // With only the grandparent waiting, the walk still has to reach it.
  const deep = board.map((t) => (t.id === "feat" ? { ...t, deps: [] } : t));
  assert.equal(blockingAncestor(deep[4], deep), "epic");
  assert.equal(isReady(deep[4], deep), false);
});

test("an ancestor's STATUS is never read — only its deps", () => {
  // `blocked` is the status for blockers OUTSIDE the board, which says nothing
  // about the work inside a container; and a feature sitting at `in-progress`
  // is the NORMAL state while its slices are the startable work. Gating on
  // either would make a slice's readiness a function of how promptly someone
  // maintains the container row.
  //
  // Swept over STATUSES — the vocabulary this module already mirrors from the
  // backend's TASK_STATUSES — rather than a list written out here, so a status
  // added later is covered the day it lands instead of escaping silently.
  for (const status of STATUSES) {
    const board = [
      { id: "t-1", status },
      { id: "t-2", status: "queued", parent: "t-1" },
    ];
    assert.equal(blockingAncestor(board[1], board), null, `container at ${status} blocks nothing`);
    assert.equal(isReady(board[1], board), true, `a child of a ${status} container is startable`);
  }
});

test("a hand-edited container never wedges readiness", () => {
  // A broken container must fail in the tolerate direction (§5 of
  // doc/design/task-hierarchy.md) — the opposite of an unknown DEP id, which
  // deliberately blocks. The asymmetry has a reason: readiness only ever reads
  // the DEPS of the containers it finds, so a chain ending nowhere contributes
  // nothing to check. An orphan has no container to be blocked by, and a cycle
  // must terminate rather than spin.
  const orphan = [{ id: "t-1", status: "queued", parent: "t-404" }];
  assert.equal(blockingAncestor(orphan[0], orphan), null);
  assert.equal(isReady(orphan[0], orphan), true);

  const selfParent = [{ id: "t-1", status: "queued", parent: "t-1" }];
  assert.equal(blockingAncestor(selfParent[0], selfParent), null, "its own deps are isReady's job");
  assert.equal(isReady(selfParent[0], selfParent), true);

  // A cycle still reports a REAL unmet dep on one of its members, having
  // visited each exactly once.
  const cycle = [
    { id: "t-0", status: "queued" },
    { id: "t-1", status: "queued", parent: "t-2" },
    { id: "t-2", status: "queued", parent: "t-1", deps: ["t-0"] },
  ];
  assert.equal(blockingAncestor(cycle[1], cycle), "t-2");
  assert.equal(isReady(cycle[1], cycle), false);
  const benign = [
    { id: "t-1", status: "queued", parent: "t-2" },
    { id: "t-2", status: "queued", parent: "t-1" },
  ];
  assert.equal(blockingAncestor(benign[0], benign), null, "a cycle is not itself a blocker");
  assert.equal(isReady(benign[0], benign), true);
});

test("the ready mark stays off a board that uses no deps", () => {
  // Every queued row on a dep-free board is trivially ready, so badging them
  // would put a mark on every queued row of every existing board and mean
  // nothing. The mark exists to separate "startable" from "waiting on
  // something", which only exists once some task declares a dep.
  assert.equal(boardUsesDeps([{ id: "t-1", status: "queued" }, { id: "t-2", status: "done" }]), false);
  assert.equal(boardUsesDeps([{ id: "t-1", status: "queued", deps: [] }]), false);
  assert.equal(boardUsesDeps([]), false);
  // One linked task turns it on for the WHOLE board: a plain queued row then
  // genuinely is startable and should say so.
  const board = [{ id: "t-1", status: "done" }, { id: "t-2", status: "queued", deps: ["t-1"] }];
  assert.equal(boardUsesDeps(board), true);
});

test("the dep picker offers every other task, minus the ones already linked", () => {
  const board = [
    { id: "t-1", status: "done" },
    { id: "t-2", status: "queued", deps: ["t-1"] },
    { id: "t-3", status: "queued" },
  ];
  assert.deepEqual(depCandidates(board[1], board).map((t) => t.id), ["t-3"]);
  // Board (priority) order, so the picker reads like the board above it.
  assert.deepEqual(depCandidates(board[2], board).map((t) => t.id), ["t-1", "t-2"]);
});

test("the picker does NOT hide a choice that would close a cycle", () => {
  // Deliberate: the backend rejects cycles inside its lock with an error
  // naming the path, and that surfaces through the board's existing error
  // toast. A frontend cycle walk would be a second copy of an authoritative
  // rule that could only ever disagree with it.
  const board = [
    { id: "t-1", status: "queued", deps: ["t-2"] },
    { id: "t-2", status: "queued" },
  ];
  assert.deepEqual(depCandidates(board[1], board).map((t) => t.id), ["t-1"]);
});

test("dep edits build the whole array — add is idempotent, remove can empty it", () => {
  // The board sends the FULL deps array on every edit (the backend's array
  // args are replace-or-untouched, never a delta), so these are what add and
  // remove actually mean on this path.
  assert.deepEqual(withDep(undefined, "t-1"), ["t-1"]);
  assert.deepEqual(withDep(["t-1"], "t-2"), ["t-1", "t-2"]);
  assert.deepEqual(withDep(["t-1", "t-2"], "t-1"), ["t-1", "t-2"]);
  assert.deepEqual(withoutDep(["t-1", "t-2"], "t-1"), ["t-2"]);
  // Removing the last one sends [] — the backend reads that as "clear",
  // where omitting the argument would mean "leave untouched".
  assert.deepEqual(withoutDep(["t-1"], "t-1"), []);
  assert.deepEqual(withoutDep(undefined, "t-1"), []);
  // Removing an id that isn't there leaves the array alone.
  assert.deepEqual(withoutDep(["t-1"], "t-9"), ["t-1"]);
});

// --- board hierarchy: containment, derived from the flat array (#958) ---

/** A board row for the hierarchy helpers — id, status, and the two #958
 *  fields, which are absent on every pre-#958 board. */
const row = (id: string, status = "queued", parent?: string, kind?: string) => ({
  id,
  status,
  ...(parent === undefined ? {} : { parent }),
  ...(kind === undefined ? {} : { kind }),
});

test("a board with no hierarchy renders flat, in board order, with no collapse gutter", () => {
  // The regression pin that matters most: every existing board arrives with no
  // `parent` key at all, and must come out of visibleRows in board order, flat,
  // with no collapse affordance anywhere.
  //
  // Every row here is LIVE on purpose (#1152). This specimen used to carry a
  // `done` row in the middle, which stopped witnessing "board order" the moment
  // finished subtrees started sinking below the live work — a done row's
  // position is now derived, so it can no longer stand for a row whose position
  // is the board's. The relevant-first tests below own that half; this one keeps
  // the flat/board-order property on rows it is still true of.
  const board = [row("t-1"), row("t-2", "in-progress"), row("t-3")];
  assert.deepEqual(
    visibleRows(board).map((r) => [r.task.id, r.depth, r.hasChildren]),
    [["t-1", 0, false], ["t-2", 0, false], ["t-3", 0, false]]
  );
});

test("children render under their container, at depth, in board order", () => {
  const board = [
    row("t-1", "queued", undefined, "epic"),
    row("t-2", "queued", "t-1", "feature"),
    row("t-3", "queued", "t-2", "story"),
    row("t-4", "queued", "t-1"),
    row("t-5"),
  ];
  assert.deepEqual(
    visibleRows(board).map((r) => [r.task.id, r.depth]),
    [["t-1", 0], ["t-2", 1], ["t-3", 2], ["t-4", 1], ["t-5", 0]]
  );
  assert.equal(visibleRows(board)[0].hasChildren, true);
  assert.equal(visibleRows(board)[2].hasChildren, false);
});

test("display order is derived, so a child stored above its container still nests", () => {
  // tasks.json order is PRIORITY order and the orchestrator writes rows in
  // whatever order work arrives — a child added before its epic must not
  // render detached above it.
  const board = [row("t-9", "queued", "t-1"), row("t-1"), row("t-2")];
  assert.deepEqual(
    visibleRows(board).map((r) => [r.task.id, r.depth]),
    [["t-1", 0], ["t-9", 1], ["t-2", 0]]
  );
});

test("collapsing a container hides its WHOLE subtree, not just its children", () => {
  const board = [row("t-1"), row("t-2", "queued", "t-1"), row("t-3", "queued", "t-2"), row("t-4")];
  const rows = visibleRows(board, ["t-1"]);
  // A grandchild left behind by a shallow hide would render stranded at the
  // top level, reading as unrelated work.
  assert.deepEqual(rows.map((r) => r.task.id), ["t-1", "t-4"]);
  assert.equal(rows[0].collapsed, true);
  assert.equal(rows[0].hasChildren, true);
  // Collapsing a leaf is inert — there is nothing under it to hide.
  assert.deepEqual(visibleRows(board, ["t-4"]).map((r) => r.task.id), ["t-1", "t-2", "t-3", "t-4"]);
  assert.equal(visibleRows(board, ["t-4"])[3].collapsed, false);
});

test("a container naming nothing, or itself, renders at top level instead of vanishing", () => {
  // Only reachable by hand-editing tasks.json (the backend validates on write
  // and re-homes survivors on delete), and the board must show such a row —
  // same reasoning as the `missing` dep chip.
  const board = [row("t-1", "queued", "t-404"), row("t-2", "queued", "t-2"), row("t-3")];
  assert.deepEqual(
    visibleRows(board).map((r) => [r.task.id, r.depth]),
    [["t-1", 0], ["t-2", 0], ["t-3", 0]]
  );
  assert.equal(hasMissingParent(board[0], board), true);
  assert.equal(hasMissingParent(board[1], board), false); // names a live row (itself)
  assert.equal(hasMissingParent(board[2], board), false); // no container at all
});

test("a hand-edited hierarchy cycle renders every row exactly once", () => {
  // The failure this pins is bidirectional: a naive walk either recurses
  // forever or drops both rows off the board entirely.
  const board = [row("t-1", "queued", "t-2"), row("t-2", "queued", "t-1"), row("t-3")];
  const ids = visibleRows(board).map((r) => r.task.id);
  assert.equal(ids.length, 3);
  assert.deepEqual([...ids].sort(), ["t-1", "t-2", "t-3"]);

  // And it holds through `buildTree`, not merely through `visibleRows`' own
  // fallback loop — whose comment says "nothing should reach this". A cyclic
  // row must be listed as a ROOT: that is the `cyclic` half of the shared
  // ancestor walk (`ancestorChain`), and it is the only consumer of that half.
  // Unpinned, a readiness-motivated change to that walk could stop reporting
  // cycles, silently move the root set (t-3 to the front, both cycle members
  // out of it) and leave the render-once invariant resting on the fallback —
  // with the assertions above still green, since they sort and count only.
  assert.deepEqual(
    buildTree(board).roots.map((r) => r.id),
    ["t-1", "t-2", "t-3"],
    "a cyclic row is a root, in board order"
  );
});

test("child counts are DIRECT children only, matching the backend's summary row", () => {
  // `children` / `children_done` on a TaskSummary are direct-only; the human's
  // board computing something else would put two different numbers for the
  // same thing in front of two different readers.
  const board = [
    row("t-1"),
    row("t-2", "done", "t-1"),
    row("t-3", "queued", "t-1"),
    row("t-4", "done", "t-2"),
  ];
  assert.deepEqual(childCounts("t-1", board), { total: 2, done: 1 });
  assert.deepEqual(childCounts("t-2", board), { total: 1, done: 1 });
  assert.deepEqual(childCounts("t-3", board), { total: 0, done: 0 });
  assert.deepEqual(childCounts("t-404", board), { total: 0, done: 0 });
});

test("the all-done nudge waits for the whole subtree, and never fires on a leaf", () => {
  const open = [row("t-1"), row("t-2", "done", "t-1"), row("t-3", "queued", "t-2")];
  // A direct-children-only reading would claim "everything under here is
  // finished" with t-3 still queued — a false statement on the human's board.
  assert.equal(subtreeAllDone("t-1", open), false);
  const finished = [row("t-1"), row("t-2", "done", "t-1"), row("t-3", "done", "t-2")];
  assert.equal(subtreeAllDone("t-1", finished), true);
  // A row with no children has nothing under it to have finished.
  assert.equal(subtreeAllDone("t-3", finished), false);
  assert.equal(subtreeAllDone("t-404", finished), false);
});

test("reordering moves a container together with its subtree", () => {
  const board = [row("t-1"), row("t-2", "queued", "t-1"), row("t-3", "queued", "t-2"), row("t-4")];
  // t-1 down one sibling step: its children ride along, or they would silently
  // re-home under whatever ends up above them in the flat array.
  assert.deepEqual(reorderWithSubtree(board, "t-1", 1), ["t-4", "t-1", "t-2", "t-3"]);
  assert.deepEqual(reorderWithSubtree(board, "t-4", -1), ["t-4", "t-1", "t-2", "t-3"]);
});

test("reordering is sibling-scoped and never leaves a row out of the order", () => {
  const board = [
    row("t-1"),
    row("t-2", "queued", "t-1"),
    row("t-3", "queued", "t-1"),
    row("t-4"),
    row("t-5", "queued", "t-4"),
  ];
  // Swapping two children rewrites only their own container's order.
  assert.deepEqual(reorderWithSubtree(board, "t-3", -1), ["t-1", "t-3", "t-2", "t-4", "t-5"]);
  // The last child has nowhere lower to go: it must NOT fall out of its
  // container into the next one's list — orch_reorder_tasks would take that
  // literally and the row would silently change container on the next render.
  assert.deepEqual(reorderWithSubtree(board, "t-3", 1), ["t-1", "t-2", "t-3", "t-4", "t-5"]);
  assert.deepEqual(reorderWithSubtree(board, "t-2", -1), ["t-1", "t-2", "t-3", "t-4", "t-5"]);
  // Unknown id: still the full current order, never a short array.
  assert.deepEqual(reorderWithSubtree(board, "t-404", 1), ["t-1", "t-2", "t-3", "t-4", "t-5"]);
});

test("the order sent is always a permutation of the board, even on a cyclic one", () => {
  // reorder_tasks appends whatever the caller omitted, so a dropped id is a
  // silent priority change nobody asked for.
  const board = [row("t-1", "queued", "t-2"), row("t-2", "queued", "t-1"), row("t-3")];
  const sent = reorderWithSubtree(board, "t-3", -1);
  assert.deepEqual([...sent].sort(), ["t-1", "t-2", "t-3"]);
  assert.equal(new Set(sent).size, 3);
});

test("sibling position counts siblings, not board rows", () => {
  // This is what disables the up/down buttons: the first child of a container
  // has nowhere higher to go, however many rows sit above it on the board.
  const board = [row("t-1"), row("t-2", "queued", "t-1"), row("t-3", "queued", "t-1"), row("t-4")];
  assert.deepEqual(siblingPosition(board, "t-2"), { index: 0, count: 2 });
  assert.deepEqual(siblingPosition(board, "t-3"), { index: 1, count: 2 });
  assert.deepEqual(siblingPosition(board, "t-4"), { index: 1, count: 2 }); // roots are siblings too
  assert.deepEqual(siblingPosition(board, "t-404"), { index: -1, count: 0 });
});

test("on a LEVEL-LESS board the nest picker still offers every other row", () => {
  // The exemption, stated as a test (#1156): a board that carries no Agile
  // levels is a flat board, and the ladder must be invisible on it — forever,
  // not as a migration allowance. This is the pre-#1156 behaviour, unchanged.
  const board = [row("t-1"), row("t-2", "queued", "t-1"), row("t-3")];
  // t-1 is already t-2's container — offering it again would send a no-op write.
  assert.deepEqual(parentCandidates(board[1], board).map((t) => t.id), ["t-3"]);
  assert.deepEqual(parentCandidates(board[2], board).map((t) => t.id), ["t-1", "t-2"]);
  // Its own descendant IS offered, deliberately: the backend rejects the cycle
  // inside its lock with an error naming the path, and a second copy of that
  // rule here could only ever disagree with the authoritative one — the same
  // call depCandidates makes. #1156 narrowed this picker by LEVEL and left that
  // decision exactly where it was.
  assert.deepEqual(parentCandidates(board[0], board).map((t) => t.id), ["t-2", "t-3"]);
});

test("the nest picker offers only containers the ladder would accept", () => {
  const board = [
    row("e-1", "queued", undefined, "epic"),
    row("f-1", "queued", "e-1", "feature"),
    row("f-2", "queued", "e-1", "feature"),
    row("us-1", "queued", "f-1", "story"),
    row("t-9"), // a level-less row on the same board
  ];
  const at = (id: string) => board.find((t) => t.id === id)!;
  // A feature may move to another epic — there is only one here, and it is
  // already its container, so nothing is offered.
  assert.deepEqual(parentCandidates(at("f-1"), board).map((t) => t.id), []);
  // A story's legal containers are the features — not the epic above them, and
  // not the level-less row.
  assert.deepEqual(parentCandidates(at("us-1"), board).map((t) => t.id), ["f-2"]);
  // The level-less row may go anywhere (the exemption), including inside a
  // story and inside the epic.
  assert.deepEqual(parentCandidates(at("t-9"), board).map((t) => t.id), [
    "e-1",
    "f-1",
    "f-2",
    "us-1",
  ]);
  // ...but nothing may be nested inside the level-less row except another
  // level-less row: "inside an epic" is a claim its container does not make.
  assert.deepEqual(parentCandidates(at("e-1"), board).map((t) => t.id), []);
});

test("the top-level escape is offered only when leaving the container is legal", () => {
  const board = [
    row("e-1", "queued", undefined, "epic"),
    row("f-1", "queued", "e-1", "feature"),
    row("t-9", "queued", "e-1"),
  ];
  const at = (id: string) => board.find((t) => t.id === id)!;
  // A feature cannot exist at top level, so "↥ top level" would be a write the
  // backend refuses — and with no other epic to move to, the picker has nothing
  // at all to say. The label has to name the missing LEVEL: "no task to nest
  // under" reads as a bug on a board that plainly has tasks.
  const feature = parentPickerChoices(at("f-1"), board);
  assert.equal(feature.topLevel, false);
  assert.deepEqual(feature.candidates, []);
  assert.equal(feature.emptyLabel, "no epic to nest this feature under");
  // A level-less row nested in that epic may always leave it.
  assert.equal(parentPickerChoices(at("t-9"), board).topLevel, true);
  // An epic is already at top level, so there is no escape to offer.
  assert.equal(parentPickerChoices(at("e-1"), board).topLevel, false);
  assert.equal(parentPickerChoices(at("e-1"), board).emptyLabel, "an epic is top-level only");
});

test("the kind picker offers only levels this row could legally take here", () => {
  const board = [
    row("e-1", "queued", undefined, "epic"),
    row("f-1", "queued", "e-1", "feature"),
    row("t-9", "queued", "f-1"),
    row("t-8"),
  ];
  const at = (id: string) => board.find((t) => t.id === id)!;
  // A row inside a feature can only ever be a story — the level below its
  // container — plus the clear.
  assert.deepEqual(kindPickerChoices(at("t-9"), board), { candidates: ["story"], clear: false });
  // A top-level row can only be an epic (or stay level-less).
  assert.deepEqual(kindPickerChoices(at("t-8"), board), { candidates: ["epic"], clear: false });
  // e-1 holds a feature, so it cannot stop being an epic: neither another level
  // nor the clear is offered, and the picker is empty.
  assert.deepEqual(kindPickerChoices(at("e-1"), board), { candidates: [], clear: false });
  // f-1's only child is level-less, so f-1 may be cleared — but it still cannot
  // become anything else, since its own container is an epic.
  assert.deepEqual(kindPickerChoices(at("f-1"), board), { candidates: [], clear: true });
});

test("the kind picker offers nothing on a row whose container is level-less", () => {
  // Not a dead end by accident: the backend refuses a level here too, because
  // "inside a feature" is a claim about the container. The way out is to level
  // the container first — which is what an empty picker sends the human to do,
  // rather than a toast after a refused write.
  const board = [row("t-1"), row("t-2", "queued", "t-1")];
  assert.deepEqual(kindPickerChoices(board[1], board), { candidates: [], clear: false });
});

test("the kind picker offers nothing but the clear on an out-of-vocabulary kind", () => {
  // Only reachable by hand-editing tasks.json — the backend refuses an unknown
  // kind on write. Such a row is EXEMPT (no rule can say where a fifth level
  // belongs), so clearing it is legal; the levels it could take still depend on
  // where it sits, and at top level that is `epic` alone.
  const board = [row("t-1", "queued", undefined, "sprint")];
  assert.deepEqual(kindPickerChoices(board[0], board), { candidates: ["epic"], clear: true });
});

test("a broken container pointer leaves no level the row can take", () => {
  // t-404 is not on the board (hand-edited only). The backend refuses a level
  // write it cannot judge, so the picker offers none — and the clear only if
  // there is something to clear.
  const board = [row("t-1", "queued", "t-404", "story")];
  assert.deepEqual(kindPickerChoices(board[0], board), { candidates: [], clear: true });
  assert.equal(kindFits(board[0], "task", board), false);
});

test("a broken container pointer does not excuse the row's own children", () => {
  // rev round 1, N1. `kindFits` used to answer the unresolvable-container case
  // with an early return, which skipped the children walk entirely — so the
  // picker offered "clear" on t-1 and the backend then refused the write
  // (`t-1 cannot have its level cleared — t-2 is a story…`). Both questions are
  // asked on every write, so both must be asked on every candidate.
  const board = [row("t-1", "queued", "t-404", "feature"), row("t-2", "queued", "t-1", "story")];
  assert.equal(
    kindFits(board[0], null, board),
    false,
    "clearing t-1's level would strand the story inside it, whatever its own container is"
  );
  assert.deepEqual(kindPickerChoices(board[0], board), { candidates: [], clear: false });
  // The control: with the child level-less, the clear IS legal again — so the
  // fix narrowed the picker by the children rule, not by refusing outright.
  const freed = [row("t-1", "queued", "t-404", "feature"), row("t-2", "queued", "t-1")];
  assert.deepEqual(kindPickerChoices(freed[0], freed), { candidates: [], clear: true });
});

test("the ladder table is the same one the backend enforces", () => {
  // The whole rule, as a table. This half pins the BOARD's copy against
  // literals; the test below reads the backend's own table out of the Rust
  // source and pins the two against each other, which is what makes "a
  // divergence reddens" true rather than merely intended (rev round 1, N2).
  assert.deepEqual(ladderRule("epic"), { rule: "top-level-only" });
  assert.deepEqual(ladderRule("feature"), { rule: "inside", container: "epic" });
  assert.deepEqual(ladderRule("story"), { rule: "inside", container: "feature" });
  assert.deepEqual(ladderRule("task"), { rule: "inside", container: "story" });
  assert.deepEqual(ladderRule(null), { rule: "exempt" });
  assert.deepEqual(ladderRule(undefined), { rule: "exempt" });
  assert.deepEqual(ladderRule("sprint"), { rule: "exempt" });
  // The ladder is a chain: no level may contain itself, and no level may skip.
  assert.equal(mayContain("epic", "story"), false);
  assert.equal(mayContain("epic", "epic"), false);
  assert.equal(mayContain("story", "task"), true);
  assert.equal(mayContain("task", "task"), false);
  // Both exemption directions, which is what a flat board depends on.
  assert.equal(mayContain("epic", null), true);
  assert.equal(mayContain(null, null), true);
  assert.equal(mayContain(null, "task"), false);
  // Only an epic (or a level-less row) belongs at top level.
  assert.equal(mayBeTopLevel("epic"), true);
  assert.equal(mayBeTopLevel(null), true);
  for (const k of ["feature", "story", "task"]) assert.equal(mayBeTopLevel(k), false);
  // Every level in the VOCABULARY has a place on the ladder, so a fifth kind
  // cannot ship silently exempt on this side — the loop the Rust table test
  // already carries, which this one was missing (rev round 1, N3).
  for (const k of KINDS) {
    assert.notDeepEqual(ladderRule(k), { rule: "exempt" }, `${k} must have a place on the ladder`);
  }
});

// The MECHANICAL half of the cross-language pin (rev round 1, N2). The board
// keeps a second copy of the backend's ladder so the pickers can offer only
// legal targets, and `taskboard.ts` argues that is safe because the rule is a
// lookup over closed vocabularies rather than a re-derivation over the tree.
// That argument is only worth anything if a divergence REDDENS — and two tests
// each asserting their own table against their own literals do not do that:
// editing the Rust rule and its Rust test leaves this side green on the old
// table, which is exactly the hole the reviewer named.
//
// So this reads the backend's table out of the Rust source and compares it to
// this side's, in the shape `tests/groupid.rs` and `test/perfpolicy.test.ts`
// already establish for a source-scanning guard: DEFAULT-DENY (an unreadable
// file, a missing function, or a table it cannot parse all FAIL), decided on a
// shape that cannot compile any other way rather than on any binding's name,
// and with its blind spots stated rather than implied.
//
// What it cannot see, stated: the arms are matched textually, so a `ladder_rule`
// rewritten to compute its answer (a nested match, a helper call, a non-literal
// container) parses to fewer arms than `KINDS` has entries — which is why the
// per-level completeness assertion below is the one that must fail loudly, and
// does. It reads the rule ONLY; the enforcement that consumes it is pinned by
// the Rust suite.
const RUST_LADDER = new URL("../src-tauri/src/orchestration/mod.rs", import.meta.url);

test("the board's ladder table is the backend's, read out of the Rust source", () => {
  const src = readFileSync(RUST_LADDER, "utf8");
  const start = src.indexOf("pub fn ladder_rule(");
  assert.notEqual(
    start,
    -1,
    "ladder_rule is gone or renamed in mod.rs — this guard reads that function by name, so " +
      "update it here rather than deleting the only thing pinning the two ladders together"
  );
  // The function body: up to the first closing brace at column 0.
  const end = src.indexOf("\n}", start);
  assert.notEqual(end, -1, "could not find the end of ladder_rule's body");
  const body = src.slice(start, end);

  const arms = [...body.matchAll(/Some\("([a-z]+)"\)\s*=>\s*LadderRule::(TopLevelOnly|Exempt|Inside\("([a-z]+)"\))/g)];
  assert.ok(
    arms.length > 0,
    `no ladder arms could be parsed out of ladder_rule — the guard matches ` +
      `\`Some("<kind>") => LadderRule::…\`, so either the table moved or it is now computed. ` +
      `Body was:\n${body}`
  );
  // Default-deny on the fall-through too: an exempt catch-all is what makes a
  // level-less row legal anywhere, and it is a load-bearing part of the table.
  assert.match(body, /_\s*=>\s*LadderRule::Exempt/, "the backend's exempt catch-all is gone");

  const rust = new Map(
    arms.map(([, kind, variant, container]) => [
      kind,
      variant.startsWith("Inside")
        ? { rule: "inside", container }
        : variant === "TopLevelOnly"
          ? { rule: "top-level-only" }
          : { rule: "exempt" },
    ])
  );
  // Every level this side knows must appear in the backend's table, and agree.
  // This is the assertion that catches a rewrite the regex cannot follow: an
  // arm it fails to parse is an arm missing from `rust`.
  for (const k of KINDS) {
    assert.ok(rust.has(k), `${k} has no arm in the backend's ladder_rule (or the guard could not parse it)`);
    assert.deepEqual(
      ladderRule(k),
      rust.get(k),
      `the board's ladder disagrees with the backend's for "${k}" — the pickers would offer ` +
        `something upsert_task refuses (or hide something it allows)`
    );
  }
  // ...and the backend must know no level this side has never heard of.
  for (const k of rust.keys()) {
    assert.ok((KINDS as readonly string[]).includes(k), `the backend has a level the board does not: ${k}`);
  }
});

test("the level tooltip is derived from the ladder, never written out beside it", () => {
  // The claim this feature exists to correct was a stale word ("advisory") in
  // prose next to a rule that had changed. Deriving the sentence is what stops
  // the next one.
  assert.equal(levelRuleText("epic"), "it sits at the top level and inside nothing");
  assert.equal(levelRuleText("feature"), "it must sit inside an epic");
  assert.equal(levelRuleText("story"), "it must sit inside a feature");
  assert.equal(levelRuleText("task"), "it must sit inside a story");
  assert.equal(levelRuleText(null), "no level, so it may sit anywhere");
});

test("indent is clamped, so a hand-edited over-deep row still fits the overlay", () => {
  assert.equal(indentLevel(0), 0);
  assert.equal(indentLevel(MAX_INDENT_DEPTH), MAX_INDENT_DEPTH);
  assert.equal(indentLevel(MAX_INDENT_DEPTH + 3), MAX_INDENT_DEPTH);
});

// --- the row's pickers: one open at a time, and no swallowed click ---
//
// Every picker takes focus on open, and its `blur` defers the close by a
// timeout so the click that caused the blur lands first. The close therefore
// runs AFTER that click, which is why it has to re-ask whether it still owns
// the open picker — by the same two signals the opening button decides on.

test("switching pickers on ONE row does not swallow the click", () => {
  // The nest picker is open and focused on t-1; the human clicks that same
  // row's dependency button. mousedown blurs the select (queuing the nest
  // picker's close), the click opens the dep picker, and the queued close then
  // runs. If it decides on the row id alone it matches, nulls the state, and
  // the dep picker opens and closes in one tick — the click reads as doing
  // nothing and the human has to click again.
  const afterClick = nextPicker({ id: "t-1", field: "parent" }, "t-1", "dep");
  assert.deepEqual(afterClick, { id: "t-1", field: "dep" });
  assert.equal(pickerIsOpen(afterClick, "t-1", "parent"), false);
  // Symmetric — the same click in the other direction must survive too.
  const other = nextPicker({ id: "t-1", field: "dep" }, "t-1", "parent");
  assert.deepEqual(other, { id: "t-1", field: "parent" });
  assert.equal(pickerIsOpen(other, "t-1", "dep"), false);
});

test("a deferred close still closes its own picker", () => {
  // The negative control for the test above: making close() field-aware must
  // not turn it into "never close", which would leave a picker stuck open on
  // blur and on Esc.
  assert.equal(pickerIsOpen({ id: "t-1", field: "parent" }, "t-1", "parent"), true);
  assert.equal(pickerIsOpen({ id: "t-1", field: "dep" }, "t-1", "dep"), true);
  // Someone else's picker is never this close's to shut.
  assert.equal(pickerIsOpen({ id: "t-2", field: "parent" }, "t-1", "parent"), false);
  assert.equal(pickerIsOpen(null, "t-1", "parent"), false);
});

test("opening a picker on another row replaces the open one, and the old close is inert", () => {
  assert.deepEqual(nextPicker({ id: "t-1", field: "dep" }, "t-2", "dep"), {
    id: "t-2",
    field: "dep",
  });
  assert.equal(pickerIsOpen({ id: "t-2", field: "dep" }, "t-1", "dep"), false);
});

test("clicking the open picker's own button closes it, and its queued close is a no-op", () => {
  const closed = nextPicker({ id: "t-1", field: "dep" }, "t-1", "dep");
  assert.equal(closed, null);
  assert.equal(pickerIsOpen(closed, "t-1", "dep"), false);
  // Nothing open yet: the first click opens.
  assert.deepEqual(nextPicker(null, "t-1", "parent"), { id: "t-1", field: "parent" });
});

test("the nesting chrome stays off a board that nests nothing", () => {
  // Same gate as boardUsesDeps: a board where nothing is nested must keep the
  // exact row shape it has today, rather than growing an empty collapse gutter
  // in front of every row for a feature it doesn't use.
  assert.equal(boardUsesHierarchy([row("t-1"), row("t-2", "done")]), false);
  assert.equal(boardUsesHierarchy([]), false);
  // One nested row turns it on for the WHOLE board — a top-level row on a
  // nested board is saying something, and needs the same left edge as the rest.
  assert.equal(boardUsesHierarchy([row("t-1"), row("t-2", "queued", "t-1")]), true);
  // A container naming nothing still counts: the row IS nested as far as the
  // data goes, and the board has to be able to say so.
  assert.equal(boardUsesHierarchy([row("t-1", "queued", "t-404")]), true);
});

test("the kind vocabulary mirrors the backend's TASK_KINDS, in order", () => {
  // The picker offers exactly these; anything else is refused on write.
  assert.deepEqual([...KINDS], ["epic", "feature", "story", "task"]);
});

test("a PR ref the backend cannot resolve is still counted as linked", () => {
  // Deliberate, and the reason the tooltip says "one per LINKED PR": only the
  // backend resolves a ref to the number a grant is keyed on, so the board
  // counts what it can actually see rather than duplicating that parser here
  // and drifting from it. Such an item lands in the notice's
  // no-PR-number-could-be-resolved sentence instead.
  assert.equal(grantableCount([{ pr: "TBD" }]), 1);
});

// --- board marker + deep-link (#1091 slice G) ---

test("the t-N -> q-N map cites only pending questions with a real task field", () => {
  const map = blockedTaskMap([
    { id: "q-1", task: "t-1" },
    { id: "q-2", task: "  " },
    { id: "q-3" },
  ]);
  assert.deepEqual([...map], [["t-1", "q-1"]]);
});

test("two pending questions citing the same task: the FIRST in list order wins", () => {
  // Pending order is ask order, oldest first (decisions.ts's projectQuestions
  // keeps file order for exactly this reason) — the row should link to the
  // oldest still-open ask, not whichever the caller iterated last.
  const map = blockedTaskMap([
    { id: "q-1", task: "t-7" },
    { id: "q-2", task: "t-7" },
  ]);
  assert.equal(map.get("t-7"), "q-1");
});

test("the chip is decision-blocked for a row a pending question cites, whatever its status", () => {
  const map = blockedTaskMap([{ id: "q-9", task: "t-1" }]);
  assert.deepEqual(boardMarker(row("t-1", "queued"), map), { kind: "decision", target: "q-9" });
  // Even on a demo-gated row: the decision is the more specific, more
  // blocking ask, and the brief calls for exactly one chip per row, not two
  // competing for the same corner.
  assert.deepEqual(boardMarker(row("t-1", "human-testing"), map), {
    kind: "decision",
    target: "q-9",
  });
});

test("the chip is demo-gated for exactly the two demo statuses, when nothing blocks it", () => {
  const nothingPending = blockedTaskMap([]);
  for (const s of DEMO_STATUSES) {
    assert.deepEqual(boardMarker(row("t-1", s), nothingPending), { kind: "demo", target: "t-1" });
  }
  assert.equal(boardMarker(row("t-1", "queued"), nothingPending), null);
  // `pr` is awaiting-human (the merge gate) but deliberately not a demo —
  // isDemoGated is narrower than isAwaitingHuman, so this row gets no chip.
  assert.equal(isDemoGated("pr"), false);
  assert.equal(boardMarker(row("t-1", "pr"), nothingPending), null);
});

test("a settled question clears the chip, because it drops out of the caller's pending list", () => {
  // There is no separate "clear" step to forget: `blockedTaskMap` is handed
  // only PENDING questions each render (tasksview.ts filters with
  // decisions.ts's own isPending), so a settled/withdrawn question simply
  // stops appearing in the list the very next time it's built.
  const whilePending = blockedTaskMap([{ id: "q-1", task: "t-1" }]);
  assert.deepEqual(boardMarker(row("t-1"), whilePending), { kind: "decision", target: "q-1" });
  const afterSettle = blockedTaskMap([]);
  assert.equal(boardMarker(row("t-1"), afterSettle), null);
});

test("a pending question citing a task id nothing on the board carries marks nothing, and does not crash", () => {
  const map = blockedTaskMap([{ id: "q-1", task: "t-404" }]);
  const board = [row("t-1"), row("t-2", "prototype")];
  assert.equal(boardMarker(board[0], map), null);
  // The orphaned citation doesn't wipe t-2's OWN demo-gated signal either.
  assert.deepEqual(boardMarker(board[1], map), { kind: "demo", target: "t-2" });
});

// --- relevant-first order and the cleared archive (#1152) ---

/** A board row for the ordering projection: the hierarchy field above plus the
 *  two #1152 reads — `updated_ms` (how the finished half is ordered) and
 *  `cleared_ms` (the human's archive stamp). Both absent on every pre-#1152
 *  board, so both are optional here too. */
const orow = (
  id: string,
  status = "queued",
  extra: { parent?: string; updated_ms?: number; cleared_ms?: number } = {}
) => ({ id, status, ...extra });

test("finished work sinks below the live work, and live priority order is untouched", () => {
  // The human's ask (#1152): on a 400-row board the items anyone can act on
  // must not be scattered among hundreds of done rows. What must NOT change is
  // the drag order among the live ones — board order IS priority order, and
  // "top = next" is a contract the orchestrator reads.
  const board = [
    orow("t-1", "done", { updated_ms: 10 }),
    orow("t-2", "queued"),
    orow("t-3", "done", { updated_ms: 30 }),
    orow("t-4", "in-progress"),
    orow("t-5", "done", { updated_ms: 20 }),
    orow("t-6", "blocked"),
  ];
  assert.deepEqual(
    visibleRows(board).map((r) => r.task.id),
    // Live rows first, in exactly the array order they had; then the finished
    // ones, most recently updated first.
    ["t-2", "t-4", "t-6", "t-3", "t-5", "t-1"]
  );
  // And the split is reported on the row, which is what turns the manual
  // reorder buttons off and explains why.
  assert.deepEqual(
    visibleRows(board).map((r) => r.settled),
    [false, false, false, true, true, true]
  );
});

test("ties in the finished half break on board order, never on an arbitrary sort", () => {
  // A board whose rows carry no `updated_ms` at all must keep its own order
  // rather than being shuffled by an unstable comparison.
  const board = [orow("t-1", "done"), orow("t-2", "queued"), orow("t-3", "done")];
  assert.deepEqual(visibleRows(board).map((r) => r.task.id), ["t-2", "t-1", "t-3"]);
  // Same when the stamps are equal.
  const tied = [
    orow("t-1", "done", { updated_ms: 7 }),
    orow("t-2", "done", { updated_ms: 7 }),
    orow("t-3", "queued"),
  ];
  assert.deepEqual(visibleRows(tied).map((r) => r.task.id), ["t-3", "t-1", "t-2"]);
});

test("a done container holding live work does NOT sink, so the live row stays visible", () => {
  // The failure this pins is losing work off the bottom of the board: sinking
  // a container takes its whole subtree with it, so the rule has to be
  // "finished, with nothing unfinished inside", not "status says done".
  const board = [
    orow("t-1", "done", { updated_ms: 50 }),
    orow("t-2", "queued", { parent: "t-1" }),
    orow("t-3", "queued"),
  ];
  assert.deepEqual(visibleRows(board).map((r) => [r.task.id, r.settled]), [
    ["t-1", false],
    ["t-2", false],
    ["t-3", false],
  ]);
  // Finish the child and the whole subtree settles as one unit.
  const finished = [
    orow("t-1", "done", { updated_ms: 50 }),
    orow("t-2", "done", { parent: "t-1", updated_ms: 51 }),
    orow("t-3", "queued"),
  ];
  assert.deepEqual(visibleRows(finished).map((r) => [r.task.id, r.settled]), [
    ["t-3", false],
    ["t-1", true],
    ["t-2", true],
  ]);
  assert.deepEqual([...settledIds(finished)].sort(), ["t-1", "t-2"]);
});

test("sinking happens inside every container, not only at the top level", () => {
  const board = [
    orow("t-1"),
    orow("t-2", "done", { parent: "t-1", updated_ms: 5 }),
    orow("t-3", "queued", { parent: "t-1" }),
    orow("t-4", "in-progress", { parent: "t-1" }),
  ];
  assert.deepEqual(
    visibleRows(board).map((r) => [r.task.id, r.depth]),
    [["t-1", 0], ["t-3", 1], ["t-4", 1], ["t-2", 1]]
  );
});

test("a hand-edited containment cycle never sinks or hides, and still renders once", () => {
  // closedSubtrees fails safe on a cycle: a row it cannot finish walking is
  // never treated as a closed finished/cleared subtree, so the tolerate-and-show
  // rule (section 5 of doc/design/task-hierarchy.md) survives the projection.
  const board = [
    orow("t-1", "done", { parent: "t-2", cleared_ms: 1 }),
    orow("t-2", "done", { parent: "t-1", cleared_ms: 1 }),
    orow("t-3", "queued"),
  ];
  const ids = visibleRows(board).map((r) => r.task.id);
  assert.equal(ids.length, 3);
  assert.deepEqual([...ids].sort(), ["t-1", "t-2", "t-3"]);
  assert.equal(settledIds(board).size, 0);
  assert.equal(clearedIds(board).size, 0);
});

test("cleared rows drop out of the list and come back on request — nothing is lost", () => {
  const board = [
    orow("t-1", "done", { updated_ms: 10, cleared_ms: 999 }),
    orow("t-2", "queued"),
    orow("t-3", "done", { updated_ms: 20 }),
  ];
  assert.deepEqual(visibleRows(board).map((r) => r.task.id), ["t-2", "t-3"]);
  const shown = visibleRows(board, [], true);
  assert.deepEqual(shown.map((r) => r.task.id), ["t-2", "t-3", "t-1"]);
  // The archive marker is reported per row so the view can label it and offer
  // the per-row undo.
  assert.deepEqual(shown.map((r) => r.cleared), [false, false, true]);
});

test("reopening a cleared task brings it straight back, with no repair pass", () => {
  // The stamp alone must not hide a row: `isCleared` honours it only while the
  // row is still `done`, so the orchestrator moving an archived task back to
  // in-progress cannot leave live work invisible on the human's board.
  const reopened = orow("t-1", "in-progress", { cleared_ms: 999 });
  assert.equal(isCleared(reopened), false);
  assert.equal(isCleared(orow("t-1", "done", { cleared_ms: 999 })), true);
  assert.equal(isCleared(orow("t-1", "done")), false);
  assert.deepEqual(visibleRows([reopened, orow("t-2")]).map((r) => r.task.id), ["t-1", "t-2"]);
});

test("a cleared container holding a live child stays on the board", () => {
  // Hiding it would take the live child off the board with it — the same
  // whole-subtree rule the sink uses, applied to the archive.
  const board = [
    orow("t-1", "done", { cleared_ms: 1 }),
    orow("t-2", "queued", { parent: "t-1" }),
  ];
  assert.deepEqual(visibleRows(board).map((r) => r.task.id), ["t-1", "t-2"]);
  assert.equal(clearedIds(board).size, 0);
});

test("the clear button counts what it would archive, not what is already archived", () => {
  const board = [
    orow("t-1", "done", { cleared_ms: 5 }),
    orow("t-2", "done"),
    orow("t-3", "queued"),
    orow("t-4", "done"),
  ];
  assert.equal(clearableCount(board), 2);
  // doneCount is the DESTRUCTIVE button's number and deliberately still counts
  // every done row, archive included: understating what one confirm click
  // destroys would be worse than two counts that look alike.
  assert.equal(doneCount(board), 3);
  // Nothing left to clear once they are all stamped.
  assert.equal(clearableCount(board.map((t) => ({ ...t, cleared_ms: 5 }))), 0);
});

test("a board whose every row is cleared renders nothing — the view says so instead", () => {
  // tasksview renders its own "everything here is cleared" line off this: an
  // empty row list on a NON-empty board must never fall through to the "no
  // tasks yet" message, which on a 400-row board would read as data loss.
  const board = [orow("t-1", "done", { cleared_ms: 1 }), orow("t-2", "done", { cleared_ms: 1 })];
  assert.equal(visibleRows(board).length, 0);
  assert.equal(visibleRows(board, [], true).length, 2);
});

test("one reorder step is one step ON SCREEN, skipping the rows the board sank", () => {
  // The dead click this pins: with a done row stored between two live ones, a
  // move computed against the stored array swaps the live row with a row the
  // human cannot see next to it, and the click appears to do nothing.
  const board = [
    orow("t-1", "queued"),
    orow("t-2", "done", { updated_ms: 1 }),
    orow("t-3", "queued"),
  ];
  assert.deepEqual(visibleRows(board).map((r) => r.task.id), ["t-1", "t-3", "t-2"]);
  // t-1 down one: it must end up BELOW t-3 on screen.
  const sent = reorderWithSubtree(board, "t-1", 1);
  assert.deepEqual(sent, ["t-2", "t-3", "t-1"]);
  const after = sent.map((id) => board.find((t) => t.id === id) as (typeof board)[number]);
  assert.deepEqual(visibleRows(after).map((r) => r.task.id), ["t-3", "t-1", "t-2"]);
  // And symmetrically upward.
  assert.deepEqual(reorderWithSubtree(board, "t-3", -1), ["t-3", "t-1", "t-2"]);
});

test("a reorder never moves a settled row out of its own stored position", () => {
  // The display rule is a projection. If a click rewrote the array into
  // relevant-first order it would be silently re-prioritising rows on the
  // orchestrator's own queue file as a side effect of a view preference.
  const board = [
    orow("t-1", "done", { updated_ms: 1 }),
    orow("t-2", "queued"),
    orow("t-3", "done", { updated_ms: 2 }),
    orow("t-4", "queued"),
  ];
  // Exactly one row moves. The order sent is the stored one with the moved row
  // lifted out and dropped beside its displayed neighbour — every other row,
  // the two settled ones included, keeps its relative place.
  const others = (sent: string[]) => sent.filter((id) => id !== "t-2" && id !== "t-4");
  const down = reorderWithSubtree(board, "t-2", 1);
  assert.deepEqual(down, ["t-1", "t-3", "t-4", "t-2"]);
  assert.deepEqual(others(down), ["t-1", "t-3"]);
  const up = reorderWithSubtree(board, "t-4", -1);
  assert.deepEqual(up, ["t-1", "t-4", "t-2", "t-3"]);
  assert.deepEqual(others(up), ["t-1", "t-3"]);
  // Either way the human sees the same thing: t-4 is now the top priority.
  const shown = (sent: string[]) =>
    visibleRows(sent.map((id) => board.find((t) => t.id === id) as (typeof board)[number])).map(
      (r) => r.task.id
    );
  assert.deepEqual(shown(down), ["t-4", "t-2", "t-3", "t-1"]);
  assert.deepEqual(shown(up), ["t-4", "t-2", "t-3", "t-1"]);
  // The live rows are at the ends of the manual list, so these are no-ops that
  // still return the whole current order.
  assert.deepEqual(reorderWithSubtree(board, "t-2", -1), ["t-1", "t-2", "t-3", "t-4"]);
  assert.deepEqual(reorderWithSubtree(board, "t-4", 1), ["t-1", "t-2", "t-3", "t-4"]);
  // A zero step is a no-op, not a move onto itself: the splice reads the
  // target's index AFTER lifting the row out, and "beside myself" has no such
  // index — an unguarded delta of 0 would corrupt the order rather than return
  // it. The array-order rule this replaced was inert for 0 by construction.
  assert.deepEqual(reorderWithSubtree(board, "t-2", 0), ["t-1", "t-2", "t-3", "t-4"]);
});

test("a settled row is not in the manual priority list, so both arrows are off", () => {
  const board = [orow("t-1", "queued"), orow("t-2", "done"), orow("t-3", "queued")];
  assert.deepEqual(siblingPosition(board, "t-1"), { index: 0, count: 2 });
  assert.deepEqual(siblingPosition(board, "t-3"), { index: 1, count: 2 });
  // {index:-1} is what disables both buttons in tasksview — a finished row's
  // place is derived (most recently updated first), so a manual step there would contradict
  // the order the board just told the human it was using.
  assert.deepEqual(siblingPosition(board, "t-2"), { index: -1, count: 0 });
  // The view passes the board-level settled set in rather than making this
  // walk the tree once per row. Same answers either way — a divergence here
  // would mean the render and the pure helper disagreed about who can move.
  const settled = settledIds(board);
  for (const id of ["t-1", "t-2", "t-3", "t-404"]) {
    assert.deepEqual(siblingPosition(board, id, settled), siblingPosition(board, id), id);
  }
  // Asking one to move is a no-op that still returns the whole current order.
  assert.deepEqual(reorderWithSubtree(board, "t-2", -1), ["t-1", "t-2", "t-3"]);
  assert.deepEqual(reorderWithSubtree(board, "t-2", 1), ["t-1", "t-2", "t-3"]);
});

test("a focus miss tells a deleted row apart from one that is merely off-screen", () => {
  // The request is CONSUMED whether or not a row renders, so without this
  // split a NEEDS-YOU deep link onto a cleared or collapsed row is
  // indistinguishable from one onto a deleted row — every case a dead click.
  const board = [
    orow("t-1", "done", { cleared_ms: 5 }),
    orow("t-2", "queued", { parent: "t-1" }),
    orow("t-3", "queued"),
  ];
  // Archived: 👁 is the way back.
  assert.equal(focusMiss("t-1", board), "cleared");
  // On the board, not archived — so it is off-screen for the OTHER reason
  // (inside a collapsed container), which pre-dates #1152.
  assert.equal(focusMiss("t-2", board), "hidden");
  assert.equal(focusMiss("t-3", board), "hidden");
  // Names nothing: stays silent, because a task really can be deleted between
  // the request being parked and the render draining it.
  assert.equal(focusMiss("t-404", board), "gone");
  // The stamp alone is not enough — the same read-time rule the hide uses, so a
  // reopened row can never be reported as "cleared, press 👁" when 👁 would not
  // bring it back.
  const reopened = [orow("t-1", "in-progress", { cleared_ms: 5 })];
  assert.equal(focusMiss("t-1", reopened), "hidden");
});

test("orderSiblings splits one sibling list without mutating the tree's own array", () => {
  const siblings = [
    orow("t-1", "done", { updated_ms: 1 }),
    orow("t-2"),
    orow("t-3", "done", { updated_ms: 9 }),
  ];
  const before = siblings.map((t) => t.id);
  const { manual, ordered } = orderSiblings(siblings, new Set(["t-1", "t-3"]));
  assert.deepEqual(manual.map((t) => t.id), ["t-2"]);
  assert.deepEqual(ordered.map((t) => t.id), ["t-2", "t-3", "t-1"]);
  assert.deepEqual(siblings.map((t) => t.id), before, "siblingRows hands back the tree's own array");
  assert.deepEqual(positionAmong(manual, "t-2"), { index: 0, count: 1 });
  assert.deepEqual(positionAmong(manual, "t-3"), { index: -1, count: 0 });
});

// ---------------------------------------------------------------------------
// Sprints (#1272) and grounding links (#1273).
// The backend owns every rule (mod.rs `current_sprint`, `normalize_task_links`);
// these pin that the board says the SAME thing, since the human's board reads
// full Tasks via orch_tasks and derives its own view.
// ---------------------------------------------------------------------------

/** A board row with a sprint. `sprint` is omitted (not null) on backlog rows,
 *  exactly as the backend serializes them. */
const sprintRow = (id: string, status: string, sprint?: number) =>
  sprint === undefined ? { id, status } : { id, status, sprint };

test("currentSprint is the lowest sprint on any NON-done row", () => {
  const board = [
    sprintRow("t-1", "done", 1),
    sprintRow("t-2", "queued", 2),
    sprintRow("t-3", "queued", 3),
  ];
  assert.equal(currentSprint(board), 2, "sprint 1 is fully done, so 2 is current");
});

test("currentSprint is null when no open row carries a sprint", () => {
  // Three distinct cases that must all read null — never 0, never undefined.
  assert.equal(currentSprint([]), null, "an empty board has no current sprint");
  assert.equal(
    currentSprint([sprintRow("t-1", "queued"), sprintRow("t-2", "blocked")]),
    null,
    "a board that runs no sprints has no current sprint"
  );
  assert.equal(
    currentSprint([sprintRow("t-1", "done", 1), sprintRow("t-2", "done", 2)]),
    null,
    "every sprint finished — null, not the highest number seen"
  );
});

test("a BLOCKED row HOLDS its sprint current — roll-over is never automatic", () => {
  // The load-bearing case of #1272: a sprint is finished only when its last
  // open row LEAVES it, never because the remaining work looked stuck. If this
  // ever reads 2, the board has silently decided a sprint was over.
  const board = [
    sprintRow("t-1", "done", 1),
    sprintRow("t-2", "blocked", 1),
    sprintRow("t-3", "queued", 2),
  ];
  assert.equal(currentSprint(board), 1, "a blocked row keeps sprint 1 current");

  // Every non-done status holds it, not just `blocked`.
  for (const status of [
    "queued",
    "in-progress",
    "review",
    "pr",
    "prototype",
    "human-testing",
    "blocked",
  ]) {
    const b = [sprintRow("t-1", status, 1), sprintRow("t-2", "queued", 2)];
    assert.equal(currentSprint(b), 1, `a row at ${status} must hold sprint 1 current`);
  }
  // `done` — and only `done` — releases it.
  const released = [sprintRow("t-1", "done", 1), sprintRow("t-2", "queued", 2)];
  assert.equal(currentSprint(released), 2, "done releases the sprint");
});

test("sprint numbers need not be contiguous or start at 1", () => {
  const board = [sprintRow("t-1", "queued", 7), sprintRow("t-2", "queued", 42)];
  assert.equal(currentSprint(board), 7);
});

test("sprintProgress counts the sprint's WHOLE scope as the denominator", () => {
  const board = [
    sprintRow("t-1", "done", 1),
    sprintRow("t-2", "done", 1),
    sprintRow("t-3", "blocked", 1),
    sprintRow("t-4", "queued", 2),
    sprintRow("t-5", "queued"),
  ];
  assert.deepEqual(
    sprintProgress(board, 1),
    { done: 2, total: 3 },
    "done rows stay in the denominator"
  );
  assert.deepEqual(sprintProgress(board, 2), { done: 0, total: 1 });
  assert.deepEqual(
    sprintProgress(board, 9),
    { done: 0, total: 0 },
    "an unused sprint is 0/0, not a throw"
  );
});

test("rollOverSet is every NON-done row in the sprint, in board order", () => {
  const board = [
    sprintRow("t-1", "done", 1),
    sprintRow("t-2", "blocked", 1),
    sprintRow("t-3", "queued", 1),
    sprintRow("t-4", "queued", 2),
  ];
  assert.deepEqual(
    rollOverSet(board, 1).map((t) => t.id),
    ["t-2", "t-3"],
    "done rows excluded, blocked INCLUDED, board order preserved"
  );
  // The blocked row is exactly what a silent roll-over would sweep up, so it is
  // the one the confirm list most needs to name.
  assert.ok(
    rollOverSet(board, 1).some((t) => t.status === "blocked"),
    "a blocked row must appear in the confirm list"
  );
  assert.deepEqual(rollOverSet(board, 3), [], "a sprint with nothing open moves nothing");
});

test("sprintAdvance reports the SAME rows the dialog shows and the number it names", () => {
  // One function feeds both the confirm list and the writes, so the list the
  // human approved cannot differ from what is recorded. This test is the
  // statement of that: `rows` is `rollOverSet` and `to` travels with it.
  const board = [
    sprintRow("t-1", "done", 2),
    sprintRow("t-2", "blocked", 2),
    sprintRow("t-3", "queued", 2),
    sprintRow("t-4", "queued", 5),
  ];
  const plan = sprintAdvance(board, 2);
  assert.equal(plan.to, 3, "one step on, never the next number already in use");
  assert.deepEqual(
    plan.rows.map((t) => t.id),
    rollOverSet(board, 2).map((t) => t.id),
    "the rows are rollOverSet's, not a second reading of the board"
  );
  // The gap case, stated on its own because it is the tempting alternative:
  // sprint 5 exists and is NOT where sprint 2's leftovers land. Parking planned
  // work in a later number must not make that number absorb a roll-over.
  assert.equal(sprintAdvance(board, 2).to, 3);
  assert.deepEqual(sprintAdvance(board, 5).to, 6);
});

test("sprintAdvance refuses when there is no next sprint to land in", () => {
  // Fail closed rather than composing a write the backend must reject: `sprint`
  // is a u32 on both wire paths, so MAX_SPRINT + 1 cannot be stored.
  const stuck = [sprintRow("t-1", "queued", MAX_SPRINT)];
  const plan = sprintAdvance(stuck, MAX_SPRINT);
  assert.equal(plan.to, null, "no sprint exists after the last one a u32 can hold");
  assert.deepEqual(
    plan.rows.map((t) => t.id),
    ["t-1"],
    "the stuck rows are still reported, so a caller can say what cannot move"
  );
  // One below the cap still advances — the refusal is the boundary, not a
  // band of numbers near it.
  assert.equal(sprintAdvance([sprintRow("t-1", "queued", MAX_SPRINT - 1)], MAX_SPRINT - 1).to, MAX_SPRINT);
  // A `from` that is not a sprint at all cannot produce one. Unreachable from
  // the board (the caller passes `currentSprint`'s own output), which is why it
  // is pinned: an unreachable path that silently returned `1` would put rows
  // into a sprint nobody asked for the first time it became reachable.
  assert.equal(sprintAdvance([], 0).to, null);
  assert.equal(sprintAdvance([], -3).to, null);
  assert.equal(sprintAdvance([], 1.5).to, null);
});

test("linkTargetKind classifies issue refs, URLs and repo paths", () => {
  assert.equal(linkTargetKind("#123"), "issue");
  assert.equal(linkTargetKind("  #7  "), "issue", "trimmed");
  assert.equal(linkTargetKind("#foo"), "other", "a fragment is not an issue ref");
  assert.equal(linkTargetKind("https://example.com/a/b"), "url");
  assert.equal(linkTargetKind("HTTP://EXAMPLE.COM"), "url", "scheme match is case-insensitive");
  assert.equal(linkTargetKind("doc/design/x.md"), "path");
  assert.equal(linkTargetKind("README.md"), "path", "an extension alone is enough");
  assert.equal(linkTargetKind("src-tauri/tests/orchestration.rs"), "path");
  // A bare word claims nothing — otherwise it would swallow every unclassified
  // target and send clicks to the editor for things that are not files.
  assert.equal(linkTargetKind("README"), "other");
  assert.equal(linkTargetKind(""), "other");
  assert.equal(linkTargetKind("   "), "other");
  // Absolute paths are not REPO paths: this classification exists to open
  // things inside the repo, so an absolute one must not claim to be one.
  assert.equal(linkTargetKind("C:/Windows/system32"), "other");
  assert.equal(linkTargetKind("/etc/passwd"), "other");
  // A URL is checked before a path — it contains slashes and would otherwise be
  // misread as a repo path and handed to the editor.
  assert.equal(linkTargetKind("https://github.com/o/r/issues/1"), "url");
});

test("boardUsesSprints / boardUsesLinks gate the chrome on actual use", () => {
  assert.equal(boardUsesSprints([sprintRow("t-1", "queued")]), false);
  assert.equal(boardUsesSprints([sprintRow("t-1", "queued", 1)]), true);
  assert.equal(boardUsesLinks([{ id: "t-1", status: "queued" }]), false);
  assert.equal(
    boardUsesLinks([{ id: "t-1", status: "queued", links: [] }]),
    false,
    "an empty array is not use"
  );
  assert.equal(
    boardUsesLinks([{ id: "t-1", status: "queued", links: [{ type: "doc", target: "README.md" }] }]),
    true
  );
});

test("the board's status vocabulary is the backend's, read out of the Rust source", () => {
  // #1321, the twin #1300's review named: `STATUSES` was a TRANSCRIPTION of
  // `TASK_STATUSES`, so a ninth status added on the backend would have been
  // refused by no test and offered by no picker — the frontend half of the
  // set-widening hazard #1300 closed on its own side. Same guard shape as
  // `TASK_LINK_TYPES` below and the ladder table above: read the Rust const,
  // never restate it.
  //
  // This vocabulary is load-bearing in more places than the pickers it feeds:
  // `isAwaitingHuman`, `isDemoGated`, `canApprove` and `taskActivityState` are
  // all swept over `STATUSES` by the tests in this file, so a status the board
  // has never heard of is a status none of those sweeps ever judges.
  const src = readFileSync(RUST_LADDER, "utf8");
  const start = src.indexOf("pub const TASK_STATUSES");
  assert.notEqual(
    start,
    -1,
    "TASK_STATUSES is gone or renamed in mod.rs — this guard reads it by name, so update it " +
      "here rather than deleting the only thing pinning the two vocabularies together"
  );
  const end = src.indexOf("];", start);
  assert.notEqual(end, -1, "could not find the end of TASK_STATUSES");
  const body = src.slice(start, end);
  // Quoted strings only. The declaration's trailing `//` comments are prose
  // ("planned, not started") and carry no quotes, so they cannot contribute a
  // phantom status.
  const rust = [...body.matchAll(/"([a-z-]+)"/g)].map(([, s]) => s);
  assert.ok(rust.length > 0, `no statuses could be parsed out of TASK_STATUSES. Body was:\n${body}`);
  // The same cross-check the link-type guard carries, for the same reason: the
  // character class is a GUESS about the alphabet of its own subjects, and a
  // future status carrying a digit or an underscore would be dropped silently,
  // leaving a short list compared against a short list. The declared length
  // cannot be fooled that way.
  const declared = body.match(/\[&str;\s*(\d+)\]/);
  assert.ok(declared, `could not read TASK_STATUSES' declared length. Body was:\n${body}`);
  assert.equal(
    rust.length,
    Number(declared[1]),
    `the parser found ${rust.length} statuses but the Rust array declares ${declared[1]} — ` +
      `the regex cannot see one of its own subjects, so this guard is not comparing what it thinks`
  );
  assert.deepEqual(
    [...STATUSES],
    rust,
    "the board's STATUSES must equal the backend's TASK_STATUSES, in the same order — the " +
      "order is the picker's order, so a reshuffle on one side is a reshuffle in the UI"
  );
});

test("the board's link-type vocabulary is the backend's, read out of the Rust source", () => {
  // Same guard shape as the ladder table above: read the Rust const rather than
  // restating it here, so a vocabulary added on one side and not the other is a
  // red test and not a silently divergent picker.
  const src = readFileSync(RUST_LADDER, "utf8");
  const start = src.indexOf("pub const TASK_LINK_TYPES");
  assert.notEqual(
    start,
    -1,
    "TASK_LINK_TYPES is gone or renamed in mod.rs — this guard reads it by name, so update it " +
      "here rather than deleting the only thing pinning the two vocabularies together"
  );
  const end = src.indexOf("];", start);
  assert.notEqual(end, -1, "could not find the end of TASK_LINK_TYPES");
  const body = src.slice(start, end);
  const rust = [...body.matchAll(/"([a-z-]+)"/g)].map(([, t]) => t);
  assert.ok(
    rust.length > 0,
    `no link types could be parsed out of TASK_LINK_TYPES. Body was:\n${body}`
  );
  // Cross-check the census against the container's own declared length, because
  // the character class above is a GUESS about the alphabet of its own subjects:
  // `[a-z-]+` stops dead at a digit or an underscore, so a future type like
  // `adr-2` would be silently dropped and this guard would compare a short list
  // to a short list and pass. The declared count cannot be fooled that way.
  const declared = body.match(/\[&str;\s*(\d+)\]/);
  assert.ok(declared, `could not read TASK_LINK_TYPES' declared length. Body was:\n${body}`);
  assert.equal(
    rust.length,
    Number(declared[1]),
    `the parser found ${rust.length} link types but the Rust array declares ${declared[1]} — ` +
      `the regex cannot see one of its own subjects, so this guard is not comparing what it thinks`
  );
  assert.deepEqual(
    [...LINK_TYPES],
    rust,
    "the board's LINK_TYPES must equal the backend's TASK_LINK_TYPES, in the same order"
  );
});

// ---------------------------------------------------------------------------
// Tree-view filtering (#1270) — the three rules in taskboard.ts's filter
// section, plus the two projection fields (`context`, `shownKids`) the strip
// and the count badges read.
// ---------------------------------------------------------------------------

/** A board row for the filter helpers: the hierarchy fields `row` already
 *  carries, plus the title the text filter reads. Named so the two never get
 *  confused — `row` deliberately has no title, and several tests below rely on
 *  a title-less row still being filterable. */
const frow = (
  id: string,
  fields: {
    status?: string;
    parent?: string;
    kind?: string;
    title?: string;
    sprint?: number;
  } = {}
) => ({
  id,
  status: fields.status ?? "queued",
  ...(fields.parent === undefined ? {} : { parent: fields.parent }),
  ...(fields.kind === undefined ? {} : { kind: fields.kind }),
  ...(fields.title === undefined ? {} : { title: fields.title }),
  // Omitted, not null, on a backlog row — the shape the backend serializes,
  // and the one `sprintFilterValue` has to read as the backlog.
  ...(fields.sprint === undefined ? {} : { sprint: fields.sprint }),
});

/** `NO_FILTER` with one family armed — so each test states only the family it
 *  is about, and a new family added to `BoardFilter` cannot silently leave
 *  these specimens under-specified (the object spread would fail to compile). */
const filterOf = (over: Partial<BoardFilter>): BoardFilter => ({ ...NO_FILTER, ...over });

test("every filter family arms the board, and blank text does not", () => {
  assert.equal(filterActive(NO_FILTER), false);
  assert.equal(filterActive(filterOf({ kind: ["epic"] })), true);
  assert.equal(filterActive(filterOf({ status: ["blocked"] })), true);
  assert.equal(filterActive(filterOf({ sprint: ["2"] })), true);
  assert.equal(filterActive(filterOf({ sprint: [BACKLOG_SPRINT] })), true);
  assert.equal(filterActive(filterOf({ text: "auth" })), true);
  assert.equal(filterActive(filterOf({ attention: true })), true);
  // A search box the human tabbed through and left holding spaces must not
  // silently empty the board — every row would fail a whitespace substring
  // test against nothing, and the strip would read as "no results" for a
  // filter nobody set.
  assert.equal(filterActive(filterOf({ text: "   " })), false);
});

test("the unlabelled chip catches a row with no kind — and one hand-edited to empty", () => {
  const none = frow("t-1");
  const empty = frow("t-2", { kind: "" });
  const epic = frow("t-3", { kind: "epic" });
  const unlabelled = filterOf({ kind: [UNLABELLED_KIND] });
  assert.equal(matchesFilter(none, unlabelled), true);
  // `||` and not `??` in matchesFilter: an empty-string kind is "no level",
  // exactly like the key being absent. Reading it with `??` would make it a
  // fifth, invisible class — matched by no chip at all.
  assert.equal(matchesFilter(empty, unlabelled), true);
  assert.equal(matchesFilter(epic, unlabelled), false);
  assert.equal(matchesFilter(epic, filterOf({ kind: ["epic"] })), true);
  assert.equal(matchesFilter(none, filterOf({ kind: ["epic"] })), false);
});

test("a hand-edited out-of-vocabulary kind still gets a chip, so no row is unreachable", () => {
  // ladderRule exempts an unknown kind on purpose (CLAUDE.md constraint 8 —
  // Orrerix must not require a methodology), so `saga` is legal. If the chip row
  // were the fixed vocabulary, a `saga` row would match neither a ladder level
  // nor `unlabelled` (it IS labelled) and could only be seen by clearing the
  // kind filter entirely.
  const board = [frow("t-1", { kind: "epic" }), frow("t-2", { kind: "saga" }), frow("t-3")];
  assert.deepEqual(kindFilterChoices(board), [...KINDS, UNLABELLED_KIND, "saga"]);
  assert.equal(matchesFilter(board[1], filterOf({ kind: ["saga"] })), true);
  assert.equal(matchesFilter(board[1], filterOf({ kind: [UNLABELLED_KIND] })), false);
  // A board using only the vocabulary gets exactly the vocabulary — no
  // duplicate `epic` chip from the row that carries one.
  assert.deepEqual(kindFilterChoices([frow("t-9", { kind: "epic" })]), [
    ...KINDS,
    UNLABELLED_KIND,
  ]);
});

test("the sprint chips are the numbers the board carries, ascending, with backlog last", () => {
  // Derived from the board and not from `1..max`, for `current_sprint`'s own
  // reason (design note §5): numbers need not be contiguous or start at 1, so a
  // range would offer chips matching nothing. Ascending because a sprint number
  // IS an ordering — board order would shuffle `1 2 3` by whichever row is on
  // top.
  const board = [
    frow("t-1", { sprint: 3 }),
    frow("t-2", { sprint: 1 }),
    frow("t-3"),
    frow("t-4", { sprint: 3 }),
  ];
  assert.deepEqual(sprintFilterChoices(board), ["1", "3", BACKLOG_SPRINT]);
  // The backlog chip exists on every board, including one where no row is in
  // the backlog right now — the bucket is what the chip names, not its
  // current occupancy. Same rule `unlabelled` follows for kinds.
  assert.deepEqual(sprintFilterChoices([frow("t-1", { sprint: 2 })]), ["2", BACKLOG_SPRINT]);
  assert.deepEqual(sprintFilterChoices([]), [BACKLOG_SPRINT]);
});

test("the backlog chip catches every row the backend serializes without a sprint", () => {
  // Three spellings of "no sprint" reach this predicate: the key absent (what
  // `skip_serializing_if` writes), an explicit null, and a hand-edited
  // non-number. All are the backlog — and absent must NEVER read as sprint 0,
  // which is the backend's CLEAR and not a sprint anybody can be in.
  const absent = frow("t-1");
  const nulled = { id: "t-2", status: "queued", sprint: null };
  const junk = { id: "t-3", status: "queued", sprint: "2" as unknown as number };
  const backlog = filterOf({ sprint: [BACKLOG_SPRINT] });
  assert.equal(matchesFilter(absent, backlog), true);
  assert.equal(matchesFilter(nulled, backlog), true);
  assert.equal(matchesFilter(junk, backlog), true, "a hand-edited string is not sprint 2");
  assert.equal(matchesFilter(junk, filterOf({ sprint: ["2"] })), false);
  // And the counter-direction: `0` on a row would be a value no chip offers, so
  // it must not silently land on the backlog either — it renders as its own
  // chip and stays reachable, the way an out-of-vocabulary kind does.
  const zero = frow("t-4", { sprint: 0 });
  assert.equal(matchesFilter(zero, backlog), false);
  assert.deepEqual(sprintFilterChoices([zero]), ["0", BACKLOG_SPRINT]);
});

test("the sprint family is OR within itself and AND across the others", () => {
  const board = [
    frow("t-1", { sprint: 1, status: "queued" }),
    frow("t-2", { sprint: 2, status: "blocked" }),
    frow("t-3", { sprint: 2, status: "queued" }),
    frow("t-4", { status: "blocked" }),
  ];
  const matching = (f: BoardFilter) => board.filter((t) => matchesFilter(t, f)).map((t) => t.id);
  assert.deepEqual(matching(filterOf({ sprint: ["1", "2"] })), ["t-1", "t-2", "t-3"], "OR within");
  assert.deepEqual(
    matching(filterOf({ sprint: ["2"], status: ["blocked"] })),
    ["t-2"],
    "AND across: sprint 2 AND blocked, not either"
  );
  assert.deepEqual(
    matching(filterOf({ sprint: [BACKLOG_SPRINT], status: ["blocked"] })),
    ["t-4"]
  );
  // The negative control: a filter cannot pass by simply not being consulted.
  // If the sprint clause were dropped from `matchesFilter`, this would return
  // the whole board rather than nothing.
  assert.deepEqual(matching(filterOf({ sprint: ["9"] })), []);
});

test("a sprint match brings its containers with it, through the one filter seam", () => {
  // The sprint family is a key in `BoardFilter`, not a second mechanism, so it
  // inherits #1270's ancestor-visibility rule for free. That is the property
  // worth pinning: a story in sprint 2 inside a feature in no sprint at all
  // still renders under its feature, with the feature marked as context rather
  // than as a hit. A parallel sprint-only filter path would have had to
  // re-implement this, and would have got it subtly different.
  const board = [
    frow("t-1", { kind: "epic" }),
    frow("t-2", { kind: "feature", parent: "t-1" }),
    frow("t-3", { kind: "story", parent: "t-2", sprint: 2 }),
    frow("t-4", { kind: "story", parent: "t-2", sprint: 3 }),
  ];
  assert.deepEqual(
    visibleRows(board, [], false, { filter: filterOf({ sprint: ["2"] }) }).map((r) => [
      r.task.id,
      r.context,
    ]),
    [
      ["t-1", true],
      ["t-2", true],
      ["t-3", false],
    ]
  );
});

test("text search reads the id and the title, case-insensitively, but never across the seam", () => {
  const t = frow("t-14", { title: "Fix the auth redirect" });
  assert.equal(matchesFilter(t, filterOf({ text: "AUTH" })), true);
  assert.equal(matchesFilter(t, filterOf({ text: "t-14" })), true);
  assert.equal(matchesFilter(t, filterOf({ text: "T-14" })), true);
  assert.equal(matchesFilter(t, filterOf({ text: "  auth  " })), true, "the needle is trimmed");
  assert.equal(matchesFilter(t, filterOf({ text: "logout" })), false);
  // Id and title are tested separately. Joined into one haystack they would
  // share a seam, and a needle spanning it would report a match no human could
  // see anywhere on the row.
  assert.equal(matchesFilter(t, filterOf({ text: "14 fix" })), false);
  // A row with no title at all is still searchable by id, and never matches a
  // title needle by defaulting to something permissive.
  const untitled = frow("t-15");
  assert.equal(matchesFilter(untitled, filterOf({ text: "t-15" })), true);
  assert.equal(matchesFilter(untitled, filterOf({ text: "auth" })), false);
});

test("the attention toggle matches only the ids the view supplied", () => {
  const a = frow("t-1");
  const b = frow("t-2");
  const armed = filterOf({ attention: true });
  assert.equal(matchesFilter(a, armed, new Set(["t-1"])), true);
  assert.equal(matchesFilter(b, armed, new Set(["t-1"])), false);
  // No set supplied: the toggle asks for rows the view flagged, and a view that
  // flagged none has none to show. The dangerous default is the other one —
  // "no set known" reading as "everything qualifies" would make the quick
  // filter a no-op exactly when the data behind it failed to load.
  assert.equal(matchesFilter(a, armed), false);
});

test("families are ANDed and members are ORed", () => {
  const board = [
    frow("t-1", { kind: "story", status: "blocked" }),
    frow("t-2", { kind: "story", status: "queued" }),
    frow("t-3", { kind: "task", status: "blocked" }),
  ];
  const f = filterOf({ kind: ["story", "task"], status: ["blocked"] });
  assert.deepEqual(board.map((t) => matchesFilter(t, f)), [true, false, true]);
  // An empty family constrains nothing — it never means "match nothing".
  const statusOnly = filterOf({ status: ["blocked"] });
  assert.deepEqual(board.map((t) => matchesFilter(t, statusOnly)), [true, false, true]);
});

test("a match keeps its whole ancestor chain, and the chain is marked as context", () => {
  // Rule 1. Without this the tree stops being a tree: a `kind=story` filter
  // would return a flat list and the containment the #1156 hierarchy model
  // exists to show would be gone from the one view that shows it.
  const board = [
    frow("e-1", { kind: "epic", title: "Billing" }),
    frow("f-1", { kind: "feature", parent: "e-1", title: "Invoices" }),
    frow("s-1", { kind: "story", parent: "f-1", title: "PDF export" }),
    frow("e-2", { kind: "epic", title: "Auth" }),
  ];
  const rows = visibleRows(board, [], false, { filter: filterOf({ kind: ["story"] }) });
  assert.deepEqual(
    rows.map((r) => [r.task.id, r.depth, r.context]),
    [["e-1", 0, true], ["f-1", 1, true], ["s-1", 2, false]],
    "the two containers render as scaffolding at their real depths; e-2 has no match under it"
  );
});

test("a match does not drag its descendants back in — the badge is what names them", () => {
  // The other half of rule 1, and the reason `shownKids` exists: an epic
  // matching `kind=epic` renders ALONE, so without a count the human cannot
  // tell it from an empty epic.
  const board = [
    frow("e-1", { kind: "epic" }),
    frow("f-1", { kind: "feature", parent: "e-1" }),
    frow("f-2", { kind: "feature", parent: "e-1", status: "done" }),
  ];
  const rows = visibleRows(board, [], false, { filter: filterOf({ kind: ["epic"] }) });
  assert.deepEqual(rows.map((r) => r.task.id), ["e-1"]);
  assert.equal(rows[0].hasChildren, true, "it still reports that it contains something");
  assert.equal(rows[0].shownKids, 0, "…and that none of it is on screen");
  assert.deepEqual(childCounts("e-1", board), { total: 2, done: 1 });
});

test("an active filter reveals a match inside a collapsed container, without mutating collapse", () => {
  // Rule 2. A search that finds a row and then hides it is worse than no
  // search. The stored set is the human's, so the filter reads past it rather
  // than rewriting it — and the proof is that the SAME set re-collapses the
  // moment the filter clears.
  const board = [
    frow("e-1", { kind: "epic" }),
    frow("s-1", { kind: "story", parent: "e-1", title: "auth redirect" }),
    frow("s-2", { kind: "story", parent: "e-1", title: "invoice pdf" }),
  ];
  const collapsed = ["e-1"];
  assert.deepEqual(
    visibleRows(board, collapsed).map((r) => r.task.id),
    ["e-1"],
    "baseline: collapsed hides both children"
  );
  const filtered = visibleRows(board, collapsed, false, { filter: filterOf({ text: "auth" }) });
  assert.deepEqual(filtered.map((r) => r.task.id), ["e-1", "s-1"]);
  assert.equal(filtered[0].collapsed, false, "the container reports itself expanded while filtering");
  // Same array, filter cleared: back to exactly the shape the human left.
  assert.deepEqual(visibleRows(board, collapsed).map((r) => r.task.id), ["e-1"]);
  assert.deepEqual(collapsed, ["e-1"], "the caller's own set was never touched");
});

test("shownKids counts what is on screen, for a collapsed row and a filtered one alike", () => {
  const board = [
    frow("e-1", { kind: "epic" }),
    frow("s-1", { kind: "story", parent: "e-1", title: "auth" }),
    frow("s-2", { kind: "story", parent: "e-1", title: "pdf" }),
    frow("s-3", { kind: "story", parent: "e-1", title: "email" }),
  ];
  const all = visibleRows(board);
  assert.equal(all[0].shownKids, 3, "nothing withheld: every child is on screen");
  assert.equal(visibleRows(board, ["e-1"])[0].shownKids, 0, "collapsed: none of them are");
  const filtered = visibleRows(board, [], false, { filter: filterOf({ text: "auth" }) });
  assert.equal(filtered[0].shownKids, 1, "filtered: one of three survived");
  assert.equal(filtered[0].hasChildren, true);
  // A leaf never claims to be withholding anything.
  assert.equal(all[1].shownKids, 0);
  assert.equal(all[1].hasChildren, false);
});

test("an archived row cannot match a filter while the archive is off screen", () => {
  // Filtering is a view and clearing is board data (#1152); the two compose
  // rather than overriding each other. A `text` hit inside the archive must not
  // drag the archive back onto the board behind the human's back — 👁 is what
  // does that, and the same needle finds it once they click it.
  const board = [
    frow("t-1", { title: "auth redirect" }),
    { ...frow("t-2", { status: "done", title: "auth cleanup" }), cleared_ms: 5 },
  ];
  const f = filterOf({ text: "auth" });
  assert.deepEqual(
    visibleRows(board, [], false, { filter: f }).map((r) => r.task.id),
    ["t-1"]
  );
  assert.deepEqual(
    visibleRows(board, [], true, { filter: f }).map((r) => [r.task.id, r.context]),
    [["t-1", false], ["t-2", false]],
    "with the archive on screen it is an ordinary row and matches like one"
  );
});

test("filtering a hand-edited containment cycle terminates, rendering each row once", () => {
  // The invariant every walk over `tasks.json` owes, applied to the new one:
  // buildSieve climbs ancestors from every match, and a cycle would otherwise
  // climb forever.
  const board = [
    frow("t-1", { parent: "t-3", title: "auth" }),
    frow("t-2", { parent: "t-1", title: "auth" }),
    frow("t-3", { parent: "t-2", title: "auth" }),
  ];
  const ids = visibleRows(board, [], false, { filter: filterOf({ text: "auth" }) }).map(
    (r) => r.task.id
  );
  assert.deepEqual([...new Set(ids)].sort(), ["t-1", "t-2", "t-3"]);
  assert.equal(ids.length, 3, "each row renders exactly once");
});

test("an unfiltered board is byte-for-byte the projection it always was", () => {
  // The regression pin for every pre-#1270 caller: no filter means no context
  // marking anywhere, and shownKids simply reports what rendered.
  const board = [
    frow("e-1", { kind: "epic" }),
    frow("s-1", { kind: "story", parent: "e-1" }),
    frow("t-9"),
  ];
  const rows = visibleRows(board);
  assert.deepEqual(
    rows.map((r) => [r.task.id, r.depth, r.context, r.shownKids]),
    [["e-1", 0, false, 1], ["s-1", 1, false, 0], ["t-9", 0, false, 0]]
  );
  // …and an EMPTY filter object is the same thing as no filter at all.
  assert.deepEqual(
    visibleRows(board, [], false, { filter: NO_FILTER }).map((r) => [r.task.id, r.context]),
    [["e-1", false], ["s-1", false], ["t-9", false]]
  );
});

test("collapse-all names every container and nothing else, in board order", () => {
  const board = [
    frow("e-1", { kind: "epic" }),
    frow("f-1", { kind: "feature", parent: "e-1" }),
    frow("s-1", { kind: "story", parent: "f-1" }),
    frow("t-9"),
  ];
  assert.deepEqual(containerIds(board), ["e-1", "f-1"]);
  // Collapsing that set leaves exactly the roots on screen, which is what the
  // button promises; expand-all is the empty set and needs no helper.
  assert.deepEqual(
    visibleRows(board, containerIds(board)).map((r) => r.task.id),
    ["e-1", "t-9"]
  );
  assert.deepEqual(containerIds([frow("t-1"), frow("t-2")]), [], "a flat board has no containers");
});
