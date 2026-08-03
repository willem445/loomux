// Audit-log timeline overlay for orchestration panes: the human's in-app
// window into a group's audit.jsonl (which until now was only greppable).
// Read-only — it never mutates state. Filterable by actor / action / agent,
// prompt texts expand inline, and a live-follow mode polls for new lines.
// Rotation is handled backend-side (orch_audit reads audit.1.jsonl before
// audit.jsonl), so the viewer never has to know about it.

import { invoke } from "@tauri-apps/api/core";
import { asObject, str, summarize, type AuditEntry } from "./auditsummary";
import { nextWindowStart, backfillWindowStart } from "./auditwindow";
import { PollGate } from "./pollgate";

export type { AuditEntry };

/** How often live-follow re-polls the backend. */
const FOLLOW_MS = 1500;

/** How close to the top of the list counts as "asking for more history"
 *  (mirrors the existing `nearBottom` follow-tail threshold below). */
const BACKFILL_THRESHOLD_PX = 40;

/** Empty-string filter value = "any". */
interface Filters {
  actor: string;
  action: string;
  agent: string;
  search: string;
}

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const fmtTime = (ms: number): string =>
  new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });

/** Agent ids an entry references (for the agent filter). An entry is "about"
 *  an agent if its detail names one via agent / to / from. */
function entryAgents(e: AuditEntry): string[] {
  const d = asObject(e.detail);
  if (!d) return [];
  const out: string[] = [];
  for (const k of ["agent", "to", "from"]) {
    const v = str(d[k]);
    if (v) out.push(v);
  }
  return out;
}

/** The full expandable body: prompt/task text verbatim first (the reason this
 *  log has been "decisive in every debugging round"), then the raw detail. */
function detailText(e: AuditEntry): string {
  const d = asObject(e.detail);
  const parts: string[] = [];
  const text = d && str(d.text);
  const task = d && str(d.task);
  if (text) parts.push(text);
  else if (task) parts.push(task);
  try {
    parts.push(JSON.stringify(e.detail, null, 2));
  } catch {
    parts.push(String(e.detail));
  }
  return parts.join("\n\n———\n\n");
}

export class AuditView {
  readonly el: HTMLElement;
  private listEl: HTMLElement;
  private actorSel: HTMLSelectElement;
  private actionSel: HTMLSelectElement;
  private agentSel: HTMLSelectElement;
  private searchInput: HTMLInputElement;
  private followBtn: HTMLButtonElement;
  private countEl: HTMLElement;
  private embedBtn: HTMLButtonElement;
  private closeBtn: HTMLButtonElement;

  private entries: AuditEntry[] = [];
  private filters: Filters = { actor: "", action: "", agent: "", search: "" };
  /** Entry keys expanded to show full detail (survives re-renders). */
  private expanded = new Set<string>();
  private follow = false;
  private followTimer: number | undefined;
  /** Window-visibility gate around the follow timer (#743 S6, pollgate.ts).
   *  Follow is opt-in and cleared on dispose, but an armed follow behind a
   *  minimized window still refetched the whole log and re-rendered it every
   *  1.5 s — component scope says nothing about whether anyone is looking. */
  private followGate: PollGate = new PollGate({
    arm: () => {
      this.followTimer = window.setInterval(() => void this.load(), FOLLOW_MS);
    },
    disarm: () => {
      if (this.followTimer !== undefined) {
        clearInterval(this.followTimer);
        this.followTimer = undefined;
      }
    },
    refresh: () => void this.load(),
  });
  private disposed = false;
  /** Signature of the last render's data, to skip no-op follow re-renders
   *  (which would otherwise fight the human's scroll/expand). */
  private lastSig = "";
  /** Signature of the entry set the filter dropdowns were last built from, so
   *  a no-op follow poll doesn't rebuild (and disrupt) an open dropdown. */
  private lastOptionsSig = "";
  /** Index into the CURRENT filtered array the rendered window starts at
   *  (#361 user-demo finding — auditwindow.ts) — everything from here to the
   *  end renders; only render a bounded tail rather than the full,
   *  potentially-thousands-long log. */
  private windowStart = 0;
  /** The filter signature `windowStart` was last computed against, so a
   *  genuine filter change (which invalidates the old index) is
   *  distinguishable from new entries simply having arrived. */
  private lastFilterSig = "";

