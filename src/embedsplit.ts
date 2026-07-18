// Pure, DOM-free geometry for the embedded-panel divider inside a pane (#361):
// the task board (or a future orchestration view) rendered beside the terminal
// as a real flex sibling, instead of floating over it. No DOM imports, so it's
// unit-testable under `node --test` (mirrors layout.ts / overlaysize.ts).
//
// This is deliberately modeled on grid.ts's own split-divider math (the inline
// mousemove handler in Pane's private `makeDivider`): before/after sizes and
// flex-grow weights, a pixel delta clamped so neither side can be dragged below
// its floor, then redistributed proportionally so the pair's total flex-grow is
// preserved. Reusing that exact shape is the point — an embedded panel resizes
// the terminal exactly the way a grid split already does (see
// doc/design/embedded-panels.md for why that's the legitimate side of the
// PTY-resize line), so its divider drag should feel identical and hit the same
// floors, not invent a second discipline.

// Deliberately no cross-module import (every other pure, node:test-covered
// module in this codebase — layout.ts, overlaysize.ts, spawnexpiry.ts,
// taskboard.ts — is self-contained): tsc's build forbids the explicit `.ts`
// extension an intra-src import would need for `node --test` to resolve it
// directly (TS5097), so a bare specifier here would work for one runner and
// not the other. `EMBED_MIN_TERM_PX`/`EMBED_MIN_PANEL_PX` below intentionally
// mirror overlaysize.ts's `TERM_RESERVE_H`/`OVERLAY_MIN_H` — same reasoning
// (a visible terminal strip; the panel's own header/list/footer chrome
// doesn't clip) — duplicated rather than shared.

/** Default share of the split the panel opens at (~third of the pane), before
 *  any drag or persisted size is applied. */
export const DEFAULT_EMBED_FRAC = 1 / 3;

/** Terminal-side floor, in px — mirrors overlaysize.ts's `TERM_RESERVE_H`. */
export const EMBED_MIN_TERM_PX = 100;

/** Panel-side floor, in px — mirrors overlaysize.ts's `OVERLAY_MIN_H`. */
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

/** The pair of flex-grow weights the terminal and the embedded panel each
 *  carry. Only their RATIO matters (CSS `flex-grow`), never their absolute
 *  scale, which is why `growFromFrac` can pick any convenient total. */
export interface EmbedGrow {
  growTerm: number;
  growPanel: number;
}

/** Turn a persisted/default fraction into a starting grow pair. */
export function growFromFrac(frac: number): EmbedGrow {
  const panel = clampEmbedFrac(frac);
  return { growTerm: 1 - panel, growPanel: panel };
}

/** The inverse: recover the panel's fraction of the split from its live
 *  flex-grow pair, e.g. to persist the size a drag settled on. */
export function fracFromGrow(growTerm: number, growPanel: number): number {
  const total = growTerm + growPanel;
  if (!(total > 0)) return DEFAULT_EMBED_FRAC;
  return clampEmbedFrac(growPanel / total);
}

/** Apply one divider-drag delta (in px, positive = dragged toward the panel,
 *  growing the terminal) to a live term/panel size + grow pair, returning the
 *  new grow pair. `minTermPx`/`minPanelPx` are the floors neither side may be
 *  dragged past — defaulted to the same constants the floating overlay's own
 *  clamp uses (`overlaysize.ts`): a terminal strip stays visible, and the
 *  panel stays tall enough that its own chrome (header, list, footer) doesn't
 *  clip. Mirrors grid.ts's split-divider math exactly (before=term, after=panel). */
export function embedDragGrow(
  sizeTerm: number,
  sizePanel: number,
  growTerm: number,
  growPanel: number,
  deltaPx: number,
  minTermPx: number = EMBED_MIN_TERM_PX,
  minPanelPx: number = EMBED_MIN_PANEL_PX
): EmbedGrow {
  const total = sizeTerm + sizePanel;
  const growTotal = growTerm + growPanel;
  if (!(total > 0) || !(growTotal > 0)) return { growTerm, growPanel };
  const delta = Number.isFinite(deltaPx) ? deltaPx : 0;
  const clampedDelta = Math.max(minTermPx - sizeTerm, Math.min(sizePanel - minPanelPx, delta));
  const newGrowTerm = ((sizeTerm + clampedDelta) / total) * growTotal;
  return { growTerm: newGrowTerm, growPanel: growTotal - newGrowTerm };
}
