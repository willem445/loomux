// Unit tests for the loss-safe refresh gate (src/refreshgate.ts). Pure state
// machine — no DOM, no async — exercised directly. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { RefreshGate } from "../src/refreshgate.ts";

test("begin() starts a run when idle", () => {
  const g = new RefreshGate();
  assert.equal(g.isRunning, false);
  assert.equal(g.begin(), true, "first begin proceeds");
  assert.equal(g.isRunning, true);
});

test("begin() while running is refused and marks a trailing re-run", () => {
  const g = new RefreshGate();
  g.begin(); // run A starts
  assert.equal(g.begin(), false, "a call arriving mid-run is refused");
  // The refused call is not lost: end() reports a re-run is owed.
  assert.equal(g.end(), true, "end() owes a re-run after a dropped call");
  assert.equal(g.isRunning, false);
});

test("end() with no dropped calls owes nothing", () => {
  const g = new RefreshGate();
  g.begin();
  assert.equal(g.end(), false, "a clean run owes no re-run");
  assert.equal(g.isRunning, false);
});

test("multiple dropped calls collapse into a single trailing re-run", () => {
  const g = new RefreshGate();
  g.begin(); // run A
  assert.equal(g.begin(), false);
  assert.equal(g.begin(), false);
  assert.equal(g.begin(), false); // three switches while A runs
  assert.equal(g.end(), true, "owes exactly one re-run");
  // The trailing run B starts and, with no further calls, owes nothing.
  assert.equal(g.begin(), true, "trailing re-run proceeds");
  assert.equal(g.end(), false, "no further re-run owed");
});

test("the exact PR #136 scenario: open→flip-mode mid-fetch ends with a re-fetch", () => {
  // A = the initial issue fetch on open; the user flips to PRs while it's in
  // flight (setMode calls refresh()); A must schedule a fetch for the new mode.
  const g = new RefreshGate();
  assert.equal(g.begin(), true, "A: initial fetch starts");
  assert.equal(g.begin(), false, "flip-to-PRs refresh is coalesced, not dropped");
  assert.equal(g.end(), true, "A completes and re-fires refresh for PR mode");
  assert.equal(g.begin(), true, "B: the PR fetch runs");
  assert.equal(g.end(), false, "B completes cleanly — view now shows PR data");
});

test("a re-run owed during the trailing run is honored (chained switches)", () => {
  // Flip to PRs during A, then back to Issues during the trailing run B: B must
  // itself owe a re-run so we don't strand on PR data.
  const g = new RefreshGate();
  g.begin(); // A
  assert.equal(g.begin(), false); // flip during A
  assert.equal(g.end(), true); // A owes B
  g.begin(); // B (PRs)
  assert.equal(g.begin(), false); // flip back during B
  assert.equal(g.end(), true, "B owes a further re-run for the latest mode");
});