  constructor(
    private groupId: string,
    opts: { onClose: () => void; onEmbedMenu?: (anchor: HTMLElement) => void }
  ) {
    this.el = el("div", "audit-view");

    const head = el("div", "audit-head");
    head.append(el("span", "audit-title", "audit log"));
    head.append(el("span", "audit-group", groupId));
    this.countEl = el("span", "audit-count");
    head.append(this.countEl);

    this.followBtn = el("button", "audit-follow", "▶ follow") as HTMLButtonElement;
    this.followBtn.title = "Live-follow: poll for new audit lines";
    this.followBtn.addEventListener("click", () => this.toggleFollow());
    head.append(this.followBtn);

    const refresh = el("button", "pane-btn", "⟳") as HTMLButtonElement;
    refresh.title = "Refresh";
    refresh.addEventListener("click", () => void this.load());
    head.append(refresh);

    // Embed side-picker (#361): switch between the floating overlay and any
    // of the pane's (up to three) embed slots.
    this.embedBtn = el("button", "pane-btn embed", "⬒") as HTMLButtonElement;
    this.embedBtn.addEventListener("click", () => opts.onEmbedMenu?.(this.embedBtn));
    head.append(this.embedBtn);

    this.closeBtn = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    this.closeBtn.title = "Close (Alt+A)";
    this.closeBtn.addEventListener("click", opts.onClose);
    head.append(this.closeBtn);
    // Now that both buttons `setPanelActive` touches exist.
    this.setPanelActive(false);

    // Filter bar.
    const filterBar = el("div", "audit-filters");
    this.actorSel = this.makeSelect("actor", (v) => (this.filters.actor = v));
    this.actionSel = this.makeSelect("action", (v) => (this.filters.action = v));
    this.agentSel = this.makeSelect("agent", (v) => (this.filters.agent = v));
    this.searchInput = document.createElement("input");
    this.searchInput.className = "dlg-input audit-search";
    this.searchInput.placeholder = "search text…";
    this.searchInput.spellcheck = false;
    this.searchInput.addEventListener("keydown", (e) => e.stopPropagation());
    this.searchInput.addEventListener("input", () => {
      this.filters.search = this.searchInput.value.trim().toLowerCase();
      this.render();
    });
    filterBar.append(this.actorSel, this.actionSel, this.agentSel, this.searchInput);

    this.listEl = el("div", "audit-list");
    this.listEl.addEventListener("scroll", () => this.maybeBackfill());

    this.el.append(head, filterBar, this.listEl);
  }

  private makeSelect(label: string, onChange: (v: string) => void): HTMLSelectElement {
    const sel = document.createElement("select");
    sel.className = "audit-select";
    sel.title = `Filter by ${label}`;
    sel.dataset.label = label;
    sel.addEventListener("change", () => {
      onChange(sel.value);
      this.render();
    });
    return sel;
  }

  /** Called by the pane whenever the view is (re)opened, in either mode. */
  show(): void {
    void this.load();
  }

  /** Reflect whether the pane currently has this view in its embed-panel
   *  slot (#361) — pure display state on the header's toggle button. */
  setPanelActive(active: boolean): void {
    this.embedBtn.classList.toggle("active", active);
    this.embedBtn.textContent = active ? "⬓" : "⬒";
    this.embedBtn.title = active
      ? "Un-embed — back to a floating overlay"
      : "Embed beside the terminal (resizes this pane)";
    // The overlay toggle (this button, the pane header's own audit button)
    // is disabled while docked (#361 user-demo finding — see embedtoggle.ts):
    // only un-embedding closes a docked log now.
    this.closeBtn.disabled = active;
    this.closeBtn.title = active ? "Docked — un-embed it (side menu) to close" : "Close (Alt+A)";
  }

  dispose(): void {
    this.disposed = true;
    this.stopFollow();
    this.el.remove();
  }

