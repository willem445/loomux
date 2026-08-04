// Pure, DOM-free core of the unfocused-pane flush policy (#720). The DOM
// wiring lives in pane.ts (`handleOutput`/`flushOutput`); everything that
// DECIDES anything is here so it is unit-testable under `node --test`, the
// same split as panefit.ts / panerename.ts.
//
// What this is for, and what it is NOT for
// ----------------------------------------
// #714 already bounded the *event* rate: the backend coalesces a pane's PTY
// output into at most one `pty-output` per 60 Hz frame, so the per-event costs
// (a GUI-thread script compilation, one `atob`, one decode loop) are already
// amortized and are NOT what this addresses. What #714 could not touch is what
// each surviving event costs INSIDE xterm, per pane, per frame:
//
//   - PARSE — `Terminal.write` appends to xterm's `WriteBuffer`, which parses
//     in 12 ms slices and yields between them
//     (node_modules/@xterm/xterm/src/common/input/WriteBuffer.ts,
//     `WRITE_TIMEOUT_MS`). Cost is a function of BYTES.
//   - RENDER — every parsed row-range change calls `RenderService.refreshRows`,
//     which funnels into `RenderDebouncer.refresh` — one `requestAnimationFrame`
//     per batch (browser/RenderDebouncer.ts). Cost is a function of DIRTY CELLS
//     and is paid once per frame in which the pane was written to at all.
//
// Throttling moves NO parse work: the same bytes still reach xterm and are
// still parsed. What it removes is RENDER PASSES — a pane written to once per
// 100 ms renders ~6× fewer times than one written to every frame, because
// `RenderDebouncer` coalesces everything inside one frame into a single
// `renderRows` call and nothing coalesces ACROSS frames. That is the entire
// mechanism, and it is why this only pays on panes whose render share is
// non-trivial. See doc/design/pane-render-throttle.md.
//
// xterm already stops rendering a pane that is not on screen at all:
// `RenderService` observes the screen element with an `IntersectionObserver`
// and sets `_isPaused` when it stops intersecting (browser/services/
// RenderService.ts, `_handleIntersectionChange`), which covers a hidden
// project tab, a maximized-behind sibling, and a docked pane. The gap this
// closes is the one xterm cannot see: a pane that IS on screen and IS being
// rendered, in a grid of six, that the human is not reading.
//
// Data loss is not on the table. Deferring means holding the arrived chunks in
// a list and writing them, in order, on the next flush — `Terminal.write`'s own
// queue does the same thing one layer down. Nothing is dropped, coalesced, or
// reordered; only the moment of the `write` call moves.

/** What to do with a chunk that just arrived for a pane. */
export type FlushDecision =
  | { kind: "flush" }
  /** Hold it; the caller schedules a flush `dueInMs` from now (>= 1). */
  | { kind: "defer"; dueInMs: number };

export interface FlushInput {
  /** This pane must keep its per-frame cadence: it is the grid's active pane,
   *  or the human has fed it input recently enough that its echo is still an
   *  interactive latency (see `WOKEN` below). */
  live: boolean;
  nowMs: number;
  /** When this pane last wrote to xterm, or `WOKEN` if it is starting from
   *  quiet — never written, or explicitly woken by human input. `WOKEN` is what
   *  makes this a LEADING-edge throttle: the first chunk into a quiet pane goes
   *  straight through, so a pane that prints one line every few seconds behaves
   *  exactly as it did before this existed. Same policy shape as the backend
   *  coalescer #714 put one layer down, for the same reason. */
  lastFlushMs: number | typeof WOKEN;
  /** Bytes held back so far, INCLUDING the chunk that just arrived. */
  pendingBytes: number;
  /** The throttle window. `<= 0` disables throttling entirely — the pre-#720
   *  policy, which is what the `unfocusedRenderThrottleMs: 0` setting selects
   *  and what the A/B arm of the tests measures against. */
  windowMs: number;
  /** Whether the whole DOCUMENT is hidden — occluded, minimized, or behind a
   *  locked workstation (`document.visibilityState === "hidden"`).
   *
   *  A property of the document, not of the pane, which is why it cannot be
   *  folded into `live`: `live` is the grid's ACTIVE pane and stays active
   *  while the window is hidden, so keying on it alone would leave exactly one
   *  pane per grid at its normal cadence and every other one behind the
   *  clamp. See `decideFlush` for why hidden must flush. */
  hidden: boolean;
  /** Hard ceiling on held bytes: a pane emitting faster than the window can
   *  drain must not grow an unbounded backlog in loomux's own list. */
  maxPendingBytes: number;
}

