// Unit tests for the pure workspace-highlight regex builder (issue #174). The
// rest of editorwidget.ts is DOM/CodeMirror wiring (human-validated); this one
// helper decides which occurrences light up inside the open file, so its
// escaping / whole-word / case behaviour is pinned here. editorwidget.ts has no
// static imports (CodeMirror is loaded lazily inside functions), so importing it
// under node:test is safe.
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildHighlightRegex } from "../src/editorwidget.ts";

test("empty query yields no highlighter", () => {
  assert.equal(buildHighlightRegex("", false, false), null);
});

test("plain query matches literally and globally", () => {
  const re = buildHighlightRegex("equal", false, false)!;
  assert.equal(re.flags, "g");
  assert.deepEqual("assert.equal(equal, x)".match(re), ["equal", "equal"]);
});

test("case-insensitive adds the i flag", () => {
  const re = buildHighlightRegex("equal", true, false)!;
  assert.ok(re.flags.includes("i"));
  assert.deepEqual("Equal EQUAL equal".match(re), ["Equal", "EQUAL", "equal"]);
});

test("whole-word only matches standalone words", () => {
  const re = buildHighlightRegex("foo", false, true)!;
  assert.equal("foobar foo_ foo".match(re)?.length, 1); // only the bare "foo"
  assert.equal("foobar".match(re), null);
});

test("regex metacharacters in the query are escaped (literal search)", () => {
  const re = buildHighlightRegex("a.b", false, false)!;
  assert.deepEqual("a.b".match(re), ["a.b"]);
  assert.equal("axb".match(re), null, "the dot must be literal, not any-char");
});
