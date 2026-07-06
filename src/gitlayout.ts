// Pure geometry + persistence-value helpers for the git view's resizable
// sub-panes. No DOM and no localStorage here — just the clamp/distribution
// math, so it can be unit-tested in isolation (see test/gitlayout.test.ts).
//
// The git view floats OVER the terminal precisely so toggling it never resizes
// the PTY (a ConPTY resize makes TUIs repaint into scrollback). These helpers
// only redistribute space *within* the overlay: the divider moves the boundary
// between two neighboring panes, the overlay's outer bounds never change.

/** Smallest on-axis size (px) a pane may shrink to before its neighbor stops
 *  giving way. Sized so headers and a row or two stay legible. */
export const GRAPH_MIN = 200;
export const DIFF_MIN = 220;
export const CHANGES_MIN = 96;
export const TOP_MIN = 140;

/** Divider thickness (px). Must match `.git-divider` in styles.css: the drag
 *  math subtracts it from the container so the two panes share the remainder. */
export const DIVIDER_PX = 5;

/** Sizes used before the user has ever dragged (and when a stored value is
 *  unusable). Chosen to match the git view's original hardcoded tracks. */
export const DEFAULT_GRAPH_W = 300;
export const DEFAULT_CHANGES_H = 172;

/** localStorage keys — one per divider, following loomux's `loomux.*`
 *  convention (see editor.ts / agents.ts). */
export const KEY_GRAPH_W = "loomux.gitview.graphW";
export const KEY_CHANGES_H = "loomux.gitview.changesH";

export interface ClampSpec {
  /** Space (px) shared by the sized pane and its neighbor: the container's
   *  on-axis size minus the divider thickness. */
  total: number;
  /** Smallest the sized pane may become. */
  min: number;
  /** Smallest its neighbor may become (caps the sized pane at total - otherMin). */
  otherMin: number;
}

/** Clamp `proposed` (a pane's on-axis px size) so that neither the pane nor its
 *  neighbor drops below its minimum.
 *
 *  When the container is too small to honor both minimums at once, the
 *  neighbor's minimum wins and the sized pane gets whatever is left (>= 0).
 *  This keeps the diff / top pane usable when the overlay itself is dragged
 *  very small. Non-finite inputs collapse to the pane's own minimum. */
export function clampPaneSize(proposed: number, spec: ClampSpec): number {
  const { total, min, otherMin } = spec;
  if (!Number.isFinite(total)) return min;
  const hi = total - otherMin;
  if (hi < min) return Math.max(0, hi); // container can't fit both minimums
  if (!Number.isFinite(proposed)) return min;
  return Math.min(hi, Math.max(min, proposed));
}

/** Parse a persisted divider size. Returns null for a missing/garbage/negative
 *  value so the caller can fall back to a default. */
export function parseStoredSize(raw: string | null | undefined): number | null {
  if (raw == null || raw.trim() === "") return null; // Number("") is 0 — reject
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? n : null;
}

/** Pick the size to apply for a pane on (re)layout: the stored value if usable,
 *  otherwise the default — then clamped to fit the current container. Used both
 *  on first render and whenever the overlay is resized around the panes. */
export function resolvePaneSize(
  rawStored: string | null | undefined,
  fallback: number,
  spec: ClampSpec
): number {
  const wanted = parseStoredSize(rawStored) ?? fallback;
  return clampPaneSize(wanted, spec);
}
