// Pure windowing decision for the audit log's rendered row count (#361
// user-demo finding): audit.jsonl is append-only and can grow into the
// thousands over a long-running session (2735 in the reported case) — one
// live DOM node per entry made the pane genuinely slow to reflow on every
// divider-drag frame once docked, since the container's cross-axis size
// changes on every mousemove and the browser has to re-lay-out every row to
// fit it. Rather than ever holding all of them in the DOM, only the newest
// `windowSize` MATCHING (post-filter) entries render by default — scrolling
// to the top of the list backfills further back, a step at a time.
//
// DOM-free and tested under `node --test` (mirrors embedsplit.ts /
// overlaysize.ts); auditview.ts owns the DOM wiring (the scroll listener,
// the actual slice+render, the scroll-position preservation on backfill).

/** Default rendered window size, and the backfill step when scrolling up
 *  reveals more. Same constant for both: one step's worth is exactly what
 *  the window shows on a fresh open, so backfilling once roughly doubles
 *  what's rendered — plenty of headroom before the human has to scroll up
 *  again. */
export const AUDIT_WINDOW_SIZE = 300;
export const AUDIT_WINDOW_STEP = 300;

/** Where the rendered window should start (an index into the CURRENT
 *  filtered array — everything from this index to the end renders) given
 *  the previous window and what changed this render:
 *  - A FILTER change invalidates the old index outright (it indexed a
 *    different array) — reset to the newest `windowSize`, same as a fresh
 *    open.
 *  - `windowStart` past the end (the filtered set shrank — a filter now
 *    excludes what used to be at that index) — same reset, defensively.
 *  - Otherwise, new entries simply arrived (a follow poll). If the human is
 *    AT THE TAIL (`nearBottom`), advance the window to stay capped at
 *    `windowSize` — old entries fall off the front as new ones arrive at the
 *    back, so a long follow session never re-grows past the cap. If they've
 *    scrolled up to read history, leave the window exactly where it is —
 *    new entries simply exist beyond what's rendered until they scroll back
 *    down, rather than yanking their place in the backlog out from under
 *    them. */
export function nextWindowStart(
  filteredLength: number,
  windowStart: number,
  filterChanged: boolean,
  nearBottom: boolean,
  windowSize: number = AUDIT_WINDOW_SIZE
): number {
  if (filterChanged || windowStart > filteredLength) {
    return Math.max(0, filteredLength - windowSize);
  }
  if (nearBottom) {
    return Math.max(windowStart, filteredLength - windowSize);
  }
  return windowStart;
}

/** Scrolling to the top asks for `step` more of the backlog — floored at 0
 *  (the very start of the filtered set), never negative. */
export function backfillWindowStart(windowStart: number, step: number = AUDIT_WINDOW_STEP): number {
  return Math.max(0, windowStart - step);
}
