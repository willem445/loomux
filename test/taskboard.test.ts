// Unit tests for the task-board "delete all done" selection hint (issue #120).
// The board shows a batch-delete button only when there are done tasks and
// reports how many will go; doneCount is the pure logic behind that. Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  canProceed,
  doneCount,
  isAwaitingHuman,
  PROTOTYPE_STATUS,
  retainExisting,
  STATUSES,
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
