// The unfocused-pane flush policy (#720) — panethrottle.ts.
//
// These tests are deliberately split into two kinds, and the split is the
// point:
//
//  - Tests that pass `windowMs` EXPLICITLY pin the policy's shape (leading
//    edge, backlog cap, live-pane passthrough, off-switch). They hold for any
//    window.
//  - Tests that read `SHIPPED_WINDOW_MS` pin the SHIPPED policy — that
//    loomux actually throttles an unfocused streaming pane at all. Those are
//    the ones that go red if the shipped window is ever set back to 0, which is
//    exactly the pre-#720 behaviour, and is how the red-before-green evidence
//    for this change was taken (see the PR).
//
// Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { decideFlush, WOKEN, MAX_PENDING_BYTES, type FlushInput } from "../src/panethrottle.ts";
import { DEFAULT_SETTINGS } from "../src/settings.ts";

/** The window loomux actually ships. Read from settings.ts, which is its single
 *  source of truth (panethrottle.ts holds the policy, not the number), so these
 *  assertions track the real default and cannot drift from it. */
const SHIPPED_WINDOW_MS = DEFAULT_SETTINGS.unfocusedRenderThrottleMs;

/** A visible, unfocused pane mid-stream: it flushed 1 ms ago and a small chunk
 *  just arrived. Every test below names only the field it exercises. */
const streaming: FlushInput = {
  live: false,
  nowMs: 1_001,
  lastFlushMs: 1_000,
  pendingBytes: 512,
  windowMs: 100,
  maxPendingBytes: MAX_PENDING_BYTES,
};

// ---------- the shipped policy ----------

test("SHIPPED: an unfocused pane written to every frame defers instead of rendering every frame", () => {
  // The whole point of #720's first half. With the shipped window, a pane that
  // was written to one frame ago (16 ms) and is not the active pane must NOT
  // write again yet — each write it skips is a `renderRows` pass it skips,
  // because xterm's RenderDebouncer coalesces within a frame and never across.
  const d = decideFlush({
    ...streaming,
    lastFlushMs: 1_000,
    nowMs: 1_016,
    windowMs: SHIPPED_WINDOW_MS,
  });
  assert.equal(d.kind, "defer", "a shipped window of 0 would make this flush — i.e. no throttling at all");
});

test("SHIPPED: the window is long enough to skip several frames, not just one", () => {
  // A window that only skipped a single frame would be a rounding error, not a
  // fix. Assert the actual ratio the design claims: at least 4 frames' worth.
  assert.ok(
    SHIPPED_WINDOW_MS >= 4 * 16,
    `the shipped window (${SHIPPED_WINDOW_MS}ms) must span several 60Hz frames to reduce render passes meaningfully`
  );
});

test("SHIPPED: a deferred chunk is scheduled to land inside the window, never dropped", () => {
  const d = decideFlush({
    ...streaming,
    lastFlushMs: 1_000,
    nowMs: 1_016,
    windowMs: SHIPPED_WINDOW_MS,
  });
  assert.equal(d.kind, "defer");
  if (d.kind !== "defer") return;
  assert.equal(d.dueInMs, SHIPPED_WINDOW_MS - 16);
  assert.ok(d.dueInMs > 0 && d.dueInMs <= SHIPPED_WINDOW_MS);
});

// ---------- policy shape ----------

test("the focused (live) pane never defers, whatever the window", () => {
  assert.deepEqual(decideFlush({ ...streaming, live: true }), { kind: "flush" });
  assert.deepEqual(
    decideFlush({ ...streaming, live: true, windowMs: 10_000, pendingBytes: 1 }),
    { kind: "flush" },
    "the pane the human is typing in keeps its per-frame cadence unconditionally"
  );
});

test("leading edge: the first chunk into a quiet pane flushes on arrival", () => {
  // A pane printing one line every few seconds must behave exactly as it did
  // before this policy existed — otherwise every low-rate pane in the grid
  // gains a visible lag for no saving at all.
  assert.deepEqual(decideFlush({ ...streaming, lastFlushMs: WOKEN }), { kind: "flush" });
});

test("leading edge: a pane quiet for longer than the window flushes on arrival", () => {
  assert.deepEqual(
    decideFlush({ ...streaming, lastFlushMs: 1_000, nowMs: 1_000 + 100 }),
    { kind: "flush" },
    "exactly one window elapsed is due"
  );
  assert.deepEqual(decideFlush({ ...streaming, lastFlushMs: 1_000, nowMs: 9_000 }), { kind: "flush" });
});

test("the backlog cap overrides the window: a firehose is written out, not accumulated", () => {
  // The failure case: a pane emitting faster than the window drains would grow
  // loomux's own held-chunk list without limit and race xterm's 50MB
  // DISCARD_WATERMARK, which THROWS rather than degrading.
  assert.deepEqual(
    decideFlush({ ...streaming, pendingBytes: MAX_PENDING_BYTES }),
    { kind: "flush" },
    "at the cap"
  );
  assert.deepEqual(
    decideFlush({ ...streaming, pendingBytes: MAX_PENDING_BYTES + 1 }),
    { kind: "flush" },
    "past the cap"
  );
  assert.equal(
    decideFlush({ ...streaming, pendingBytes: MAX_PENDING_BYTES - 1 }).kind,
    "defer",
    "just under the cap still respects the window"
  );
});

test("windowMs <= 0 is the off switch — exactly the pre-#720 policy, every chunk straight through", () => {
  // This is what `unfocusedRenderThrottleMs: 0` in settings.json selects, and
  // it must be a true bypass: an unfocused, mid-stream, sub-cap chunk that the
  // shipped window would defer.
  assert.deepEqual(decideFlush({ ...streaming, windowMs: 0 }), { kind: "flush" });
  assert.deepEqual(decideFlush({ ...streaming, windowMs: -1 }), { kind: "flush" });
});

test("a backwards clock waits a full window instead of scheduling a zero/negative timer", () => {
  // A never-firing or immediately-refiring timer is how a throttle turns into
  // either a stall or a spin. Neither is allowed for any clock the host can
  // hand us.
  const d = decideFlush({ ...streaming, lastFlushMs: 5_000, nowMs: 1_000, windowMs: 100 });
  assert.equal(d.kind, "defer");
  if (d.kind !== "defer") return;
  assert.equal(d.dueInMs, 100);
});

test("dueInMs is never below 1ms, so a flush is always actually scheduled", () => {
  const d = decideFlush({ ...streaming, lastFlushMs: 1_000, nowMs: 1_099.6, windowMs: 100 });
  assert.equal(d.kind, "defer");
  if (d.kind !== "defer") return;
  assert.ok(d.dueInMs >= 1, `dueInMs was ${d.dueInMs}`);
});
