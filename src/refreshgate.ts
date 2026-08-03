// A tiny single-flight *coalescing* gate for a view's refresh loop (the issues
// view first; `CoalescingRefresh` at the bottom is the same gate with its
// async loop attached, for refreshes driven by an event stream).
// DOM-free and pure so its state machine is unit-tested in Node
// (test/refreshgate.test.ts) rather than by racing real async calls — the
// repo's extract-pure-logic convention (layout.ts, steer.ts, …).
//
// The bug it fixes (PR #136 review): IssuesView.refresh() is single-flight — a
// second call while one is in flight must not start a concurrent fetch. But
// simply *dropping* the second call loses the state change that prompted it —
// e.g. flipping Issues→PRs while the initial issue fetch is still running —
// stranding PR mode on its empty list until a manual ↻. The gate instead
// remembers that a call was dropped and tells the in-flight run to fire exactly
// one more refresh when it finishes, collapsing any number of dropped calls into
// a single trailing re-fetch. Because that trailing run reads the *current*
// mode, the switch always ends on fresh data for the new mode.
//
// Note this is orthogonal to the stale-response guard in refresh(): the gate
// guarantees a re-fetch happens; the mode check guarantees an old-mode response
// never renders into the new mode. Both are needed — the gate alone would still
// let a slow old-mode fetch paint stale data, and the mode check alone would
// leave the new mode with nothing to render.

export class RefreshGate {
  private running = false;
  private pending = false;

  /** Try to start a run. Returns true if the caller may proceed (the gate is now
   *  marked running); false if a run is already in flight — in which case the
   *  gate records that a trailing re-run is owed to whoever is running. */
  begin(): boolean {
    if (this.running) {
      this.pending = true;
      return false;
    }
    this.running = true;
    return true;
  }

  /** End the current run. Returns true iff a re-run is owed (at least one call
   *  was dropped while this run was in flight) — the caller should then invoke
   *  its refresh once more. Clears the pending flag so the trailing run fires
   *  exactly once no matter how many calls were coalesced. */
  end(): boolean {
    this.running = false;
    if (this.pending) {
      this.pending = false;
      return true;
    }
    return false;
  }

  /** Whether a run is currently in flight (for assertions/debugging). */
  get isRunning(): boolean {
    return this.running;
  }
}

/** The async loop around `RefreshGate`, for a refresh driven by an EVENT
 *  STREAM rather than by a human gesture.
 *
 *  `IssuesView`/`SessionBrowser`/`TimelineView` each hand-roll the
 *  begin/try/finally around their own `refresh()`, which is fine when the
 *  callers are a mode switch and a ↻ button. `orch-tasks-changed` is not that:
 *  the backend emits one per `write_tasks`, agents write in bursts, and every
 *  event was a full board refetch plus re-render in every open view (#743's
 *  census, part 2b). Wrapping the gate makes the bound the object rather than a
 *  pattern each view remembers, and makes it testable without a DOM
 *  (test/refreshgate.test.ts) — which a bound INV-3 relies on should be.
 *
 *  The guarantee, for a burst of N requests: **one in-flight run plus one
 *  trailing run**, and the trailing one reads current state, so no update is
 *  lost — the same loss-safety argument as the gate itself.
 *
 *  Read as a throttle, its window is the duration of a run rather than a
 *  constant: the stream can never cost more than back-to-back refreshes, and it
 *  self-clocks — a slower backend coalesces harder, which is the right way
 *  round. That is why it needs no number to tune, unlike `refreshthrottle.ts`,
 *  whose window bounds work that would otherwise be instantaneous. */
export class CoalescingRefresh {
  private readonly gate = new RefreshGate();
  private readonly run: () => Promise<void>;

  /** @param run one refresh. Must be safe to re-enter after it resolves; its
   *    own errors are its business (see `pump`).
   *
   *  (An assigned field rather than a TypeScript parameter property: the node
   *  test runner strips types without transforming, and refuses that syntax.) */
  constructor(run: () => Promise<void>) {
    this.run = run;
  }

  /** Ask for a refresh. Returns immediately; at most one run is in flight. */
  request(): void {
    void this.pump();
  }

  private async pump(): Promise<void> {
    if (!this.gate.begin()) return;
    try {
      await this.run();
    } catch (err) {
      // A rejected run must not wedge the gate — `finally` below releases it
      // either way, and swallowing here (loudly) keeps `request()`'s void
      // contract from producing an unhandled rejection. Views already toast
      // their own failures; this is the backstop, not the reporting path.
      console.error("[loomux] coalesced refresh failed", err);
    } finally {
      if (this.gate.end()) this.request();
    }
  }

  /** Whether a run is in flight (for assertions/tests). */
  get isRunning(): boolean {
    return this.gate.isRunning;
  }
}
