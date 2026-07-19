// Unit tests for the embed-panel divider math (#361), including the
// three-side (left/right/bottom) generalization. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampEmbedFrac,
  growFromFrac,
  fracFromGrow,
  embedDragGrow,
  embedSideFloors,
  embedCenterFloor,
  DEFAULT_EMBED_FRAC,
  EMBED_MIN_TERM_PX,
  EMBED_MIN_PANEL_PX,
  EMBED_DIVIDER_PX,
  EMBED_SIDES,
} from "../src/embedsplit.ts";
import { TERM_RESERVE_H, OVERLAY_MIN_H } from "../src/overlaysize.ts";

// embedsplit.ts deliberately duplicates these two numbers rather than
// importing them (see the file's own top-of-file comment: tsc rejects a
// `.ts` import extension, node --test can't resolve a bare one — no single
// spelling satisfies both runners for an intra-src import). A test file is
// under neither constraint (excluded from tsc's project; node --test
// resolves `.ts`-suffixed specifiers directly), so THIS is where the two
// copies are pinned equal — the guard the duplication itself can't provide.
test("the duplicated floors stay equal to overlaysize.ts's originals (drift guard)", () => {
  assert.equal(EMBED_MIN_TERM_PX, TERM_RESERVE_H, "terminal floor must mirror TERM_RESERVE_H");
  assert.equal(EMBED_MIN_PANEL_PX, OVERLAY_MIN_H, "panel floor must mirror OVERLAY_MIN_H");
});

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
    assert.equal(grow.growBefore + grow.growAfter, 1);
    assert.equal(fracFromGrow(grow.growBefore, grow.growAfter), clampEmbedFrac(frac));
  }
});

test("fracFromGrow falls back to the default when the pair is degenerate", () => {
  assert.equal(fracFromGrow(0, 0), DEFAULT_EMBED_FRAC);
  assert.equal(fracFromGrow(-1, -1), DEFAULT_EMBED_FRAC);
});

test("a drag within bounds redistributes grow proportionally, preserving the total", () => {
  // 800px pane, even split (400/400), grow 1/1 → drag 100px toward "after"
  // (growing "before") should move roughly a quarter of the total weight.
  const result = embedDragGrow(400, 400, 1, 1, 100);
  assert.ok(result.growBefore > 1, "before's share grew");
  assert.ok(result.growAfter < 1, "after's share shrank");
  assert.equal(result.growBefore + result.growAfter, 2); // growTotal preserved
});

test("a drag that would starve the before side clamps at its floor", () => {
  // Before side already at exactly its floor; any further push toward after
  // must not shrink it past minBeforePx worth of the total.
  const sizeBefore = EMBED_MIN_TERM_PX;
  const sizeAfter = 600;
  const result = embedDragGrow(sizeBefore, sizeAfter, 1, 1, -9999); // drag hard toward before
  const total = sizeBefore + sizeAfter;
  const impliedBeforePx = (result.growBefore / (result.growBefore + result.growAfter)) * total;
  assert.ok(impliedBeforePx >= EMBED_MIN_TERM_PX - 0.001, `before floor held: ${impliedBeforePx}`);
});

test("a drag that would starve the after side clamps at its floor", () => {
  const sizeBefore = 600;
  const sizeAfter = EMBED_MIN_PANEL_PX;
  const result = embedDragGrow(sizeBefore, sizeAfter, 1, 1, 9999); // drag hard toward after
  const total = sizeBefore + sizeAfter;
  const impliedAfterPx = (result.growAfter / (result.growBefore + result.growAfter)) * total;
  assert.ok(impliedAfterPx >= EMBED_MIN_PANEL_PX - 0.001, `after floor held: ${impliedAfterPx}`);
});

test("a non-finite delta is treated as zero (no NaN grow)", () => {
  const result = embedDragGrow(400, 400, 1, 1, NaN);
  assert.equal(result.growBefore, 1);
  assert.equal(result.growAfter, 1);
});

