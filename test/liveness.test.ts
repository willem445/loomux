// #1601 Phase 0.4 — the webview's half of the liveness heartbeat.
//
// `LivenessPulse` takes its clock, its frame scheduler and its transport as
// injected dependencies specifically so this file can exist (`framegate.test.ts`
// is the precedent), and for one review round it did not — the module comment
// and the PR body both said it was tested while `npm test` was green either way
// (#1605 review B2). What that costs is not abstract: every value asserted below
// feeds `selfwatch::liveness`, and a regression in the frame rule is the
// difference between a `GuiStuck` diagnosis and a `GuiHidden` one.

import { test } from "node:test";
import assert from "node:assert/strict";
import { LivenessPulse, LIVENESS_STAMP_MS, type LivenessStamp } from "../src/liveness.ts";

/** A hand-driven world: the clock only moves when a test moves it, and frames
 *  are serviced only when a test services them. No DOM, no waiting. */
function harness() {
  let now = 0;
  const sent: LivenessStamp[] = [];
  const frames: Array<() => void> = [];
  const pulse = new LivenessPulse({
    now: () => now,
    hidden: () => hidden,
    requestFrame: (cb) => void frames.push(cb),
    send: (s) => void sent.push(s),
  });
  let hidden = false;
  return {
    pulse,
    sent,
    /** Frame callbacks booked and not yet run. */
    frames,
    advance: (ms: number) => {
      now += ms;
    },
    setHidden: (v: boolean) => {
      hidden = v;
    },
    /** Service the oldest outstanding frame, as a real rAF would. */
    serviceFrame: () => {
      const cb = frames.shift();
      assert.ok(cb, "expected an outstanding frame to service");
      cb();
    },
  };
}

test("the first tick reports no lag, because there is no previous tick to be late against", () => {
  const h = harness();
  h.pulse.tick();
  assert.equal(h.sent.length, 1);
  assert.equal(h.sent[0].timerLagMs, 0);
  // And no frame has been serviced yet, so there is nothing to report about one.
  assert.equal(h.sent[0].frameLagMs, null);
});

test("timer lag is how far the tick overshot the cadence, and is never negative", () => {
  const h = harness();
  h.pulse.tick();

  // Exactly on time: no lag. This is the assertion that fails if the cadence is
  // subtracted with the wrong sign or not at all.
  h.advance(LIVENESS_STAMP_MS);
  h.pulse.tick();
  assert.equal(h.sent[1].timerLagMs, 0, "a tick that arrives on schedule is not late");

  // Late by 400 ms.
  h.advance(LIVENESS_STAMP_MS + 400);
  h.pulse.tick();
  assert.equal(h.sent[2].timerLagMs, 400);

  // EARLY (a timer can fire a hair early, and a clock can be coarse) reports 0,
  // not a negative number the backend would then compare against a threshold.
  h.advance(LIVENESS_STAMP_MS - 50);
  h.pulse.tick();
  assert.equal(h.sent[3].timerLagMs, 0, "an early tick is not negatively late");
});

test("a tick reports the frame booked by the PREVIOUS tick, then forgets it", () => {
  const h = harness();
  h.pulse.tick(); // books frame A
  assert.equal(h.frames.length, 1, "the first tick books one frame");

  h.advance(30);
  h.serviceFrame(); // A serviced 30 ms after it was booked

  h.advance(LIVENESS_STAMP_MS - 30);
  h.pulse.tick();
  assert.equal(h.sent[1].frameLagMs, 30, "the second tick reports the first tick's frame");

  // Consumed, not latched: with no frame serviced in the next window, the tick
  // after it reports `null` rather than repeating 30. A latched value would
  // read as a healthy GUI forever after one good frame.
  h.advance(LIVENESS_STAMP_MS);
  h.pulse.tick();
  assert.equal(h.sent[2].frameLagMs, null, "a stale reading is not repeated");
});

test("at most ONE frame is outstanding, so a hidden window cannot queue a burst", () => {
  // The module calls this rule "load-bearing rather than tidy": a hidden window
  // services no frames, so booking one per tick would queue a minute's worth and
  // fire them together on restore, each reporting a lag measured from a
  // different request. This is the test that fails if the guard is dropped.
  const h = harness();
  h.setHidden(true);

  h.pulse.tick();
  assert.equal(h.frames.length, 1);
  for (let i = 0; i < 60; i++) {
    h.advance(LIVENESS_STAMP_MS);
    h.pulse.tick();
  }
  assert.equal(h.frames.length, 1, "sixty hidden ticks must not queue sixty frames");
  assert.ok(
    h.sent.every((s) => s.frameLagMs === null),
    "a window that serviced no frame reports none — `null` is 'no evidence', not 'lag 0'"
  );

  // On restore the single outstanding frame runs, reporting the true (large)
  // lag of the request that was actually outstanding, and the next tick books
  // a fresh one.
  h.setHidden(false);
  // The outstanding frame was booked by the FIRST tick, at t = 0; the loop above
  // advanced 60 cadences, so it is serviced at t = 60 * LIVENESS_STAMP_MS and
  // that is its true lag. Derived rather than written as a literal, so the
  // number cannot drift away from the fixture that produces it.
  const bookedAt = 0;
  const servicedAt = 60 * LIVENESS_STAMP_MS;
  h.serviceFrame();
  h.advance(LIVENESS_STAMP_MS);
  h.pulse.tick();
  assert.equal(h.sent.at(-1)!.frameLagMs, servicedAt - bookedAt);
  assert.equal(h.frames.length, 1, "and the pulse is booking again");
});

test("`hidden` is reported as the window's state at the moment of the stamp", () => {
  const h = harness();
  h.pulse.tick();
  assert.equal(h.sent[0].hidden, false);

  h.setHidden(true);
  h.advance(LIVENESS_STAMP_MS);
  h.pulse.tick();
  assert.equal(h.sent[1].hidden, true, "the backend declines to call a hidden window stuck");

  h.setHidden(false);
  h.advance(LIVENESS_STAMP_MS);
  h.pulse.tick();
  assert.equal(h.sent[2].hidden, false, "and the flag is re-read, not latched");
});

test("every tick sends exactly one stamp", () => {
  // The vacuity control for the whole file: each assertion above reads
  // `sent[n]`, and all of them hold just as well against a pulse that sends
  // nothing at all if the indices happen not to exist. They do exist because
  // of this.
  const h = harness();
  for (let i = 0; i < 5; i++) {
    h.advance(LIVENESS_STAMP_MS);
    h.pulse.tick();
  }
  assert.equal(h.sent.length, 5);
});
