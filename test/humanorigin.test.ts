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
    /** Run ONE macrotask round: the callbacks queued as of now, and no
     *  callbacks they themselves queue. Round 1 is where xterm's IME send
     *  lands, so "still open after one round" is the property that matters. */
    drainTask: () => {
      for (const fn of micro.splice(0)) fn();
      for (const fn of macro.splice(0)) fn();
    },
    /** Run rounds until nothing is left — the latch's deferred close takes two
     *  hops deliberately (see `humanorigin.ts`), so a test that wants the
     *  CLOSED state has to say so rather than assume one round suffices. */
    settle: () => {
      for (let round = 0; round < 8 && (micro.length || macro.length); round++) {
        for (const fn of micro.splice(0)) fn();
        for (const fn of macro.splice(0)) fn();
      }
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

test("a deferred mark survives its FIRST macrotask round — where the competing send lands", () => {
  // The manual-queue mirror of the real-timer regression test below. Round 1
  // is the round xterm's `_finalizeComposition` send is queued into, whichever
  // of the two timers was registered first; the close is deliberately not in
  // it. This is the assertion the one-hop version failed.
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

  latch.markDeferred();
  s.drainTask();

  assert.equal(latch.isHuman, true, "the IME send's own round must still read human");
});

test("a deferred mark IS closed once both its hops have run", () => {
  // Bounded widening, not an open door: two rounds, then shut. An auto-reply
  // arriving after that reads non-human exactly like any other.
  const s = manualScheduler();
  const latch = createHumanOriginLatch(s.schedule, s.scheduleTask);

  latch.markDeferred();
  s.settle();

  assert.equal(latch.isHuman, false, "the deferred window is bounded, not indefinite");
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
  s.settle();
  assert.equal(latch.isHuman, false, "and still closes on its own schedule");
});

/** Wait for an observable condition rather than a guessed duration, with a
 *  bounded backstop so a regression fails with a real diff instead of hanging
 *  (`ptywrite.test.ts`'s `timeoutAfter` precedent). Necessary here and not
 *  merely tidy: a zero-delay timer costs a full platform tick — ~15ms on this
 *  Windows host — so the two-hop close lands tens of milliseconds out, and any
 *  fixed sleep is either flaky or arbitrary. */
async function waitUntil(cond: () => boolean, capMs = 2000): Promise<void> {
  const start = Date.now();
  while (!cond() && Date.now() - start < capMs) {
    await new Promise((r) => setTimeout(r, 2));
  }
}

test("an IME commit reads human even though xterm registers its send AFTER our close (#528 B1)", async () => {
  // THE REGRESSION TEST. Everything else in this file models the deferred
  // window's SHAPE; none of it raced the competing timer, which is exactly how
  // a broken first cut shipped green.
  //
  // The real registration order on `compositionend`, and it is this way round
  // BECAUSE of a deliberate choice elsewhere: our listener is registered in the
  // capture phase on an ANCESTOR of the textarea (needed so the synchronous
  // `_inputEvent` path is marked before xterm sends), so it runs FIRST and our
  // close timer is registered FIRST. xterm's own textarea handler runs second
  // and registers the send. Equal-delay timers fire in registration order — so
  // a single-hop close fires BEFORE the send, and the commit reads non-human.
  //
  // Nothing later rescues it on the shipped platform: WebView2 is Chromium, and
  // Chromium fires the final `input` (insertCompositionText) BEFORE
  // `compositionend`, so no generation bump lands in between.
  const latch = createHumanOriginLatch(); // shipped defaults, real timers

  latch.markDeferred(); // 1. our capture-phase listener
  let readAtSend: boolean | null = null;
  setTimeout(() => {
    readAtSend = latch.isHuman; // 2. xterm's send, registered AFTER our close
  }, 0);

  await waitUntil(() => readAtSend !== null);
  assert.notEqual(readAtSend, null, "the simulated send must actually have run");
  assert.equal(
    readAtSend,
    true,
    "the IME commit must read human when it lands — a close that wins this race is the " +
      "CJK regression this whole mechanism exists to prevent",
  );
});

test("the deferred window still closes on its own, and does not outlive the send it protects", async () => {
  // The bound half: two hops is registration-order-independent, not open-ended.
  // A query auto-reply arriving after the window reads non-human like any other.
  const latch = createHumanOriginLatch();

  latch.markDeferred();
  await Promise.resolve();
  assert.equal(latch.isHuman, true, "a microtask boundary must not close a deferred mark");

  await waitUntil(() => !latch.isHuman);
  assert.equal(
    latch.isHuman,
    false,
    "the window is two timer rounds wide, not indefinite — an auto-reply arriving after it " +
      "must read non-human like any other",
  );
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
