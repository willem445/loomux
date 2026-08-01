// Pure geometry for computing which parts of a plugin pane are covered by
// open DOM overlays (#391, folded into #380). DOM-free — no live Tauri window
// or DOM element involved. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { computeExcludeRects } from "../src/pluginocclusion.ts";

const PANE = { left: 100, top: 100, width: 400, height: 300 };

test("an overlay that doesn't touch the pane contributes nothing", () => {
  const overlay = { left: 600, top: 600, width: 200, height: 200 };
  assert.deepEqual(computeExcludeRects(PANE, [overlay]), []);
});

test("an overlay fully inside the pane translates to pane-local coordinates unchanged in size", () => {
  const overlay = { left: 150, top: 120, width: 50, height: 40 };
  assert.deepEqual(computeExcludeRects(PANE, [overlay]), [{ x: 50, y: 20, width: 50, height: 40 }]);
});

test("an overlay that fully covers the pane clips to exactly the pane's own box", () => {
  const overlay = { left: 0, top: 0, width: 2000, height: 2000 };
  assert.deepEqual(computeExcludeRects(PANE, [overlay]), [{ x: 0, y: 0, width: 400, height: 300 }]);
});

test("an overlay straddling the pane's left edge clips to the overlapping slice only", () => {
  // Sessions sidebar: docked left, wider than tall, hanging off the pane's
  // left edge — the real shape the #391 bug was reported through.
  const sidebar = { left: 0, top: 0, width: 260, height: 900 };
  assert.deepEqual(computeExcludeRects(PANE, [sidebar]), [{ x: 0, y: 0, width: 160, height: 300 }]);
});

test("an overlay only touching the pane at a shared edge (zero-area overlap) is excluded", () => {
  const flushRight = { left: 500, top: 100, width: 100, height: 100 };
  assert.deepEqual(computeExcludeRects(PANE, [flushRight]), []);
});

test("multiple overlays each contribute their own translated rect, unmerged", () => {
  const a = { left: 100, top: 100, width: 50, height: 50 };
  const b = { left: 400, top: 300, width: 50, height: 50 };
  assert.deepEqual(computeExcludeRects(PANE, [a, b]), [
    { x: 0, y: 0, width: 50, height: 50 },
    { x: 300, y: 200, width: 50, height: 50 },
  ]);
});

test("no open overlays yields no exclude rects", () => {
  assert.deepEqual(computeExcludeRects(PANE, []), []);
});
