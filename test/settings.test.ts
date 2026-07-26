// Durable app-settings encode/decode (#370) — settings.ts. Mirrors
// tabstore.test.ts's shape: round-trip, defaults on absence, and per-key
// fallback so a malformed or partial hand-edit degrades gracefully rather
// than losing the whole file.
import { test } from "node:test";
import assert from "node:assert/strict";
import { encodeSettings, decodeSettings, DEFAULT_SETTINGS, type AppSettings } from "../src/settings.ts";

test("round-trips a non-default value", () => {
  const s: AppSettings = { pasteOnPlainCtrlV: false };
  assert.deepEqual(decodeSettings(encodeSettings(s)), s);
});

test("round-trips the default value", () => {
  assert.deepEqual(decodeSettings(encodeSettings(DEFAULT_SETTINGS)), DEFAULT_SETTINGS);
});

test("null (first run) decodes to null, not a thrown error", () => {
  assert.equal(decodeSettings(null), null);
});

test("invalid JSON decodes to null rather than throwing", () => {
  assert.equal(decodeSettings("{ not json"), null);
});

test("a non-object JSON value decodes to null", () => {
  assert.equal(decodeSettings("42"), null);
  assert.equal(decodeSettings("null"), null);
  assert.equal(decodeSettings('"a string"'), null);
});

test("a wrong-typed pasteOnPlainCtrlV falls back to the default, not the whole file", () => {
  assert.deepEqual(decodeSettings('{"pasteOnPlainCtrlV":"yes"}'), DEFAULT_SETTINGS);
});

test("an empty object decodes to all defaults (a hand-edit that clears the file to {})", () => {
  assert.deepEqual(decodeSettings("{}"), DEFAULT_SETTINGS);
});

test("unknown extra keys are ignored rather than rejecting the file", () => {
  assert.deepEqual(decodeSettings('{"pasteOnPlainCtrlV":false,"someFutureKey":123}'), {
    pasteOnPlainCtrlV: false,
  });
});
