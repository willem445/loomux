// Unit tests for the new-pane focus decision (issue #117). The bug: a pane
// spawned programmatically by the orchestrator (MCP spawn_agent →
// orch-spawn-request → openPane) grabbed keyboard focus, pulling the cursor away
// from the pane the human was typing in. Focus must move to a new pane only when
// the human opened it directly — with the one exception that an empty grid still
// focuses so the app is never left without an active terminal. This pins that
// rule; grid.ts's DOM wiring is validated by hand. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { shouldFocusNewPane } from "../src/panefocus.ts";

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
