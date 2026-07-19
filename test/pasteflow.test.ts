// Pure paste/copy keydown gesture decisions for terminal panes (#370) —
// pasteflow.ts. Pins the key matching (plain Ctrl+V pastes only when the
// pasteOnPlainCtrlV setting allows it, Ctrl+Shift+V always pastes, Ctrl+C
// never copies, AltGr/Ctrl+Alt+V is never eaten as a paste) and the
// keyDisposition enum that drives pane.ts's preventDefault() calls.
import { test } from "node:test";
import assert from "node:assert/strict";
import { isPasteKey, isCopyKey, keyDisposition, type PasteKeyEvent } from "../src/pasteflow.ts";

const key = (overrides: Partial<PasteKeyEvent>): PasteKeyEvent => ({
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  code: "",
  ...overrides,
});

test("plain Ctrl+V pastes when the setting allows it (#370 — the gesture nearly everyone reaches for first)", () => {
  assert.equal(isPasteKey(key({ ctrlKey: true, code: "KeyV" }), true), true);
});

test("plain Ctrl+V passes through to the pane when the setting is off (vim VISUAL BLOCK / readline quoted-insert)", () => {
  assert.equal(isPasteKey(key({ ctrlKey: true, code: "KeyV" }), false), false);
});

test("Ctrl+Shift+V always pastes, regardless of the setting", () => {
  assert.equal(isPasteKey(key({ ctrlKey: true, shiftKey: true, code: "KeyV" }), true), true);
  assert.equal(isPasteKey(key({ ctrlKey: true, shiftKey: true, code: "KeyV" }), false), true);
});

test("Shift+V alone (no Ctrl) is not a paste", () => {
  assert.equal(isPasteKey(key({ shiftKey: true, code: "KeyV" }), true), false);
});

test("Ctrl+Alt+V (AltGr on many layouts) is never a paste, even with the setting on", () => {
  assert.equal(isPasteKey(key({ ctrlKey: true, altKey: true, code: "KeyV" }), true), false);
});

test("Ctrl+Shift+Alt+V is not a paste either — Alt held always defers to the pane", () => {
  assert.equal(isPasteKey(key({ ctrlKey: true, shiftKey: true, altKey: true, code: "KeyV" }), true), false);
});

test("Ctrl+C alone is never a copy — it must stay SIGINT", () => {
  assert.equal(isCopyKey(key({ ctrlKey: true, code: "KeyC" })), false);
});

test("Ctrl+Shift+C is a copy", () => {
  assert.equal(isCopyKey(key({ ctrlKey: true, shiftKey: true, code: "KeyC" })), true);
});

// ---------- keyDisposition (#402 review: the DOM layer must preventDefault
// on every disposition except "pass" — see pasteflow.ts's own doc comment
// for the double-paste bug this collapsing-to-one-enum exists to prevent) ----------

test("keyDisposition: Ctrl+Shift+C is 'copy'", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, shiftKey: true, code: "KeyC" }), true), "copy");
});

test("keyDisposition: plain Ctrl+C is 'pass' (stays SIGINT)", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyC" }), true), "pass");
});

test("keyDisposition: plain Ctrl+V is 'paste' when the setting allows it", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyV" }), true), "paste");
});

test("keyDisposition: plain Ctrl+V is 'pass' when the setting is off", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyV" }), false), "pass");
});

test("keyDisposition: Ctrl+Shift+V is 'paste' regardless of the setting", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, shiftKey: true, code: "KeyV" }), false), "paste");
});

test("keyDisposition: an unrelated key is 'pass'", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyA" }), true), "pass");
});
