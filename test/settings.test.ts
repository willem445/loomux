// Durable app-settings encode/decode (#370) — settings.ts. Mirrors
// tabstore.test.ts's shape: round-trip, defaults on absence, and per-key
// fallback so a malformed or partial hand-edit degrades gracefully rather
// than losing the whole file.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  encodeSettings,
  decodeSettings,
  DEFAULT_SETTINGS,
  MAX_RENDER_THROTTLE_MS,
  type AppSettings,
} from "../src/settings.ts";

test("round-trips a non-default value", () => {
  const s: AppSettings = { pasteOnPlainCtrlV: false, unfocusedRenderThrottleMs: 250 };
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
    ...DEFAULT_SETTINGS,
    pasteOnPlainCtrlV: false,
  });
});

// ---------- unfocusedRenderThrottleMs (#720) ----------

test("unfocusedRenderThrottleMs: 0 is honoured — it is the documented off switch, not a missing key", () => {
  // The load-bearing one. `0` disables the render throttle entirely, which is
  // how the human A/Bs the change; reading it as "absent, use the default"
  // would make the off switch silently do nothing.
  assert.equal(decodeSettings('{"unfocusedRenderThrottleMs":0}')?.unfocusedRenderThrottleMs, 0);
});

test("a negative unfocusedRenderThrottleMs takes the DEFAULT, not 0", () => {
  // A typo must not be read as "off" — that would disable the feature while
  // looking like it was never implemented.
  assert.equal(
    decodeSettings('{"unfocusedRenderThrottleMs":-1}')?.unfocusedRenderThrottleMs,
    DEFAULT_SETTINGS.unfocusedRenderThrottleMs
  );
});

test("an absurd unfocusedRenderThrottleMs is clamped rather than honoured", () => {
  assert.equal(
    decodeSettings('{"unfocusedRenderThrottleMs":10000}')?.unfocusedRenderThrottleMs,
    MAX_RENDER_THROTTLE_MS
  );
});

test("a non-numeric or non-finite unfocusedRenderThrottleMs falls back per-key", () => {
  for (const raw of ['"100"', "null", "true", "1e999"]) {
    assert.equal(
      decodeSettings(`{"pasteOnPlainCtrlV":false,"unfocusedRenderThrottleMs":${raw}}`)
        ?.unfocusedRenderThrottleMs,
      DEFAULT_SETTINGS.unfocusedRenderThrottleMs,
      `raw ${raw} should fall back`
    );
    assert.equal(
      decodeSettings(`{"pasteOnPlainCtrlV":false,"unfocusedRenderThrottleMs":${raw}}`)?.pasteOnPlainCtrlV,
      false,
      `raw ${raw} must not invalidate the sibling key`
    );
  }
});
