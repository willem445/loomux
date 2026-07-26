// matchShortcut (shortcuts.ts) — focused on the #379 tab-reorder bindings
// added alongside the existing next/prev-tab bracket keys, and the
// modifier-set boundaries that keep them from colliding.
import { test } from "node:test";
import assert from "node:assert/strict";
import { matchShortcut } from "../src/shortcuts.ts";

function evt(overrides: Partial<KeyboardEvent> & { code: string }): KeyboardEvent {
  return {
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    ...overrides,
  } as KeyboardEvent;
}

test("Ctrl+Shift+Alt+BracketRight moves the active tab right", () => {
  assert.equal(
    matchShortcut(evt({ ctrlKey: true, shiftKey: true, altKey: true, code: "BracketRight" })),
    "move-tab-right"
  );
});

test("Ctrl+Shift+Alt+BracketLeft moves the active tab left", () => {
  assert.equal(
    matchShortcut(evt({ ctrlKey: true, shiftKey: true, altKey: true, code: "BracketLeft" })),
    "move-tab-left"
  );
});

test("Ctrl+Shift+BracketRight (no Alt) is still plain next-tab, not move", () => {
  assert.equal(matchShortcut(evt({ ctrlKey: true, shiftKey: true, code: "BracketRight" })), "next-tab");
});

test("Ctrl+Shift+BracketLeft (no Alt) is still plain prev-tab, not move", () => {
  assert.equal(matchShortcut(evt({ ctrlKey: true, shiftKey: true, code: "BracketLeft" })), "prev-tab");
});

test("Alt+BracketRight alone (no Ctrl+Shift) matches nothing", () => {
  assert.equal(matchShortcut(evt({ altKey: true, code: "BracketRight" })), null);
});
