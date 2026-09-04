// The Agents tab (#2122 slice B): one row per pane in the window, saying what
// each agent is doing right now, with a click to go there.
//
// WHAT IT COSTS. Nothing on the wire. Every row is derived from
// `Pane.facts()` — a projection of state the pane already holds, which reads no
// geometry, starts no IPC and touches no timer (slice A's contract, the same one
// `tabPaneInfo()` carries) — so a refresh is a walk over open panes and a diff
// against the rows already on screen. The only clock is the 1 s ticker below,
// which exists because two of the inputs (the output burst, the roster reading)
// change without an event, and it is armed ONLY while this tab is the one being
// looked at, gated on window visibility on top of that.
//
// THE DIFF IS THE POINT. Rows are keyed by `PaneFacts.key` and updated in
// place: a re-render that replaced the list would restart the working spinner's
// CSS animation on every tick, which is the per-frame churn the issue rules out
// in as many words. Only a pane appearing, disappearing or moving in the sort
// order touches the list's child order.

import {
  needsYouCount,
  toAgentRow,
  type AgentFilter,
  type AgentRow,
  type AgentState,
  type PaneFacts,
} from "./agentrows";
import { AGENT_STATE_LABEL, agentIdentityLine, filterChips, visibleRows } from "./agentsviewmodel";
import { PollGate } from "./pollgate";
import { spinnerSvg } from "./spinner";

/** How often an open Agents tab re-derives its rows.
 *
 *  1 s because two inputs move with no event behind them: the output burst
 *  `PaneActivity` accumulates (a pane that stops painting has to be NOTICED to
 *  stop reading as working) and the roster's idle reading, which lands on the
 *  4 s strip poll. A tick costs one `facts()` per open pane and a diff — no
 *  IPC, no layout read — and it is armed only while this tab is on screen and
 *  the window is visible. Declared in `test/perfpolicy.test.ts`'s TIMERS
 *  manifest (performance.md §3 INV-4), which refuses an undeclared interval. */
const AGENTS_TICK_MS = 1000;

/** What the view needs from the app. Injected rather than imported so this file
 *  never reaches for `tabs` or a grid — it is handed a reading and an action. */
export interface AgentsViewDeps {
  /** Every pane in the window, as facts, read fresh. */
  facts(): PaneFacts[];
  /** Focus the pane carrying this key: switch to its tab, make it active,
   *  focus it — the same three steps the `orch-focus` route takes. */
  focus(key: string): void;
  /** The rows changed in a way the tab badge cares about. */
  onCountChanged(count: number): void;
}

/** The elements of one row, held so a refresh can update them in place. */
interface RowEls {
  el: HTMLButtonElement;
  name: HTMLElement;
  identity: HTMLElement;
  state: HTMLElement;
  row: AgentRow;
}

export class AgentsView {
  private filter: AgentFilter = "all";
  private chipsEl: HTMLElement;
  private listEl: HTMLElement;
  private emptyEl: HTMLElement;
  private rows = new Map<string, RowEls>();
  /** One button per filter, keyed so it outlives a refresh — see
   *  `renderChips`, which is why this is a map and not a rebuild. */
  private chips = new Map<AgentFilter, HTMLButtonElement>();
  private open = false;
  private tickTimer: number | undefined;
  private gate: PollGate = new PollGate({
    arm: () => {
      // Defensive clear-before-arm, the same shape `groupview.ts` keeps: a
      // stray leftover timer would double the cadence rather than restart it.
      if (this.tickTimer !== undefined) clearInterval(this.tickTimer);
      this.tickTimer = window.setInterval(() => this.refresh(), AGENTS_TICK_MS);
    },
    disarm: () => {
      if (this.tickTimer !== undefined) {
        clearInterval(this.tickTimer);
        this.tickTimer = undefined;
      }
    },
    refresh: () => this.refresh(),
  });

