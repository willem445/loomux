// The webview's half of the liveness heartbeat (#1601, plan §3 Phase 0.4).
//
// beta5 and beta6 look identical from outside — the window is up and the app
// does not work — and they are opposite failures. beta5 stalled the webview
// thread; beta6 left it perfectly healthy and starved everything behind it.
// Nothing in the app could tell the two apart, and finding out which one had
// happened cost a release cycle each time.
//
// So both halves stamp, and the backend compares them. The watchdog thread
// stamps once a second with its own scheduling lag; this module stamps once a
// second with what only the webview can measure — how late its own timer was,
// and how late a frame was actually serviced.
//
// WHAT IT COSTS. One `setInterval` at 1 Hz whose body is a rounding subtraction
// and one `invoke` of a SYNC command with a six-atomic-store body: no lock, no
// IO, no allocation on either side. For scale, the tab strip polls every 4 s
// and the group view every 2 s, each issuing ONE invoke since #1608 (two per
// group-bound tab and ten respectively before it), and both of those do real
// work at the far end.
//
// WHY IT IS NOT VISIBILITY-GATED, when `performance.md` INV-4's default is that
// a timer driving IPC should be. A hidden window is one of the states the app
// can be frozen IN — minimized while an agent works is the normal way this app
// is used — so gating the heartbeat would blind it exactly where a freeze is
// least likely to be noticed early. The cost of not gating is the paragraph
// above; the cost of gating is the instrument.
//
// The platform still throttles a hidden window's timers, which would look like
// a stuck GUI thread if the backend took it at face value. It does not: every
// stamp carries `hidden`, and `selfwatch::liveness` declines to call a stale
// stamp a hang when the last one said the window was not on screen. "No
// evidence" is a different answer from "stuck", and this instrument is only
// worth having if it keeps them apart.
//
// The class below takes its clock, its frame scheduler and its transport as
// injected dependencies, so `test/liveness.test.ts` drives it without a DOM and
// without waiting a second per assertion (`framegate.ts` is the precedent).

import { invoke } from "./transport.ts";

/** The heartbeat cadence. Three of these is `selfwatch::LIVENESS_STALE_MS`, so
 *  a single descheduled tick is never enough to call either half stuck. */
export const LIVENESS_STAMP_MS = 1000;

/** What one stamp reports. Field names match the Rust command's arguments. */
export interface LivenessStamp {
  /** How much this tick overshot {@link LIVENESS_STAMP_MS}, never negative. */
  timerLagMs: number;
  /** How late the frame booked by the PREVIOUS tick was serviced, or `null` if
   *  it has not been serviced yet — which a hidden window is entitled to. */
  frameLagMs: number | null;
  hidden: boolean;
}

/** Everything the pulse needs from the world, injected so tests can supply it. */
export interface LivenessDeps {
  /** A monotonic millisecond clock (`performance.now()` in the app). */
  now(): number;
  hidden(): boolean;
  /** Book one frame callback (`requestAnimationFrame` in the app). */
  requestFrame(cb: () => void): void;
  send(stamp: LivenessStamp): void;
}

export class LivenessPulse {
  private readonly deps: LivenessDeps;
  /** When the previous tick ran, or `null` before the first. */
  private lastTickAt: number | null = null;
  /** When the outstanding frame was booked, or `null` if none is outstanding.
   *
   *  At most ONE is ever outstanding, and that is load-bearing rather than
   *  tidy: a hidden window services no frames, so booking one per tick would
   *  queue a minute's worth and then fire them all at once on restore, each
   *  reporting the lag of a request made at a different time. One outstanding
   *  booking reports one true number — the frame really was serviced that
   *  late — instead of a burst of stale ones. */
  private frameBookedAt: number | null = null;
  /** The lag of the most recently serviced frame, consumed by the next tick. */
  private frameLagMs: number | null = null;

  // Fields are declared and assigned rather than written as TypeScript
  // parameter properties: `node --test` runs these modules in strip-only mode,
  // which refuses that syntax.
  constructor(deps: LivenessDeps) {
    this.deps = deps;
  }

  /** One heartbeat: report the window that just ended, then book the frame the
   *  next one will report on. */
  tick(): void {
    const now = this.deps.now();
    const timerLagMs =
      this.lastTickAt === null
        ? 0
        : Math.max(0, Math.round(now - this.lastTickAt - LIVENESS_STAMP_MS));
    this.lastTickAt = now;

    const frameLagMs = this.frameLagMs;
    this.frameLagMs = null;
    this.deps.send({ timerLagMs, frameLagMs, hidden: this.deps.hidden() });

    if (this.frameBookedAt !== null) return; // one outstanding, see the field
    const bookedAt = now;
    this.frameBookedAt = bookedAt;
    this.deps.requestFrame(() => {
      this.frameBookedAt = null;
      this.frameLagMs = Math.max(0, Math.round(this.deps.now() - bookedAt));
    });
  }
}

/** Start the heartbeat. Returns the stop function; the app never calls it (the
 *  heartbeat is app-lifetime by design), and it exists so a test or a future
 *  teardown path can. */
export function startLiveness(): () => void {
  const pulse = new LivenessPulse({
    now: () => performance.now(),
    hidden: () => document.visibilityState === "hidden",
    requestFrame: (cb) => void requestAnimationFrame(() => cb()),
    // Best-effort, and silent on failure on purpose: a heartbeat that toasted
    // the human when it could not reach the backend would be an alarm about
    // the thing it exists to report quietly, and the backend already notices a
    // stamp that stopped arriving.
    send: (stamp) => void invoke("liveness_stamp", { ...stamp }).catch(() => {}),
  });
  const id = setInterval(() => pulse.tick(), LIVENESS_STAMP_MS);
  return () => clearInterval(id);
}
