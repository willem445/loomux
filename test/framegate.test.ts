// Unit tests for the rAF dirty-flag gate (src/framegate.ts) — the bound
// `fm-hash` now declares in test/perfpolicy.test.ts's stream manifest.
//
// The property under test is a COUNT: however many batches arrive inside one
// frame, the DOM pass runs once. The frame scheduler is injected, so "one
// frame" is a function this test calls rather than a real animation frame —
// which is what lets the bound be checked in `node --test` at all.
//
// Red arm (the #714/#733 idiom, mechanically): make the gate pass-through —
// delete the `if (this.scheduled) return` guard in FrameGate.request, or hand
// the constructor an immediate scheduler — and the burst test below reports 8
// paints instead of 1.
import { test } from "node:test";
import assert from "node:assert/strict";
import { FrameGate } from "../src/framegate.ts";

/** A hand-cranked frame scheduler: `next()` runs the frame the gate booked. */
function fakeFrames() {
  const queue: (() => void)[] = [];
  return {
    schedule: (cb: () => void) => void queue.push(cb),
    /** Run every callback booked so far (one frame's worth). */
    next(): void {
      const due = queue.splice(0, queue.length);
      for (const cb of due) cb();
    },
    get booked(): number {
      return queue.length;
    },
  };
}

test("a burst of batches inside one frame costs exactly one paint", () => {
  const frames = fakeFrames();
  let paints = 0;
  const gate = new FrameGate(() => paints++, frames.schedule);

  // fm_hash_start emits a batch per 8 files; this is a 64-file folder's worth
  // arriving before the browser gets a frame in.
  for (let i = 0; i < 8; i++) gate.request();
  assert.equal(frames.booked, 1, "the gate booked more than one frame for one burst");
  assert.equal(paints, 0, "nothing may paint before the frame runs");

  frames.next();
  assert.equal(paints, 1, "eight batches in one frame must cost one paint, not eight");
});

test("the gate re-arms: the next frame's batches paint again", () => {
  const frames = fakeFrames();
  let paints = 0;
  const gate = new FrameGate(() => paints++, frames.schedule);

  gate.request();
  frames.next();
  gate.request();
  frames.next();
  assert.equal(paints, 2, "a batch after the frame must schedule a fresh paint");
});

test("a batch arriving DURING the paint is not swallowed", () => {
  // The flag is cleared before the paint runs, so a re-entrant request (a
  // synchronous listener firing off the paint, or a batch delivered while the
  // paint is on the stack) books the NEXT frame instead of being absorbed by a
  // flag that has not been cleared yet — the one way a coalescer loses data.
  const frames = fakeFrames();
  let paints = 0;
  let reentered = false;
  const gate: FrameGate = new FrameGate(() => {
    paints++;
    if (!reentered) {
      reentered = true;
      gate.request();
    }
  }, frames.schedule);

  gate.request();
  frames.next();
  assert.equal(paints, 1);
  assert.equal(frames.booked, 1, "the re-entrant request must have booked another frame");
  frames.next();
  assert.equal(paints, 2, "the batch that arrived during the paint never got painted");
});

test("pending reports whether a frame is booked", () => {
  const frames = fakeFrames();
  const gate = new FrameGate(() => {}, frames.schedule);
  assert.equal(gate.pending, false);
  gate.request();
  assert.equal(gate.pending, true);
  frames.next();
  assert.equal(gate.pending, false);
});
