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

/** Whether to restore keyboard focus to the element that held it before a pane
 *  open (issue #117 round 2).
 *
 *  Removing the explicit focus() call (round 1) wasn't enough: inserting a pane
 *  restructures the grid DOM (renderSplit → replaceChildren detaches every child
 *  of a split and re-appends it), which implicitly BLURS whatever the human was
 *  typing into — the steering strip or a terminal — dropping focus to <body> so
 *  their keystrokes go nowhere. This is the same DOM-detach class as the #113
 *  rename crash. The caller snapshots document.activeElement before the relayout
 *  and, when this returns true, refocuses it (with caret/selection) afterward.
 *
 *  Restore only when: the new pane isn't meant to take focus (`takeFocus` false,
 *  i.e. a background spawn onto a non-empty grid); something meaningful actually
 *  held focus (`hadPriorFocus` — not <body>/null); and that element is STILL in
 *  the document (`priorStillConnected`) — a pane that closed mid-open has no
 *  element to hand focus back to. */
export function shouldRestoreFocus(
  takeFocus: boolean,
  hadPriorFocus: boolean,
  priorStillConnected: boolean
): boolean {
  return !takeFocus && hadPriorFocus && priorStillConnected;
}

/** Whether an opening pane must PRESERVE the current fullscreen (#155).
 *
 *  A background (orchestrator-driven) spawn while the human has a pane maximized
 *  used to collapse the fullscreen view: openPane exits maximize unconditionally
 *  before growing the split tree. It shouldn't — the human is watching one pane
 *  full-screen and an agent spawning in the background must not yank them back to
 *  the grid. So keep the pane maximized and grow the tree underneath it (the new
 *  pane lands in the hidden subtree — zero width, no PTY fit — and shows on
 *  unmaximize). A human-initiated open still exits fullscreen, because the human
 *  asked for a pane and expects to see the layout it landed in.
 *
 *  Returns true only for a background open while something is maximized —
 *  exactly the case that would otherwise strand the human out of fullscreen. */
export function shouldPreserveMaximize(
  humanInitiated: boolean,
  isMaximized: boolean
): boolean {
  return !humanInitiated && isMaximized;
}
