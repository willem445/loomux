// Unit tests for the line-ending helpers (issue #174). Run with `npm test`.
// These pin the fix for the demo bug where opening a CRLF file and touching
// nothing tripped the "discard unsaved changes" warning.
import { test } from "node:test";
import assert from "node:assert/strict";
import { detectEol, stripCr, applyEol, textDiffers } from "../src/eol.ts";

test("detectEol picks CRLF only when a CRLF is present", () => {
  assert.equal(detectEol("a\r\nb"), "\r\n");
  assert.equal(detectEol("a\nb"), "\n");
  assert.equal(detectEol("no newline"), "\n");
});

test("stripCr collapses CRLF to LF", () => {
  assert.equal(stripCr("a\r\nb\r\nc"), "a\nb\nc");
  assert.equal(stripCr("a\nb"), "a\nb");
});

test("applyEol round-trips a document back to its on-disk ending", () => {
  const crlf = "line1\r\nline2\r\n";
  assert.equal(applyEol(stripCr(crlf), detectEol(crlf)), crlf);
  const lf = "line1\nline2\n";
  assert.equal(applyEol(stripCr(lf), detectEol(lf)), lf);
});

test("regression: open a CRLF file, make no edit, and it stays clean", () => {
  // What the view does: it stores the RAW disk text (CRLF) as the baseline, and
  // the editor hands back LF text. An EOL-normalized compare must call this
  // NOT dirty — the bug was a raw compare flagging it dirty on load.
  const onDisk = "function f() {\r\n  return 1;\r\n}\r\n";
  const inEditor = stripCr(onDisk); // CodeMirror normalizes to LF
  assert.equal(textDiffers(onDisk, inEditor), false, "no edit → not dirty");
  // A real edit is still detected.
  const edited = inEditor.replace("return 1", "return 2");
  assert.equal(textDiffers(onDisk, edited), true, "an actual change → dirty");
});
