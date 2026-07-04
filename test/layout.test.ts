// Unit tests for the pure drag-reorder geometry. Run with `npm test`
// (Node's built-in test runner strips the TypeScript types natively, so no
// test-framework dependency is pulled into the build).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  dropZoneFor,
  indicatorFor,
  zoneToPlacement,
  DEFAULT_EDGE_RATIO,
  type DropZone,
} from "../src/layout.ts";

test("center of a pane is the swap zone", () => {
  assert.equal(dropZoneFor(200, 100, 100, 50), "center");
  assert.equal(dropZoneFor(200, 100, 110, 45), "center");
});

test("each edge band classifies to its side", () => {
  // 30% edge band: within 60px horizontally / 30px vertically of an edge.
  assert.equal(dropZoneFor(200, 100, 5, 50), "left");
  assert.equal(dropZoneFor(200, 100, 195, 50), "right");
  assert.equal(dropZoneFor(200, 100, 100, 3), "top");
  assert.equal(dropZoneFor(200, 100, 100, 97), "bottom");
});

test("edge band boundary is exactly the edge ratio", () => {
  const w = 100;
  // Just inside the band (left fraction < 0.3) → left; at/over → center.
  assert.equal(dropZoneFor(w, 100, 0.29 * w, 50), "left");
  assert.equal(dropZoneFor(w, 100, DEFAULT_EDGE_RATIO * w, 50), "center");
});

test("nearest edge wins in a corner", () => {
  // Deep in the top-left corner but closer to the top edge than the left.
  assert.equal(dropZoneFor(200, 100, 40, 2), "top");
  // Closer to the left edge than the top.
  assert.equal(dropZoneFor(200, 100, 2, 40), "left");
});

test("degenerate sizes fall back to center rather than dividing by zero", () => {
  assert.equal(dropZoneFor(0, 0, 0, 0), "center");
  assert.equal(dropZoneFor(-5, 100, 10, 10), "center");
});

test("a custom edge ratio widens the swap zone", () => {
  // With a tiny edge band, a point 10% in is now center, not left.
  assert.equal(dropZoneFor(200, 100, 20, 50, 0.05), "center");
  assert.equal(dropZoneFor(200, 100, 20, 50, 0.3), "left");
});

test("indicator covers the correct half per zone", () => {
  assert.deepEqual(indicatorFor("center"), { left: 0, top: 0, width: 1, height: 1 });
  assert.deepEqual(indicatorFor("left"), { left: 0, top: 0, width: 0.5, height: 1 });
  assert.deepEqual(indicatorFor("right"), { left: 0.5, top: 0, width: 0.5, height: 1 });
  assert.deepEqual(indicatorFor("top"), { left: 0, top: 0, width: 1, height: 0.5 });
  assert.deepEqual(indicatorFor("bottom"), { left: 0, top: 0.5, width: 1, height: 0.5 });
});

test("indicator halves never overflow the pane box", () => {
  const zones: DropZone[] = ["center", "left", "right", "top", "bottom"];
  for (const z of zones) {
    const r = indicatorFor(z);
    assert.ok(r.left >= 0 && r.top >= 0, `${z} origin in bounds`);
    assert.ok(r.left + r.width <= 1 + 1e-9, `${z} width in bounds`);
    assert.ok(r.top + r.height <= 1 + 1e-9, `${z} height in bounds`);
  }
});

test("zone maps to the expected split placement", () => {
  assert.deepEqual(zoneToPlacement("left"), { dir: "row", before: true });
  assert.deepEqual(zoneToPlacement("right"), { dir: "row", before: false });
  assert.deepEqual(zoneToPlacement("top"), { dir: "column", before: true });
  assert.deepEqual(zoneToPlacement("bottom"), { dir: "column", before: false });
  assert.equal(zoneToPlacement("center"), null);
});
