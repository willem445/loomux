// Unit tests for the audit log's rendered-window sizing (#361 user-demo
// finding: 2735 live DOM rows made the panel genuinely slow to reflow on
// every divider-drag frame once docked). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { nextWindowStart, backfillWindowStart, AUDIT_WINDOW_SIZE } from "../src/auditwindow.ts";

test("a fresh/filter-changed view windows to the newest WINDOW_SIZE", () => {
  assert.equal(nextWindowStart(2735, 0, true, false), 2735 - AUDIT_WINDOW_SIZE);
});

test("a filter change resets the window even if the human was scrolled up mid-backlog", () => {
  // Old windowStart indexed the PREVIOUS filtered array — must not carry over.
  assert.equal(nextWindowStart(500, 200, true, false), 500 - AUDIT_WINDOW_SIZE);
});

test("fewer entries than the window size just shows everything from the start", () => {
  assert.equal(nextWindowStart(120, 0, true, false), 0);
});

test("new entries arriving while at the tail (following) keep the window capped", () => {
  // The window was already at the cap; 50 more entries arrived and the human
  // is still at the bottom — the window slides forward to stay capped, not
  // grow unboundedly over a long follow session.
  const prevFiltered = 2735;
  const prevWindowStart = prevFiltered - AUDIT_WINDOW_SIZE;
  const nextFiltered = prevFiltered + 50;
  assert.equal(nextWindowStart(nextFiltered, prevWindowStart, false, true), nextFiltered - AUDIT_WINDOW_SIZE);
});

test("new entries arriving while scrolled up (reading history) leave the window untouched", () => {
  // The human backfilled to windowStart=900 and is reading old entries; a
  // follow poll must not yank their place in the backlog just because new
  // entries landed at the tail they aren't looking at.
  assert.equal(nextWindowStart(2800, 900, false, false), 900);
});

test("a windowStart past the (now-shrunk) filtered length is defensively reset", () => {
  // E.g. a filter excluded enough entries that the old index is out of range.
  assert.equal(nextWindowStart(50, 900, false, false), 0);
});

test("backfill steps the window back by one step, floored at 0", () => {
  assert.equal(backfillWindowStart(600), 300);
  assert.equal(backfillWindowStart(200), 0); // floored, not negative
  assert.equal(backfillWindowStart(0), 0);
});
