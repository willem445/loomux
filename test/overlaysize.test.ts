// Unit tests for the pure overlay-splitter clamp (#83 finding 3). Run with
// `npm test`. Guards the two invariants that keep the drag bar reachable and
// the terminal visible at every splitter position.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampOverlayHeight,
  OVERLAY_MIN_H,
  TERM_RESERVE_H,
} from "../src/overlaysize.ts";

// A comfortably tall pane where the reserve, not the floor, sets the ceiling.
const TALL = 800;

test("a request inside the range passes through unchanged", () => {
  assert.equal(clampOverlayHeight(400, TALL), 400);
});

test("collapse below the floor clamps up to the minimum (bar stays grabbable)", () => {
  assert.equal(clampOverlayHeight(20, TALL), OVERLAY_MIN_H);
  assert.equal(clampOverlayHeight(0, TALL), OVERLAY_MIN_H);
  assert.equal(clampOverlayHeight(-500, TALL), OVERLAY_MIN_H);
});

test("expansion past the reserved terminal strip is capped", () => {
  // max = termHeight - reserve; the terminal strip is always preserved.
  assert.equal(clampOverlayHeight(TALL, TALL), TALL - TERM_RESERVE_H);
  assert.equal(clampOverlayHeight(TALL - TERM_RESERVE_H, TALL), TALL - TERM_RESERVE_H);
  assert.equal(clampOverlayHeight(TALL - TERM_RESERVE_H + 1, TALL), TALL - TERM_RESERVE_H);
});

test("the result always leaves at least the reserve of terminal when the pane allows", () => {
  for (const req of [0, 100, 500, 5000]) {
    const h = clampOverlayHeight(req, TALL);
    assert.ok(h <= TALL - TERM_RESERVE_H, `overlay ${h} must leave the reserve in an ${TALL}px pane`);
    assert.ok(h >= OVERLAY_MIN_H, `overlay ${h} must never drop below the floor`);
  }
});

test("a short pane floors at the minimum rather than going negative", () => {
  // termHeight - reserve < minH → max collapses to minH, so any request → minH.
  const shortPane = OVERLAY_MIN_H + TERM_RESERVE_H - 40; // reserve can't be honored
  assert.equal(clampOverlayHeight(9999, shortPane), OVERLAY_MIN_H);
  assert.equal(clampOverlayHeight(10, shortPane), OVERLAY_MIN_H);
});

test("the floor is never below the minimum even when min == max", () => {
  // Pane exactly minH + reserve: max == minH, so the single legal height is minH.
  const pane = OVERLAY_MIN_H + TERM_RESERVE_H;
  assert.equal(clampOverlayHeight(1000, pane), OVERLAY_MIN_H);
  assert.equal(clampOverlayHeight(50, pane), OVERLAY_MIN_H);
});

test("a non-finite request falls back to the floor (no NaN height)", () => {
  assert.equal(clampOverlayHeight(NaN, TALL), OVERLAY_MIN_H);
  assert.equal(clampOverlayHeight(Infinity, TALL), OVERLAY_MIN_H);
});

test("custom min/reserve are honored", () => {
  assert.equal(clampOverlayHeight(50, 500, 200, 120), 200); // below custom floor
  assert.equal(clampOverlayHeight(500, 500, 200, 120), 380); // capped at 500-120
});
