// The working spinner's sprite (#2122 slice B). DOM-free: `spinner.ts` returns
// markup as a string, so the geometry the CSS animation depends on can be
// checked here rather than by looking at it.

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  SPINNER_CELL,
  SPINNER_FRAMES,
  SPINNER_TRAIL,
  spinnerFrameDots,
  spinnerSvg,
} from "../src/spinner.ts";

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
