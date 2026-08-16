// Pure, DOM-free scheduling policy for a pane's fit tick (#1149). `pane.ts`
// owns the timer and the `ResizeObserver`; this module owns the only question
// that has a wrong answer — HOW LONG to wait before fitting — so it is
// unit-testable under `node --test`, the same split as panefit.ts (which owns
// the other half: whether the fit that eventually runs reaches the PTY).
//
// WHAT WAS WRONG (#1149)
//
// `applyFit` debounced on **16 ms**. A `ResizeObserver` callback is delivered
// once per frame, in the rendering steps — 16.7 ms apart at 60 Hz — and a
// `setTimeout` armed for 16 ms fires from the task queue BEFORE the next
// frame's delivery. So the debounce could never coalesce two consecutive
// frames: a debounce window narrower than the interval between the events it
// debounces is not a debounce, it is a one-frame delay. Every burst of
// geometry change fitted, reflowed and resized the ConPTY once per frame for
// its whole duration.
//
// That is invisible for a one-shot change (equalize, autosize, a split — one
// tick, one fit either way) and ruinous for anything ANIMATED. `#sessions` is
// an in-flow flex item with `transition: width 0.24s` (styles.css), so opening
// or closing the session browser shrinks `#grid-area` on every frame of a
// 240 ms transition: ~15 fits per pane per toggle, each one an xterm buffer
// reflow over the whole scrollback AND a `ResizePseudoConsole` — the call
// CLAUDE.md constraint 1 exists to ration, and the one #430's cursor-desync
// class rides on (doc/design/xterm-resize-reflow.md). With six panes open that
// is ~90 ConPTY resizes for one click, which is what the human felt.
//
// #432 fixed the DRAG shape of this storm by BRACKETING it — `beginResizeHold`
// / `endResizeHold` around a divider drag. That works because a drag has a
// start and an end to hook. A CSS transition has neither, and neither will the
// side-dock autosize (#1150); a mechanism that has to be wired per gesture
// covers only the gestures somebody remembered. This one is in the debounce
// itself, so every consumer of the resize path inherits it whether or not it
// knows the path exists.
//
// THE POLICY. Trailing-edge, with a ceiling:
//
//   - wait `windowMs` after the LAST geometry change, so an unbroken burst of
//     them — at any frame rate down to ~17 fps — collapses into ONE fit at the
//     settled geometry, and
//   - never withhold a fit longer than `maxWaitMs` from the moment the burst
//     began, so a gesture that keeps moving (a window-edge drag, which has no
//     settled geometry for as long as the human holds the mouse) still reflows
//     periodically instead of freezing at a stale size.
//
// The ceiling is the bounded-suppression half required of anything that holds
// an action "while X is true" (performance.md §2 P4): it releases on the
// clock, which does not depend on the burst signal still being right, so no
// pattern of ResizeObserver deliveries can withhold a pane's fit indefinitely.
//
// STRICTLY FEWER, NEVER MORE. Every fit this schedules happens at a time the
// old 16 ms debounce would also have fitted at or before — the window only ever
// grows the wait, and the ceiling only ever cuts it back to a point still
// inside the burst. So no input sequence produces more PTY resizes than
// shipped today; `test/resizeburst.test.ts` pins that as a property over the
// tick patterns the app actually generates, not just as a claim here.

/** How long a pane waits for its geometry to stop moving before it fits.
 *
 *  Chosen against the thing it has to beat: `ResizeObserver` delivery, once
 *  per frame. 60 ms coalesces an unbroken burst at any frame rate down to
 *  ~17 fps, which is well past the point where the animation driving it has
 *  stopped looking like an animation. It is also the whole added latency of a
 *  ONE-SHOT layout change (equalize, autosize, a split): those fitted 16 ms
 *  after the change and now fit 60 ms after it — one extra frame or two, below
 *  what a human reads as a delay, and the cost of not having to bracket every
 *  gesture by hand. */
export const FIT_WINDOW_MS = 60;

/** Ceiling on how long a fit may be withheld while geometry keeps moving,
 *  measured from the start of the burst.
 *
 *  Two-sided. It must be comfortably ABOVE the longest animated transition in
 *  the app (`#sessions`, 240 ms) plus one window — a ceiling that fired
 *  mid-transition would put a fit, and therefore a ConPTY resize, at an
 *  intermediate geometry, which is precisely the repaint this module exists to
 *  remove. And it must be low enough that a continuous gesture with no settled
 *  geometry (dragging the window's edge) still reflows often enough to look
 *  alive — ~2.5 times a second here, against ~60 before. */
export const FIT_MAX_WAIT_MS = 400;

/** One `ResizeObserver` delivery, as this policy sees it. */
export interface BurstTick {
  nowMs: number;
  /** When the current unbroken run of geometry changes began, or `null` when
   *  the pane is quiet — nothing scheduled, so this tick starts a fresh burst.
   *  The caller clears it to `null` whenever a fit actually runs. */
  burstStartMs: number | null;
  windowMs: number;
  maxWaitMs: number;
}

/** What the caller arms its single fit timer with, plus the burst start to
 *  carry into the next tick. Handed back together so the caller cannot store a
 *  burst start that disagrees with the delay computed from it. */
export interface FitPlan {
  /** Delay for the (one) fit timer. At least 1 ms: a zero-delay reschedule on
   *  a tick stream that never stops is a busy loop, not a coalescer. */
  dueInMs: number;
  burstStartMs: number;
}

/** Plan the fit for one geometry-change tick.
 *
 *  Both inputs are repaired rather than trusted, because both have a failure
 *  mode that would make this WORSE than the debounce it replaces:
 *
 *   - a `maxWaitMs` at or below the window would make the ceiling bind on every
 *     tick, so every tick would fit — the degenerate 16 ms behaviour again,
 *     just spelled differently. The ceiling is floored at the window.
 *   - a `burstStartMs` in the future is a clock that went backwards
 *     (`Date.now()` follows the wall clock, so an NTP step or a DST-adjacent
 *     correction can do it). Left alone it would make `ceiling` enormous, which
 *     is harmless, or — after a backwards step of more than `maxWaitMs` — make
 *     it negative for the rest of the burst, which is the busy loop above. A
 *     start that cannot be in the past is treated as this tick starting the
 *     burst. */
export function planFit(t: BurstTick): FitPlan {
  const windowMs = Math.max(1, t.windowMs);
  const start =
    t.burstStartMs === null || !Number.isFinite(t.burstStartMs) || t.burstStartMs > t.nowMs
      ? t.nowMs
      : t.burstStartMs;
  const untilCeiling = start + Math.max(windowMs, t.maxWaitMs) - t.nowMs;
  return { dueInMs: Math.max(1, Math.min(windowMs, untilCeiling)), burstStartMs: start };
}
