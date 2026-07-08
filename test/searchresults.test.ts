// Unit tests for the pure search-results display model (issue #174). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  groupMatches,
  countSummary,
  toggleFile,
  setAll,
  selectedFiles,
  selectedMatchCount,
  paramsEqual,
  type SearchParams,
} from "../src/searchresults.ts";
import type { SearchMatch } from "../src/fileapi.ts";

const m = (rel: string, line: number): SearchMatch => ({
  rel,
  line,
  col: 1,
  line_text: `line ${line}`,
});

test("groupMatches groups by file, preserves first-seen order, selects all", () => {
  const groups = groupMatches([m("b.ts", 1), m("a.ts", 2), m("b.ts", 5)]);
  assert.deepEqual(groups.map((g) => g.rel), ["b.ts", "a.ts"]);
  assert.equal(groups[0].matches.length, 2);
  assert.ok(groups.every((g) => g.selected));
});

test("countSummary reports files and total matches", () => {
  const groups = groupMatches([m("a", 1), m("a", 2), m("b", 3)]);
  assert.deepEqual(countSummary(groups), { files: 2, matches: 3 });
});

test("toggleFile flips one file's selection and the preview reflects it", () => {
  let groups = groupMatches([m("a", 1), m("b", 2), m("b", 3)]);
  groups = toggleFile(groups, "b");
  assert.deepEqual(selectedFiles(groups), ["a"]);
  assert.equal(selectedMatchCount(groups), 1, "deselecting b drops its 2 matches");
  // Unknown file is a no-op.
  const same = toggleFile(groups, "zzz");
  assert.deepEqual(selectedFiles(same), ["a"]);
});

test("setAll selects or clears everything", () => {
  const groups = groupMatches([m("a", 1), m("b", 2)]);
  assert.deepEqual(selectedFiles(setAll(groups, false)), []);
  assert.deepEqual(selectedFiles(setAll(groups, true)), ["a", "b"]);
});

test("paramsEqual detects a changed query or option (preview-vs-apply guard)", () => {
  const base: SearchParams = { query: "foo", caseInsensitive: false, wholeWord: false };
  assert.equal(paramsEqual(base, { ...base }), true);
  // Any single change makes them unequal, so the stale preview is invalidated
  // before a replace can apply divergent params (finding #1).
  assert.equal(paramsEqual(base, { ...base, query: "bar" }), false);
  assert.equal(paramsEqual(base, { ...base, caseInsensitive: true }), false);
  assert.equal(paramsEqual(base, { ...base, wholeWord: true }), false);
});

test("edge: zero matches and all-deselected", () => {
  assert.deepEqual(groupMatches([]), []);
  assert.deepEqual(countSummary([]), { files: 0, matches: 0 });
  const cleared = setAll(groupMatches([m("a", 1)]), false);
  assert.deepEqual(selectedFiles(cleared), []);
  assert.equal(selectedMatchCount(cleared), 0);
});
