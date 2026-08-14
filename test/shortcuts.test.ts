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

// --- the progress timeline's binding (#608) --------------------------------

test("Alt+W toggles the progress timeline", () => {
  assert.equal(matchShortcut(evt({ altKey: true, code: "KeyW" })), "toggle-timeline");
});

test("Alt+W does not disturb the audit log's own Alt+A", () => {
  // The two views are siblings on the same panes; a copy-paste that pointed
  // both at one action would be invisible until someone pressed Alt+A.
  assert.equal(matchShortcut(evt({ altKey: true, code: "KeyA" })), "toggle-audit");
});

test("Ctrl+Shift+W is still close-pane, not the timeline", () => {
  // W is now bound under two different modifier sets; the modifier guards are
  // what keep them apart, and close-pane is the destructive one.
  assert.equal(matchShortcut(evt({ ctrlKey: true, shiftKey: true, code: "KeyW" })), "close-pane");
});

// --- autosize (#936) -------------------------------------------------------

test("Ctrl+Shift+A autosizes the panes", () => {
  assert.equal(matchShortcut(evt({ ctrlKey: true, shiftKey: true, code: "KeyA" })), "autosize-panes");
});

test("plain Ctrl+A is NOT taken — it is the shell's and the agent's start-of-line", () => {
  // Claude Code's interactive-mode reference documents `Ctrl+A` as "Move cursor
  // to start of current line", and readline binds it the same way. Autosize
  // rides the SHIFTED chord precisely so that one keeps reaching the pane; a
  // guard that let Ctrl+A through to this action would be a silent regression
  // inside every agent and shell pane.
  assert.equal(matchShortcut(evt({ ctrlKey: true, code: "KeyA" })), null);
  assert.equal(matchShortcut(evt({ code: "KeyA" })), null);
});

test("Alt+A still opens the audit log, and Ctrl+Shift+Alt+A is nobody's", () => {
  // A is now bound under two modifier sets on the same panes — the same
  // collision the timeline/audit pair above guards against.
  assert.equal(matchShortcut(evt({ altKey: true, code: "KeyA" })), "toggle-audit");
  assert.equal(
    matchShortcut(evt({ ctrlKey: true, shiftKey: true, altKey: true, code: "KeyA" })),
    null
  );
});

test("plain W, Ctrl+W and Alt+Shift+W are not the timeline (Ctrl+W is the shell's kill-word)", () => {
  assert.equal(matchShortcut(evt({ code: "KeyW" })), null);
  // Ctrl+W must keep reaching the shell: it is readline's unix-word-rubout,
  // and swallowing it inside an agent pane would be a real regression.
  assert.equal(matchShortcut(evt({ ctrlKey: true, code: "KeyW" })), null);
  assert.equal(matchShortcut(evt({ altKey: true, shiftKey: true, code: "KeyW" })), null);
});
