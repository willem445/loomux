// Pure paste/copy gesture decisions for terminal panes (#370) — pasteflow.ts.
// Pins the key matching (plain Ctrl+V now pastes, Ctrl+C never copies) and the
// right-click menu shape (Copy disabled without a selection, Paste always live).
import { test } from "node:test";
import assert from "node:assert/strict";
import { isPasteKey, isCopyKey, buildTerminalMenu, type PasteKeyEvent } from "../src/pasteflow.ts";

const key = (overrides: Partial<PasteKeyEvent>): PasteKeyEvent => ({
  ctrlKey: false,
  shiftKey: false,
  code: "",
  ...overrides,
});

test("plain Ctrl+V is a paste (#370 — the gesture nearly everyone reaches for first)", () => {
  assert.equal(isPasteKey(key({ ctrlKey: true, code: "KeyV" })), true);
});

test("Ctrl+Shift+V is still a paste (Windows Terminal convention, kept)", () => {
  assert.equal(isPasteKey(key({ ctrlKey: true, shiftKey: true, code: "KeyV" })), true);
});

test("Shift+V alone (no Ctrl) is not a paste", () => {
  assert.equal(isPasteKey(key({ shiftKey: true, code: "KeyV" })), false);
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
