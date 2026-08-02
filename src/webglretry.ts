// Pure, DOM-free core of the WebGL re-acquire policy (#720). DOM wiring is
// pane.ts's `tryWebgl` / `handleWebglLoss`.
//
// The failure this exists to end
// ------------------------------
// `WebglAddon.onContextLoss` fires and pane.ts disposes the addon, which drops
// that terminal back to xterm's DOM renderer — the correct immediate response,
// and where it used to stop. Nothing ever retried, so a single lost context
// left ONE pane in a grid of six several times more expensive to render than
// its neighbours, permanently and invisibly, until the human happened to hide
// and re-show the project tab (`Pane.setHidden`, which drops and re-acquires
// the context for unrelated reasons). A pane that is quietly the expensive one
// for the rest of a session is exactly the kind of cost nobody attributes.
//
// Why a retry needs a bound at all
// --------------------------------
// A WebGL context is a capped resource: the browser holds a fixed number live
// and evicts the oldest when a new one is created past the cap (the same cap
// that makes `setHidden` drop contexts for inactive tabs at all — see
// pane.ts's `setHidden` and doc/design/project-tabs.md). So "lost context" and
// "someone else created a context" are the SAME event seen from two panes, and
// an unbounded retry turns that into a live-lock: pane A re-acquires, evicting
// pane B, whose retry evicts pane A, forever, each round burning a context
// creation and a full texture-atlas rebuild. The ladder below is what makes
// that sequence terminate.
//
// Two things damp it before this policy is even consulted, and they set the
// scale of the delays. `WebglRenderer` does not report a loss immediately: it
// preventDefaults `webglcontextlost` and waits 3000 ms for a
// `webglcontextrestored` that the browser may deliver on its own, firing
// `onContextLoss` only if none arrives (node_modules/@xterm/addon-webgl/src/
// WebglRenderer.ts). So by the time this policy runs, the browser's OWN
// restoration path has already been given its chance and declined — which is
// why the first rung is seconds, not milliseconds: there is nothing to be won
// by racing back in.

/** Backoff ladder, in order, for successive losses in one streak. Three rungs
 *  spanning ~72 s: a transient loss (a driver reset, a burst of context
 *  creation while a tab switch re-acquires several panes at once) is over well
 *  inside the first, and a pane still losing its context on the third has a
 *  standing reason to — too many live panes for the cap, a machine with no
 *  usable GPU path — that a fourth attempt would not change. */
export const WEBGL_RETRY_DELAYS_MS: readonly number[] = []; // RED HALF (#720): the pre-#720 policy

/** A context that stayed alive this long before dying was not part of a storm,
 *  so it opens a NEW streak with a full budget rather than consuming the tail
 *  of an old one. This is the release-on-independent-evidence half of the
 *  bound: the evidence is the context's own healthy lifetime, not elapsed wall
 *  clock since some earlier event. Without it, three unlucky losses spread
 *  across an eight-hour session would strand a pane on the DOM renderer for
 *  the rest of that session — a bound that outlives its own justification. */
export const WEBGL_HEALTHY_MS = 5 * 60_000;

export interface RetryInput {
  /** Losses already handled in the current streak — 0 for the first. */
  priorLosses: number;
  /** How long the context that just died had been alive, in ms. */
  healthyMs: number;
}

export interface RetryPlan {
  /** How long to wait before building a fresh `WebglAddon`, or `null` to stay
   *  on the DOM renderer until something independent resets the streak (a tab
   *  hide/show, i.e. `Pane.setHidden`). */
  delayMs: number | null;
  /** The streak count to carry into the next loss. */
  losses: number;
}

/** Plan the response to one `onContextLoss`. Pure: the caller owns the timer,
 *  the streak counter, and the clock. */
export function planWebglRetry(i: RetryInput): RetryPlan {
  // A long-lived context that died is a fresh incident, not the next rung.
  const streak = i.healthyMs >= WEBGL_HEALTHY_MS ? 0 : Math.max(0, i.priorLosses);
  const delayMs = streak < WEBGL_RETRY_DELAYS_MS.length ? WEBGL_RETRY_DELAYS_MS[streak] : null;
  // Count the loss either way: an exhausted streak that keeps losing stays
  // exhausted, and only a healthy lifetime (or a hide/show) clears it.
  return { delayMs, losses: streak + 1 };
}
