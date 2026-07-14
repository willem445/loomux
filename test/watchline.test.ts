// Unit tests for the group view's "⏳ waiting on …" per-agent watch indicator
// (issue #248): the countdown math (formatExpiry) and the sentence it feeds
// into (watchLine). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { formatExpiry, watchLine, type WatchLike } from "../src/watchline.ts";

// ---------- formatExpiry ----------

test("formatExpiry rounds a sub-minute remainder UP to 1 min, never 0", () => {
  const now = 1_000_000;
  assert.equal(formatExpiry(now + 30_000, now), "1 min");
});

test("formatExpiry shows plain minutes under an hour", () => {
  const now = 1_000_000;
  assert.equal(formatExpiry(now + 43 * 60_000, now), "43 min");
});

test("formatExpiry switches to Hh Mm at 60+ minutes", () => {
  const now = 1_000_000;
  assert.equal(formatExpiry(now + 125 * 60_000, now), "2h 5m");
});

test("formatExpiry drops the minutes on an exact hour", () => {
  const now = 1_000_000;
  assert.equal(formatExpiry(now + 120 * 60_000, now), "2h");
});

test("formatExpiry at exactly 60 minutes reads 1h, not 60 min", () => {
  const now = 1_000_000;
  assert.equal(formatExpiry(now + 60 * 60_000, now), "1h");
});

test("formatExpiry reads 'expiring' once the deadline has passed", () => {
  const now = 1_000_000;
  assert.equal(formatExpiry(now - 1, now), "expiring");
  assert.equal(formatExpiry(now - 60_000, now), "expiring");
});

test("formatExpiry reads 'expiring' AT the deadline too (not '0 min')", () => {
  // now == expires_ms: the boundary case a naive `> 0` check would mishandle.
  const now = 1_000_000;
  assert.equal(formatExpiry(now, now), "expiring");
});

// ---------- watchLine ----------

test("watchLine is empty for no watches, so the caller can skip the line entirely", () => {
  assert.equal(watchLine([], 0), "");
});

test("watchLine names the target and the countdown for a single watch", () => {
  const now = 1_000_000;
  const w: WatchLike[] = [{ target: "PR #241 checks", expires_ms: now + 43 * 60_000 }];
  assert.equal(watchLine(w, now), "⏳ waiting on PR #241 checks (expires in 43 min)");
});

test("watchLine picks the SOONEST-expiring watch regardless of array order", () => {
  const now = 1_000_000;
  // Deliberately unsorted, with the soonest-expiring watch listed LAST — a
  // naive "first in the array" pick would get this wrong.
  const w: WatchLike[] = [
    { target: "run 999", expires_ms: now + 90 * 60_000 },
    { target: "PR #5 checks", expires_ms: now + 10 * 60_000 },
  ];
  assert.equal(watchLine(w, now), "⏳ waiting on PR #5 checks (expires in 10 min) +1 more");
});

test("watchLine collapses extra watches to a '+N more' suffix, not one line each", () => {
  const now = 1_000_000;
  const w: WatchLike[] = [
    { target: "PR #1 checks", expires_ms: now + 5 * 60_000 },
    { target: "PR #2 checks", expires_ms: now + 10 * 60_000 },
    { target: "PR #3 checks", expires_ms: now + 15 * 60_000 },
  ];
  const line = watchLine(w, now);
  assert.match(line, /^⏳ waiting on PR #1 checks \(expires in 5 min\) \+2 more$/);
});
