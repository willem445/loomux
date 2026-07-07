// Pure focus-decision for a newly opened pane (issue #117) — no Tauri or DOM
// imports, so it's unit-testable under `node --test` (mirrors spawnexpiry.ts).
// grid.ts imports this to decide whether a fresh pane should grab keyboard
// focus and become active.
//
// The bug this guards: a programmatically spawned agent pane (orchestrator MCP
// spawn_agent → orch-spawn-request → openPane) grabbed keyboard focus, yanking
// the cursor away from whatever pane the human was typing in — jarring mid-type.
// Focus should move to a new pane only when the human opened it directly (split
// button, launcher, session restore, launching an orchestrator). An
// orchestrator-driven spawn opens the pane in the background instead.

/** Whether a newly opened pane should take keyboard focus (and become the
 *  active pane).
 *
 *  `humanInitiated` is true for panes the human opened directly and false for
 *  orchestrator-driven background spawns. `gridWasEmpty` is true when there was
 *  no existing pane to leave focus on — then the new pane must take focus
 *  regardless of who opened it, or the app would be left with no focused
 *  terminal at all. */
export function shouldFocusNewPane(
  humanInitiated: boolean,
  gridWasEmpty: boolean
): boolean {
  return humanInitiated || gridWasEmpty;
}