  constructor(
    private el: HTMLElement,
    private deps: AgentsViewDeps
  ) {
    const head = document.createElement("div");
    head.className = "sessions-head";
    const title = document.createElement("h2");
    title.textContent = "Agents";
    head.append(title);

    this.chipsEl = document.createElement("div");
    this.chipsEl.className = "agents-chips";

    this.listEl = document.createElement("div");
    this.listEl.className = "agents-list";

    this.emptyEl = document.createElement("div");
    this.emptyEl.className = "sessions-empty";
    this.emptyEl.hidden = true;

    this.listEl.append(this.emptyEl);
    this.el.append(head, this.chipsEl, this.listEl);
  }

  /** The tab was selected (and the panel is open). */
  show(): void {
    this.open = true;
    this.refresh();
    this.gate.enable();
  }

  /** The tab was deselected, or the panel closed. Stops the ticker outright —
   *  component scope and window visibility are different questions and the gate
   *  only answers the second. */
  hide(): void {
    this.open = false;
    this.gate.disable();
  }

  /** Re-derive every row and reconcile the DOM. Safe to call at any time; a
   *  call while the tab is closed updates the badge and returns, so an
   *  attention flip still moves the number on a tab nobody is looking at
   *  without paying for a render. That is also what keeps the badge honest with
   *  the ticker off — the count is what makes the tab useful unopened.
   *
   *  WHAT A CLOSED-TAB CALL COSTS, since it is not zero (#2259 review,
   *  rev-final premortem 2). It walks every pane of every tab and allocates one
   *  `PaneFacts` plus one `ActivitySnapshot` each, to produce one integer. No
   *  IPC, no geometry read, no DOM write beyond the badge — and it is driven
   *  only by things that already moved (an attention pass that got through its
   *  gate, a resolved strip read, a tab-set change, a rename), never by a
   *  clock, because the ticker is disarmed while the tab is closed. It is
   *  O(panes) with no ceiling stated, which is fine at the tens of panes a
   *  window holds and is the honest bound rather than a claim of free.
   *
   *  IT CANNOT BE COUNTED OFF THE ATTENTION PAYLOAD INSTEAD, which is the
   *  obvious cheaper shape: `needsYouCount` counts LADDER states, and the
   *  ladder masks — a pane that is both held and blocked resolves to `held` and
   *  must not count. Counting reasons out of `items` would be a second,
   *  disagreeing rule for the same number, which is the divergence
   *  `agentrows.ts` exists to prevent. */
  refresh(): void {
    const rows = this.deps.facts().map((f) => toAgentRow(f));
    this.deps.onCountChanged(needsYouCount(rows));
    if (!this.open) return;
    this.renderChips(rows);
    this.renderRows(visibleRows(rows, this.filter));
  }

  /** Reconcile the chip strip, keyed by filter — NOT rebuilt.
   *
   *  An earlier draft opened with `replaceChildren()`, and `refresh()` is
   *  driven by the 1 s ticker while this tab is open: removing the focused
   *  element blurs it, so a keyboard user who tabbed onto a chip was returned
   *  to `<body>` within a second, with the next `Tab` restarting traversal from
   *  the top of the document. `leftpanel.ts` already states that hazard for the
   *  tab buttons — "a tab strip that rebuilds its own buttons loses the focus
   *  ring mid-keyboard-use" — and these are the same control class, rebuilt far
   *  more often (#2259 review, rev-final N1).
   *
   *  So the chips get exactly what the rows get: the element for a given filter
   *  outlives every refresh, only its text and its pressed state are written,
   *  and a chip is created or removed only when its presence actually changes.
   *  The selected chip is never one of the removed — `filterChips` keeps it
   *  even at count 0, which is also what stops a filter becoming unclearable. */
  private renderChips(rows: readonly AgentRow[]): void {
    const seen = new Set<AgentFilter>();
    let prev: HTMLElement | null = null;
    for (const chip of filterChips(rows, this.filter)) {
      seen.add(chip.filter);
      let btn = this.chips.get(chip.filter);
      if (btn === undefined) {
        btn = document.createElement("button");
        btn.className = "agents-chip";
        btn.type = "button";
        // The filter is captured from the KEY, so this handler stays correct
        // for the life of the element — the chip for `held` is always the chip
        // for `held`, whatever its count does.
        const filter = chip.filter;
        btn.addEventListener("click", () => {
          this.filter = filter;
          this.refresh();
        });
        this.chips.set(chip.filter, btn);
      }
      const label = `${chip.label} ${chip.count}`;
      if (btn.textContent !== label) btn.textContent = label;
      btn.classList.toggle("active", chip.selected);
      const pressed = String(chip.selected);
      if (btn.getAttribute("aria-pressed") !== pressed) btn.setAttribute("aria-pressed", pressed);
      const want: ChildNode | null = prev === null ? this.chipsEl.firstChild : prev.nextSibling;
      if (want !== btn) this.chipsEl.insertBefore(btn, want);
      prev = btn;
    }
    for (const [filter, btn] of [...this.chips]) {
      if (seen.has(filter)) continue;
      btn.remove();
      this.chips.delete(filter);
    }
  }

