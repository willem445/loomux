// Pure paste/copy gesture decisions for terminal panes (#370) — pasteflow.ts.
// Pins the key matching (plain Ctrl+V pastes only when the pasteOnPlainCtrlV
// setting allows it, Ctrl+Shift+V always pastes, Ctrl+C never copies, AltGr
// (Ctrl+Alt+V) is never eaten as a paste) and the right-click menu shape
// (Copy disabled without a selection, Paste always live).
import { test } from "node:test";
import assert from "node:assert/strict";
import { isPasteKey, isCopyKey, buildTerminalMenu, type PasteKeyEvent } from "../src/pasteflow.ts";

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

test("terminal menu: Copy is disabled with a reason when nothing is selected", () => {
  const items = buildTerminalMenu(false);
  const copy = items.find((i) => i.action?.kind === "copy")!;
  assert.equal(copy.disabled, true);
  assert.ok(copy.reason);
});

test("terminal menu: Copy is enabled once there is a selection", () => {
  const items = buildTerminalMenu(true);
  const copy = items.find((i) => i.action?.kind === "copy")!;
  assert.equal(copy.disabled, false);
});

test("terminal menu: Paste is always offered and never disabled", () => {
  for (const hasSelection of [false, true]) {
    const paste = buildTerminalMenu(hasSelection).find((i) => i.action?.kind === "paste")!;
    assert.ok(paste);
    assert.ok(!paste.disabled);
  }
});
