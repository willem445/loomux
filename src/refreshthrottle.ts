// The leading-edge time-window throttle the git surfaces react through, as a
// pure decision (P4's shape, doc/design/performance.md §2 — panethrottle.ts is
// the same split: the module owns the policy, the caller owns the timer).
//
// What it bounds. A pane learns its repository may have moved from two
// signals: the shell's own OSC 7 prompt report, and the backend `git-changed`
// watcher (1 s poll, emits on signature change). Each reaction is a `dir_info`
// invoke for the header's branch chip plus a git-view refresh. The view half
// was already throttled to 500 ms; the `dir_info` half rode every event
// ungated, so a rebase or a checkout loop turned a 1 Hz watcher into an
// unbounded stream of sync commands on the webview thread (#743's census, part
// 2b). Both halves now run through this policy, each bounded to one pass per
// window, with the trailing edge guaranteeing the last signal is not the one
// that gets dropped.
//
// Two windows, one policy — not one shared window. Each half keeps its own
// `lastRunMs`, and they are meant to drift: the view's advances only while it
// is visible, the pane's whenever a cwd is known, and either can fire in a
// window the other has already spent. What is shared is the decision and the
// constant, so the two cannot disagree about how long a window is. A single
// window across both would be a different (and worse) design: whichever half
// signalled first would suppress the other, which is how a chip goes stale
// behind a view that refreshed instead.
//
// Leading edge, deliberately: the first signal into a quiet pane runs
// immediately (a `cd` or a commit must move the chip now, not in half a
// second), and only a *second* signal inside the window is deferred — the same
// argument #714/#733 make one layer down for PTY output.

/** What to do with a signal that just arrived. */
export type RefreshDecision =
  /** Run the refresh now (and record the time as the window's start). */
  | { kind: "run" }
  /** Nothing to do: a trailing run is already booked for this window. */
  | { kind: "drop" }
  /** Book a trailing run `dueInMs` from now (>= 1). */
  | { kind: "schedule"; dueInMs: number };

export interface RefreshInput {
  nowMs: number;
  /** When the last refresh ran. `0` (or any time older than the window) means
   *  "quiet", so the leading edge fires. */
  lastRunMs: number;
  /** Whether a trailing run is already booked — the caller's timer handle. */
  timerPending: boolean;
  /** The window. `<= 0` disables throttling: every signal runs, which is the
   *  pre-#743 behavior of the `dir_info` half and the A/B arm the tests use. */
  windowMs: number;
}

/** How long a pane coalesces repository-change signals for. 500 ms is the
 *  window `GitView.notifyPrompt` has always used; keeping ONE constant here is
 *  what stops the two halves of the reaction being two numbers free to drift
 *  apart — see the note above on why they are still two windows. */
export const REPO_SIGNAL_WINDOW_MS = 500;

export function decideRefresh(i: RefreshInput): RefreshDecision {
  if (i.windowMs <= 0) return { kind: "run" }; // throttling off
  const since = i.nowMs - i.lastRunMs;
  // A clock that jumped backwards must not park a pane forever on a window
  // that never elapses: treat a negative `since` as "the window just started"
  // and let the trailing timer (>= 1 ms) release it.
  if (since >= i.windowMs) return { kind: "run" };
  if (i.timerPending) return { kind: "drop" };
  return { kind: "schedule", dueInMs: Math.max(1, i.windowMs - Math.max(0, since)) };
}