test("a degenerate size or grow pair passes through unchanged rather than dividing by zero", () => {
  assert.deepEqual(embedDragGrow(0, 0, 1, 1, 50), { growBefore: 1, growAfter: 1 });
  assert.deepEqual(embedDragGrow(400, 400, 0, 0, 50), { growBefore: 0, growAfter: 0 });
});

test("custom floors are honored", () => {
  const result = embedDragGrow(200, 200, 1, 1, -500, 50, 50);
  const total = 400;
  const impliedBeforePx = (result.growBefore / (result.growBefore + result.growAfter)) * total;
  assert.ok(impliedBeforePx >= 50 - 0.001);
});

// ---------- output clamp: a pane too small for its own floors must degrade,
// never emit a NEGATIVE flex-grow (#361 rev-58 NB1 — CSS silently drops an
// invalid grow value rather than rejecting it, so this fails silently at
// render time instead of throwing) ----------

test("a before-floor bigger than the whole region clamps growBefore at growTotal, never past it", () => {
  // The pane itself (300px) is smaller than the "before" floor alone (400px)
  // — impossible to honor, but the output must still be a valid, non-negative
  // grow pair rather than overshooting past all-the-grow-to-one-side.
  const result = embedDragGrow(150, 150, 1, 1, 9999, 400, 50);
  assert.ok(result.growBefore >= 0 && result.growBefore <= 2, `growBefore in range: ${result.growBefore}`);
  assert.ok(result.growAfter >= 0 && result.growAfter <= 2, `growAfter in range: ${result.growAfter}`);
  assert.equal(result.growBefore + result.growAfter, 2, "growTotal still preserved");
});

test("an after-floor bigger than the whole region clamps growAfter at growTotal, never past it", () => {
  const result = embedDragGrow(150, 150, 1, 1, -9999, 50, 400);
  assert.ok(result.growBefore >= 0 && result.growBefore <= 2, `growBefore in range: ${result.growBefore}`);
  assert.ok(result.growAfter >= 0 && result.growAfter <= 2, `growAfter in range: ${result.growAfter}`);
  assert.equal(result.growBefore + result.growAfter, 2, "growTotal still preserved");
});

test("both floors together exceeding the region still yields a valid, non-negative grow pair", () => {
  // Neither side's floor alone exceeds the total, but their SUM does — the
  // reviewer's own reported failing cases (roughly -0.064 / -1.5 / -0.25)
  // came from exactly this shape: a small region, two competing floors.
  const result = embedDragGrow(60, 60, 1, 1, 30, 90, 90);
  assert.ok(result.growBefore >= 0, `growBefore non-negative: ${result.growBefore}`);
  assert.ok(result.growAfter >= 0, `growAfter non-negative: ${result.growAfter}`);
  assert.equal(result.growBefore + result.growAfter, 2, "growTotal still preserved");
});

// ---------- multi-slot: embedSideFloors / embedCenterFloor (clamp precedence, #361) ----------

test("EMBED_SIDES lists exactly left, right, bottom — no top", () => {
  assert.deepEqual(EMBED_SIDES, ["left", "right", "bottom"]);
});

test("right: the terminal's width floor is BEFORE, the panel is AFTER", () => {
  const floors = embedSideFloors("right", 222);
  assert.deepEqual(floors, { beforeFloorPx: EMBED_MIN_TERM_PX, afterFloorPx: 222 });
});

test("bottom: the row's height floor (the terminal's own) is BEFORE, the panel is AFTER", () => {
  const floors = embedSideFloors("bottom", 222);
  assert.deepEqual(floors, { beforeFloorPx: EMBED_MIN_TERM_PX, afterFloorPx: 222 });
});

test("precedence: the terminal's own floor is ALWAYS one side of right's and bottom's divider pair", () => {
  // Whichever of these two a panel docks to, EMBED_MIN_TERM_PX (never the
  // panel's own floor) is what guards the terminal — this is the "terminal
  // floor wins" half of "terminal floor > panel floors > shares", pinned per
  // side. (Left is covered separately below — its far side is the composite
  // `embedCenterEl`, not the terminal directly; see `embedCenterFloor`.)
  for (const side of ["right", "bottom"] as const) {
    const floors = embedSideFloors(side, 999); // an oversized panel floor must not displace the term one
    assert.ok(
      floors.beforeFloorPx === EMBED_MIN_TERM_PX || floors.afterFloorPx === EMBED_MIN_TERM_PX,
      `${side}'s divider must guard the terminal with EMBED_MIN_TERM_PX on one side`
    );
  }
});

