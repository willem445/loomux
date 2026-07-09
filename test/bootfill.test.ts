// Unit tests for the boot-fill decision (issue #178). The regression these pin:
// on a fresh start in agent mode the app opened plain shells instead of the
// "new agent" launcher, because the boot flow filled every empty tab with a
// silent shell. bootFillKind() restores the rule that the tab the human lands
// on honors agent mode — while background and group-bound tabs stay silent so
// no launcher modal pops over them. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { bootFillKind } from "../src/bootfill.ts";

// --- The core regression: active tab in agent mode gets the launcher --------

test("active + non-group tab in agent mode boots the launcher (the #178 fix)", () => {
  assert.equal(
    bootFillKind({ isActive: true, isGroupBound: false }, true),
    "launcher"
  );
});

test("agent mode OFF always boots a silent shell, even on the active tab", () => {
  // Terminal mode is unchanged: every tab is a plain shell.
  assert.equal(
    bootFillKind({ isActive: true, isGroupBound: false }, false),
    "silent-shell"
  );
});

// --- Only the tab the human lands on may pop a launcher ----------------------

test("a background tab never boots a launcher (no modal over an unfocused tab)", () => {
  assert.equal(
    bootFillKind({ isActive: false, isGroupBound: false }, true),
    "silent-shell"
  );
  assert.equal(
    bootFillKind({ isActive: false, isGroupBound: false }, false),
    "silent-shell"
  );
});

// --- A group-bound tab is a placeholder, not a launcher ----------------------

test("a group-bound active tab boots a silent shell (placeholder until resume)", () => {
  // Its pane is a stand-in until the group's session is restored into it;
  // popping a launcher over it would fight the restore.
  assert.equal(
    bootFillKind({ isActive: true, isGroupBound: true }, true),
    "silent-shell"
  );
});

test("group-bound tabs stay silent regardless of active/agent-mode combo", () => {
  for (const isActive of [true, false]) {
    for (const agentMode of [true, false]) {
      assert.equal(
        bootFillKind({ isActive, isGroupBound: true }, agentMode),
        "silent-shell",
        `isActive=${isActive} agentMode=${agentMode}`
      );
    }
  }
});

// --- Launcher requires ALL THREE conditions together ------------------------

test("launcher only when active AND non-group AND agent mode — nothing else", () => {
  // The single positive cell in the truth table; every other combination is a
  // silent shell. This is the invariant that keeps the fix from over-reaching
  // into a background modal.
  for (const isActive of [true, false]) {
    for (const isGroupBound of [true, false]) {
      for (const agentMode of [true, false]) {
        const expected =
          isActive && !isGroupBound && agentMode ? "launcher" : "silent-shell";
        assert.equal(
          bootFillKind({ isActive, isGroupBound }, agentMode),
          expected,
          `isActive=${isActive} isGroupBound=${isGroupBound} agentMode=${agentMode}`
        );
      }
    }
  }
});
