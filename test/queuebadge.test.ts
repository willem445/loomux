// Unit tests for the pure delivery-queue badge presentation (#814).
//
// What these are FOR, since a label test can easily be a tautology: #814 asks
// for a badge a human can read at a glance — "is this flowing, and for how long
// has it not been" — and #813's lesson (its own scope addition) is that a detail
// only a hover reveals is a detail nobody sees. So each test below pins a
// property that would make the feature FAIL its brief if it regressed: the count
// and the age are in the label rather than the tooltip, a stalled queue is
// visibly different from a busy one, and an age is never rounded UP or shown as
// "0m" on a pane that has genuinely been waiting.
//
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  dockChipQueue,
  formatWaiting,
  queuePresentation,
  readingsByPty,
  type QueueDepthReading,
} from "../src/queuebadge.ts";

function reading(over: Partial<QueueDepthReading> = {}): QueueDepthReading {
  return {
    pty_id: 7,
    agent_id: "w-1",
    depth: 3,
    cap: 8,
    waiting_ms: 12_000,
    stalled: false,
    ...over,
  };
}

test("the label carries the count, the cap and the age — not the tooltip alone", () => {
  // The #813 lesson, as an assertion: everything a human needs to decide
  // "flowing or stuck" has to survive with the mouse nowhere near the pane.
  const { label } = queuePresentation(reading());
  assert.match(label, /\b3\/8\b/, `the depth and cap must be in the label: ${label}`);
  assert.match(label, /\b12s\b/, `the age of the oldest wait must be in the label: ${label}`);
});

test("a stalled queue reads differently from a busy one, in the label itself", () => {
  const busy = queuePresentation(reading({ waiting_ms: 12_000, stalled: false }));
  const stuck = queuePresentation(reading({ waiting_ms: 240_000, stalled: true }));
  assert.ok(!busy.label.includes("stalled"), `a draining queue must not cry stall: ${busy.label}`);
  assert.match(stuck.label, /stalled/, `a stalled queue must say so on screen: ${stuck.label}`);
  assert.notEqual(
    stuck.label[0],
    busy.label[0],
    "the two states must not open with the same glyph — the glyph is what reads at a glance"
  );
  assert.equal(stuck.stalled, true, "the styling flag has to follow the backend's verdict");
  assert.equal(busy.stalled, false);
});

test("the stalled tooltip names what a human can actually clear, without asserting a hold", () => {
  // `queue::pressure_notice`'s rule, on the frontend side: this reading is depth
  // and age only, so a pane whose senders simply outrun a healthy drainer must
  // not be told to go answer a question that isn't there.
  const { title } = queuePresentation(reading({ waiting_ms: 240_000, stalled: true }));
  assert.match(title, /question/i, "the two clearable causes must be named");
  assert.match(title, /unsubmitted text/i);
  assert.match(title, /\bCheck\b/, `the hold must be offered as a thing to check, not asserted: ${title}`);
  assert.ok(
    !/is held|is waiting on a question/i.test(title),
    `the tooltip must not claim a hold depth cannot establish: ${title}`
  );
});

test("a queue at its cap says deliveries are being DROPPED, matching the backend's own notice", () => {
  const atCap = queuePresentation(reading({ depth: 8, cap: 8 }));
  assert.match(atCap.title, /FULL \(8\/8\)/);
  assert.match(atCap.title, /DROPPED/, "the human must learn that arrivals are being lost, not queued");
  const below = queuePresentation(reading({ depth: 7, cap: 8 }));
  assert.ok(!/DROPPED/.test(below.title), "a pane with headroom must not claim loss");
});

test("the tooltip counts in singular and plural, and survives a pane with no agent id", () => {
  assert.match(queuePresentation(reading({ depth: 1 })).title, /^1 delivery queued for w-1\./);
  assert.match(queuePresentation(reading({ depth: 2 })).title, /^2 deliveries queued for w-1\./);
  // A plain (non-agent) pane, or a queue that outlived its agent record: the
  // backend sends an empty id rather than inventing one, so the wording has to
  // hold without it.
  assert.match(queuePresentation(reading({ agent_id: "" })).title, /queued for this pane\./);
});

test("an age floors at every step and never reads as no-wait while work is queued", () => {
  assert.equal(formatWaiting(0), "0s");
  assert.equal(formatWaiting(999), "0s");
  assert.equal(formatWaiting(59_999), "59s", "a sub-minute wait must not round up to 1m");
  assert.equal(formatWaiting(60_000), "1m");
  assert.equal(formatWaiting(119_999), "1m", "1m59s is 1m, not 2m — a badge may not overstate a wait");
  assert.equal(formatWaiting(3_599_999), "59m");
  assert.equal(formatWaiting(3_600_000), "1h");
  assert.equal(formatWaiting(3_900_000), "1h5m");
  // A clock that stepped backward, or a garbled payload: "0s" is the honest
  // reading, never "NaNs" or a negative age on the header.
  assert.equal(formatWaiting(-5_000), "0s");
  assert.equal(formatWaiting(Number.NaN), "0s");
});

test("the dock marker shows the count for a minimized pane, and nothing for an empty queue", () => {
  // Delegate panes open minimized, so this is the surface most agent queues are
  // actually seen on — a marker that collapsed to a dot would lose the one fact
  // the feature is about.
  assert.deepEqual(dockChipQueue(reading({ depth: 3 })), { marker: "⇥3", stalled: false });
  assert.deepEqual(dockChipQueue(reading({ depth: 5, stalled: true })), { marker: "⚠5", stalled: true });
  assert.equal(dockChipQueue(null), null, "a pane absent from the pushed set wears no marker");
  assert.equal(dockChipQueue(reading({ depth: 0 })), null, "and neither does an empty queue");
});

test("readingsByPty indexes the pushed set so absence clears a pane", () => {
  const byPty = readingsByPty([reading({ pty_id: 7 }), reading({ pty_id: 9, depth: 1 })]);
  assert.equal(byPty.get(7)?.depth, 3);
  assert.equal(byPty.get(9)?.depth, 1);
  assert.equal(
    byPty.get(11),
    undefined,
    "a pane the backend did not mention has an empty queue — the handler clears it by absence, " +
      "so this must be a miss rather than a stale hit"
  );
  assert.equal(readingsByPty([]).size, 0, "an empty push clears every pane");
});
