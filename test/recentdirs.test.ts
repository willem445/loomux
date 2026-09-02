// The recent-directories decision behind the launcher's repo dropdown (#2010).
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_RECENT_REPOS, mergeRecentDir } from "../src/recentdirs.ts";

test("a new path goes to the front — newest first", () => {
  assert.deepEqual(mergeRecentDir(["C:\\a", "C:\\b"], "C:\\c"), ["C:\\c", "C:\\a", "C:\\b"]);
});

test("a repeat is deduplicated and moves to the front", () => {
  assert.deepEqual(mergeRecentDir(["C:\\a", "C:\\b"], "C:\\b"), ["C:\\b", "C:\\a"]);
  assert.deepEqual(mergeRecentDir(["C:\\a"], "C:\\a"), ["C:\\a"]);
});

test("the list is capped at MAX_RECENT_REPOS, dropping the oldest", () => {
  const seeded = Array.from({ length: 20 }, (_, i) => `C:\\r${i}`);
  const next = mergeRecentDir(seeded, "C:\\new");
  assert.ok(next !== null);
  assert.equal(next.length, MAX_RECENT_REPOS);
  assert.equal(next[0], "C:\\new");
  // The tail is what the cap drops: the oldest entries fall off first.
  assert.equal(next[next.length - 1], "C:\\r6");
});

test("an empty or whitespace-only path records nothing", () => {
  const seeded = ["C:\\a"];
  assert.deepEqual(mergeRecentDir(seeded, ""), seeded);
  assert.deepEqual(mergeRecentDir(seeded, "   "), seeded);
});

test("a path is trimmed before deduping and storing", () => {
  // The same directory spelled with and without stray whitespace is ONE entry.
  assert.deepEqual(mergeRecentDir(["C:\\a"], " C:\\a "), ["C:\\a"]);
});

test("the input list is never mutated", () => {
  const seeded = ["C:\\a", "C:\\b"];
  mergeRecentDir(seeded, "C:\\c");
  assert.deepEqual(seeded, ["C:\\a", "C:\\b"]);
});

test("a failed read declines the write — null is not empty", () => {
  // getItem throwing must not produce a write that replaces the stored list
  // with one built from nothing (#2010).
  assert.equal(mergeRecentDir(null, "C:\\a"), null);
});
