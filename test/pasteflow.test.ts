// Pure paste/copy keydown gesture decisions for terminal panes (#370) —
// pasteflow.ts. Pins the key matching (plain Ctrl+V pastes only when the
// pasteOnPlainCtrlV setting allows it, Ctrl+Shift+V always pastes, plain
// Ctrl+C copies only WITH a selection — else it must stay SIGINT,
// AltGr/Ctrl+Alt+V is never eaten as a paste) and the keyDisposition enum
// that drives pane.ts's preventDefault() calls.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  isPasteKey,
  isCopyKey,
  isConditionalCopyKey,
  keyDisposition,
  type PasteKeyEvent,
} from "../src/pasteflow.ts";

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

test("Ctrl+Shift+C is the explicit copy key", () => {
  assert.equal(isCopyKey(key({ ctrlKey: true, shiftKey: true, code: "KeyC" })), true);
});

test("plain Ctrl+C does not match the EXPLICIT copy key (isConditionalCopyKey owns it instead)", () => {
  assert.equal(isCopyKey(key({ ctrlKey: true, code: "KeyC" })), false);
});

test("plain Ctrl+C matches the CONDITIONAL copy key", () => {
  assert.equal(isConditionalCopyKey(key({ ctrlKey: true, code: "KeyC" })), true);
});

test("Ctrl+Shift+C does not match the conditional matcher — isCopyKey owns it", () => {
  assert.equal(isConditionalCopyKey(key({ ctrlKey: true, shiftKey: true, code: "KeyC" })), false);
});

test("Ctrl+Alt+C does not match the conditional matcher either (mirrors the paste-side AltGr guard)", () => {
  assert.equal(isConditionalCopyKey(key({ ctrlKey: true, altKey: true, code: "KeyC" })), false);
});

// ---------- keyDisposition (#402 review: the DOM layer must preventDefault
// on every disposition except "pass" — see pasteflow.ts's own doc comment
// for the double-paste bug this collapsing-to-one-enum exists to prevent) ----------

test("keyDisposition: Ctrl+Shift+C is 'copy' regardless of selection (explicit gesture, harmless no-op without one)", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, shiftKey: true, code: "KeyC" }), true, false), "copy");
  assert.equal(keyDisposition(key({ ctrlKey: true, shiftKey: true, code: "KeyC" }), true, true), "copy");
});

test("keyDisposition: plain Ctrl+C with a selection is 'copy' (#402 third round — this is the fix)", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyC" }), true, true), "copy");
});

test("keyDisposition: plain Ctrl+C with NO selection is 'pass' — CRITICAL, this is what keeps SIGINT reachable", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyC" }), true, false), "pass");
});

test("keyDisposition: plain Ctrl+V is 'paste' when the setting allows it", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyV" }), true, false), "paste");
});

test("keyDisposition: plain Ctrl+V is 'pass' when the setting is off", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyV" }), false, false), "pass");
});

test("keyDisposition: Ctrl+Shift+V is 'paste' regardless of the setting", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, shiftKey: true, code: "KeyV" }), false, false), "paste");
});

test("keyDisposition: an unrelated key is 'pass'", () => {
  assert.equal(keyDisposition(key({ ctrlKey: true, code: "KeyA" }), true, true), "pass");
});

// ---------- pane-kind/selection matrix (#402 third round) ----------
//
// keyDisposition takes NO pane-kind input at all — there is nothing in its
// signature to distinguish "plain terminal pane" from "agent pane" from
// "orchestrator pane". These tests pin that directly: a plain terminal
// pane's keydown and an agent pane's keydown, built from identical
// (event, setting, selection) inputs, are passed through the exact same
// call and MUST produce the exact same disposition — there is no branch
// left anywhere for the two to diverge on. The bug this exists to catch:
// copy appearing to work in one pane kind and not another despite there
// being no pane-kind-aware code in this module or in pane.ts's wiring.

interface PaneLikeInput {
  label: string;
  e: PasteKeyEvent;
  hasSelection: boolean;
}

const PANE_KINDS: readonly PaneLikeInput[] = [
  { label: "plain terminal pane", e: key({ ctrlKey: true, code: "KeyC" }), hasSelection: true },
  { label: "agent pane", e: key({ ctrlKey: true, code: "KeyC" }), hasSelection: true },
];

test("pane-kind matrix: Ctrl+C with a selection is 'copy' in every pane kind, identically", () => {
  const dispositions = PANE_KINDS.map((p) => keyDisposition(p.e, true, p.hasSelection));
  assert.deepEqual(dispositions, ["copy", "copy"]);
});

test("pane-kind matrix: Ctrl+C with NO selection passes through as interrupt in every pane kind, identically", () => {
  const noSelection = PANE_KINDS.map((p) => ({ ...p, hasSelection: false }));
  const dispositions = noSelection.map((p) => keyDisposition(p.e, true, p.hasSelection));
  assert.deepEqual(dispositions, ["pass", "pass"]);
});
