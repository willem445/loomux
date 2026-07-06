// Unit tests for the git view's pure sub-pane clamp/distribution math. Run
// with `npm test` (Node's built-in runner strips the TypeScript types
// natively, so no test-framework dependency lands in the build).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampPaneSize,
  parseStoredSize,
  resolvePaneSize,
  type ClampSpec,
} from "../src/gitlayout.ts";

// A generous container where both minimums fit comfortably.
const roomy: ClampSpec = { total: 1000, min: 200, otherMin: 220 };

test("a proposal within bounds is returned unchanged", () => {
  assert.equal(clampPaneSize(500, roomy), 500);
  assert.equal(clampPaneSize(200, roomy), 200); // exactly its own min
  assert.equal(clampPaneSize(780, roomy), 780); // exactly total - otherMin
});

test("the sized pane never shrinks below its own minimum", () => {
  assert.equal(clampPaneSize(50, roomy), 200);
  assert.equal(clampPaneSize(0, roomy), 200);
  assert.equal(clampPaneSize(-100, roomy), 200);
});

test("the neighbor never shrinks below its minimum", () => {
  // total 1000, neighbor needs 220 → sized pane capped at 780.
  assert.equal(clampPaneSize(900, roomy), 780);
  assert.equal(clampPaneSize(1000, roomy), 780);
});

test("when both minimums cannot fit, the neighbor's minimum wins", () => {
  // total 300, mins 200 + 220 = 420 > 300. Neighbor keeps 220, pane gets 80.
  const tight: ClampSpec = { total: 300, min: 200, otherMin: 220 };
  assert.equal(clampPaneSize(250, tight), 80);
  assert.equal(clampPaneSize(0, tight), 80);
});

test("a container smaller than even the neighbor's minimum clamps to zero", () => {
  const tiny: ClampSpec = { total: 100, min: 200, otherMin: 220 };
  assert.equal(clampPaneSize(50, tiny), 0); // total - otherMin = -120 → 0
});

test("non-finite inputs collapse to safe values", () => {
  assert.equal(clampPaneSize(NaN, roomy), 200); // non-finite proposal → own min
  assert.equal(clampPaneSize(Infinity, roomy), 200); // non-finite proposal → own min
  assert.equal(clampPaneSize(300, { total: NaN, min: 200, otherMin: 220 }), 200);
});

test("parseStoredSize accepts finite non-negative numbers only", () => {
  assert.equal(parseStoredSize("240"), 240);
  assert.equal(parseStoredSize("0"), 0);
  assert.equal(parseStoredSize("172.5"), 172.5);
});

test("parseStoredSize rejects missing / garbage / negative values", () => {
  assert.equal(parseStoredSize(null), null);
  assert.equal(parseStoredSize(undefined), null);
  assert.equal(parseStoredSize(""), null); // Number("") is 0, but empty → reject
  assert.equal(parseStoredSize("abc"), null);
  assert.equal(parseStoredSize("-5"), null);
  assert.equal(parseStoredSize("NaN"), null);
});

test("resolvePaneSize uses the stored value when usable, else the default", () => {
  // Stored value present and within bounds → used verbatim.
  assert.equal(resolvePaneSize("400", 300, roomy), 400);
  // Stored value present but too large → clamped to the neighbor cap.
  assert.equal(resolvePaneSize("5000", 300, roomy), 780);
  // Garbage stored value → falls back to the default (then clamped).
  assert.equal(resolvePaneSize("oops", 300, roomy), 300);
  assert.equal(resolvePaneSize(null, 300, roomy), 300);
  // Default itself is clamped to fit a tiny container.
  assert.equal(resolvePaneSize(null, 300, { total: 300, min: 200, otherMin: 220 }), 80);
});

test("empty string is treated as unset, not as zero", () => {
  // Guards against a corrupted "" in localStorage collapsing a pane.
  assert.equal(resolvePaneSize("", 300, roomy), 300);
});