  private toggleFollow(): void {
    this.follow = !this.follow;
    this.followBtn.classList.toggle("on", this.follow);
    this.followBtn.textContent = this.follow ? "⏸ following" : "▶ follow";
    if (this.follow) {
      // Poll on an interval; each tick reloads and (if the human is at the
      // bottom) sticks to the newest line. The gate owns the timer, so the
      // interval exists only while the window is visible.
      this.followGate.enable();
      void this.load();
    } else {
      this.stopFollow();
    }
  }

  private stopFollow(): void {
    this.followGate.disable();
  }

  private async load(): Promise<void> {
    if (this.disposed) return;
    try {
      this.entries = await invoke<AuditEntry[]>("orch_audit", { groupId: this.groupId });
    } catch {
      // Best-effort: a missing/unreadable log just renders empty.
      this.entries = [];
    }
    this.render();
  }

  /** Rebuild a filter dropdown's options from the current entries, keeping the
   *  current selection if it still exists. */
  private syncSelect(sel: HTMLSelectElement, values: string[], current: string): void {
    const sorted = [...new Set(values)].filter(Boolean).sort();
    sel.replaceChildren();
    const any = document.createElement("option");
    any.value = "";
    any.textContent = `${sel.dataset.label}: any`;
    sel.appendChild(any);
    for (const v of sorted) {
      const opt = document.createElement("option");
      opt.value = v;
      opt.textContent = v;
      sel.appendChild(opt);
    }
    sel.value = sorted.includes(current) ? current : "";
    // Selection may have been dropped (value no longer present) — reflect it.
    if (sel.value !== current) {
      if (sel === this.actorSel) this.filters.actor = "";
      else if (sel === this.actionSel) this.filters.action = "";
      else if (sel === this.agentSel) this.filters.agent = "";
    }
  }

  private passes(e: AuditEntry): boolean {
    const f = this.filters;
    if (f.actor && e.actor !== f.actor) return false;
    if (f.action && e.action !== f.action) return false;
    if (f.agent && !entryAgents(e).includes(f.agent)) return false;
    if (f.search) {
      const hay = `${e.actor} ${e.action} ${JSON.stringify(e.detail ?? "")}`.toLowerCase();
      if (!hay.includes(f.search)) return false;
    }
    return true;
  }

  private entryKey(e: AuditEntry): string {
    // ts_ms|actor|action alone collides when the same actor logs the same
    // action within one millisecond (e.g. two prompt deliveries) — expanding
    // one row would toggle both. Mix in a hash of the detail to disambiguate.
    let h = 5381;
    const detail = JSON.stringify(e.detail ?? "");
    for (let i = 0; i < detail.length; i++) h = ((h << 5) + h + detail.charCodeAt(i)) | 0;
    return `${e.ts_ms}|${e.actor}|${e.action}|${h >>> 0}`;
  }

