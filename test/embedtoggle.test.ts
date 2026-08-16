// Unit tests for the overlay-toggle disposition (#361 user-demo finding): a
// docked view's toggle button/keybinding must no-op, regardless of the
// view's current visibility — see embedtoggle.ts's own doc comment for why
// this is disabled outright rather than fixed to correctly close/reopen a
// docked view. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  CWD_DECLARING_VIEWS,
  embedToggleAction,
  toggleDeclaresCwd,
  type EmbedToggleAction,
} from "../src/embedtoggle.ts";

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

// ---------- #1042: which toggle gestures DECLARE the pane's cwd ----------
//
// The rule the human approved: a view-OPEN gesture on a pane's cwd is a human
// declaring a root; nothing else is. The cwd itself arrives via OSC-7, emitted
// by whatever process runs in the pane, so it is agent-controllable and
// resolve-only.
//
// This shipped WRONG in #1092's first round — the declaration was made in the
// `toggleXView` wrappers, before `toggleView` had decided what the gesture was,
// so close and the docked no-op declared too. An agent that had `cd`'d to an
// interesting directory got it permanently declared the moment the human
// DISMISSED a panel. These tests pin both halves of the fixed decision.

const ALL_ACTIONS: readonly EmbedToggleAction[] = ["open", "close", "noop"];
// Every `EmbedKind` in pane.ts. Listed literally rather than imported, so this
// stays a DOM-free unit — and so a kind added to the union without a decision
// here shows up as a gap a reader can see.
const ALL_KINDS = [
  "tasks",
  "decisions",
  "git",
  "issues",
  "audit",
  "group",
  "editor",
  "timeline",
] as const;

test("a view-OPEN gesture on a cwd-reading view declares the cwd", () => {
  for (const kind of CWD_DECLARING_VIEWS) {
    assert.equal(toggleDeclaresCwd(kind, "open"), true, `${kind} + open must declare`);
  }
  // The set is non-empty — without this, a `CWD_DECLARING_VIEWS` emptied by a
  // bad edit would make this test vacuously pass.
  assert.ok(CWD_DECLARING_VIEWS.length > 0, "the declaring set must not be empty");
});

test("CLOSE never declares — a dismissal is not a human asking for anything", () => {
  // The regression this exists for. Before the fix, closing the git view on a
  // pane whose agent had `cd`'d outside every declared root DECLARED that
  // directory, permanently, for the rest of the process.
  for (const kind of CWD_DECLARING_VIEWS) {
    assert.equal(toggleDeclaresCwd(kind, "close"), false, `${kind} + close must NOT declare`);
  }
});

test("the docked NO-OP never declares — the gesture did not even do anything", () => {
  // Sharper than the close case: a docked view's toggle is refused outright
  // (`embedToggleAction`), so the human saw a toast and no state changed. A
  // declaration would be the only lasting effect of a gesture that was refused.
  for (const kind of CWD_DECLARING_VIEWS) {
    assert.equal(toggleDeclaresCwd(kind, "noop"), false, `${kind} + noop must NOT declare`);
  }
});

test("a view that does not read the cwd never declares, in any direction", () => {
  // The negative control on the OTHER axis.
  // `tasks`/`decisions`/`audit`/`group`/`timeline` are group-scoped — their repo
  // was declared when the group was created — so opening one says nothing about
  // this pane's cwd. Without this, a `toggleDeclaresCwd` that ignored `kind`
  // entirely would pass every assertion above.
  //
  // `decisions` (#1091) is the first kind added since this guard was written,
  // and it went in WITHOUT being added here — so the guard silently stopped
  // covering the newest panel while still passing, which is the one thing the
  // `ALL_KINDS` comment above promises cannot happen. It fails closed
  // (`CWD_DECLARING_VIEWS` is git/issues/editor), which is the correct decision
  // for a group-scoped panel, and now that decision is pinned rather than
  // merely true.
  const nonDeclaring = ALL_KINDS.filter((k) => !(CWD_DECLARING_VIEWS as readonly string[]).includes(k));
  assert.deepEqual(nonDeclaring, ["tasks", "decisions", "audit", "group", "timeline"]);
  for (const kind of nonDeclaring) {
    for (const action of ALL_ACTIONS) {
      assert.equal(toggleDeclaresCwd(kind, action), false, `${kind} + ${action} must NOT declare`);
    }
  }
});

test("an unknown view kind declares nothing — fail closed", () => {
  // A view added later must declare nothing until someone puts it in
  // `CWD_DECLARING_VIEWS` on purpose. Fail-open here would mean every future
  // panel silently acquired an admit path.
  for (const action of ALL_ACTIONS) {
    assert.equal(toggleDeclaresCwd("a-view-invented-tomorrow", action), false);
  }
});
