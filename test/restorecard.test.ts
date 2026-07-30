// #479: a dormant restore card must acknowledge a click immediately (never a
// silent lag) and a failure must always land on a persistent error state
// (never a spinner that quietly clears back to "click here to continue").
// This pins the pure transition table main.ts's dormant-card DOM wiring
// drives; DOM wiring itself is hand-validated (CLAUDE.md convention), not
// simulated here. Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  IDLE_RESTORE_CARD_STATE,
  errorRestoreCardState,
  nextRestoreCardState,
} from "../src/restorecard.ts";

test("a click from idle acknowledges immediately by moving to pending", () => {
  const next = nextRestoreCardState(IDLE_RESTORE_CARD_STATE, { type: "click" });
  assert.deepEqual(next, { status: "pending", message: null });
});

test("a second click while already pending is ignored, not restarted", () => {
  const pending = nextRestoreCardState(IDLE_RESTORE_CARD_STATE, { type: "click" });
  const again = nextRestoreCardState(pending, { type: "click" });
  // Same reference, not just an equal value: a fresh pending object here
  // would be indistinguishable from a genuine restart to any caller that
  // reference-checks to decide whether to re-render — the double-spawn
  // guard (#194 P4 MED-3) this generalizes depends on the click being a
  // true no-op, not a same-shaped replay.
  assert.equal(again, pending, "a re-entrant click while pending must be a no-op");
});

test("a failure from pending lands on error and keeps the diagnostic message", () => {
  const pending = nextRestoreCardState(IDLE_RESTORE_CARD_STATE, { type: "click" });
  const failed = nextRestoreCardState(pending, {
    type: "fail",
    message: "resume-not-found: session was not found",
  });
  assert.deepEqual(failed, {
    status: "error",
    message: "resume-not-found: session was not found",
  });
});

test("settle from any state returns to plain idle", () => {
  const pending = nextRestoreCardState(IDLE_RESTORE_CARD_STATE, { type: "click" });
  const failed = nextRestoreCardState(pending, { type: "fail", message: "boom" });
  assert.deepEqual(nextRestoreCardState(failed, { type: "settle" }), IDLE_RESTORE_CARD_STATE);
  assert.deepEqual(nextRestoreCardState(pending, { type: "settle" }), IDLE_RESTORE_CARD_STATE);
});

test("a click from the error state retries (moves to pending), not stuck", () => {
  const failed = errorRestoreCardState("workspace no longer exists");
  const retried = nextRestoreCardState(failed, { type: "click" });
  assert.deepEqual(retried, { status: "pending", message: null });
});

test("errorRestoreCardState mounts directly into error with its message (#479 B: the dormant-agent card is ALREADY known-unresumable at mount, no click needed to discover that)", () => {
  const mounted = errorRestoreCardState("This agent had no resumable session.");
  assert.equal(mounted.status, "error");
  assert.equal(mounted.message, "This agent had no resumable session.");
});
