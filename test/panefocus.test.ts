// Unit tests for the new-pane focus decision (issue #117). The bug: a pane
// spawned programmatically by the orchestrator (MCP spawn_agent →
// orch-spawn-request → openPane) grabbed keyboard focus, pulling the cursor away
// from the pane the human was typing in. Focus must move to a new pane only when
// the human opened it directly — with the one exception that an empty grid still
// focuses so the app is never left without an active terminal. This pins that
// rule; grid.ts's DOM wiring is validated by hand. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  shouldFocusNewPane,
  shouldRestoreFocus,
  shouldPreserveMaximize,
} from "../src/panefocus.ts";

test("a human-initiated pane on a populated grid takes focus", () => {
  // Split button, launcher fleet, session restore, launching an orchestrator.
  assert.equal(shouldFocusNewPane(true, false), true);
});

test("an orchestrator-driven spawn on a populated grid does NOT take focus", () => {
  // The regression this fixes: the human is typing in another pane and the
  // cursor must stay put when the orchestrator spawns a worker.
  assert.equal(shouldFocusNewPane(false, false), false);
});

test("an orchestrator-driven spawn onto an empty grid still takes focus", () => {
  // No existing pane to leave focus on — focusing anyway is correct, or the app
  // would have no active terminal. (Not a real path today since the orchestrator
  // pane opens first, but the rule must be safe if it ever is.)
  assert.equal(shouldFocusNewPane(false, true), true);
});

test("a human-initiated pane onto an empty grid takes focus", () => {
  // The very first pane at startup.
  assert.equal(shouldFocusNewPane(true, true), true);
});

// --- focus-restore decision (issue #117 round 2) ---
// The live test failed: spawning an agent while the human typed in the steering
// box pulled focus away and their text went nowhere. Cause: inserting a pane
// restructures the grid DOM (renderSplit → replaceChildren), which detaches the
// focused subtree and blurs it to <body>. The caller snapshots focus before the
// relayout and restores it after — but only in the right cases. This pins them.

test("a background spawn restores focus to the human's input", () => {
  // The regression: not taking focus for the new pane, something held focus, and
  // it survived the relayout — hand it straight back so typing continues.
  assert.equal(shouldRestoreFocus(false, true, true), true);
});

test("a focus-taking (human) open does NOT restore prior focus", () => {
  // The new pane is meant to take focus — restoring the old element would fight
  // the intended move even though something was focused and is still connected.
  assert.equal(shouldRestoreFocus(true, true, true), false);
});

test("nothing to restore when no element held focus", () => {
  // Focus was already on <body>/nothing before the open — no caret to preserve.
  assert.equal(shouldRestoreFocus(false, false, true), false);
});

test("don't restore focus to an element the relayout removed", () => {
  // The prior element left the document mid-open (e.g. its pane closed) — there's
  // no live node to focus; guarding avoids a focus() on a detached element.
  assert.equal(shouldRestoreFocus(false, true, false), false);
});

// --- preserve-maximize decision (issue #155) ---
// The bug: with a pane maximized, an orchestrator-driven spawn collapsed the
// fullscreen view because openPane exits maximize before growing the split tree.
// A background spawn must keep the human's fullscreen; a human open still exits
// it (they asked for a pane and want to see the layout). This pins that rule;
// grid.ts's lift/re-lift DOM wiring is validated by hand.

test("a background spawn while maximized preserves fullscreen", () => {
  // The regression: the human is watching one pane full-screen and an agent
  // spawns — the view must stay maximized (new pane grows the tree underneath).
  assert.equal(shouldPreserveMaximize(false, true), true);
});

test("a human-initiated open while maximized exits fullscreen (unchanged)", () => {
  // The human asked for a pane (split/launcher) — show them the layout it
  // landed in, as before.
  assert.equal(shouldPreserveMaximize(true, true), false);
});

test("a background spawn with nothing maximized has no fullscreen to preserve", () => {
  // Normal grid — the #117 focus path applies; nothing to keep maximized.
  assert.equal(shouldPreserveMaximize(false, false), false);
});

test("a human open with nothing maximized has no fullscreen to preserve", () => {
  assert.equal(shouldPreserveMaximize(true, false), false);
});
