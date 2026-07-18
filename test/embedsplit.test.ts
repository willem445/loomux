// Unit tests for the embedded-panel divider math (#361). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampEmbedFrac,
  growFromFrac,
  fracFromGrow,
  embedDragGrow,
  DEFAULT_EMBED_FRAC,
  EMBED_MIN_TERM_PX,
  EMBED_MIN_PANEL_PX,
} from "../src/embedsplit.ts";

test("clampEmbedFrac holds a normal fraction unchanged", () => {
  assert.equal(clampEmbedFrac(0.4), 0.4);
});

test("clampEmbedFrac floors and ceils a runaway fraction", () => {
  assert.equal(clampEmbedFrac(0), 0.1);
  assert.equal(clampEmbedFrac(-3), 0.1);
  assert.equal(clampEmbedFrac(1), 0.85);
  assert.equal(clampEmbedFrac(50), 0.85);
});

test("clampEmbedFrac falls back to the default on a non-finite input", () => {
  assert.equal(clampEmbedFrac(NaN), DEFAULT_EMBED_FRAC);
  assert.equal(clampEmbedFrac(Infinity), DEFAULT_EMBED_FRAC);
});

test("growFromFrac / fracFromGrow round-trip", () => {
  for (const frac of [0.15, 0.25, 0.33, 0.5, 0.7]) {
    const grow = growFromFrac(frac);
    assert.equal(grow.growTerm + grow.growPanel, 1);
    assert.equal(fracFromGrow(grow.growTerm, grow.growPanel), clampEmbedFrac(frac));
  }
});

test("fracFromGrow falls back to the default when the pair is degenerate", () => {
  assert.equal(fracFromGrow(0, 0), DEFAULT_EMBED_FRAC);
  assert.equal(fracFromGrow(-1, -1), DEFAULT_EMBED_FRAC);
});

test("a drag within bounds redistributes grow proportionally, preserving the total", () => {
  // 800px pane, even split (400/400), grow 1/1 → drag 100px toward the panel
  // (growing the terminal) should move roughly a quarter of the total weight.
  const before = embedDragGrow(400, 400, 1, 1, 100);
  assert.ok(before.growTerm > 1, "terminal's share grew");
  assert.ok(before.growPanel < 1, "panel's share shrank");
  assert.equal(before.growTerm + before.growPanel, 2); // growTotal preserved
});

test("a drag that would starve the terminal clamps at its floor", () => {
  // Terminal already at exactly the reserve; any further push toward the panel
  // must not shrink it past EMBED_MIN_TERM_PX worth of the total.
  const sizeTerm = EMBED_MIN_TERM_PX;
  const sizePanel = 600;
  const result = embedDragGrow(sizeTerm, sizePanel, 1, 1, -9999); // drag hard toward the terminal
  const total = sizeTerm + sizePanel;
  const impliedTermPx = (result.growTerm / (result.growTerm + result.growPanel)) * total;
  assert.ok(impliedTermPx >= EMBED_MIN_TERM_PX - 0.001, `terminal floor held: ${impliedTermPx}`);
});

test("a drag that would starve the panel clamps at its floor", () => {
  const sizeTerm = 600;
  const sizePanel = EMBED_MIN_PANEL_PX;
  const result = embedDragGrow(sizeTerm, sizePanel, 1, 1, 9999); // drag hard toward the panel
  const total = sizeTerm + sizePanel;
  const impliedPanelPx = (result.growPanel / (result.growTerm + result.growPanel)) * total;
  assert.ok(impliedPanelPx >= EMBED_MIN_PANEL_PX - 0.001, `panel floor held: ${impliedPanelPx}`);
});

test("a non-finite delta is treated as zero (no NaN grow)", () => {
  const result = embedDragGrow(400, 400, 1, 1, NaN);
  assert.equal(result.growTerm, 1);
  assert.equal(result.growPanel, 1);
});

test("a degenerate size or grow pair passes through unchanged rather than dividing by zero", () => {
  assert.deepEqual(embedDragGrow(0, 0, 1, 1, 50), { growTerm: 1, growPanel: 1 });
  assert.deepEqual(embedDragGrow(400, 400, 0, 0, 50), { growTerm: 0, growPanel: 0 });
});

test("custom floors are honored", () => {
  const result = embedDragGrow(200, 200, 1, 1, -500, 50, 50);
  const total = 400;
  const impliedTermPx = (result.growTerm / (result.growTerm + result.growPanel)) * total;
  assert.ok(impliedTermPx >= 50 - 0.001);
});
