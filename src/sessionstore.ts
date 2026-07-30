// The app's one session list (#493).
//
// `listSessions()` is a real disk scan of every recorded Claude/Copilot session
// on the machine (see `src-tauri/src/sessions.rs`). #493 measured it running
// TWICE, concurrently, on one launch — 826 files, 12.9s and 16.7s, contending
// with each other. The breadcrumb log names both callers:
//
//   20260730-115247 startup v1.0.0            <- boot
//   20260730-115305 list_sessions: … 12.923s  <- the group-restore click's scan
//   20260730-115305 list_sessions: … 16.699s  <- the boot prefetch's scan
//   20260730-115305 agent-spawn group=… resume=true
//
// i.e. the sidebar's background prefetch started at boot, then ~4s later a
// "restore this group" click ran `listSessions()` AGAIN for its own resumability
// check — and the group restore then sat behind that second scan (the spawn
// breadcrumb lands the moment it finishes). That is the second, unfixed half of
// the restore lag #479 reported: the answer the click needed was already being
// computed, seconds from landing, and asking for it again made BOTH copies
// slower.
//
// This module is the single owner of that scan. DOM-free and dependency-free
// (the fetch is injected, and it imports no sibling module — every node-tested
// module in this repo is a leaf, because `node --test` runs them unbundled and
// src/ imports are extensionless), so its concurrency contract is unit-tested in
// Node (test/sessionstore.test.ts) rather than by racing real IPC — the repo's
// extract-pure-logic convention (layout.ts, steer.ts, refreshgate.ts, …).
//
// Two accessors, deliberately different:
//
//   refresh()      — "I need a current read": the ↻ button, opening the sidebar,
//                    the #440 reconciler looking for transcripts that appeared
//                    since boot.
//   ensureLoaded() — "I need the list, whatever the newest read said": the
//                    group-restore resumability check.
//
// Neither can start a scan while one is in flight — that is the whole point.
// What this module deliberately does NOT own is the loss-safe COALESCING of
// dropped refresh calls (a call arriving mid-scan owing exactly one trailing
// re-run, rev-9 review): that stays in `SessionBrowser`'s `RefreshGate`, which
// also has to cover the `loadRoles()` half of its refresh and the render. Two
// copies of one state machine would be worse than the seam.

import type { SessionInfo } from "./pty";

export class SessionStore {
  private rows: SessionInfo[] = [];
  private loadedOnce = false;
  /** The run every concurrent caller joins instead of starting its own. */
  private inflight: Promise<void> | null = null;
  private fetchRows: () => Promise<SessionInfo[]>;

  // Assigned in the body, not as a constructor parameter property: Node's
  // strip-only TypeScript mode (what `npm test` runs these modules under)
  // rejects parameter properties outright.
  constructor(fetchRows: () => Promise<SessionInfo[]>) {
    this.fetchRows = fetchRows;
  }

  /** The last-fetched list, without triggering a scan (#440). Empty before the
   *  first successful load. */
  get cached(): readonly SessionInfo[] {
    return this.rows;
  }

  /** Whether a load has ever succeeded. False after a failed one — a rejected
   *  scan must never be remembered as "we have the list". */
  get loaded(): boolean {
    return this.loadedOnce;
  }

  /** Read the list from disk. Single-flight: a call arriving while a scan is in
   *  flight JOINS that scan rather than starting a second concurrent one — the
   *  #493 property. A call arriving after one finished does scan again, which is
   *  what makes this the freshness-carrying accessor. */
  async refresh(): Promise<readonly SessionInfo[]> {
    if (this.inflight) {
      await this.inflight;
      return this.rows;
    }
    const run = this.fetchRows().then((rows) => {
      this.rows = rows;
      this.loadedOnce = true;
    });
    this.inflight = run;
    try {
      await run;
    } finally {
      // Only clear if it's still ours: a rejected run is cleared here, and
      // nothing else can have replaced it while it was in flight.
      if (this.inflight === run) this.inflight = null;
    }
    return this.rows;
  }

  /** The list, scanning only if no scan can answer it (#493). Returns the
   *  cached rows if a load has already succeeded, joins the run in flight if one
   *  is (the boot prefetch, typically), and only otherwise starts one.
   *
   *  Rejects if the scan it depended on rejected, instead of recording a failure
   *  as a successful empty load. That is NOT "the caller can tell empty from
   *  error" — main.ts's `seenAny` guard treats both alike ("assume resumable"),
   *  and #493 left that resume-path semantic alone. It is that one transient
   *  failure must not leave this store answering "empty, already loaded" for the
   *  rest of the session, which would strand the sidebar and silently turn the
   *  resumability check into a no-op with nothing to retrigger it. */
  async ensureLoaded(): Promise<readonly SessionInfo[]> {
    if (this.loadedOnce) return this.rows;
    if (this.inflight) {
      await this.inflight;
      return this.rows;
    }
    return this.refresh();
  }
}
