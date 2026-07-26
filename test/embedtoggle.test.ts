// Unit tests for the overlay-toggle disposition (#361 user-demo finding): a
// docked view's toggle button/keybinding must no-op, regardless of the
// view's current visibility — see embedtoggle.ts's own doc comment for why
// this is disabled outright rather than fixed to correctly close/reopen a
// docked view. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { embedToggleAction } from "../src/embedtoggle.ts";

test("docked + visible: no-op, not close — the bug this fixes", () => {
  // The exact shape of the user-demo bug: a docked, open view's overlay
  // toggle used to close/reparent it, leaving the slot's panel visible with
  // nothing inside. Docked now wins outright.
  assert.equal(embedToggleAction(true, true), "noop");
});

test("docked + not visible: still no-op, not open", () => {
  // A docked-but-closed state can no longer be reached through THIS guard,
  // but the decision must still be defensively correct if one existed —
  // docking is what drives a docked view's visibility now, not the toggle.
  assert.equal(embedToggleAction(true, false), "noop");
});

test("not docked + visible: closes, exactly the pre-#361 overlay behavior", () => {
  assert.equal(embedToggleAction(false, true), "close");
});

test("not docked + not visible: opens, exactly the pre-#361 overlay behavior", () => {
  assert.equal(embedToggleAction(false, false), "open");
});