  private renderRows(rows: readonly AgentRow[]): void {
    const seen = new Set<string>();
    let prev: HTMLElement | null = null;
    for (const row of rows) {
      seen.add(row.key);
      const els = this.rows.get(row.key) ?? this.createRow(row);
      this.rows.set(row.key, els);
      this.updateRow(els, row);
      // Place it after the previous row only if it is not already there —
      // `insertBefore` on an element that is already in position still counts
      // as a move, and a move inside an animating subtree restarts it.
      const want: ChildNode | null = prev === null ? this.listEl.firstChild : prev.nextSibling;
      if (want !== els.el) this.listEl.insertBefore(els.el, want);
      prev = els.el;
    }
    for (const [key, els] of [...this.rows]) {
      if (seen.has(key)) continue;
      els.el.remove();
      this.rows.delete(key);
    }
    // The empty line lives at the end and is unhidden rather than created, so
    // it is never in the way of the diff above.
    this.emptyEl.hidden = rows.length > 0;
    this.emptyEl.textContent =
      this.filter === "all"
        ? "No panes open in this window."
        : `No panes are ${AGENT_STATE_LABEL[this.filter]}.`;
    if (!this.emptyEl.hidden) this.listEl.appendChild(this.emptyEl);
  }

  private createRow(row: AgentRow): RowEls {
    const el = document.createElement("button");
    el.className = "agents-item";
    el.type = "button";
    const top = document.createElement("div");
    top.className = "agents-top";
    const name = document.createElement("span");
    name.className = "agents-name";
    const state = document.createElement("span");
    state.className = "agents-state";
    top.append(name, state);
    const identity = document.createElement("div");
    identity.className = "agents-identity";
    el.append(top, identity);
    el.addEventListener("click", () => this.deps.focus(row.key));
    return { el, name, identity, state, row };
  }

  /** Write only what changed. The state cell is guarded on the previous
   *  reading, because assigning it unconditionally rewrites its `innerHTML`
   *  every tick and takes the spinner's CSS animation back to frame 0 once a
   *  second — a spinner that never appears to move. */
  private updateRow(els: RowEls, row: AgentRow): void {
    const was = els.row;
    els.row = row;
    if (els.name.textContent !== row.name) els.name.textContent = row.name;
    const identity = agentIdentityLine(row);
    if (els.identity.textContent !== identity) els.identity.textContent = identity;
    // `was === row` is the freshly-created row: `createRow` seeds `row` with
    // the same object, so the state cell is still empty and has to be painted.
    if (was === row || was.state !== row.state) {
      this.paintState(els.state, row.state);
      els.el.className = `agents-item state-${row.state}`;
    }
    const title = this.rowTitle(row);
    if (els.el.title !== title) els.el.title = title;
  }

  private paintState(el: HTMLElement, state: AgentState): void {
    // The spinner only ever exists on a `working` row, so no other state pays
    // for an animated element it then has to hide.
    el.innerHTML = state === "working" ? spinnerSvg() : "";
    const word = document.createElement("span");
    word.textContent = AGENT_STATE_LABEL[state];
    el.appendChild(word);
  }

  private rowTitle(row: AgentRow): string {
    const identity = agentIdentityLine(row);
    const what = `${row.name} — ${AGENT_STATE_LABEL[row.state]}`;
    return identity
      ? `${what}\n${identity}\nClick to focus this pane`
      : `${what}\nClick to focus this pane`;
  }
}