  private render(): void {
    if (this.disposed) return;

    // Refresh filter option lists, but only when the entry set actually
    // changed. The audit is append-only, so distinct actors/actions/agents can
    // only appear alongside new entries — length + newest ts is a sufficient
    // signature. Gating this keeps a 1.5s follow poll from rebuilding (and
    // collapsing) a dropdown the human has open.
    const optionsSig = `${this.entries.length}|${this.entries.at(-1)?.ts_ms ?? 0}`;
    if (optionsSig !== this.lastOptionsSig) {
      this.lastOptionsSig = optionsSig;
      this.syncSelect(this.actorSel, this.entries.map((e) => e.actor), this.filters.actor);
      this.syncSelect(this.actionSel, this.entries.map((e) => e.action), this.filters.action);
      this.syncSelect(this.agentSel, this.entries.flatMap(entryAgents), this.filters.agent);
    }

    const filtered = this.entries.filter((e) => this.passes(e));

    // Skip a no-op re-render during follow so we don't clobber scroll/expand;
    // the signature covers data + active filters.
    const sig = `${this.entries.length}|${this.entries.at(-1)?.ts_ms ?? 0}|${JSON.stringify(this.filters)}|${this.expanded.size}`;
    const listAlreadyBuilt = this.listEl.childElementCount > 0 || filtered.length === 0;
    if (sig === this.lastSig && listAlreadyBuilt) return;
    this.lastSig = sig;

    this.countEl.textContent =
      filtered.length === this.entries.length
        ? `${this.entries.length}`
        : `${filtered.length} / ${this.entries.length}`;

    // Stick to the bottom if the human is already there (live tailing).
    const nearBottom =
      this.listEl.scrollHeight - this.listEl.scrollTop - this.listEl.clientHeight < 40;

    // Only the newest WINDOW_SIZE matching entries render by default — the
    // log is append-only and can grow into the thousands over a long
    // session, and a full unbounded DOM list is genuinely slow to reflow on
    // every divider-drag frame once docked (#361 user-demo finding).
    // `maybeBackfill` (on scroll-to-top) is the other place this advances.
    const filterSig = JSON.stringify(this.filters);
    const filterChanged = filterSig !== this.lastFilterSig;
    this.lastFilterSig = filterSig;
    this.windowStart = nextWindowStart(filtered.length, this.windowStart, filterChanged, nearBottom);
    const windowed = filtered.slice(this.windowStart);

    this.listEl.replaceChildren();
    if (this.entries.length === 0) {
      this.listEl.appendChild(el("div", "audit-empty", "No audit entries yet for this group."));
      return;
    }
    if (filtered.length === 0) {
      this.listEl.appendChild(el("div", "audit-empty", "No entries match the current filters."));
      return;
    }
    if (this.windowStart > 0) {
      const older = this.windowStart;
      this.listEl.appendChild(
        el("div", "audit-window-hint", `${older} earlier ${older === 1 ? "entry" : "entries"} — scroll up to load more`)
      );
    }
    for (const e of windowed) this.listEl.appendChild(this.renderRow(e));

    if (this.follow && nearBottom) this.listEl.scrollTop = this.listEl.scrollHeight;
  }

  /** Scrolling near the top asks for more of the backlog (#361 user-demo
   *  finding — the windowed render above). Forces the next `render()` to
   *  rebuild (bypassing its no-op-skip signature check, the same trick the
   *  expand/collapse click handler below already uses) and preserves the
   *  human's scroll position across the rebuild — without this, replacing
   *  the list's children resets `scrollTop` to 0, which would immediately
   *  re-trigger this same backfill on the NEXT scroll event, in a loop. */
  private maybeBackfill(): void {
    if (this.windowStart === 0) return;
    if (this.listEl.scrollTop > BACKFILL_THRESHOLD_PX) return;
    this.windowStart = backfillWindowStart(this.windowStart);
    const prevScrollHeight = this.listEl.scrollHeight;
    const prevScrollTop = this.listEl.scrollTop;
    this.lastSig = ""; // force the rebuild even though the underlying data hasn't changed
    this.render();
    this.listEl.scrollTop = prevScrollTop + (this.listEl.scrollHeight - prevScrollHeight);
  }

  private renderRow(e: AuditEntry): HTMLElement {
    const key = this.entryKey(e);
    const row = el("div", "audit-row");

    const top = el("div", "audit-top");
    top.appendChild(el("span", "audit-time", fmtTime(e.ts_ms)));
    top.appendChild(el("span", `audit-actor actor-${e.actor.replace(/[^a-z0-9]/gi, "-")}`, e.actor));
    top.appendChild(el("span", `audit-action act-${e.action}`, e.action));

    const summary = el("span", "audit-summary", summarize(e));
    top.appendChild(summary);

    // Whole row toggles the detail body (expandable prompt/task text + raw).
    const body = detailText(e);
    const hasBody = body.trim() !== "{}" && body.trim() !== "null" && body.trim() !== "";
    if (hasBody) {
      const caret = el("span", "audit-caret", this.expanded.has(key) ? "▾" : "▸");
      top.insertBefore(caret, top.firstChild);
      top.classList.add("expandable");
      top.addEventListener("click", () => {
        if (this.expanded.has(key)) this.expanded.delete(key);
        else this.expanded.add(key);
        this.lastSig = ""; // force the next render even under follow
        this.render();
      });
    } else {
      top.appendChild(el("span", "audit-caret-spacer", ""));
    }
    row.appendChild(top);

    if (hasBody && this.expanded.has(key)) {
      const pre = el("pre", "audit-detail");
      pre.textContent = body;
      row.appendChild(pre);
    }
    return row;
  }
}
