// Unit tests for the task-board "delete all done" selection hint (issue #120).
// The board shows a batch-delete button only when there are done tasks and
// reports how many will go; doneCount is the pure logic behind that. Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  approvableSelection,
  boardUsesDeps,
  canApprove,
  canProceed,
  depCandidates,
  depState,
  doneCount,
  grantableCount,
  isAwaitingHuman,
  isReady,
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

test("a PR ref the backend cannot resolve is still counted as linked", () => {
  // Deliberate, and the reason the tooltip says "one per LINKED PR": only the
  // backend resolves a ref to the number a grant is keyed on, so the board
  // counts what it can actually see rather than duplicating that parser here
  // and drifting from it. Such an item lands in the notice's
  // no-PR-number-could-be-resolved sentence instead.
  assert.equal(grantableCount([{ pr: "TBD" }]), 1);
});
