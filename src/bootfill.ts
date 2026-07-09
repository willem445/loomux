// Boot-fill decision: what a restored-but-empty tab opens with (issue #178).
//
// The bug: on a fresh start in agent mode, every pane came up as a plain shell
// instead of the "new agent" launcher. Tabs always persist, so the boot flow's
// `didRestore` branch was ~always taken and filled EVERY empty tab with a
// silent shell (openShellIn) — the agent-mode-aware openPane() path was dead
// after the very first run. This pure module isolates the one decision so the
// invariant is unit-testable and can't silently regress: the tab the human
// lands on honors agent mode, while background and group-bound tabs never pop a
// launcher modal on boot.
//
// DOM-free and Tauri-free (mirrors bootfill's siblings layout.ts / steer.ts);
// main.ts maps the result to openPaneIn (launcher) vs openShellIn (shell).

/** What an empty tab boots with. `launcher` routes through openPaneIn, which in
 *  agent mode shows the new-agent dialog; `silent-shell` opens a plain shell. */
export type BootFillKind = "launcher" | "silent-shell";

/** The only tab facts the decision needs. */
export interface BootTab {
  /** The tab the human lands on after boot (the active tab). */
  isActive: boolean;
  /** The tab owns an orchestration group — its pane is a placeholder until the
   *  group's session is restored into it, so it must never get a launcher. */
  isGroupBound: boolean;
}

/** Decide what an empty tab boots with. Only the active, non-group-bound tab in
 *  agent mode gets the launcher; everything else fills with a silent shell.
 *  This keeps the design-doc rules intact — no background modals, no launcher
 *  over a restored group placeholder — while restoring the fresh-start agent UI
 *  the #178 regression swallowed. */
export function bootFillKind(tab: BootTab, agentMode: boolean): BootFillKind {
  if (agentMode && tab.isActive && !tab.isGroupBound) return "launcher";
  return "silent-shell";
}
