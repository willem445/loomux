// Pure watch-display formatting for the group view's per-agent "⏳ waiting on …"
// indicator (issue #248): the countdown math and the one-line sentence, kept
// DOM-free so they're unit-testable under `node --test` (mirrors layout.ts /
// steer.ts / spawnexpiry.ts). groupview.ts is the only DOM caller — it feeds
// this the backend's `orch_group_watches` rows (via orchestration.ts's
// `groupWatches` wrapper) filtered to one agent.

/** The fields watchLine needs from a backend GroupWatch row (see orchestration.ts) — a
 *  minimal shape rather than the full interface, so this module has no dependency
 *  on orchestration.ts (avoids a cycle; also keeps the pure module importable with
 *  no Tauri types in scope). */
export interface WatchLike {
  target: string;
  expires_ms: number;
}

/** Compact countdown to `expiresMs` from `nowMs`: "N min" under an hour, "Hh Mm"
 *  (or bare "Hh" on the hour) beyond it. A watch already past its deadline —
 *  the backend hasn't ticked it away yet, or a clock skew — reads "expiring"
 *  rather than a confusing negative/zero duration. Sub-minute remainders round
 *  UP (`Math.ceil`) so "expires in 0 min" never appears for a watch that is
 *  still, technically, alive. */
export function formatExpiry(expiresMs: number, nowMs: number): string {
  const remainingMs = expiresMs - nowMs;
  if (remainingMs <= 0) return "expiring";
  const mins = Math.ceil(remainingMs / 60_000);
  if (mins < 60) return `${mins} min`;
  const hours = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem ? `${hours}h ${rem}m` : `${hours}h`;
}

/** The group-view's one-line "⏳ waiting on …" indicator for an agent's live
 *  watches. Shows the soonest-expiring watch (the one most likely to need
 *  attention first); additional watches collapse to a "+N more" suffix rather
 *  than stacking a line per watch — an agent is capped at 4 live watches
 *  (`MAX_WATCHES_PER_AGENT`), but the roster row is one line. Empty input →
 *  empty string, so the caller can skip rendering the line entirely rather
 *  than showing a blank one. */
export function watchLine(watches: WatchLike[], nowMs: number): string {
  if (watches.length === 0) return "";
  const soonest = [...watches].sort((a, b) => a.expires_ms - b.expires_ms)[0];
  const extra = watches.length - 1;
  const suffix = extra > 0 ? ` +${extra} more` : "";
  return `⏳ waiting on ${soonest.target} (expires in ${formatExpiry(soonest.expires_ms, nowMs)})${suffix}`;
}
