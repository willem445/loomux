// The working spinner (#2122 slice B): the pixel-dot glyph a row wears while
// its agent is doing something, drawn as one inline SVG sprite and animated
// entirely in CSS.
//
// DOM-FREE ON PURPOSE. This module returns markup as a STRING, so the geometry
// the stylesheet's animation depends on — the viewBox is one cell, the strip is
// SPINNER_FRAMES cells wide, the step is exactly one cell — is checkable in
// `test/spinner.test.ts` rather than by squinting at it. Get that arithmetic
// wrong by one and the animation shows two half-frames at once, which reads as
// a smeared glyph rather than as a bug worth reporting.
//
// WHY A SPRITE AND NOT A GLYPH. The reference the human gave (the group's
// `attachments/1788403968265-0.png`) is Claude Code's own terminal spinner: a
// small pixel cluster rotating in a character cell. Three ways to get that were
// considered and two rejected. A braille `content:` keyframe animation is what
// a terminal does, but on Windows braille falls back to Segoe UI Symbol, whose
// baseline and advance drift against the UI stack, and a `content:` glyph
// cannot be dyed per state. A per-frame DOM swap is per-frame DOM churn on a
// list that can hold every pane in the window, which the issue rules out in as
// many words. A sprite is neither: ONE element, ONE `transform` animation the
// compositor owns, no JavaScript per frame, and it takes its colour from
// `currentColor` so the row's `--state-working` reaches it.
//
// NOT IN `icons.ts`. That registry is vendored Lucide, pinned verbatim by
// `test/icons.test.ts` — a hand-drawn sprite added there would fail that pin,
// correctly. This is its own module.

/** The sprite's cell: SPINNER_CELL × SPINNER_CELL pixel units, one frame. Five
 *  is the smallest odd grid that holds a ring of eight around a centre, which
 *  is what makes the rotation read as a rotation rather than as a wobble. */
export const SPINNER_CELL = 5;

/** How many frames one full turn takes. Eight is the ring below. */
export const SPINNER_FRAMES = 8;

/** Dots per frame: a solid head plus a fading tail. Four is the reference
 *  cluster's density — enough that the shape reads as a comet at 12px, few
 *  enough that the ring never closes into a solid annulus. */
export const SPINNER_TRAIL = 4;

/** The ring, clockwise from the top. Radius 2 on the 5×5 grid, so every
 *  position is a whole pixel — a sub-pixel ring is what `crispEdges` would
 *  then have to round, unevenly, and the rotation would visibly stutter. */
const RING: readonly { readonly x: number; readonly y: number }[] = [
  { x: 2, y: 0 }, // N
  { x: 4, y: 0 }, // NE
  { x: 4, y: 2 }, // E
  { x: 4, y: 4 }, // SE
  { x: 2, y: 4 }, // S
  { x: 0, y: 4 }, // SW
  { x: 0, y: 2 }, // W
  { x: 0, y: 0 }, // NW
];

/** One dot of one frame. */
export interface SpinnerDot {
  readonly x: number;
  readonly y: number;
  readonly opacity: number;
}

/** The dots of frame `frame`, head first. The head sits at ring position
 *  `frame` and the tail trails BEHIND it (counter-clockwise), fading, so
 *  stepping frames forward moves the bright end forward. `frame` wraps, so
 *  callers can index past the end without a modulo of their own. */
export function spinnerFrameDots(frame: number): SpinnerDot[] {
  const head = ((frame % SPINNER_FRAMES) + SPINNER_FRAMES) % SPINNER_FRAMES;
  const dots: SpinnerDot[] = [];
  for (let i = 0; i < SPINNER_TRAIL; i++) {
    const pos = RING[(head - i + SPINNER_FRAMES * SPINNER_TRAIL) % SPINNER_FRAMES];
    // Linear fade from 1 to just above 0. Never reaching 0 is deliberate: a
    // fully transparent tail dot is a dot the trail claims to have and does not
    // draw, so the shape would be shorter than the constant says it is.
    dots.push({ x: pos.x, y: pos.y, opacity: Number((1 - i / SPINNER_TRAIL).toFixed(2)) });
  }
  return dots;
}

/** The whole sprite as one inline `<svg>`.
 *
 *  The window is one cell (`viewBox`); the strip behind it is every frame laid
 *  out left to right, and `styles.css` walks it with a `steps(SPINNER_FRAMES)`
 *  translate of exactly one cell per step. `test/spinner.test.ts` pins that
 *  arithmetic from this side so the two cannot drift apart silently.
 *
 *  `aria-hidden`: the row's state word carries the meaning, so the glyph is
 *  decoration to a screen reader — and under `prefers-reduced-motion` the
 *  stylesheet stops the animation outright and the word is ALL that is left,
 *  which is why the word is not optional. */
export function spinnerSvg(): string {
  const rects: string[] = [];
  for (let f = 0; f < SPINNER_FRAMES; f++) {
    for (const d of spinnerFrameDots(f)) {
      rects.push(
        `<rect x="${f * SPINNER_CELL + d.x}" y="${d.y}" width="1" height="1" opacity="${d.opacity}"/>`
      );
    }
  }
  return (
    `<svg class="pixel-spinner" viewBox="0 0 ${SPINNER_CELL} ${SPINNER_CELL}" ` +
    `width="${SPINNER_CELL * 2}" height="${SPINNER_CELL * 2}" ` +
    `fill="currentColor" shape-rendering="crispEdges" aria-hidden="true" focusable="false">` +
    `<g class="pixel-spinner-strip">${rects.join("")}</g>` +
    `</svg>`
  );
}
