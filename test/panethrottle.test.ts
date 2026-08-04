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
  hidden: false,
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

// ---------- #813: a hidden window has no render passes to save ----------
//
// The throttle's ENTIRE justification is render passes: an unfocused pane
// written to once per window renders ~6x fewer times, because xterm's
// RenderDebouncer coalesces within a frame and never across (see the module
// doc). A hidden document performs none of them — RenderDebouncer schedules
// every refresh on requestAnimationFrame, which does not fire in a hidden page
// (node_modules/@xterm/xterm/src/browser/RenderDebouncer.ts, `_animationFrame`).
// So while the window is hidden the deferral buys exactly zero.
//
// It is not free, though, and that is why this is a fix rather than a tidy-up.
// Deferring re-arms `window.setTimeout`, which a hidden page clamps; xterm's own
// `WriteBuffer.write` then schedules its parse behind a SECOND setTimeout
// ("if (this._didUserInput) {...} setTimeout(() => this._innerWrite())" — that
// file, verbatim). Two clamped timer stages in series sit in front of the one
// path out of xterm that is NOT display: the bytes the terminal writes back
// DOWN the pty — DA/DSR/OSC-colour/XTVERSION auto-replies and focus reports,
// which xterm emits only when it PARSES the query. A Windows session lock marks
// the window occluded, so that is the whole duration of a lock.
//
// This module cannot test the reply path — it is a pure policy core, and the
// coupling is a browser one. What it CAN pin is the policy: while hidden, do not
// defer. See doc/design/pane-render-throttle.md for the mechanism and the
// live-validation residue.

test("SHIPPED: a hidden window never defers — the deferral buys no render pass and costs the reply path", () => {
  // The exact input the shipped window defers when visible (asserted above):
  // an unfocused pane, mid-stream, one frame after its last flush, well under
  // the backlog cap. Hidden, it must go straight through.
  const input = {
    ...streaming,
    lastFlushMs: 1_000,
    nowMs: 1_016,
    windowMs: SHIPPED_WINDOW_MS,
  };
  assert.equal(
    decideFlush({ ...input, hidden: false }).kind,
    "defer",
    "control: visible, this is the case the throttle exists for"
  );
  assert.deepEqual(
    decideFlush({ ...input, hidden: true }),
    { kind: "flush" },
    "hidden: nothing renders, so holding it back saves nothing and delays every terminal " +
      "auto-reply behind a clamped timer"
  );
});

test("hidden flushes EVERY pane, not just the grid's active one", () => {
  // `live` is the grid's active pane — still 'active' while the window is
  // hidden, so exactly one pane per grid would keep its cadence and every other
  // one would sit behind the clamp. The whole point is that hidden is a
  // property of the DOCUMENT, so it cannot be answered per pane.
  for (const live of [false, true]) {
    assert.deepEqual(
      decideFlush({ ...streaming, live, hidden: true }),
      { kind: "flush" },
      `live=${live} must still flush while hidden`
    );
  }
});

test("hidden overrides a window a visible pane would still be inside", () => {
  // Not merely 'hidden happens to agree with the leading edge': pick a moment
  // deep inside the window, where the visible answer is a defer with most of
  // the window left to run.
  const midWindow = { ...streaming, lastFlushMs: 1_000, nowMs: 1_005, windowMs: 100 };
  const visible = decideFlush({ ...midWindow, hidden: false });
  assert.equal(visible.kind, "defer");
  if (visible.kind === "defer") assert.equal(visible.dueInMs, 95, "95ms of window still to run");
  assert.deepEqual(decideFlush({ ...midWindow, hidden: true }), { kind: "flush" });
});

test("hidden:false leaves every pre-existing rule exactly as it was", () => {
  // The regression guard. This change must be invisible to a visible window —
  // every assertion in this file above ran with hidden:false via `streaming`,
  // and these re-state the three shapes most likely to be disturbed by an
  // early return added at the top of the function.
  assert.deepEqual(decideFlush({ ...streaming, hidden: false, live: true }), { kind: "flush" });
  assert.deepEqual(
    decideFlush({ ...streaming, hidden: false, lastFlushMs: WOKEN }),
    { kind: "flush" },
    "leading edge"
  );
  assert.equal(
    decideFlush({ ...streaming, hidden: false }).kind,
    "defer",
    "and an unfocused mid-stream chunk still defers, which is #720 itself"
  );
});

test("the off switch still wins, hidden or not", () => {
  // `unfocusedRenderThrottleMs: 0` is a true bypass and must not acquire a new
  // reason to be consulted: with throttling off there is no deferral for hidden
  // to override, and both answers must agree.
  for (const hidden of [false, true]) {
    assert.deepEqual(decideFlush({ ...streaming, windowMs: 0, hidden }), { kind: "flush" });
  }
});
