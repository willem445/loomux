// Unit tests for the git view's pure commit-row formatting helpers. Run with
// `npm test`. Locale + timeZone are pinned so the assertions are deterministic
// regardless of the machine running the suite.
import { test } from "node:test";
import assert from "node:assert/strict";
import { shortRev, fmtWhen, fmtWhenFull, authorLine } from "../src/gitformat.ts";

test("shortRev abbreviates to git's 7 chars", () => {
  assert.equal(shortRev("1924ce5abcdef0123456789"), "1924ce5");
});

test("shortRev leaves shorter or empty input untouched (never throws)", () => {
  assert.equal(shortRev("abc"), "abc");
  assert.equal(shortRev(""), "");
});

test("fmtWhen shows the locale date plus 24h HH:mm", () => {
  // 1700000000 = 2023-11-14T22:13:20Z.
  assert.equal(fmtWhen(1700000000, "en-US", "UTC"), "11/14/2023 22:13");
});

test("fmtWhen pads single-digit hours/minutes and uses 24h time", () => {
  // 2024-01-01T09:04Z → early morning, single-digit hour padded.
  assert.equal(fmtWhen(1704099840, "en-US", "UTC"), "1/1/2024 09:04");
  // 2024-01-01T21:22Z → afternoon stays 24h, not 9:22 PM.
  assert.equal(fmtWhen(1704144120, "en-US", "UTC"), "1/1/2024 21:22");
});

test("fmtWhenFull includes seconds-level detail for the tooltip", () => {
  const full = fmtWhenFull(1700000000, "en-US", "UTC");
  assert.match(full, /11\/14\/2023/);
  assert.match(full, /10:13:20 PM/); // en-US's natural full format is 12h
});

test("authorLine shows only the author when committer matches", () => {
  const line = authorLine("Alice", "Alice", 1700000000);
  assert.match(line, /^Alice · /);
  assert.doesNotMatch(line, /committed by/);
});

test("authorLine surfaces a differing committer (rebase / cherry-pick)", () => {
  const line = authorLine("Alice", "Bob", 1700000000);
  assert.match(line, /^Alice \(committed by Bob\) · /);
});

test("authorLine treats an empty committer as 'same as author'", () => {
  const line = authorLine("Alice", "", 1700000000);
  assert.doesNotMatch(line, /committed by/);
});
