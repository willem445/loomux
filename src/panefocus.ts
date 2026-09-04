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

// ---------------------------------------------------------------------------
// Reveal, not focus (#2365)
// ---------------------------------------------------------------------------
//
// The reported failure was "the orchestrator pane vanished and I could not get
// it back". Nothing removed it: the only mechanism in the app that hides a
// pane while leaving its PTY bound is fullscreen — `Grid.toggleMaximize` lifts
// the target under `.grid-root` and `styles.css`'s `.grid-root.has-maximized >
// :not(.maximized)` hides the whole split tree — and the dock, which pulls a
// pane out of the tree entirely.
//
// What made it UNRECOVERABLE is that every "go to this pane" path was blind to
// both states. `Grid.setActive` on a pane behind a maximized sibling flips a
// class nobody can see, and `Pane.focus()` → `term.focus()` on a `display:none`
// textarea is a browser no-op — so the pane's own Agents row, `orch-focus`, and
// the Sessions tab's "focus its orchestrator pane instead" all did nothing
// visible. `toggleMaximize` additionally refuses a docked pane outright, so a
// pane the blind `setActive` had made active from the dock could not even be
// maximized back into view.
//
// So the decision has to name the STRUCTURAL steps as well as the focus ones,
// in order. It lives here rather than in `grid.ts` for the reason the rest of
// this module does: it is a rule, the grid is its executor, and a rule with no
// DOM in it can be pinned across every crossing under `node --test`.

/** One step of a reveal, in the order the executor must perform it.
 *
 *  Deliberately a closed union of five NON-DESTRUCTIVE steps. A reveal exists
 *  because a pane became unreachable; one that could close, minimize or rebuild
 *  the tree would be the same defect with a new trigger, so the type says it
 *  cannot. */
export type RevealStep =
  | "switch-tab"
  | "restore-from-dock"
  | "exit-maximize"
  | "set-active"
  | "focus";

/** Everything the reveal decision reads. `maximized` is about the GRID the
 *  pane belongs to: `"self"` when the pane being revealed is the maximized one,
 *  `"other"` when a sibling is, `null` when nothing is. */
export interface RevealState {
  /** Whether the pane's workspace is the tab currently on screen. */
  tabIsActive: boolean;
  /** Whether the pane is parked in the dock (outside the split tree). */
  docked: boolean;
  maximized: "self" | "other" | null;
  /** True when a HUMAN asked for this pane (an Agents row click, the Sessions
   *  tab's Focus button), false when an AGENT did (`orch-focus`, emitted by
   *  the orchestrator's `focus_agent` MCP tool).
   *
   *  This is the #155 bit, and it is here for the same reason it exists there:
   *  the two structural steps below are the ones that RESIZE A PTY, and
   *  constraint 1 bars an agent from triggering that. See `shouldPreserveMaximize`
   *  above — same question, same answer, one module apart. */
  humanInitiated: boolean;
}

/** The ordered steps that make a pane visible, active and focused.
 *
 *  WHO ASKED decides whether the structural steps run at all. `restore-from-dock`
 *  and `exit-maximize` are the two steps that genuinely resize a PTY —
 *  `toggleMaximize`'s own doc says the maximized pane "alone issues one debounced
 *  fit", and `restore`'s says re-attaching "triggers a single genuine fit" and
 *  makes its new siblings fit too. Constraint 1 permits that from a DISCRETE
 *  HUMAN CLICK and bars it from anything else, so both are gated on
 *  `humanInitiated`. An agent-initiated reveal — `orch-focus`, which the
 *  orchestrator's `focus_agent` MCP tool emits with no human anywhere on the
 *  path — gets `switch-tab` / `set-active` / `focus` and nothing structural: it
 *  can say which pane it wants attention on without dropping the human out of a
 *  fullscreen they chose or pulling a pane they docked back into the grid.
 *  That is `shouldPreserveMaximize`'s (#155) ruling applied to the reveal path,
 *  and it is why the two live in the same module.
 *
 *  `switch-tab` first when the pane is in a background tab: its whole workspace
 *  is `display:none`, so nothing below it is visible until the tab is showing.
 *
 *  `restore-from-dock` when docked, and then NO `exit-maximize` — `Grid.restore`
 *  already exits fullscreen on its way in, so emitting both would be one
 *  redundant relayout (constraint 1 counts a relayout as a potential PTY fit).
 *
 *  For a HUMAN reveal: `exit-maximize` only when a DIFFERENT pane is maximized. Revealing the
 *  fullscreen pane itself leaves fullscreen alone: a human who maximized a pane
 *  and then clicked its own Agents row is already looking at it, and yanking
 *  them out of fullscreen would be a layout change they did not ask for.
 *
 *  When a sibling IS maximized, the decision is to EXIT fullscreen rather than
 *  swap it to the target. A swap keeps the human in a mode they did not choose
 *  for this pane and hides whatever they had maximized with no visible cause;
 *  exiting is one step back to the layout they already know, and re-entering is
 *  one Ctrl+Shift+M. */
export function revealPlan(state: RevealState): RevealStep[] {
  const plan: RevealStep[] = [];
  if (!state.tabIsActive) plan.push("switch-tab");
  // The two STRUCTURAL steps, and the only two that resize a PTY, are gated on
  // the human. An agent-initiated reveal switches tab, activates and focuses —
  // enough that the pane is the one receiving keystrokes and its tab is the one
  // on screen — and stops there. See `humanInitiated` on the state.
  if (state.humanInitiated) {
    if (state.docked) plan.push("restore-from-dock");
    else if (state.maximized === "other") plan.push("exit-maximize");
  }
  plan.push("set-active");
  plan.push("focus");
  return plan;
}

/** What clicking a recorded orchestration session row should actually do. */
export type LiveSessionAction = "reveal" | "resume" | "explain";

/** The inputs to that decision.
 *
 *  `isOrchestratorRow` is not decoration: the backend refusal this stands in
 *  for is ROLE-GATED. `resume_orch_session` reads
 *  `if record.role == "orchestrator" { if record.group_live { return Err(…) } }`
 *  (`src-tauri/src/orchestration/mod.rs`), so a worker or reviewer row in a
 *  live group is rejoined, not refused — short-circuiting it to a reveal of the
 *  group's orchestrator pane would answer a question the human did not ask. */
export interface LiveSessionState {
  groupLive: boolean;
  /** Whether the group's orchestrator pane is open in THIS window. */
  paneInWindow: boolean;
  isOrchestratorRow: boolean;
}

/** Route a Sessions-tab click on a row that belongs to an orchestration group.
 *
 *  `explain` exists so the app never calls into a refusal it can already
 *  predict: with the group live and its orchestrator pane in another window (or
 *  no window at all), `resume_recorded_session` can only come back with
 *  "already has a live orchestrator — focus its pane instead", which is not an
 *  answer the human can act on. Saying so directly costs no IPC round-trip and
 *  names the real situation. */
export function liveSessionAction(state: LiveSessionState): LiveSessionAction {
  if (!state.groupLive) return "resume";
  if (!state.isOrchestratorRow) return "resume";
  return state.paneInWindow ? "reveal" : "explain";
}
