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
// This module is the single owner of that scan. DOM-free and pure (the fetch is
// injected), so its concurrency contract is unit-tested in Node
// (test/sessionstore.test.ts) rather than by racing real IPC — the repo's
// extract-pure-logic convention (layout.ts, steer.ts, refreshgate.ts, …).
//
// Two accessors, deliberately different:
//
//   refresh()      — "I need current data": the ↻ button, opening the sidebar,
//                    the #440 reconciler looking for transcripts that appeared
//                    since boot. Coalescing single-flight, unchanged from what
//                    SessionBrowser did before this module existed.
//   ensureLoaded() — "I need the list, whatever the newest read said": the
//                    group-restore resumability check. Never starts a scan that
//                    a completed or in-flight one can answer.

import type { SessionInfo } from "./pty";
import { RefreshGate } from "./refreshgate";

export class SessionStore {
  private rows: SessionInfo[] = [];
  private loadedOnce = false;
  /** The run every concurrent caller joins instead of starting its own. */
  private inflight: Promise<void> | null = null;
  private gate = new RefreshGate();

  constructor(private fetchRows: () => Promise<SessionInfo[]>) {}

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

  /** Fetch fresh rows. Single-flight and loss-safe (the pre-#493 SessionBrowser
   *  contract, moved here intact): a call arriving while one is in flight is
   *  coalesced into ONE trailing re-run rather than a second concurrent scan, so
   *  any number of dropped calls still end in exactly one fresh fetch. The
   *  coalesced caller resolves off the run it joined — the trailing run updates
   *  the cache behind it. */
  async refresh(): Promise<readonly SessionInfo[]> {
    if (!this.gate.begin()) {
      if (this.inflight) await this.inflight;
      return this.rows;
    }
    try {
      this.inflight = this.fetchRows().then((rows) => {
        this.rows = rows;
        this.loadedOnce = true;
      });
      await this.inflight;
    } finally {
      this.inflight = null;
      if (this.gate.end()) {
        void this.refresh().catch(() => {
          // Trailing re-run: nobody awaits it, so its failure is the same
          // best-effort no-op a failed prefetch is — the next explicit refresh
          // (or the caller's own catch) still reports for itself.
        });
      }
    }
    return this.rows;
  }

  /** The list, scanning only if no scan can answer it (#493). Returns the
   *  cached rows if a load has already succeeded, joins the run in flight if one
   *  is (the boot prefetch, typically), and only otherwise starts one.
   *
   *  Rejects if the scan it depended on rejected — callers that treat "no list"
   *  as "assume resumable" (main.ts's group restore) need to be able to tell an
   *  empty answer from a failed one, exactly as they could when they called
   *  `listSessions()` directly. */
  async ensureLoaded(): Promise<readonly SessionInfo[]> {
    if (this.loadedOnce) return this.rows;
    if (this.inflight) {
      await this.inflight;
      return this.rows;
    }
    return this.refresh();
  }
}
