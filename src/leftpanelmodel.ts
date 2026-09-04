// The DOM-free half of the left panel's tab host (#2122 slice B): which tab a
// toggle lands on, and what survives a restart. `src/leftpanel.ts` is the DOM
// that applies these decisions — the same split `sidedock.ts` /
// `sidedockmodel.ts` already uses, and for the same reason: a transition table
// is worth testing, and a panel is not something `node --test` can hold.
//
// WHY THE TRANSITION IS A FUNCTION AT ALL. `#sessions` is an in-flow panel, so
// its width change is the one DISCRETE HUMAN CLICK that CLAUDE.md constraint 1
// sanctions and `resizeburst.ts` coalesces into one fit per pane. The width is
// bound to `visible` and to nothing else, so "does this gesture resize the
// grid?" is exactly "does this gesture change `visible`?" — one question, asked
// of one boolean, in one place a test can enumerate. A tab SWITCH must answer
// no: it moves no column, so it costs no PTY resize, which is what makes two
// tabs in one panel permissible where a third in-flow panel would not be
// (doc/design/side-dock.md).

/** The two views the left panel hosts. */
export type LeftPanelTab = "sessions" | "agents";

/** Both tabs, in the order they are rendered. Exported so a caller enumerating
 *  them cannot fall out of step with the type — `Record<LeftPanelTab, …>`
 *  elsewhere is total, and this is its runtime companion. */
export const LEFT_PANEL_TABS: readonly LeftPanelTab[] = ["sessions", "agents"];

/** Where the last-selected tab lives. UI chrome state is localStorage in this
 *  codebase (the `loomux.*` convention `agents.ts`, `gitlayout.ts` and
 *  `sidedockmodel.ts` already use); the backend settings file is for durable
 *  app/session config, and which tab a panel was left on is not that. */
export const LEFT_PANEL_TAB_KEY = "loomux.leftpanel.tab";

/** Sessions, because it is the tab that existed before this one and because a
 *  human who has never opened the Agents tab should get the panel they already
 *  know from `Ctrl+Shift+P`. */
export const DEFAULT_LEFT_PANEL_TAB: LeftPanelTab = "sessions";

/** Whether the panel is open, and which tab it is showing. `tab` is meaningful
 *  even while `visible` is false: it is the tab the panel will come back on. */
export interface LeftPanelState {
  readonly visible: boolean;
  readonly tab: LeftPanelTab;
}

function isLeftPanelTab(v: unknown): v is LeftPanelTab {
  return typeof v === "string" && (LEFT_PANEL_TABS as readonly string[]).includes(v);
}

/** Read the persisted tab back, tolerating anything. Total by construction —
 *  it never throws — so no caller needs a try/catch or a `??` chain, which is
 *  the contract `decodeDockPrefs` set for this kind of preference. A value it
 *  cannot use costs the human their tab choice and nothing else. */
export function decodeLeftPanelTab(raw: string | null): LeftPanelTab {
  return isLeftPanelTab(raw) ? raw : DEFAULT_LEFT_PANEL_TAB;
}

/** Where a toggle of `requested` lands, from `state`.
 *
 *  Three cases, and the third is the one that matters:
 *  - closed → open on the requested tab;
 *  - open on the requested tab → close (the button that opened it closes it);
 *  - open on the OTHER tab → switch, still open.
 *
 *  The close deliberately KEEPS `tab` rather than resetting it, so reopening
 *  returns to what the human was last reading and the persisted value is the
 *  tab they chose rather than the one they dismissed. */
export function toggleTarget(state: LeftPanelState, requested: LeftPanelTab): LeftPanelState {
  if (!state.visible) return { visible: true, tab: requested };
  if (state.tab === requested) return { visible: false, tab: state.tab };
  return { visible: true, tab: requested };
}
