// Pure spawn-request expiry decision (issue #106) — no Tauri or DOM imports, so
// it's unit-testable under `node --test` (mirrors orchbadge.ts / attention.ts).
// orchestration.ts (which listens for the backend event and opens panes) imports
// from here.
//
// The bug this guards: the spawn round-trip (MCP spawn_agent → orch-spawn-request
// → frontend opens pane → bind_agent, 20s backend timeout) had no cancellation
// path. A frontend stalled past the timeout would, on recovery, still service the
// queued request — opening a zombie pane whose CLI boots against a config the
// bind-timeout already cleaned up. The backend now stamps each request with the
// deadline of its own bind wait; the frontend drops any request already past it.

/** Whether a queued spawn request stamped with `deadlineMs` has expired by
 *  `nowMs` and must be dropped unserviced. A `deadlineMs` of 0 (or missing, from
 *  a legacy payload) means "unstamped" and never expires — so an older backend
 *  degrades to the previous behaviour rather than dropping every request. Mirrors
 *  the backend `spawn_request_expired` so both sides agree on one rule. */
export function isSpawnRequestExpired(deadlineMs: number, nowMs: number): boolean {
  return deadlineMs !== 0 && nowMs > deadlineMs;
}
