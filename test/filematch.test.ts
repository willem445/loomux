// The pure file-NAME matcher behind the file explorer's "Go to file" box (#214) —
// filematch.ts. Pins the ranking rules that make the box usable on a real repo:
// name beats directory, prefix beats mid-word, an exact name wins outright, and
// the order is fully deterministic (enumeration order must not leak into results).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  rankFileNames,
  queryTerms,
  mergeRanges,
  basenameStart,
  moveSelection,
} from "../src/filematch.ts";

/** A slice of the real repo — the paths a user would actually be jumping between. */
const FILES = [
  "src/pane.ts",
  "src/panesetup.ts",
  "src/panerestore.ts",
  "src/panefit.ts",
  "src/grid.ts",
  "src/fileedit.ts",
  "test/panerestore.test.ts",
  "test/panesetup.test.ts",
  "doc/design/session-restore.md",
  "README.md",
];

const rels = (query: string, limit = 20): string[] =>
  rankFileNames(FILES, query, limit).map((h) => h.rel);

test("an exact file name outranks every partial match of it", () => {
  // "pane.ts" is a substring of nothing else here, but the point stands generally:
  // when the user types the file's actual name, that file is first, full stop.
  assert.equal(rels("pane.ts")[0], "src/pane.ts");
});

test("a match in the file NAME beats a match only in the directory", () => {
  const ranked = rankFileNames(["test/grid.ts", "src/test-helpers.ts"], "test", 10);
  assert.equal(ranked[0].rel, "src/test-helpers.ts", "the NAME match wins over the folder match");
});

test("the BEST occurrence of a term is scored, not the first one found", () => {
  // `test/panesetup.test.ts` contains "test" in its directory AND in its name.
  // Scoring `indexOf`'s first hit would grade it as a directory match and sink it
  // below a file merely living under test/ — collapsing the rule above on exactly
  // the paths where it matters. Both of these are real repo paths.
  const ranked = rankFileNames(["test/grid.ts", "test/panesetup.test.ts"], "test", 10);
  assert.equal(ranked[0].rel, "test/panesetup.test.ts");
});

test("a name-prefix match beats a mid-word match", () => {
  // Every candidate contains "restore", but panerestore.ts has it mid-word while
  // session-restore.md has it at a segment boundary — and .ts files starting with
  // the term would beat both. Pin the specific rule: segment start scores higher.
  const ranked = rankFileNames(["src/panerestore.ts", "doc/restore.md"], "restore", 10);
  assert.equal(ranked[0].rel, "doc/restore.md", "a name that STARTS with the term wins");
});

test("space-separated terms are AND-ed across the whole path", () => {
  // The useful half of fuzzy matching, without the noise: two substrings, both of
  // which must appear somewhere in the path.
  assert.deepEqual(rels("pane rest"), ["src/panerestore.ts", "test/panerestore.test.ts"]);
  assert.deepEqual(rels("test pane fit"), [], "one absent term disqualifies the path");
});

test("a term absent from a path disqualifies it — matching is AND, not OR", () => {
  const ranked = rels("pane grid");
  assert.deepEqual(ranked, [], "no path contains both 'pane' and 'grid'");
});

test("matching is case-insensitive", () => {
  assert.deepEqual(rels("README"), ["README.md"]);
  assert.deepEqual(rels("readme"), ["README.md"]);
  assert.deepEqual(rels("ReAdMe"), ["README.md"]);
});

test("a blank query filters nothing — it returns NO results, not all of them", () => {
  // The box is a filter over the tree; an empty filter means "go back to the tree",
  // not "dump all 20,000 paths into the result list".
  assert.deepEqual(rankFileNames(FILES, "", 20), []);
  assert.deepEqual(rankFileNames(FILES, "   ", 20), []);
});

test("results are capped at the limit, keeping the BEST ones", () => {
  const ranked = rankFileNames(FILES, "pane", 2);
  assert.equal(ranked.length, 2);
  assert.equal(ranked[0].rel, "src/pane.ts", "the cap must not cost us the best hit");
});

test("ties break deterministically: shorter path, then alphabetical", () => {
  // Enumeration order differs between `git ls-files` and the walk, so it must not
  // leak into the result order — the same query must always rank the same way.
  const a = rankFileNames(["b/x/thing.ts", "a/thing.ts", "c/thing.ts"], "thing.ts", 10);
  assert.deepEqual(
    a.map((h) => h.rel),
    ["a/thing.ts", "c/thing.ts", "b/x/thing.ts"],
    "shortest first, then alphabetical",
  );
  // Same set, reversed input: identical output.
  const b = rankFileNames(["c/thing.ts", "a/thing.ts", "b/x/thing.ts"], "thing.ts", 10);
  assert.deepEqual(a.map((h) => h.rel), b.map((h) => h.rel));
});

test("hit ranges mark what matched, merged and non-overlapping", () => {
  const [hit] = rankFileNames(["src/panerestore.ts"], "pane rest", 1);
  // "pane" at 4..8 and "rest" at 8..12 are adjacent → one merged span, so the view
  // can't double-wrap a character when it paints the highlight.
  assert.deepEqual(hit.ranges, [[4, 12]]);
});

test("overlapping term spans merge instead of nesting", () => {
  // "ane" (5..8) sits inside "pane" (4..8): one span out, not two.
  const [hit] = rankFileNames(["src/pane.ts"], "pane ane", 1);
  assert.deepEqual(hit.ranges, [[4, 8]]);
});

// ---------- helpers ----------

test("queryTerms lowercases and drops blank runs", () => {
  assert.deepEqual(queryTerms("  Pane   REST "), ["pane", "rest"]);
  assert.deepEqual(queryTerms("   "), []);
});

test("basenameStart finds the name after the last slash, or 0 with no directory", () => {
  assert.equal(basenameStart("src/a/pane.ts"), 6);
  assert.equal(basenameStart("README.md"), 0);
});

test("mergeRanges sorts, merges touching/overlapping spans, and leaves gaps alone", () => {
  assert.deepEqual(mergeRanges([[5, 8], [0, 3]]), [[0, 3], [5, 8]]);
  assert.deepEqual(mergeRanges([[0, 4], [2, 6]]), [[0, 6]]);
  assert.deepEqual(mergeRanges([[0, 4], [4, 6]]), [[0, 6]], "adjacent spans merge");
  assert.deepEqual(mergeRanges([]), []);
});

test("moveSelection wraps at both ends and is safe on an empty list", () => {
  assert.equal(moveSelection(0, 1, 3), 1);
  assert.equal(moveSelection(2, 1, 3), 0, "Down from the last result wraps to the first");
  assert.equal(moveSelection(0, -1, 3), 2, "Up from the first wraps to the last");
  assert.equal(moveSelection(5, 1, 0), 0, "no results → no stale index");
});
