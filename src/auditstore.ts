// The pane's one audit-log read (#1317).
//
// `orch_audit` answers up to `AUDIT_VIEW_LIMIT` (5000) entries, and audit
// `detail` is where the app's biggest strings live — a `prompt` detail carries
// the whole delivered prompt. TWO views read it: the audit viewer and the
// progress timeline. Each fetched independently, each on its own 1.5 s follow
// tick, and each held its own full copy of the result — so a pane with both
// docked paid two IPC round trips, two JSON parses and two 5000-row arrays for
// one file, forever, and the pair could be showing the same group's log from
// two different reads a fraction of a second apart.
//
// This module is the single owner of that read, on `SessionStore`'s pattern
// (#493) and for its reason: the answer a second caller needs is the one
// already being computed. It differs from `SessionStore` in having a
// FRESHNESS WINDOW as well as single-flight — two views following the same log
// at the same cadence rarely have their ticks land inside one another, so
// joining an in-flight run is not enough to collapse them; `maxAgeMs` is what
// makes the second view's tick share the first view's read rather than start
// its own a beat later. That is `USAGE_POLL_MAX_AGE`'s shape (#743 S4b),
// applied on the frontend because this read has no backend memo.
//
// DOM-free and dependency-free (the fetch is injected, and the only import is
// a type, which Node's strip-only TypeScript erases), so the concurrency and
// freshness contracts are unit-tested in Node — test/auditstore.test.ts — the
// repo's extract-pure-logic convention.
//
// **Retention (INV-8a).** This holds ONE array, replaced wholesale, bounded by
// the backend's own `AUDIT_VIEW_LIMIT`. It is not a collection keyed by
// anything and nothing accumulates in it. Its release site is its owner's:
// `Pane` holds one per pane alongside the two views that read it, and it goes
// when the pane does. There is deliberately no module-level per-group map —
// that would need a release rule, and two panes on one group double-reading is
// what the app already did.

import type { AuditEntry } from "./auditsummary";

/** How stale a shared read may be before a caller's request re-fetches.
 *
 *  Both readers follow at `FOLLOW_MS` (1500 ms), so this has to be close to
 *  that to collapse their ticks — but strictly under it, or a view's own tick
 *  would be served its own previous read and the log would advance at half the
 *  cadence it advertises. 1200 ms leaves 300 ms of slack for the two views'
 *  timers to drift apart while still making the second one free. */
export const AUDIT_READ_MAX_AGE_MS = 1200;

export class AuditStore {
  private rows: AuditEntry[] = [];
  private loadedOnce = false;
  /** When `rows` was last successfully replaced. Never advanced by a failed
   *  read: "I could not look" is not "there is nothing new". */
  private stampMs = 0;
  /** The run every concurrent caller joins instead of starting its own. */
  private inflight: Promise<void> | null = null;
  private fetchRows: () => Promise<AuditEntry[]>;
  private now: () => number;

  // Assigned in the body, not as constructor parameter properties: Node's
  // strip-only TypeScript mode (what `npm test` runs this under) rejects them.
  constructor(fetchRows: () => Promise<AuditEntry[]>, now: () => number = Date.now) {
    this.fetchRows = fetchRows;
    this.now = now;
  }

  /** The last-fetched rows, without triggering a read. Empty before the first
   *  successful one.
   *
   *  Handed out by reference on purpose — this is the de-duplication. Both
   *  views render from it and neither copies it; the array is replaced
   *  wholesale on each successful read, so a holder of the previous one sees a
   *  consistent snapshot rather than a list mutating under it. `readonly` says
   *  the rest: a reader that sorted or spliced this in place would be editing
   *  the other view's data. */
  get cached(): readonly AuditEntry[] {
    return this.rows;
  }

  /** Whether a read has ever succeeded. False after a failed one — a rejected
   *  read must never be remembered as "we have the log". */
  get loaded(): boolean {
    return this.loadedOnce;
  }

  /** The rows, re-reading unless a recent-enough read can answer.
   *
   *  Three cases, in order:
   *  - a successful read younger than `maxAgeMs` → serve it, no IPC. This is
   *    the case that makes a second view's follow tick free.
   *  - a read in flight → JOIN it rather than start a concurrent second one
   *    (`SessionStore`'s property).
   *  - otherwise → read.
   *
   *  `maxAgeMs` of 0 disables the window entirely, which is what an explicit
   *  gesture (the ⟳ button, opening the panel) passes: a human asking for a
   *  refresh must never be served a cached answer.
   *
   *  **A failed read keeps the previous rows and does NOT rethrow.** Both
   *  callers already render an unreadable log as an empty/unchanged chart
   *  rather than a broken one, and the shared store must not turn one view's
   *  transient failure into the other's. It leaves `loaded` and the stamp
   *  where they were, so the next request retries rather than latching. */
  async read(maxAgeMs: number = AUDIT_READ_MAX_AGE_MS): Promise<readonly AuditEntry[]> {
    if (this.loadedOnce && this.now() - this.stampMs < maxAgeMs) return this.rows;
    if (this.inflight) {
      await this.inflight;
      return this.rows;
    }
    const run = this.fetchRows().then(
      (rows) => {
        this.rows = rows;
        this.loadedOnce = true;
        this.stampMs = this.now();
      },
      () => {
        /* keep the last good rows — see the doc above */
      }
    );
    this.inflight = run;
    try {
      await run;
    } finally {
      // Only clear if it is still ours; nothing else can have replaced it
      // while it was in flight, but a future caller might.
      if (this.inflight === run) this.inflight = null;
    }
    return this.rows;
  }
}
