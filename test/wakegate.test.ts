// The visibility policy for event-driven embed views (#1318) — `src/wakegate.ts`.
//
// The defect this pins: `TasksView`/`DecisionsView` refetched and rebuilt on every
// `orch-tasks-changed` / `orch-questions-changed` / `orch-needs-you-changed` event in
// every pane that had ever opened one, on screen or not. The policy is one predicate,
// so it is tested as one — the DOM wiring (`EmbedEntry.hide`, the views' `show()`/
// `hide()`) is validated by hand through `__wakeGateStats()`, per the module header.
//
// WHY THE FOUR CROSSINGS. The gate reads TWO inputs — the `wake()`/`sleep()` latch and
// the pane's live `confirm` probe — and the repo's standing rule is that a guard reading
// two inputs pins all four crossings plus a negative control, so "suppress everything"
// and "run everything" both fail. The negative control here is `a visible view refreshes
// on every wake`: without it, a gate hard-wired to `return false` would pass every
// suppression assertion in the file.
import { test } from "node:test";
import assert from "node:assert/strict";
import { WakeGate, resetWakeGateStats, wakeGateStats } from "../src/wakegate.ts";

/** A `confirm` probe whose answer the test owns, and which counts its own reads —
 *  "was it consulted at all" is part of the contract (the awake path must not pay it). */
function probe(visible: boolean) {
  const p = {
    visible,
    reads: 0,
    fn: () => {
      p.reads++;
      return p.visible;
    },
  };
  return p;
}

// ---------- the four crossings ----------

test("latch awake, probe would say hidden: the wake RUNS and the probe is never read", () => {
  // The union direction, stated as a test: one true input is enough, and a missed
  // `sleep()` costs exactly what the pre-#1318 code cost rather than showing stale data.
  const p = probe(false);
  const gate = new WakeGate(p.fn);
  gate.wake();
  assert.equal(gate.accepts(), true);
  assert.equal(p.reads, 0, "the awake path must not pay for the probe");
});

test("latch awake, probe says visible: the wake runs", () => {
  const p = probe(true);
  const gate = new WakeGate(p.fn);
  gate.wake();
  assert.equal(gate.accepts(), true);
});

test("latch asleep, probe says hidden: the wake is SUPPRESSED — the whole point", () => {
  const p = probe(false);
  const gate = new WakeGate(p.fn);
  gate.wake();
  gate.sleep();
  assert.equal(gate.accepts(), false);
  assert.equal(p.reads, 1, "the suppressing path must consult the pane, not just the latch");
});

test("latch asleep, probe says visible: the wake runs anyway — the release is not the event", () => {
  // A `wake()` the pane never delivered must not leave a panel the human is looking
  // straight at silently frozen. This is the bounded release: it depends on a live read
  // of the pane's own state, not on a call having arrived.
  const p = probe(true);
  const gate = new WakeGate(p.fn);
  gate.sleep();
  assert.equal(gate.accepts(), true);
});

// ---------- the negative control ----------

test("negative control: a visible view refreshes on EVERY wake, not just the first", () => {
  // Without this, a gate that simply answered `false` forever would satisfy every
  // suppression assertion above.
  const gate = new WakeGate(() => true);
  gate.wake();
  for (let i = 0; i < 5; i++) assert.equal(gate.accepts(), true, `wake ${i} must run`);
});

// ---------- the state machine around them ----------

test("born asleep: a view constructed but never shown suppresses its stream", () => {
  // `Pane.requestEmbedFocus` constructs a view before deciding whether it can open one,
  // and `restoreEmbeds` can build one whose slot never opens. Starting awake would leave
  // exactly those refreshing forever, which is the bug in a different disguise.
  const gate = new WakeGate(() => false);
  assert.equal(gate.asleep, true);
  assert.equal(gate.accepts(), false);
});

test("hide then show: the wakes in between are dropped, and the view is live again after", () => {
  const p = probe(false);
  const gate = new WakeGate(p.fn);
  gate.wake();
  gate.sleep();
  for (let i = 0; i < 20; i++) assert.equal(gate.accepts(), false);
  // `show()` re-wakes AND refreshes unconditionally (see both views' `show()`), so the
  // 20 dropped wakes are re-earned by one open — nothing has to be remembered.
  gate.wake();
  assert.equal(gate.accepts(), true);
});

test("sleep and wake are idempotent — the pane calls them in bursts", () => {
  // `embedViewAtSide` closes and reopens the same view to move it between edges, and
  // `requestEmbedFocus` calls `show()` on an already-visible view to drain a focus park.
  const gate = new WakeGate(() => false);
  gate.wake();
  gate.wake();
  assert.equal(gate.accepts(), true);
  gate.sleep();
  gate.sleep();
  assert.equal(gate.accepts(), false);
});

test("a stray HEALS the latch: the probe is read once, not on every later wake", () => {
  // Otherwise a single hole in the pane's wiring would make every subsequent event pay a
  // DOM read forever — a gate that is right and expensive instead of right and cheap.
  const p = probe(true);
  const gate = new WakeGate(p.fn);
  gate.sleep();
  assert.equal(gate.accepts(), true);
  assert.equal(gate.accepts(), true);
  assert.equal(gate.accepts(), true);
  assert.equal(p.reads, 1, "the latch must have healed on the first stray");
  assert.equal(gate.asleep, false);
});

// ---------- the hand-validation instrument ----------

test("the stats instrument counts what the human is told to look at", () => {
  // The module documents `__wakeGateStats()` as how the human validates the DOM wiring
  // an agent cannot run: `suppressed` climbs while a panel is closed, `delivered` stops,
  // and `strays` stays 0. A counter that does not move makes that instruction a lie.
  resetWakeGateStats();
  const gate = new WakeGate(() => false);
  gate.wake();
  gate.accepts();
  gate.accepts();
  gate.sleep();
  gate.accepts();
  gate.accepts();
  gate.accepts();
  assert.deepEqual(wakeGateStats(), { delivered: 2, suppressed: 3, strays: 0 });
});

test("a stray is counted separately — it is the signal that the latch has a hole", () => {
  resetWakeGateStats();
  const gate = new WakeGate(() => true);
  gate.sleep();
  gate.accepts();
  assert.deepEqual(wakeGateStats(), { delivered: 1, suppressed: 0, strays: 1 });
});
