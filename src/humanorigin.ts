// Human-origin latch for a pane's PTY input (#518).
//
// THE PROBLEM. `term.onData` is xterm's entire "data destined for the PTY"
// channel, not a human-input one: it fires identically for a keystroke, a
// paste, and for replies the terminal manufactures ENTIRELY ON ITS OWN in
// answer to a program's queries — OSC 10/11/4 colour probes, primary/secondary
// device attributes, cursor-position and focus reports. GitHub Copilot's TUI
// issues those at boot and again mid-session on redraw/focus churn, so a
// copilot pane produces a steady trickle of PTY input with nobody at the
// keyboard. `test/xterm-humaninput.test.ts` pins that fact against real xterm.
//
// The backend used to tell the two apart by BYTE SHAPE (#496 PR-A / #499:
// skip anything that parses as a CSI/OSC/DCS sequence). That works for every
// reply shape anyone has catalogued, and it is still in force — but it is a
// pattern match against an OPEN set, and #496's own plan closed with "which
// copilot emission recurs mid-session" unanswered. #518 is that gap firing in
// production: a false "a human is typing here" pinned a delivery's
// human-input block, and the prompt sat unsubmitted behind a badge with no
// live fact behind it.
//
// THE FIX, and why it is this shape. #440 B2-R hit the identical problem for
// this pane's own `firstInputMs` and did NOT answer it with a smarter
// `onData` filter — deliberately, because bracketed paste itself starts with
// ESC, so shape-filtering misfires on real pastes. It took the signal from
// `term.onKey` and the two explicit `term.paste()` call sites instead: those
// fire only for genuine keyboard/paste events and are unreachable by anything
// the terminal generates for itself. That is a structural guarantee rather
// than a guess about an open set, and this latch is how that already-proven
// bit reaches the backend's keystroke clock too.
//
// WHY A LATCH RATHER THAN A PARAMETER. xterm fires `onKey` synchronously
// immediately before the matching `onData`, and `term.paste()` triggers its
// `onData` synchronously from the call — but `onData` itself carries no
// origin. So the input events mark this latch, `onData` reads it, and the mark
// is dropped at the end of the same synchronous turn. Data the terminal
// manufactures arrives while its own `term.write()` is being parsed — always a
// different turn — and therefore always reads false. Nothing here decays on
// wall-clock time: this is an origin test, not a recency one.

/** A one-turn "the data leaving right now came from a human" flag. */
export interface HumanOriginLatch {
  /** A genuine keyboard or paste event just happened. */
  mark(): void;
  /** Whether we are still inside the turn a `mark()` opened. */
  readonly isHuman: boolean;
}

/**
 * Create a human-origin latch.
 *
 * `schedule` runs the un-mark and defaults to `queueMicrotask`, which fires
 * after the current synchronous turn drains and before any later task — so a
 * mark covers exactly the `onData` its own key/paste event produced, and
 * nothing that arrives later. It is injectable so the latch is unit-testable
 * with a manual queue, keeping this logic out of `pane.ts`'s hand-validated
 * DOM wiring (the repo's standing split).
 *
 * Marks are generation-stamped: a second `mark()` inside the same turn
 * invalidates the first one's pending un-mark, so the earlier scheduled
 * callback cannot close the latch out from under a mark that came after it.
 */
export function createHumanOriginLatch(
  schedule: (fn: () => void) => void = queueMicrotask,
): HumanOriginLatch {
  let human = false;
  let generation = 0;
  return {
    mark(): void {
      human = true;
      const mine = ++generation;
      schedule(() => {
        if (generation === mine) human = false;
      });
    },
    get isHuman(): boolean {
      return human;
    },
  };
}
