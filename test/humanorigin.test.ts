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

/** A manual scheduler: collects callbacks, runs them when a test says so.
 *  Two queues, mirroring the two real ones — microtasks (end of the current
 *  synchronous turn) and macrotasks (the next task, where xterm's IME commit
 *  lands). */
function manualScheduler() {
  const micro: (() => void)[] = [];
  const macro: (() => void)[] = [];
  return {
    schedule: (fn: () => void) => micro.push(fn),
    scheduleTask: (fn: () => void) => macro.push(fn),
    /** End the current synchronous turn. */
    drain: () => {
      for (const fn of micro.splice(0)) fn();
    },
    /** Run the next macrotask — where an IME commit's data actually arrives. */
    drainTask: () => {
      for (const fn of micro.splice(0)) fn();
      for (const fn of macro.splice(0)) fn();
    },
  };
}

test("a fresh latch reads false — nothing is human until something says so", () => {
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);
  assert.equal(latch.isHuman, false);
});

test("a mark makes the rest of its own turn read human", () => {
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

  latch.mark(); // term.onKey
  // xterm fires the matching onData synchronously right after onKey, so this
  // is the read that must see the mark.
  assert.equal(latch.isHuman, true);
});

test("the mark is closed at the end of its turn, so LATER data reads non-human", () => {
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

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
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

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
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

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
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

  latch.mark();
  assert.equal(latch.isHuman, true);
  s.drain();
  assert.equal(latch.isHuman, false);

  latch.mark();
  assert.equal(latch.isHuman, true, "a later keystroke is human input too — the latch is not one-shot");
});

// ---------- the deferred scope: IME commits and `input`/insertText ----------
//
// `@xterm/xterm` 6.0.0 sends an IME commit from inside a `setTimeout(…, 0)`
// (`_finalizeComposition`), i.e. a LATER TASK than the `compositionend` that
// triggered it. A microtask-scoped mark is already closed by then, so without
// `markDeferred` every CJK/Japanese/Korean typist's input would classify as
// non-human — the guard would silently stop protecting the people most likely
// to have an unfinished composition sitting in the box.

test("a deferred mark survives the turn boundary a plain mark does not", () => {
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

  latch.markDeferred(); // compositionend
  s.drain(); // the compositionend handler's own turn ends

  assert.equal(
    latch.isHuman,
    true,
    "xterm has not sent the composed text yet — it is queued in a macrotask, and the " +
      "latch must still be open when it lands",
  );
});

test("a deferred mark IS closed once its macrotask has run", () => {
  // It is a bounded widening, not an open door: one task, then shut. An
  // auto-reply arriving after that reads non-human exactly like any other.
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

  latch.markDeferred();
  s.drainTask();

  assert.equal(latch.isHuman, false, "the deferred window is one macrotask wide, not indefinite");
});

test("a keystroke's microtask close cannot shut a composition's deferred mark", () => {
  // Generation stamping across BOTH kinds, and not a hypothetical: a
  // composition is routinely punctuated by key events, so a microtask close
  // queued by one of them would otherwise land inside the composition's
  // window and shut it early — silently dropping the IME fix in exactly the
  // interleaving it exists for.
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

  latch.mark(); // a key event during composition
  latch.markDeferred(); // compositionend right after
  s.drain(); // the key's microtask close runs — and must NOT win

  assert.equal(latch.isHuman, true, "the later deferred mark outranks the earlier pending close");
  s.drainTask();
  assert.equal(latch.isHuman, false, "and still closes on its own schedule");
});

test("the default deferred scheduler is a zero-delay timer — same primitive xterm's IME send uses", async () => {
  // The shipped default, not the injected one. Equal-delay timers fire in
  // registration order, which is what makes a close registered after xterm's
  // own send land after the data rather than before it.
  const latch = createHumanOriginLatch();

  latch.markDeferred();
  await Promise.resolve();
  assert.equal(latch.isHuman, true, "a microtask boundary must not close a deferred mark");

  await new Promise((r) => setTimeout(r, 0));
  assert.equal(latch.isHuman, false, "one macrotask later it is closed");
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
