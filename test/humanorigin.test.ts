// #518: the human-origin latch — the structural half of "did a human produce
// this PTY write". See `src/humanorigin.ts` for why an origin test, rather
// than a smarter `onData` byte filter, is what closes this.
//
// The scheduler is injected so these tests drive the turn boundary explicitly
// instead of awaiting real microtasks: the property is "a mark covers exactly
// the turn it was made in", and a test that has to guess when a microtask ran
// is testing the runtime, not the latch.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHumanOriginLatch } from "../src/humanorigin.ts";

/** A manual scheduler: collects callbacks, runs them when a test says so. */
function manualScheduler() {
  const queue: (() => void)[] = [];
  return {
    schedule: (fn: () => void) => queue.push(fn),
    /** End the current synchronous turn. */
    drain: () => {
      for (const fn of queue.splice(0)) fn();
    },
  };
}

test("a fresh latch reads false — nothing is human until something says so", () => {
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule);
  assert.equal(latch.isHuman, false);
});

test("a mark makes the rest of its own turn read human", () => {
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule);

  latch.mark(); // term.onKey
  // xterm fires the matching onData synchronously right after onKey, so this
  // is the read that must see the mark.
  assert.equal(latch.isHuman, true);
});

test("the mark is closed at the end of its turn, so LATER data reads non-human", () => {
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule);

  latch.mark();
  s.drain(); // the key event's turn ends

  // This is #518's actual failure mode: xterm answers a program's colour /
  // device-attribute query while parsing program output — a different turn,
  // no key pressed — and that write used to be indistinguishable from typing.
  assert.equal(
    latch.isHuman,
    false,
    "a terminal auto-reply arriving after the key event's turn must not inherit its origin",
  );
});

test("data with no preceding input event at all is never human", () => {
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule);

  // The copilot-at-boot case: the pane has never been typed into, and the TUI
  // queries the terminal the moment it starts.
  s.drain();
  assert.equal(latch.isHuman, false);
});

test("a second mark in the same turn is not closed by the first one's scheduled un-mark", () => {
  // Generation stamping. Without it, mark-mark-drain would run TWO un-marks
  // and the second mark would be closed by a callback queued before it — a
  // real ordering in xterm, where a paste can follow a keystroke inside one
  // dispatch.
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule);

  latch.mark();
  latch.mark();
  assert.equal(latch.isHuman, true, "still inside the turn");

  s.drain();
  assert.equal(latch.isHuman, false, "and both un-marks resolve to the same closed state");
});

test("marking again after a turn boundary re-opens the latch", () => {
  // The steady state of a human actually typing: key, data, turn ends, key,
  // data... Each keystroke must be recognized on its own.
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule);

  latch.mark();
  assert.equal(latch.isHuman, true);
  s.drain();
  assert.equal(latch.isHuman, false);

  latch.mark();
  assert.equal(latch.isHuman, true, "a later keystroke is human input too — the latch is not one-shot");
});

test("the default scheduler is queueMicrotask — the mark closes before the next task", async () => {
  // The one property the injected scheduler cannot pin: that the SHIPPED
  // default closes the mark at the right boundary. `queueMicrotask` drains
  // after the current synchronous turn and before any later task, which is
  // exactly "the onData that this key event produced, and nothing after it".
  const latch = createHumanOriginLatch();

  latch.mark();
  assert.equal(latch.isHuman, true, "synchronously after the mark: still the same turn");

  await Promise.resolve(); // let the microtask queue drain
  assert.equal(latch.isHuman, false, "by the next turn the mark is closed");
});
