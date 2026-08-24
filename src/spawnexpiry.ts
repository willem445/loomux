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

/** Agent ids from a `cancelledSpawns` map (agentId -> groupId, orchestration.ts)
 *  that belong to `groupId` (#1316). `cancelledSpawns`' only delete is in
 *  `openAgentPane`'s `finally` — a cancel for a request already dropped as
 *  expired (`isSpawnRequestExpired` above) never reaches `openAgentPane`, so
 *  the id is stranded there forever. `orch-group-ended` sweeps every entry for
 *  its group as a backstop: by the time a group has ended, any spawn it was
 *  still waiting to bind is moot regardless of which race stranded it. */
export function spawnsForGroup(entries: Iterable<readonly [string, string]>, groupId: string): string[] {
  const out: string[] = [];
  for (const [agentId, gid] of entries) if (gid === groupId) out.push(agentId);
  return out;
}
