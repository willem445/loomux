// The working spinner's sprite (#2122 slice B). DOM-free: `spinner.ts` returns
// markup as a string, so the geometry the CSS animation depends on can be
// checked here rather than by looking at it.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  SPINNER_CELL,
  SPINNER_FRAMES,
  SPINNER_TRAIL,
  spinnerFrameDots,
  spinnerSvg,
} from "../src/spinner.ts";

// COMMENTS OUT FIRST, for `test/hiddenrule.test.ts`'s reason: the spinner's own
// CSS comment is prose ABOUT this arithmetic and spells out "8 frames", "5 units
// per cell" and "40 units of strip" in words. A parser that read the comment
// would be reading the ARGUMENT and reporting it as the code — green on a
// stylesheet whose declarations say something else entirely.
const CSS = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8").replace(
  /\/\*[\s\S]*?\*\//g,
  ""
);

/** The body of the first block opening with `head`, brace-matched.
 *
 *  Brace-MATCHED rather than sliced to the next `}`, because `@keyframes` and
 *  `@media` both nest one level: an `indexOf("}")` slice would return the first
 *  inner rule and stop, so a `to {}` step outside it would be invisible to
 *  every assertion built on it. Anchored to the start of a line for
 *  `resizeburst.test.ts`'s reason — a selector also appears in prose above its
 *  own rule. */
function blockAt(index: number, what: string): string {
  const from = CSS.indexOf("{", index);
  assert.ok(from >= 0, `${what} has no block`);
  let depth = 0;
  for (let i = from; i < CSS.length; i++) {
    if (CSS[i] === "{") depth++;
    else if (CSS[i] === "}" && --depth === 0) return CSS.slice(from + 1, i);
  }
  assert.fail(`${what}'s block is unterminated`);
}

function cssBlock(head: RegExp): string {
  const open = head.exec(CSS);
  assert.ok(open, `src/styles.css must contain ${String(head)}`);
  return blockAt(open.index, String(head));
}

/** EVERY block opening with `head` (which must be `g`-flagged).
 *
 *  The single-block form above is right for a selector that appears once, and
 *  wrong for an at-rule that recurs: reading the first
 *  `prefers-reduced-motion` block and judging the spinner by it is a guard
 *  whose population is one arbitrary member. Same defect class as #1327/#1344 —
 *  a green that is about the list the assertion ran over, not about the
 *  property. */
function cssBlocksAll(head: RegExp): string[] {
  const out: string[] = [];
  for (const m of CSS.matchAll(head)) out.push(blockAt(m.index, String(head)));
  return out;
}

/** Every `translateX()` in a keyframes body, in source order, as numbers of px.
 *
 *  The unit is checked rather than assumed, and that is not pedantry: `0` is
 *  legal unitless CSS and every other length is NOT, so a hand-edit to
 *  `translateX(-40)` makes the browser drop the whole declaration and the
 *  spinner sits still — with the stylesheet parsing fine and this file's
 *  arithmetic still agreeing, if the parser had quietly accepted it. */
function translateXs(frames: string, where: string): number[] {
  return [...frames.matchAll(/translateX\(\s*(-?[\d.]+)(px)?\s*\)/g)].map((m) => {
    const value = Number(m[1]);
    assert.ok(
      value === 0 || m[2] === "px",
      `${where}: translateX(${m[1]}) has no unit — only 0 may be unitless, so this declaration ` +
        `is invalid and the browser drops it`
    );
    return value;
  });
}

test("every frame is a distinct arrangement of dots", () => {
  // The whole point of a sprite is that stepping through it LOOKS like motion.
  // Two frames that render the same are a stall the eye reads as a hang, and
  // they are indistinguishable from a broken rotation in a screenshot.
  const shapes = new Set<string>();
  for (let f = 0; f < SPINNER_FRAMES; f++) {
    const key = spinnerFrameDots(f)
      .map((d) => `${d.x},${d.y}@${d.opacity}`)
      .join("|");
    shapes.add(key);
  }
  assert.equal(shapes.size, SPINNER_FRAMES, "two frames render identically");
});

