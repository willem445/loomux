// Unit tests for the pure wheel-delta translation used by the shortcut hint
// bar. Run with `npm test` (Node's built-in runner strips the TypeScript
// types natively).
import { test } from "node:test";
import assert from "node:assert/strict";
import { wheelToScrollDelta } from "../src/hintbar.ts";

test("a plain mouse wheel (vertical only) scrolls horizontally", () => {
  assert.equal(wheelToScrollDelta(0, 40), 40);
  assert.equal(wheelToScrollDelta(0, -40), -40);
});

test("a trackpad's horizontal delta is used directly", () => {
  assert.equal(wheelToScrollDelta(30, 0), 30);
  assert.equal(wheelToScrollDelta(-30, 0), -30);
});

test("the larger-magnitude axis wins on a diagonal gesture", () => {
  assert.equal(wheelToScrollDelta(5, 40), 40);
  assert.equal(wheelToScrollDelta(40, 5), 40);
  // Ties favour the horizontal axis (already the intended direction).
  assert.equal(wheelToScrollDelta(20, -20), 20);
});

test("no movement yields no scroll", () => {
  assert.equal(wheelToScrollDelta(0, 0), 0);
});
