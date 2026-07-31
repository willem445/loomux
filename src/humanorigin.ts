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

// TWO SCOPES, because xterm does not emit all human input on one schedule.
// Read from `@xterm/xterm` 6.0.0's own `lib/xterm.js` (quoted, not
// paraphrased — the repo's standing rule for third-party facts):
//
//   `_finalizeComposition(e){…this._isSendingComposition=!0,setTimeout((()=>{
//    …t.length>0&&this._coreService.triggerDataEvent(t,!0)}),0)}`
//
// An IME commit reaches the PTY from inside a `setTimeout(…, 0)` — a LATER
// TASK than the `compositionend` that caused it. A microtask-scoped mark is
// already closed by then, so CJK/Japanese/Korean typing would have been
// classified non-human: the guard would silently stop protecting exactly the
// users who most need a composition left alone. (#500's own doc names "a
// future IME/composition path" as the kind of refresher its bound exists to
// backstop; this is that path, and #518 must not create it.)
//
//   `_inputEvent(e){if(e.data&&"insertText"===e.inputType&&(!e.composed||
//    !this._keyDownSeen)…{…this.coreService.triggerDataEvent(t,!0)…}}`
//
// And a plain `input` event — dead keys/accents, soft keyboards — sends
// synchronously with no `onKey` involved at all.
//
// Both are still STRUCTURAL signals: they originate in DOM events on the
// terminal's own textarea, and xterm never routes a query auto-reply through
// the textarea — it calls `triggerDataEvent` directly. So the fix is to mark
// from those events too, on the scope each one actually needs.
//
// The deferred scope fails toward "human" if its window is ever mis-sized,
// which is the PRE-#518 behaviour: believing a human typed only ever makes
// delivery hold more. Failing the other way is the clobber every guard here
// exists to prevent, so the asymmetry is deliberate.

/** A short-lived "the data leaving right now came from a human" flag. */
export interface HumanOriginLatch {
  /** A genuine keyboard or paste event just happened, and the data it
   *  produces is emitted synchronously — `term.onKey`, `term.paste()`. Open
   *  for the rest of this synchronous turn only. */
  mark(): void;
  /** A human input event happened whose data xterm emits on a LATER TASK (an
   *  IME commit) or through a path `onKey` never sees (`input`/`insertText`).
   *  Open across the current turn AND the following macrotask, which is where
   *  `_finalizeComposition`'s `setTimeout(…, 0)` lands. */
  markDeferred(): void;
  /** Whether a mark is currently open. */
  readonly isHuman: boolean;
}

/**
 * Create a human-origin latch.
 *
 * `schedule` closes a `mark()` and defaults to `queueMicrotask`, which fires
 * after the current synchronous turn drains and before any later task — so a
 * mark covers exactly the `onData` its own key/paste event produced, and
 * nothing that arrives later.
 *
 * `scheduleTask` closes a `markDeferred()` and defaults to a zero-delay
 * `setTimeout` — the same primitive xterm's `_finalizeComposition` uses to send
 * an IME commit — but the close takes **TWO hops of it**, and that is the whole
 * correctness argument. The first cut took one hop and reasoned that our close
 * would be registered *after* xterm's send. It is registered BEFORE: our
 * listener is deliberately capture-phase on an ancestor (so the synchronous
 * `_inputEvent` path is marked before xterm sends), which means it runs first,
 * which means its timer is queued first. Equal-delay timers fire in
 * registration order, so a one-hop close beat the send and every IME commit
 * read non-human — the exact CJK regression this mechanism exists to prevent
 * (#528 review B1, reproduced against the real module).
 *
 * Two hops removes the dependency on ordering entirely rather than inverting
 * it. Whichever of the two timers is registered first, both run in the same
 * timer round; the second hop is only scheduled once the first has run, so it
 * necessarily lands in a LATER round than any send registered during the
 * original dispatch. There is no registration order that defeats it, which
 * matters because the order is a consequence of DOM capture semantics and a
 * future listener change could flip it back.
 *
 * Both schedulers are injectable so the rules are unit-testable with manual
 * queues, keeping this logic out of `pane.ts`'s hand-validated DOM wiring (the
 * repo's standing split) — but the ordering property above can only be pinned
 * with REAL timers and a competing send, so `humanorigin.test.ts` does exactly
 * that. A manual-queue test cannot see this class of bug: that is how the first
 * cut shipped green.
 *
 * Marks are generation-stamped, across BOTH kinds: a later mark of either kind
 * invalidates any earlier pending close, so a microtask close queued by a
 * keystroke can never shut a composition's deferred mark, and vice versa. That
 * matters in practice — a composition is routinely punctuated by key events.
 */
export function createHumanOriginLatch(
  schedule: (fn: () => void) => void = queueMicrotask,
  scheduleTask: (fn: () => void) => void = (fn) => {
    setTimeout(fn, 0);
  },
): HumanOriginLatch {
  let human = false;
  let generation = 0;
  const open = (closeWith: (fn: () => void) => void): void => {
    human = true;
    const mine = ++generation;
    closeWith(() => {
      if (generation === mine) human = false;
    });
  };
  return {
    mark(): void {
      open(schedule);
    },
    markDeferred(): void {
      // Two hops — see the doc above. The close is scheduled from INSIDE the
      // first hop, so it can never share a timer round with a send registered
      // during the same dispatch, whichever of the two was queued first.
      open((close) => scheduleTask(() => scheduleTask(close)));
    },
    get isHuman(): boolean {
      return human;
    },
  };
}
