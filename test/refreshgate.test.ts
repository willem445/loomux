// Unit tests for the loss-safe refresh gate (src/refreshgate.ts). Pure state
// machine — no DOM, no async — exercised directly. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { RefreshGate, CoalescingRefresh } from "../src/refreshgate.ts";

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

// ---------- CoalescingRefresh: the gate with its async loop attached ----------
//
// This is the bound `orch-tasks-changed` declares in test/perfpolicy.test.ts's
// stream manifest (#743 S5): the property is a COUNT — a burst of N board
// writes costs the run already in flight plus exactly one trailing run.
//
// Red arm: make `request()` call `this.run()` directly (the pre-#743 handler,
// which is what `void this.refresh()` on every event was) and the burst test
// reports 11 refetches instead of 2.

/** A refresh whose completion this test controls. */
function deferred(): { promise: Promise<void>; resolve: () => void; reject: (e: unknown) => void } {
  let resolve!: () => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("a burst of events during one in-flight refresh costs one trailing refresh", async () => {
  let runs = 0;
  let current = deferred();
  const r = new CoalescingRefresh(() => {
    runs++;
    current = deferred();
    return current.promise;
  });

  r.request(); // the first orch-tasks-changed starts a refetch
  assert.equal(runs, 1);
  const first = current;
  // Ten more board writes land while that refetch is still in flight.
  for (let i = 0; i < 10; i++) r.request();
  assert.equal(runs, 1, "a second concurrent refetch must never start");

  first.resolve();
  await first.promise;
  await Promise.resolve(); // let the finally-block's trailing request run
  assert.equal(runs, 2, "the coalesced burst owes exactly one trailing refetch, and it must fire");

  const trailing = current;
  trailing.resolve();
  await trailing.promise;
  await Promise.resolve();
  assert.equal(runs, 2, "a quiet trailing run must not chain another");
});

test("the trailing run reads current state, so nothing is lost by coalescing", async () => {
  // The loss-safety half of the bound: whatever the last event carried, the
  // trailing run happens AFTER it, so it observes the final board.
  let board = "initial";
  const seen: string[] = [];
  let current = deferred();
  const r = new CoalescingRefresh(() => {
    seen.push(board);
    current = deferred();
    return current.promise;
  });

  r.request(); // reads "initial"
  const first = current;
  board = "after ten writes";
  r.request();
  first.resolve();
  await first.promise;
  await Promise.resolve();
  current.resolve();
  await current.promise;

  assert.deepEqual(seen, ["initial", "after ten writes"]);
});

test("a rejected refresh does not wedge the gate, and the trailing run still fires", async () => {
  // The failure case: `orch_tasks` can throw (group torn down, IO error). If a
  // rejection left the gate marked running, the board would go permanently
  // deaf to orch-tasks-changed — a worse bug than the one being bounded.
  const errors: unknown[] = [];
  const realError = console.error;
  console.error = (...args: unknown[]) => void errors.push(args);
  try {
    let runs = 0;
    let current = deferred();
    const r = new CoalescingRefresh(() => {
      runs++;
      current = deferred();
      return current.promise;
    });

    r.request();
    const first = current;
    r.request(); // an event arrives mid-flight, owing a trailing run
    first.reject(new Error("orch_tasks failed"));
    await first.promise.catch(() => {});
    await Promise.resolve();
    await Promise.resolve();

    assert.equal(runs, 2, "the trailing run must still fire after a failed run");
    assert.equal(r.isRunning, true, "the trailing run is in flight");
    current.resolve();
    await current.promise;
    await Promise.resolve();
    assert.equal(r.isRunning, false, "the gate is released once the trailing run completes");
    assert.equal(errors.length, 1, "the failure is reported, not silently swallowed");
  } finally {
    console.error = realError;
  }
});

test("sequential requests each run — coalescing only applies to overlap", async () => {
  let runs = 0;
  let current = deferred();
  const r = new CoalescingRefresh(() => {
    runs++;
    current = deferred();
    return current.promise;
  });
  for (let i = 0; i < 3; i++) {
    r.request();
    const d = current;
    d.resolve();
    await d.promise;
    await Promise.resolve();
  }
  assert.equal(runs, 3, "a gate that suppressed non-overlapping refreshes would be a stale board");
});
