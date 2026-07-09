// Unit tests for the in-file find widget's pure logic (issue #174): regex build
// from the toggle state, the "n of m" match count, and its formatting. The panel
// DOM is human-validated; this pins the logic. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildSearchRegex,
  buildHighlightRegex,
  matchInfo,
  formatMatchCount,
  type FindFlags,
} from "../src/findwidget.ts";

const F = (o: Partial<FindFlags> = {}): FindFlags => ({
  caseSensitive: false,
  wholeWord: false,
  regexp: false,
  ...o,
});

test("buildSearchRegex escapes literals but honors regexp mode", () => {
  // Literal: the dot is escaped, so "a.b" only matches "a.b".
  const lit = buildSearchRegex("a.b", F())!;
  assert.deepEqual("a.b axb".match(lit), ["a.b"]);
  // Regexp mode: the dot is a wildcard.
  const re = buildSearchRegex("a.b", F({ regexp: true }))!;
  assert.deepEqual("a.b axb".match(re), ["a.b", "axb"]);
  // Empty query and invalid regex both yield null (never throw).
  assert.equal(buildSearchRegex("", F()), null);
  assert.equal(buildSearchRegex("(", F({ regexp: true })), null);
});

test("buildSearchRegex applies case + whole-word flags", () => {
  assert.equal("Foo".match(buildSearchRegex("foo", F())!)?.length, 1); // case-insensitive default
  assert.equal("Foo".match(buildSearchRegex("foo", F({ caseSensitive: true }))!), null);
  const ww = buildSearchRegex("foo", F({ wholeWord: true }))!;
  assert.equal("foobar foo".match(ww)?.length, 1); // only the standalone word
});

test("matchInfo counts matches and marks the one at the selection", () => {
  const text = "ab ab ab"; // matches at 0, 3, 6
  const re = buildSearchRegex("ab", F());
  assert.deepEqual(matchInfo(text, re, 3), { count: 3, current: 2 }); // caret on 2nd
  assert.deepEqual(matchInfo(text, re, 0), { count: 3, current: 1 });
  assert.deepEqual(matchInfo(text, re, 1), { count: 3, current: 0 }); // not on a match
  assert.deepEqual(matchInfo(text, null, 0), { count: 0, current: 0 });
});

test("matchInfo terminates on a zero-width regexp match", () => {
  const re = buildSearchRegex("x*", F({ regexp: true }));
  // Must not infinite-loop; count is finite.
  const info = matchInfo("abc", re, 0);
  assert.ok(Number.isFinite(info.count));
});

test("buildHighlightRegex (workspace highlight) is literal + case/whole-word", () => {
  // Literal always (no regexp mode): the dot stays literal.
  assert.deepEqual("a.b axb".match(buildHighlightRegex("a.b", false, false)!), ["a.b"]);
  // ci=true → case-insensitive; ci=false → case-sensitive.
  assert.equal("Equal".match(buildHighlightRegex("equal", true, false)!)?.length, 1);
  assert.equal("Equal".match(buildHighlightRegex("equal", false, false)!), null);
  // whole-word wraps in boundaries.
  assert.equal("foobar foo".match(buildHighlightRegex("foo", false, true)!)?.length, 1);
  assert.equal(buildHighlightRegex("", false, false), null);
});

test("formatMatchCount renders the states", () => {
  assert.equal(formatMatchCount("", { count: 0, current: 0 }), "");
  assert.equal(formatMatchCount("q", { count: 0, current: 0 }), "No results");
  assert.equal(formatMatchCount("q", { count: 6, current: 0 }), "6 found");
  assert.equal(formatMatchCount("q", { count: 6, current: 3 }), "3 of 6");
});
