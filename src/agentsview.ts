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
   *  the ticker off — the count is what makes the tab useful unopened. */
  refresh(): void {
    const rows = this.deps.facts().map((f) => toAgentRow(f));
    this.deps.onCountChanged(needsYouCount(rows));
    if (!this.open) return;
    this.renderChips(rows);
    this.renderRows(visibleRows(rows, this.filter));
  }

  private renderChips(rows: readonly AgentRow[]): void {
    // The chip strip IS rebuilt each refresh: it is a handful of buttons whose
    // set changes as states appear and vanish, and it holds no animation for a
    // rebuild to restart. The row list, which does, is diffed instead.
    this.chipsEl.replaceChildren();
    for (const chip of filterChips(rows, this.filter)) {
      const btn = document.createElement("button");
      btn.className = "agents-chip";
      btn.type = "button";
      btn.classList.toggle("active", chip.selected);
      btn.setAttribute("aria-pressed", String(chip.selected));
      if (chip.filter !== "all") btn.classList.add(`state-${chip.filter}`);
      btn.textContent = `${chip.label} ${chip.count}`;
      btn.addEventListener("click", () => {
        this.filter = chip.filter;
        this.refresh();
      });
      this.chipsEl.appendChild(btn);
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
