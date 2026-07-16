// Unit tests for the task-board "delete all done" selection hint (issue #120).
// The board shows a batch-delete button only when there are done tasks and
// reports how many will go; doneCount is the pure logic behind that. Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  canApprove,
  canProceed,
  doneCount,
  isAwaitingHuman,
  PROTOTYPE_STATUS,
  REQUEST_CHANGES_STATUS,
  retainExisting,
  STATUSES,
  taskActivityState,
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