test("a right-docked panel's divider clamps a hard drag at the terminal's floor, never past it", () => {
  const { beforeFloorPx, afterFloorPx } = embedSideFloors("right", EMBED_MIN_PANEL_PX);
  // Terminal (before) at exactly its floor; drag hard toward the terminal
  // (negative delta shrinks "before") must not push it under the floor.
  const result = embedDragGrow(EMBED_MIN_TERM_PX, 600, 1, 1, -9999, beforeFloorPx, afterFloorPx);
  const total = EMBED_MIN_TERM_PX + 600;
  const impliedTermPx = (result.growBefore / (result.growBefore + result.growAfter)) * total;
  assert.ok(impliedTermPx >= EMBED_MIN_TERM_PX - 0.001);
});

test("a bottom-docked panel with a GROWN dynamic floor (e.g. the group panel) still can't push the row under the terminal's floor", () => {
  const grownFloor = 420; // e.g. the group panel's measured chrome after the suspended banner appeared
  const { beforeFloorPx, afterFloorPx } = embedSideFloors("bottom", grownFloor);
  const result = embedDragGrow(EMBED_MIN_TERM_PX, 600, 1, 1, -9999, beforeFloorPx, afterFloorPx);
  const total = EMBED_MIN_TERM_PX + 600;
  const impliedRowPx = (result.growBefore / (result.growBefore + result.growAfter)) * total;
  assert.ok(impliedRowPx >= EMBED_MIN_TERM_PX - 0.001, "the row (term) floor holds even against a much larger panel floor");
});

test("embedCenterFloor collapses to the plain terminal floor when right isn't occupied", () => {
  assert.equal(embedCenterFloor(null), EMBED_MIN_TERM_PX);
});

test("embedCenterFloor composes the terminal floor + the right panel's floor + one divider width when right IS occupied", () => {
  assert.equal(embedCenterFloor(EMBED_MIN_PANEL_PX), EMBED_MIN_TERM_PX + EMBED_MIN_PANEL_PX + EMBED_DIVIDER_PX);
});

test("a left-docked panel's divider clamps a hard drag at the panel's own floor, never past it (right unoccupied)", () => {
  const beforeFloorPx = EMBED_MIN_PANEL_PX; // left slot's own floor
  const afterFloorPx = embedCenterFloor(null); // center = term alone
  // Panel (before) at exactly its floor; drag hard toward the panel (negative
  // delta shrinks "before") must not push it under the floor.
  const result = embedDragGrow(EMBED_MIN_PANEL_PX, 600, 1, 1, -9999, beforeFloorPx, afterFloorPx);
  const total = EMBED_MIN_PANEL_PX + 600;
  const impliedPanelPx = (result.growBefore / (result.growBefore + result.growAfter)) * total;
  assert.ok(impliedPanelPx >= EMBED_MIN_PANEL_PX - 0.001);
});

test("a left-docked panel's divider, with right ALSO occupied, reserves room for both nested inside center", () => {
  const beforeFloorPx = EMBED_MIN_PANEL_PX;
  const afterFloorPx = embedCenterFloor(EMBED_MIN_PANEL_PX); // center = term + right divider + right slot
  // Drag hard toward the left panel — center (containing term AND the
  // occupied right slot) must never be squeezed below its composed floor.
  const result = embedDragGrow(EMBED_MIN_PANEL_PX, 800, 1, 1, 9999, beforeFloorPx, afterFloorPx);
  const total = EMBED_MIN_PANEL_PX + 800;
  const impliedCenterPx = (result.growAfter / (result.growBefore + result.growAfter)) * total;
  assert.ok(
    impliedCenterPx >= afterFloorPx - 0.001,
    `center (term + right) floor held: ${impliedCenterPx} >= ${afterFloorPx}`
  );
});
