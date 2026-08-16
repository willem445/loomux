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
// WHAT THIS CAN AND CANNOT PROMISE, EXACTLY. The tempting claim — "strictly
// fewer resizes than the 16 ms debounce, on every input" — is FALSE, and the
// boundary is sharp enough to state rather than hedge. It has two conditions
// and needs BOTH:
//
//   - the burst OUTLASTS `maxWaitMs`, so a ceiling fit fires inside it, AND
//   - the ResizeObserver gap is under 16 ms — a display above 62.5 Hz — where
//     the old window happened to be WIDER than the frame interval and so
//     collapsed a burst of any length into a single trailing fit.
//
// Cross both and this schedules more: a 2 s window-edge drag on a 144 Hz
// display is 1 fit today and 5 here. Everywhere else it schedules fewer or the
// same — and in particular for every burst SHORTER than the ceiling, which is
// every animated transition in this app and the whole of #1149, it schedules
// exactly one against the old policy's one-per-frame.
//
// The trade is deliberate, and the thing being given up is worth naming: on a
// high-refresh display today, a gesture with no settled geometry leaves the
// terminal FROZEN at its pre-gesture size for the entire gesture, because the
// old debounce never fired. That is not a property anyone chose — it is the
// same accident that produced 15 fits at 60 Hz, read off the other side of the
// same comparison. The ceiling replaces both accidents with one deliberate
// cadence. `test/resizeburst.test.ts` pins the boundary in BOTH directions
// (62.5 Hz vs 63 Hz, and ceiling-length vs longer) so this stays a measured
// trade rather than a sentence.
//
// The alternative that would make the universal claim true, considered and NOT
// taken: let a ceiling fit reflow xterm but withhold its `resizePty` until the
// burst settles — the `held` state #432 already built for divider drags. That
// caps ConPTY resizes at one per burst on every display. It is not free: it
// puts xterm and the child at DIFFERENT geometries for up to `maxWaitMs` at a
// time, which under conpty's resize quirk is #430's own failure class, and it
// would introduce that divergence on the window-drag path, where no display
// rate has it today. Trading a resize-count regression above 62.5 Hz for a
// correctness-class regression at every rate is a product call, not a
// refactor — so it is written down here rather than taken quietly.

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
 *  alive — ~2.5 times a second here, whatever the display does.
 *
 *  "Whatever the display does" is the half that is easy to get wrong, because
 *  the OLD behaviour was not one behaviour: the same drag reflowed ~60 times a
 *  second at 60 Hz and NOT ONCE at 144 Hz, purely from where the frame gap fell
 *  relative to a 16 ms window. This constant is where that accident is replaced
 *  by a chosen number, and it is also where this policy can schedule MORE than
 *  its predecessor — see the header's boundary. */
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
   *  a tick stream that never stops is a busy loop, not a coalescer.
   *
   *  The residue that floor leaves, stated because everything else here reasons
   *  about its degenerate inputs (rev-1 on #1157): past the ceiling `dueInMs`
   *  pins to 1 ms, so a tick stream arriving faster than 1 ms apart would
   *  re-arm the timer forever and the fit would starve. The producer is a
   *  `ResizeObserver`, which delivers once per frame, so nothing in this app
   *  can reach it — and the floor is still right over 0, which would starve on
   *  a stream merely faster than the task queue. If a sub-millisecond geometry
   *  producer is ever wired to `applyFit`, this is the line that stops being
   *  true. */
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
