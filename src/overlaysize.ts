// Pure clamp for the floating orchestration-overlay splitter height (the git /
// tasks / audit / group panels that hover over an orchestrator pane's terminal).
// No DOM imports, so it's unit-testable under `node --test` (mirrors
// spawnexpiry.ts / layout.ts). pane.ts's `overlayClamp` calls this.
//
// The overlay FLOATS over the terminal and never resizes the PTY (hard
// constraint 1). The clamp enforces two invariants at EVERY splitter position,
// which together fix the resize glitch in #83 finding 3 (panel content painting
// over the terminal, and the drag bar collapsing out of reach):
//   - a minimum overlay height (`OVERLAY_MIN_H`) so a drag can't shrink the
//     panel below the point where its bottom drag bar is still a grabbable strip
//     inside the pane, and
//   - a reserved terminal strip (`TERM_RESERVE_H`) at the bottom so the bar and
//     some live terminal always stay on-screen.
// (Content no longer *overflows* past the box — that's `.group-view { overflow:
// hidden }` in the CSS — but the floor keeps the panel usable rather than
// clipped to nothing, and keeps the bar reachable regardless of chrome height.)

/** Smallest the overlay may collapse to. Chosen so the panel's fixed chrome
 *  (header + a control row or two) plus the 5px drag bar remain on-screen; the
 *  bar is a flex sibling pinned to the overlay's bottom edge, so at this height
 *  it's always a grabbable strip inside the pane. Raised from the historical
 *  160 because the autonomous-mode controls (#83) made the fixed chrome taller. */
export const OVERLAY_MIN_H = 180;

/** Terminal height always left visible below the overlay, so the drag bar can
 *  never sit at or past the pane's bottom edge (where it'd be unreachable) and
 *  the terminal is never fully covered. */
export const TERM_RESERVE_H = 100;

/** Clamp a requested overlay height (from a splitter drag or an open-size calc)
 *  to `[minH, max]`, where `max = max(minH, termHeight - reserveH)` reserves a
 *  terminal strip. `minH` wins when the pane is too short to honor the reserve,
 *  so the result is never below `minH`. A non-finite request falls back to
 *  `minH`. Mirrors the old inline math but with the raised floor and guards. */
export function clampOverlayHeight(
  requestedH: number,
  termHeight: number,
  minH: number = OVERLAY_MIN_H,
  reserveH: number = TERM_RESERVE_H
): number {
  const max = Math.max(minH, termHeight - reserveH);
  const req = Number.isFinite(requestedH) ? requestedH : minH;
  return Math.max(minH, Math.min(max, req));
}
