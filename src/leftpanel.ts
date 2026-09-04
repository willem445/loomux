// The left panel's tab host (#2122 slice B): `#sessions` gains a tablist and
// hosts TWO views — the session browser it always had, and the Agents overview.
//
// WHY A TAB AND NOT A THIRD PANEL. CLAUDE.md constraint 1: `#sessions` and
// `.sidedock` are the two in-flow panels, and a third needs the argument
// `doc/design/side-dock.md` describes before it may exist. Two tabs inside one
// panel need none — the panel's own open/close is the same discrete human click
// it always was, `resizeburst.ts` coalesces the fit exactly as before, and a tab
// SWITCH moves no column at all, so it costs no PTY resize. The transition table
// that guarantees that lives in `leftpanelmodel.ts`, where a test enumerates it;
// this file only applies it.
//
// THE `#sessions` CSS RULES ARE UNTOUCHED. `test/resizeburst.test.ts` reads the
// panel's width transition off the stylesheet to derive its frame budget, so
// the width, the transition and the `.hidden` rule are exactly what they were.
// Everything this slice adds sits INSIDE `.sessions-inner`, which is already a
// fixed-width flex column.

import {
  DEFAULT_LEFT_PANEL_TAB,
  LEFT_PANEL_TABS,
  LEFT_PANEL_TAB_KEY,
  decodeLeftPanelTab,
  toggleTarget,
  type LeftPanelState,
  type LeftPanelTab,
} from "./leftpanelmodel";

const TAB_LABEL: Record<LeftPanelTab, string> = {
  sessions: "Sessions",
  agents: "Agents",
};

/** What a hosted view is told about its own visibility. Exactly the two
 *  transitions a poll gate needs: `onShow` when (panel open AND this tab
 *  selected) becomes true, `onHide` when it becomes false. Neither fires twice
 *  in a row — that is this class's job, not each view's. */
export interface LeftPanelTabHandlers {
  onShow(): void;
  onHide(): void;
}

export class LeftPanel {
  /** Where `SessionBrowser` mounts. It used to build `.sessions-inner` itself;
   *  the panel owns that column now, and hands each view a body inside it. */
  readonly sessionsBody: HTMLElement;
  /** Where `AgentsView` mounts. */
  readonly agentsBody: HTMLElement;

  private state: LeftPanelState;
  private handlers = new Map<LeftPanelTab, LeftPanelTabHandlers>();
  private tabBtns = new Map<LeftPanelTab, HTMLButtonElement>();
  private badgeEls = new Map<LeftPanelTab, HTMLElement>();
  private bodies: Record<LeftPanelTab, HTMLElement>;
  /** Which tab last had `onShow` called without a matching `onHide`. Held
   *  rather than recomputed so the two can never be delivered out of order — a
   *  view that is told to show twice arms a second timer over the first. */
  private shown: LeftPanelTab | null = null;

  constructor(private el: HTMLElement) {
    // The panel starts hidden (the markup's own `class="hidden"`), on whichever
    // tab the human last used. Reading the preference at construction rather
    // than at first open means the first toggle already lands on the right tab.
    this.state = { visible: !el.classList.contains("hidden"), tab: this.loadTab() };

    const inner = document.createElement("div");
    inner.className = "sessions-inner";

    const tabsEl = document.createElement("div");
    tabsEl.className = "leftpanel-tabs";
    tabsEl.setAttribute("role", "tablist");
    for (const tab of LEFT_PANEL_TABS) {
      const btn = document.createElement("button");
      btn.className = "leftpanel-tab";
      btn.type = "button";
      btn.setAttribute("role", "tab");
      const label = document.createElement("span");
      label.textContent = TAB_LABEL[tab];
      // One badge element per tab, built once and emptied rather than created
      // on demand: the count changes on every attention scan, and a tab strip
      // that rebuilds its own buttons loses the focus ring mid-keyboard-use.
      const badge = document.createElement("span");
      badge.className = "leftpanel-tab-badge";
      badge.hidden = true;
      btn.append(label, badge);
      btn.addEventListener("click", () => this.toggle(tab));
      this.tabBtns.set(tab, btn);
      this.badgeEls.set(tab, badge);
      tabsEl.appendChild(btn);
    }

    this.sessionsBody = document.createElement("div");
    this.sessionsBody.className = "leftpanel-body";
    this.agentsBody = document.createElement("div");
    this.agentsBody.className = "leftpanel-body";
    this.bodies = { sessions: this.sessionsBody, agents: this.agentsBody };

    inner.append(tabsEl, this.sessionsBody, this.agentsBody);
    this.el.appendChild(inner);
    this.sync();
  }

  /** Register a view's show/hide handlers. Called after construction because
   *  each view needs its body element to exist before it can be built. */
  attach(tab: LeftPanelTab, handlers: LeftPanelTabHandlers): void {
    this.handlers.set(tab, handlers);
    // A view attached while its tab is already the shown one has just missed
    // its own `onShow` — deliver it now rather than waiting for a toggle.
    if (this.shown === tab) handlers.onShow();
  }

  get visible(): boolean {
    return this.state.visible;
  }

  get tab(): LeftPanelTab {
    return this.state.tab;
  }

  /** Open on `tab`, switch to it, or close — see `toggleTarget`. */
  toggle(tab: LeftPanelTab): void {
    this.apply(toggleTarget(this.state, tab));
  }

  /** Close the panel, keeping the tab it was on. */
  hide(): void {
    this.apply({ visible: false, tab: this.state.tab });
  }

  /** The "needs you" count on a tab's own button, so the tab is useful while
   *  the panel is closed. Zero hides the badge rather than rendering "0" — a
   *  zero badge is a mark that means nothing, which is worse than no mark. */
  setBadge(tab: LeftPanelTab, count: number): void {
    const badge = this.badgeEls.get(tab);
    if (!badge) return;
    const show = count > 0;
    if (show) badge.textContent = String(count);
    badge.hidden = !show;
  }

  private apply(next: LeftPanelState): void {
    this.state = next;
    this.saveTab(next.tab);
    this.sync();
  }

  /** Bring the DOM in line with `state`, and deliver the show/hide transitions.
   *  The ONE place that touches `.hidden` on `#sessions`, so "does this gesture
   *  resize the grid?" has exactly one answer site. */
  private sync(): void {
    this.el.classList.toggle("hidden", !this.state.visible);
    for (const tab of LEFT_PANEL_TABS) {
      const active = this.state.visible && tab === this.state.tab;
      this.bodies[tab].hidden = !active;
      const btn = this.tabBtns.get(tab);
      if (btn) {
        btn.classList.toggle("active", tab === this.state.tab);
        btn.setAttribute("aria-selected", String(active));
      }
    }
    const wanted = this.state.visible ? this.state.tab : null;
    if (wanted === this.shown) return;
    if (this.shown !== null) this.handlers.get(this.shown)?.onHide();
    this.shown = wanted;
    if (wanted !== null) this.handlers.get(wanted)?.onShow();
  }

  private loadTab(): LeftPanelTab {
    try {
      return decodeLeftPanelTab(localStorage.getItem(LEFT_PANEL_TAB_KEY));
    } catch {
      // Storage can be unavailable outright (a hardened webview, a cleared
      // profile mid-session). Losing the preference is the whole cost.
      return DEFAULT_LEFT_PANEL_TAB;
    }
  }

  private saveTab(tab: LeftPanelTab): void {
    try {
      localStorage.setItem(LEFT_PANEL_TAB_KEY, tab);
    } catch {
      /* see loadTab */
    }
  }
}