/** `lastFlushMs` sentinel: this pane is starting from quiet, so the next chunk
 *  flushes on arrival. Deliberately not `null` — the field is read on every
 *  chunk and a named constant says WHY the leading edge exists at the call
 *  site. */
export const WOKEN = "woken" as const;

// The shipped WINDOW is deliberately not a constant here: it is
// `DEFAULT_SETTINGS.unfocusedRenderThrottleMs` in settings.ts, because the value
// a build ships is exactly the value a hand-edited `settings.json` overrides,
// and a second constant in this file would be a second source of truth free to
// drift from it. This module owns the policy; settings.ts owns the number.

/** Ceiling on bytes held for one pane before the window is overridden and the
 *  backlog is written out. 1 MiB per 100 ms window is 10 MB/s — an order above
 *  what any interactive PTY sustains, so in practice this only fires on a
 *  `cat` of something large, where it bounds loomux's own array rather than
 *  letting it race xterm's 50 MB `DISCARD_WATERMARK` (WriteBuffer.ts, which
 *  THROWS rather than degrading). A cap, not a threshold noticed afterwards:
 *  the flush happens on the chunk that crosses it. */
export const MAX_PENDING_BYTES = 1024 * 1024;

/** Decide whether a pane writes an arriving chunk straight through to xterm or
 *  holds it for the rest of its window. Ordered so the cheap always-flush
 *  cases (throttling off, a live pane, a quiet pane) short-circuit before any
 *  arithmetic — this runs on every chunk of every pane. */
export function decideFlush(i: FlushInput): FlushDecision {
  if (i.windowMs <= 0) return { kind: "flush" }; // throttling off: pre-#720 policy
  // #813. A hidden document renders NOTHING, so the deferral has nothing left
  // to buy — and it is not free. Grouped with the off switch above because both
  // are "the throttle does not apply at all", not "this pane is special".
  //
  // What it buys: zero. Every saving this module claims is a `renderRows` pass,
  // and `RenderDebouncer` schedules every refresh on `requestAnimationFrame`
  // (browser/RenderDebouncer.ts), which does not fire in a hidden page.
  //
  // What it costs: the reply path. Deferring re-arms `window.setTimeout`, which
  // a hidden page clamps, and xterm's `WriteBuffer.write` then schedules its
  // parse behind a SECOND setTimeout of its own. Two clamped stages in series
  // sit in front of the one path out of xterm that is not display — the bytes
  // the terminal writes back DOWN the pty (DA/DSR/OSC-colour/XTVERSION replies
  // and DEC-1004 focus reports), which xterm emits only when it PARSES the
  // query that asked for them. An agent CLI waiting on one of those waits on
  // our timer. A Windows session lock marks the window occluded for its whole
  // duration, which is #813's reported window.
  //
  // Deliberately NOT expressed as `live`: see `FlushInput.hidden`.
  if (i.hidden) return { kind: "flush" };
  if (i.live) return { kind: "flush" }; // focused / just-typed-into: per-frame cadence
  if (i.lastFlushMs === WOKEN) return { kind: "flush" }; // leading edge
  if (i.pendingBytes >= i.maxPendingBytes) return { kind: "flush" }; // backlog bound
  const elapsed = i.nowMs - i.lastFlushMs;
  // A clock that went backwards (or a same-millisecond arrival) must not
  // produce a negative/zero-length wait that never gets rescheduled: treat any
  // non-positive elapsed as "the window just started".
  if (elapsed >= i.windowMs) return { kind: "flush" };
  return { kind: "defer", dueInMs: Math.max(1, i.windowMs - Math.max(0, elapsed)) };
}
