// Single-flight gate for a fixed-cadence poll (#1602, plan §3 Phase 2.2 of
// EPIC #1600). The sibling of pollgate.ts (arms/disarms a timer around window
// visibility) and wakegate.ts (suppresses a wake when nobody is looking at
// the panel): this gate answers a third, orthogonal question — "the timer
// fired again, but is the LAST call it made still running?" — for a poll body
// that is itself an async IPC round trip and can therefore outlive its own
// tick.
//
// WHY THIS EXISTS (EPIC #1600 §1.2, the beta6 mechanism). The group view's
// ten-invoke batch (groupview.ts, 2 s) and the tab strip's per-tab status
// sweep (tabbar.ts, 4 s) both fire on a bare `setInterval`, which does not
// wait for the async body's promise to settle. When the backend is slow or a
// registry lock is stuck, each tick parks another `spawn_blocking` thread on
// the shared 512-thread pool instead of reusing the one still waiting; at
// roughly 2-5 threads/s that exhausts the pool in minutes, and once it does,
// `write_pty`'s own `spawn_blocking` can no longer be scheduled and every
// pane stops accepting input at once. Refusing to issue a second call while
// the first is still outstanding makes that accumulation UNREACHABLE from the
// poll path, regardless of what the hold itself turns out to be — Phase 0/1
// of the plan diagnose the hold separately; this phase does not need to know
// what it is.
//
// SCOPED PER SITE, NEVER GLOBAL. Each poll site (one open group view, the
// app's one tab strip) owns its own `SingleFlight` instance, held as a
// private field the same way `PollGate` is. A shared/global gate would let
// one group's stuck call silence every other site's poll — the poll-path
// analogue of the cross-tenant coupling this repo's other guards (GroupId,
// the membership check) exist to rule out. Never hoist one `SingleFlight` to
// module scope and hand it to more than one caller.
//
// A REJECTED CALL RELEASES THE GATE. The `finally` below runs on both the
// resolved and the rejected path, so a poll body that throws (a refused group
// id, a dropped connection) never wedges the flight open — the very next
// tick tries again, which matters more here than on the happy path, since an
// error is exactly the moment the backend is likely to be in trouble.

export interface SingleFlightStats {
  /** Calls that were allowed to run (across every `SingleFlight` instance). */
  ran: number;
  /** Ticks skipped because the previous call from that SAME instance had not
   *  yet settled. */
  skipped: number;
}

/** Module-global counters (wakegate.ts's shape, not pollgate.ts's per-instance
 *  registry — a `SingleFlight` has no timer to tear down, so there is nothing
 *  a disposed view could leave running behind it): every instance in every
 *  view adds to the same two numbers, which is enough for the hand-validation
 *  instrument below. A per-site breakdown is not needed to see whether
 *  skipping is happening at all. */
let ranCount = 0;
let skippedCount = 0;

/** The hand-validation instrument, reachable from devtools as
 *  `__singleFlightStats()` — the same affordance `__pollGateStats()` and
 *  `__wakeGateStats()` give their own half of the poll policy. Stall the
 *  backend (or just watch a slow group) and `skipped` must climb while `ran`
 *  stops advancing past whatever is already in flight; unstall it and `ran`
 *  resumes climbing. */
export function singleFlightStats(): SingleFlightStats {
  return { ran: ranCount, skipped: skippedCount };
}

(globalThis as unknown as { __singleFlightStats?: () => SingleFlightStats }).__singleFlightStats =
  singleFlightStats;

/** Reset the module-level counters. Test-only — they are cumulative by design
 *  so a human reading `__singleFlightStats()` sees a whole session. */
export function resetSingleFlightStats(): void {
  ranCount = 0;
  skippedCount = 0;
}

/** Single-flights an async poll body for ONE poll site. A `run` call made
 *  while a previous call through the SAME instance has not yet settled is
 *  skipped — counted, never queued and never thrown — and resolves to
 *  `undefined` immediately; the call that started the outstanding flight is
 *  left running on its own and is what eventually clears `pending`. The next
 *  call made after that flight settles, whether it resolved or rejected,
 *  runs normally. */
export class SingleFlight {
  private inFlight = false;

  /** True while a call started through `run` has not yet settled. Exposed for
   *  tests and for a caller that wants to short-circuit (e.g. skip building
   *  the request) without paying the `run` call. */
  get pending(): boolean {
    return this.inFlight;
  }

  async run<T>(fn: () => Promise<T>): Promise<T | undefined> {
    if (this.inFlight) {
      skippedCount++;
      return undefined;
    }
    this.inFlight = true;
    ranCount++;
    try {
      return await fn();
    } finally {
      this.inFlight = false;
    }
  }
}
