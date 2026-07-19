// Pure, DOM-free geometry for the pane's embed-panel dividers (#361): up to
// three views — one each on the left, right, and bottom edges of the
// terminal — rendered as real flex siblings instead of floating over it. No
// DOM imports, so it's unit-testable under `node --test` (mirrors layout.ts
// / overlaysize.ts).
//
// This is deliberately modeled on grid.ts's own split-divider math (the
// inline mousemove handler in Pane's private `makeDivider`): before/after
// sizes and flex-grow weights, a pixel delta clamped so neither side can be
// dragged below its floor, then redistributed proportionally so the pair's
// total flex-grow is preserved. Reusing that exact shape is the point — an
// embedded panel resizes the terminal exactly the way a grid split already
// does (see doc/design/embedded-panels.md for why that's the legitimate
// side of the PTY-resize line), so each of the three dividers should feel
// identical to a grid split's, not invent a second discipline.
//
// THREE dividers, ONE function. `embedDragGrow` only ever reasons about a
// generic "before"/"after" pair — which pair it's given (left-slot | term,
// term | right-slot, or the row-as-a-whole | bottom-slot) is a `pane.ts`
// wiring decision, not something this module needs to know. That is also
// why there is no "compose the floor across nested regions" function here:
// with a FLAT 5-child row (left-slot, left-divider, term, right-divider,
// right-slot) each divider only ever touches its own two immediate
// neighbors — exactly grid.ts's own N-way-split convention (a divider
// trades space with its immediate before/after only, never reaching past
// them) — so no divider's clamp needs to know whether the OTHER side is
// even occupied.

// Deliberately no cross-module import (every other pure, node:test-covered
// module in this codebase — layout.ts, overlaysize.ts, spawnexpiry.ts,
// taskboard.ts — is self-contained): tsc's build forbids the explicit `.ts`
// extension an intra-src import would need for `node --test` to resolve it
// directly (TS5097), so a bare specifier here would work for one runner and
// not the other. `EMBED_MIN_TERM_PX`/`EMBED_MIN_PANEL_PX` below intentionally
// mirror overlaysize.ts's `TERM_RESERVE_H`/`OVERLAY_MIN_H` — same reasoning
// (a visible terminal strip; the panel's own header/list/footer chrome
// doesn't clip) — duplicated rather than shared.

/** Default share of the split a newly-embedded panel opens at (~third of the
 *  pane), before any drag or persisted size is applied. */
export const DEFAULT_EMBED_FRAC = 1 / 3;

/** The terminal's own floor, in px — mirrors overlaysize.ts's
 *  `TERM_RESERVE_H`. Used as BOTH the terminal's minimum WIDTH (the left/
 *  right dividers' term-side floor) and its minimum HEIGHT (the bottom
 *  divider's row-side floor): one constant, two axes, the same "keep a
 *  usable strip visible" reasoning either way. */
export const EMBED_MIN_TERM_PX = 100;

/** The generic panel-side floor, in px — mirrors overlaysize.ts's
 *  `OVERLAY_MIN_H`. Used for EVERY panel's minimum HEIGHT when docked to the
 *  bottom (a view can override this per its own `floorPx()` — the group
 *  panel's measured chrome is the one that does), and for EVERY panel's
 *  minimum WIDTH when docked to the left or right, full stop, never
 *  overridden. That second half is a deliberate v1 simplification: a view's
 *  `floorPx()` measures how much VERTICAL chrome it needs (header, list,
 *  footer stacked) — a concept that doesn't transfer to "how narrow can this
 *  same vertical stack go," so left/right don't route through it at all. */
export const EMBED_MIN_PANEL_PX = 180;

/** Bounds on the persisted/requested fraction itself — independent of the
 *  pixel floors below, which are what actually bite during a drag on a small
 *  pane. Keeps a stray persisted value (a hand-edited tabs.json, a future unit
 *  mismatch) from ever requesting a degenerate split. */
const MIN_FRAC = 0.1;
const MAX_FRAC = 0.85;

export function clampEmbedFrac(frac: number): number {
  if (!Number.isFinite(frac)) return DEFAULT_EMBED_FRAC;
  return Math.max(MIN_FRAC, Math.min(MAX_FRAC, frac));
}

/** The pair of flex-grow weights the two sides of one divider each carry.
 *  Only their RATIO matters (CSS `flex-grow`), never their absolute scale,
 *  which is why `growFromFrac` can pick any convenient total. Named
 *  before/after (not e.g. term/panel): which side is literally the terminal
 *  differs per divider — see the module doc comment. */
export interface EmbedGrow {
  growBefore: number;
  growAfter: number;
}

/** Turn a persisted/default fraction (the AFTER side's share) into a
 *  starting grow pair. */
export function growFromFrac(frac: number): EmbedGrow {
  const after = clampEmbedFrac(frac);
  return { growBefore: 1 - after, growAfter: after };
}

/** The inverse: recover the after side's fraction of the split from its live
 *  flex-grow pair, e.g. to persist the size a drag settled on. */
export function fracFromGrow(growBefore: number, growAfter: number): number {
  const total = growBefore + growAfter;
  if (!(total > 0)) return DEFAULT_EMBED_FRAC;
  return clampEmbedFrac(growAfter / total);
}

/** Apply one divider-drag delta (in px, positive = dragged toward the AFTER
 *  side, growing BEFORE) to a live before/after size + grow pair, returning
 *  the new grow pair. `minBeforePx`/`minAfterPx` are the floors neither side
 *  may be dragged past. Mirrors grid.ts's split-divider math exactly. */
