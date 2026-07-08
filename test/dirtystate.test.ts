// Unit tests for the pure dirty/conflict decisions (issue #174). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { isDirty, closeDecision, hasConflict } from "../src/dirtystate.ts";

test("isDirty reflects buffer vs last-saved", () => {
  assert.equal(isDirty("abc", "abc"), false);
  assert.equal(isDirty("abc", "abcd"), true);
  // Edge: re-typing the original clears dirty.
  assert.equal(isDirty("abc", "abx"), true);
  assert.equal(isDirty("", ""), false);
});

test("closeDecision confirms only when dirty", () => {
  assert.equal(closeDecision(false), "close");
  assert.equal(closeDecision(true), "confirm");
});

test("hasConflict fires when the on-disk hash drifted from the opened hash", () => {
  assert.equal(hasConflict("aaaa", "aaaa"), false);
  assert.equal(hasConflict("aaaa", "bbbb"), true);
  // Edge: a new file (no expected hash) never conflicts.
  assert.equal(hasConflict("", "bbbb"), false);
});