test("a frame is a comet: one full-opacity head and a strictly fading tail", () => {
  for (let f = 0; f < SPINNER_FRAMES; f++) {
    const dots = spinnerFrameDots(f);
    assert.equal(dots.length, SPINNER_TRAIL, `frame ${f} has ${dots.length} dots`);
    assert.equal(dots[0].opacity, 1, `frame ${f}'s head is not solid`);
    for (let i = 1; i < dots.length; i++) {
      assert.ok(
        dots[i].opacity < dots[i - 1].opacity,
        `frame ${f} dot ${i} (${dots[i].opacity}) does not fade below ${dots[i - 1].opacity}`
      );
      assert.ok(dots[i].opacity > 0, `frame ${f} dot ${i} is invisible, so the trail is shorter than it claims`);
    }
  }
});

test("the head walks the ring one step per frame, and comes back round", () => {
  // Frame N's head is frame N-1's second dot: that is what makes the sprite one
  // rotation rather than eight unrelated pictures.
  for (let f = 0; f < SPINNER_FRAMES; f++) {
    const head = spinnerFrameDots(f)[0];
    const nextHead = spinnerFrameDots((f + 1) % SPINNER_FRAMES)[0];
    assert.notDeepEqual(
      { x: head.x, y: head.y },
      { x: nextHead.x, y: nextHead.y },
      `frame ${f} and ${(f + 1) % SPINNER_FRAMES} share a head position`
    );
  }
  // …and after a full turn it is exactly where it started, so the CSS loop has
  // no visible seam.
  assert.deepEqual(spinnerFrameDots(0), spinnerFrameDots(SPINNER_FRAMES));
});

test("every dot lands inside its own cell", () => {
  for (let f = 0; f < SPINNER_FRAMES; f++) {
    for (const d of spinnerFrameDots(f)) {
      assert.ok(d.x >= 0 && d.x < SPINNER_CELL, `frame ${f}: x=${d.x} outside 0..${SPINNER_CELL}`);
      assert.ok(d.y >= 0 && d.y < SPINNER_CELL, `frame ${f}: y=${d.y} outside 0..${SPINNER_CELL}`);
      assert.ok(Number.isInteger(d.x) && Number.isInteger(d.y), `frame ${f}: ${d.x},${d.y} is off the pixel grid`);
    }
  }
});

test("the sprite's arithmetic is what the stylesheet steps through", () => {
  const svg = spinnerSvg();
  // The viewBox is ONE cell — the window; the strip behind it is eight cells
  // wide and is translated one cell per step. If these two disagree the
  // animation shows two half-frames at once, which reads as a smear rather
  // than as a bug.
  assert.match(svg, new RegExp(`viewBox="0 0 ${SPINNER_CELL} ${SPINNER_CELL}"`));
  assert.ok(
    svg.includes(`class="pixel-spinner-strip"`),
    "the strip needs the class the stylesheet animates, or nothing moves"
  );
  // Every frame's dots are present, at that frame's own horizontal offset.
  for (let f = 0; f < SPINNER_FRAMES; f++) {
    for (const d of spinnerFrameDots(f)) {
      assert.ok(
        svg.includes(`x="${f * SPINNER_CELL + d.x}" y="${d.y}"`),
        `frame ${f}'s dot ${d.x},${d.y} is not at sprite offset ${f * SPINNER_CELL + d.x}`
      );
    }
  }
});