export function embedDragGrow(
  sizeBefore: number,
  sizeAfter: number,
  growBefore: number,
  growAfter: number,
  deltaPx: number,
  minBeforePx: number = EMBED_MIN_TERM_PX,
  minAfterPx: number = EMBED_MIN_PANEL_PX
): EmbedGrow {
  const total = sizeBefore + sizeAfter;
  const growTotal = growBefore + growAfter;
  if (!(total > 0) || !(growTotal > 0)) return { growBefore, growAfter };
  const delta = Number.isFinite(deltaPx) ? deltaPx : 0;
  const clampedDelta = Math.max(minBeforePx - sizeBefore, Math.min(sizeAfter - minAfterPx, delta));
  // When a floor exceeds the pair's own total (a pane too small to honor
  // every active floor at once — the same accepted degradation this
  // module's own doc comment already names), `clampedDelta` alone can still
  // push the raw ratio past [0, growTotal] — a NEGATIVE flex-grow, which CSS
  // silently drops rather than rejecting (#361 rev-58 NB1). Clamp the
  // OUTPUT, not just the delta: a too-small pane still degrades (one side
  // ends up below its floor — unavoidable, per the doc comment above), but
  // never past all-the-grow-to-one-side.
  const rawGrowBefore = ((sizeBefore + clampedDelta) / total) * growTotal;
  const newGrowBefore = Math.max(0, Math.min(growTotal, rawGrowBefore));
  return { growBefore: newGrowBefore, growAfter: growTotal - newGrowBefore };
}

/** Which edge of the terminal a panel can be docked to (#361 generalization
 *  from the single bottom-only slot). No `"top"` — the header already owns
 *  that edge, and nothing has asked for it. */
export type EmbedSide = "left" | "right" | "bottom";

export const EMBED_SIDES: readonly EmbedSide[] = ["left", "right", "bottom"];

/** The `embedDragGrow` floor pair for `side`'s own divider, given the
 *  occupying view's own panel floor (`panelFloorPx` — a fixed constant for
 *  left/right, per the `EMBED_MIN_PANEL_PX` doc above; a view's own live
 *  `floorPx()` for bottom, unchanged from the pre-multi-slot single-bottom
 *  design). Encodes PRECEDENCE, not just plumbing: the terminal's own floor
 *  (`EMBED_MIN_TERM_PX`) is ALWAYS one side of the pair — right guards the
 *  terminal's WIDTH directly, bottom guards the ROW's HEIGHT (which is the
 *  terminal's own height, since left/right sit in the same row at the same
 *  height) — so no divider's clamp can ever push the terminal below its own
 *  floor. This is a PER-DIVIDER guarantee, not a global solver: if the pane
 *  itself is smaller than the sum of every currently-active floor, some
 *  region necessarily ends up smaller than its stated floor from pure
 *  arithmetic — the same accepted degradation `overlayClamp`
 *  (`overlaysize.ts`) already documents for the single-overlay case ("min
 *  wins when the pane is too short to honor the reserve"). A drag can never
 *  make that WORSE; it just can't retroactively fix an already-too-small
 *  pane.
 *
 *  Only `"right"` and `"bottom"` are valid here: both dock a panel DIRECTLY
 *  beside `termEl` (or the row, for bottom), a plain two-element pair. Left
 *  is NOT — the pane's DOM nests `embedCenterEl` (containing `termEl` and
 *  the optional right slot) as ONE element inside `embedRowEl` alongside the
 *  left slot (see `Pane.ensureEmbedHost`'s doc comment for why: a nested
 *  tree, not a flat 5-child row, keeps every OTHER divider's pair a real,
 *  single DOM element too). The left divider's far side is that composite
 *  `embedCenterEl`, whose own floor has to account for whatever's nested
 *  inside it — see `embedCenterFloor` below, which is what the left
 *  divider's clamp actually uses. */
export function embedSideFloors(
  side: "right" | "bottom",
  panelFloorPx: number
): { beforeFloorPx: number; afterFloorPx: number } {
  switch (side) {
    case "right":
      return { beforeFloorPx: EMBED_MIN_TERM_PX, afterFloorPx: panelFloorPx };
    case "bottom":
      return { beforeFloorPx: EMBED_MIN_TERM_PX, afterFloorPx: panelFloorPx };
  }
}

/** Divider thickness in px — matches `.pane-embed-divider` in styles.css.
 *  Needed only for composing `embedCenterFloor` below: the left divider's
 *  far side (`embedCenterEl`) must be big enough for the terminal's own
 *  floor AND, if the right slot is occupied, its floor AND its own divider's
 *  width too — the one place this module's floor math has to reach one
 *  level past a divider's own immediate pair. */
export const EMBED_DIVIDER_PX = 6;

/** The minimum size `embedCenterEl` (the left divider's composite far side —
 *  `termEl` plus, if occupied, the right divider and right slot) may be
 *  clamped to. `rightPanelFloorPx` is the right slot's own floor when it's
 *  currently occupied, or `null` when it isn't — passing `null` collapses
 *  this to plain `EMBED_MIN_TERM_PX`, i.e. exactly what the left divider's
 *  far-side floor would be if there were nothing nested inside it at all. */
export function embedCenterFloor(rightPanelFloorPx: number | null): number {
  return rightPanelFloorPx === null
    ? EMBED_MIN_TERM_PX
    : EMBED_MIN_TERM_PX + rightPanelFloorPx + EMBED_DIVIDER_PX;
}
