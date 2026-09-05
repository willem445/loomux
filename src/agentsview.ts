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

import { getAgentOrder, setAgentOrder } from "./agentorder";
import {
  needsYouCount,
  toAgentRow,
  type AgentFilter,
  type AgentGroup,
  type AgentOrder,
  type AgentRow,
  type AgentState,
  type PaneFacts,
} from "./agentrows";
import {
  AGENT_ORDER_LABEL,
  AGENT_STATE_LABEL,
  ORDER_CHOICES,
  agentIdentityLine,
  agentRowMark,
  listSlots,
  filterChips,
  visibleGroups,
} from "./agentsviewmodel";
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
  mark: HTMLElement;
  name: HTMLElement;
  identity: HTMLElement;
  state: HTMLElement;
  row: AgentRow;
}

/** The header element for one tab's group, plus the title it currently shows —
 *  so a rename rewrites the text and a re-order moves the element, and neither
 *  rebuilds it. */
interface GroupEls {
  el: HTMLElement;
  title: string;
}

/** The key a group's header element is held under: the tab's own id. Only a
 *  group that HAS a tab gets an entry, so this map never needs a sentinel for
 *  the headerless one — it has no header to hold. */
type GroupKey = string;

export class AgentsView {
  private filter: AgentFilter = "all";
  /** The human's group order, read back from storage at construction so an
   *  open-panel boot shows the order they left it in rather than the default
   *  for a frame. `getAgentOrder` never throws and answers the default when
   *  storage is unavailable. */
  private order: AgentOrder = getAgentOrder();
  private chipsEl: HTMLElement;
  private listEl: HTMLElement;
  private emptyEl: HTMLElement;
  private rows = new Map<string, RowEls>();
  /** One header per tab that currently has rows, keyed so it outlives a refresh
   *  — the same reason the chips and rows are keyed: rebuilding a subtree the
   *  keyboard is standing in drops focus to `<body>`. */
  private groups = new Map<GroupKey, GroupEls>();
  /** One button per order choice, keyed for the same reason. */
  private orderBtns = new Map<AgentOrder, HTMLButtonElement>();
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
    // The order control sits in the head rather than among the filter chips
    // because it answers a different question: the chips say WHICH rows, this
    // says in what order their groups come. Built once — the choices are a
    // fixed pair, so unlike the chips nothing here ever appears or disappears.
    // A local, not a field: nothing after construction reads the container —
    // `renderOrder` writes the pressed state on the BUTTONS, which are keyed in
    // `orderBtns`.
    const orderEl = document.createElement("div");
    orderEl.className = "agents-order";
    orderEl.setAttribute("role", "group");
    orderEl.setAttribute("aria-label", "Group order");
    for (const choice of ORDER_CHOICES) {
      const btn = document.createElement("button");
      btn.className = "agents-order-btn";
      btn.type = "button";
      btn.textContent = AGENT_ORDER_LABEL[choice];
      btn.title = `Order groups: ${AGENT_ORDER_LABEL[choice]}`;
      btn.addEventListener("click", () => {
        if (this.order === choice) return;
        this.order = choice;
        // Persisted on the GESTURE, not on a debounce: the value is one word
        // and the human just made the decision, so there is nothing to
        // coalesce and nothing that could be lost by closing the panel.
        setAgentOrder(choice);
        this.refresh();
      });
      this.orderBtns.set(choice, btn);
      orderEl.append(btn);
    }
    head.append(title, orderEl);

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
    this.renderOrder();
    this.renderGroups(visibleGroups(rows, this.filter, this.order));
  }

  /** Mark the selected order. The buttons themselves are built once and never
   *  removed, so this only ever writes the pressed state — there is no element
   *  churn here to lose focus to. */
  private renderOrder(): void {
    for (const [choice, btn] of this.orderBtns) {
      const on = choice === this.order;
      btn.classList.toggle("active", on);
      const pressed = String(on);
      if (btn.getAttribute("aria-pressed") !== pressed) btn.setAttribute("aria-pressed", pressed);
    }
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
      // HAND FOCUS ON BEFORE THE ELEMENT GOES (#2259 review round 2, rev-std
      // finding 2). Keying the chips cured the once-a-second blur, but not this
      // narrower form of it: a chip whose count reaches 0 while it is not the
      // selected one is genuinely removed, and if the keyboard user happened to
      // be standing on it, focus drops to `<body>` and the next Tab restarts
      // traversal from the top of the document.
      //
      // The selected chip is the destination because it is the one element here
      // that always exists — `filterChips` keeps it at count 0, which is the
      // same guarantee that stops a filter becoming unclearable. It is still in
      // the map at this point: it was in `seen`.
      if (btn.contains(document.activeElement)) this.chips.get(this.filter)?.focus();
      btn.remove();
      this.chips.delete(filter);
    }
  }

  /** Reconcile the list: a header per tab, its rows under it (#2371).
   *
   *  ONE FLAT WALK over the sequence the groups spell out — header, its rows,
   *  next header — placing each element after the previous one only when it is
   *  not already there. That is the same rule the ungrouped list followed and
   *  it is what keeps a re-order cheap: a group whose rows did not move inside
   *  it costs one `insertBefore` for the header and none for the rows, and a
   *  row that never moved is never touched, so the working spinner's CSS
   *  animation is not taken back to frame 0.
   *
   *  Headers are keyed by TAB ID and rows by pane key, in two maps, because
   *  they have different lifetimes: a row survives its tab's header
   *  disappearing (the human moved the pane), and a header survives every one
   *  of its rows being replaced. */
  private renderGroups(groups: readonly AgentGroup[]): void {
    const seenGroups = new Set<GroupKey>();
    const seenRows = new Set<string>();
    let prev: HTMLElement | null = null;
    let rowCount = 0;
    const place = (el: HTMLElement): void => {
      const want: ChildNode | null = prev === null ? this.listEl.firstChild : prev.nextSibling;
      if (want !== el) this.listEl.insertBefore(el, want);
      prev = el;
    };
    // WHAT to place, and under which key, is a pure projection — `listSlots`,
    // pinned in `test/agentsviewmodel.test.ts` (#2371 review round 2,
    // premortem). What is left here is the placement and the sweep, which is
    // the part this repo validates by hand.
    for (const slot of listSlots(groups)) {
      if (slot.kind === "header") {
        seenGroups.add(slot.key);
        const header = this.groups.get(slot.key) ?? this.createGroup();
        this.groups.set(slot.key, header);
        // A rename re-labels the header in place. Guarded, so the common case
        // (nothing renamed) writes no text at all.
        if (header.title !== slot.title) {
          header.title = slot.title;
          header.el.textContent = slot.title;
        }
        place(header.el);
      } else {
        seenRows.add(slot.key);
        rowCount += 1;
        const els = this.rows.get(slot.key) ?? this.createRow(slot.row);
        this.rows.set(slot.key, els);
        this.updateRow(els, slot.row);
        place(els.el);
      }
    }
    for (const [key, header] of [...this.groups]) {
      if (seenGroups.has(key)) continue;
      header.el.remove();
      this.groups.delete(key);
    }
    for (const [key, els] of [...this.rows]) {
      if (seenRows.has(key)) continue;
      els.el.remove();
      this.rows.delete(key);
    }
    // The empty line lives at the end and is unhidden rather than created, so
    // it is never in the way of the diff above.
    this.emptyEl.hidden = rowCount > 0;
    this.emptyEl.textContent =
      this.filter === "all"
        ? "No panes open in this window."
        : `No panes are ${AGENT_STATE_LABEL[this.filter]}.`;
    if (!this.emptyEl.hidden) this.listEl.appendChild(this.emptyEl);
  }

  private createGroup(): GroupEls {
    const el = document.createElement("div");
    el.className = "agents-group";
    // A heading, not a decoration: a screen reader walking the list gets the
    // same "which workspace am I in" the sighted reader gets from the rule.
    el.setAttribute("role", "heading");
    el.setAttribute("aria-level", "3");
    // Seeded empty and filled by the caller's guarded write, so `title` below
    // ("") and the element's text agree from the first paint — the seed and the
    // pristine check are one question asked twice.
    return { el, title: "" };
  }

  private createRow(row: AgentRow): RowEls {
    const el = document.createElement("button");
    el.className = "agents-item";
    el.type = "button";
    const top = document.createElement("div");
    top.className = "agents-top";
    const mark = document.createElement("span");
    mark.className = "agents-mark";
    const name = document.createElement("span");
    name.className = "agents-name";
    const state = document.createElement("span");
    state.className = "agents-state";
    top.append(mark, name, state);
    const identity = document.createElement("div");
    identity.className = "agents-identity";
    el.append(top, identity);
    el.addEventListener("click", () => this.deps.focus(row.key));
    return { el, mark, name, identity, state, row };
  }

  /** Paint the agent-type mark (#2371).
   *
   *  Guarded on the harness because the write is `innerHTML`: a pane's CLI
   *  changes only on a respawn, so writing it every tick would be a subtree
   *  rebuild once a second for a glyph that never moves — the same reason the
   *  state cell is guarded.
   *
   *  `view.svg` is the module's own markup and is documented safe for
   *  `innerHTML`; `view.label` is TEXT and is assigned to `title`, never
   *  interpolated (`agenticons.ts` §Safety). */
  private paintMark(el: HTMLElement, row: AgentRow): void {
    const view = agentRowMark(row);
    el.innerHTML = view?.svg ?? "";
    // Hidden rather than empty-but-present: an empty inline element still takes
    // the flex gap, so a shell pane's name would sit indented from an agent's.
    el.hidden = view === null;
    if (view === null) el.removeAttribute("title");
    else el.title = view.label;
  }

  /** Write only what changed. The state cell is guarded on the previous
   *  reading, because assigning it unconditionally rewrites its `innerHTML`
   *  every tick and takes the spinner's CSS animation back to frame 0 once a
   *  second — a spinner that never appears to move. */
  private updateRow(els: RowEls, row: AgentRow): void {
    const was = els.row;
    els.row = row;
    if (els.name.textContent !== row.name) els.name.textContent = row.name;
    // `was === row` is the freshly-created row — see the state cell below.
    if (was === row || was.harness !== row.harness) this.paintMark(els.mark, row);
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