test("the sprite is dyed by its position, not by a colour of its own", () => {
  const svg = spinnerSvg();
  // `currentColor` is what lets `--state-working` reach it from the row that
  // holds it. A literal hex here would be a state dye baked into a glyph,
  // which is the thing styles.css's icon block refuses.
  assert.ok(svg.includes("currentColor"), "the sprite hard-codes a colour");
  assert.doesNotMatch(svg, /#[0-9a-fA-F]{3,8}\b/, "the sprite carries a literal colour");
  // Pixel art, not a blurred scale: without this the 1-unit rects come out
  // anti-aliased at the sizes this renders at.
  assert.ok(svg.includes('shape-rendering="crispEdges"'), "the sprite is not pinned to the pixel grid");
});

test("the markup is inert: no script, no external reference", () => {
  const svg = spinnerSvg();
  assert.doesNotMatch(svg, /<script/i);
  assert.doesNotMatch(svg, /https?:/i);
});

// ---------- the sprite and the stylesheet, pinned to each other ----------
//
// WHY THESE EXIST (#2259 review, rev-final B1). `spinner.ts` and `styles.css`
// each carried a comment saying the other was pinned to it — "so the two cannot
// drift apart silently", "change one and change both" — and neither was: the
// suite imported `../src/spinner.ts` and nothing else, so `steps(8)` and
// `translateX(-40px)` were unguarded numbers in a stylesheet. The reviewer cut
// the exact drift both comments name (`-40px` -> `-35px`, `steps(8)` kept: each
// step advances 4.375 units against a 5-unit sprite, so every window straddles
// two frames) and the whole suite stayed 2642/0. `tsc` cannot see it either;
// the drift lives in CSS. These three assertions are the guard those two claims
// were describing.

test("the stylesheet steps the strip once per frame, over the sprite's own width", () => {
  const strip = cssBlock(/^\.pixel-spinner-strip\s*\{/m);
  const name = /animation:\s*([\w-]+)\s/.exec(strip);
  assert.ok(name, `.pixel-spinner-strip must animate something; block was:\n${strip}`);

  const steps = /\bsteps\(\s*(\d+)/.exec(strip);
  assert.ok(steps, `.pixel-spinner-strip must step rather than ease; block was:\n${strip}`);
  assert.equal(
    Number(steps[1]),
    SPINNER_FRAMES,
    "the stylesheet takes a different number of steps than the sprite has frames, so a step " +
      "lands between two of them and the glyph renders as a smear"
  );

  // The keyframes block this animation actually names — not a block whose name
  // is assumed, so renaming one half is caught rather than stepped over.
  const frames = cssBlock(new RegExp(String.raw`^@keyframes\s+${name[1]}\s*\{`, "m"));
  const shifts = translateXs(frames, `@keyframes ${name[1]}`);
  // The instrument before its finding: a parse that matched nothing would agree
  // with every implementation, including one that translates nothing at all.
  assert.equal(
    shifts.length,
    2,
    `expected a from- and a to-translate in @keyframes ${name[1]}, found ${shifts.length}:\n${frames}`
  );
  assert.deepEqual(
    shifts,
    [0, -(SPINNER_FRAMES * SPINNER_CELL)],
    "the strip must travel exactly its own width — SPINNER_FRAMES cells of SPINNER_CELL units — " +
      "from a standing start, or the last frame is not the last frame"
  );
});

test("the viewBox window is one cell, so exactly one frame is visible at a time", () => {
  // The other half of the same arithmetic: the sprite's window and the step
  // size have to be the SAME number. Read off the emitted markup rather than
  // off the constant, so a hand-edited `viewBox` in `spinnerSvg` is caught too.
  const viewBox = /viewBox="0 0 (\d+) (\d+)"/.exec(spinnerSvg());
  assert.ok(viewBox, "the sprite must declare a viewBox");
  assert.equal(Number(viewBox[1]), SPINNER_CELL);
  assert.equal(Number(viewBox[2]), SPINNER_CELL);

  const frames = cssBlock(/^@keyframes\s+pixel-spin\s*\{/m);
  const total = translateXs(frames, "@keyframes pixel-spin").reduce(
    (a, b) => Math.max(a, Math.abs(b)),
    0
  );
  assert.equal(
    total / SPINNER_FRAMES,
    Number(viewBox[1]),
    "one step must move the strip by exactly one viewBox width; anything else shows two " +
      "half-frames at once"
  );
});

test("reduced motion stops the spinner, and does it to the element that animates", () => {
  // #2122's own acceptance criterion, and it was as unguarded as the arithmetic
  // above: a rename of `.pixel-spinner-strip` on one side only would leave the
  // media query switching off an element that no longer exists, with nothing
  // red and nothing visibly wrong until someone who needs it opens the app.
  //
  // EVERY such block, not the first one. `styles.css` carries several
  // `prefers-reduced-motion` blocks (the tab attention pulse is the first), so a
  // single-block read would judge the spinner by a rule that says nothing about
  // it — the population defect this file's own first draft shipped, and the
  // reason the count below is asserted rather than assumed.
  const blocks = cssBlocksAll(/@media\s*\(\s*prefers-reduced-motion:\s*reduce\s*\)\s*\{/g);
  assert.ok(
    blocks.length > 0,
    "no prefers-reduced-motion block found at all — the scan is blind, not the stylesheet clean"
  );
  const rule = /\.pixel-spinner-strip\s*\{[^}]*animation:\s*none/;
  assert.ok(
    blocks.some((b) => rule.test(b)),
    `none of the ${blocks.length} prefers-reduced-motion blocks switches off ` +
      `.pixel-spinner-strip — the element the animation is actually on — by name`
  );
});
