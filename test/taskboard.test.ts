// Unit tests for the task-board "delete all done" selection hint (issue #120).
// The board shows a batch-delete button only when there are done tasks and
// reports how many will go; doneCount is the pure logic behind that. Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
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
  DEMO_STATUSES,
  depCandidates,
  depState,
  doneCount,
  grantableCount,
  hasMissingParent,
  indentLevel,
  isAwaitingHuman,
  isDemoGated,
  isReady,
  KINDS,
  kindCandidates,
  MAX_INDENT_DEPTH,
  nextPicker,
  parentCandidates,
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

test("a board with no hierarchy renders exactly as it does today", () => {
  // The regression pin that matters most: every existing board arrives with no
  // `parent` key at all, and must come out of visibleRows in board order, flat,
  // with no collapse affordance anywhere.
  const board = [row("t-1"), row("t-2", "done"), row("t-3")];
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

test("the nest-under picker offers every other row, minus the current container", () => {
  const board = [row("t-1"), row("t-2", "queued", "t-1"), row("t-3")];
  // t-1 is already t-2's container — offering it again would send a no-op write.
  assert.deepEqual(parentCandidates(board[1], board).map((t) => t.id), ["t-3"]);
  assert.deepEqual(parentCandidates(board[2], board).map((t) => t.id), ["t-1", "t-2"]);
  // Its own descendant IS offered, deliberately: the backend rejects the cycle
  // inside its lock with an error naming the path, and a second copy of that
  // rule here could only ever disagree with the authoritative one — the same
  // call depCandidates makes.
  assert.deepEqual(parentCandidates(board[0], board).map((t) => t.id), ["t-2", "t-3"]);
});

test("the kind picker offers the three levels a row doesn't already carry", () => {
  const epic = row("t-1", "queued", undefined, "epic");
  assert.deepEqual(kindCandidates(epic), ["feature", "story", "task"]);
  const story = row("t-2", "queued", undefined, "story");
  assert.deepEqual(kindCandidates(story), ["epic", "feature", "task"]);
});

test("the kind picker offers all four levels on a plain, kind-less row", () => {
  assert.deepEqual(kindCandidates(row("t-1")), [...KINDS]);
});

test("the kind picker offers all four levels to fix an out-of-vocabulary kind", () => {
  // Only reachable by hand-editing tasks.json — the backend refuses an
  // unknown kind on write — but nothing here should silently exclude one of
  // the four real levels because the current value doesn't match any of them.
  const broken = row("t-1", "queued", undefined, "sprint");
  assert.deepEqual(kindCandidates(broken), [...KINDS]);
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
