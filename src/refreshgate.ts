// A tiny single-flight *coalescing* gate for the issues view's refresh loop.
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
