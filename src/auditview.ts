// Audit-log timeline overlay for orchestration panes: the human's in-app
// window into a group's audit.jsonl (which until now was only greppable).
// Read-only — it never mutates state. Filterable by actor / action / agent,
// prompt texts expand inline, and a live-follow mode polls for new lines.
// Rotation is handled backend-side (orch_audit reads audit.1.jsonl before
// audit.jsonl), so the viewer never has to know about it.

import { invoke } from "@tauri-apps/api/core";

export interface AuditEntry {
  ts_ms: number;
  actor: string;
  action: string;
  // detail is per-action JSON; the viewer renders it generically.
  detail: unknown;
}

/** How often live-follow re-polls the backend. */
const FOLLOW_MS = 1500;

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

/** A detail object as a plain record, or null when it isn't one. */
function asObject(v: unknown): Record<string, unknown> | null {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

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

/** Short one-line summary per action. Falls back to compact detail JSON so an
 *  unknown/new action is never opaque. */
function summarize(e: AuditEntry): string {
  const d = asObject(e.detail) ?? {};
  const firstLine = (s: string): string => {
    const line = s.split("\n", 1)[0];
    return line.length > 160 ? line.slice(0, 160) + "…" : line;
  };
  switch (e.action) {
    case "prompt":
      return `→ ${str(d.to) ?? "?"}: ${firstLine(str(d.text) ?? "")}`;
    case "prompt-typed":
      return `→ ${str(d.to) ?? "?"} delivered (waited ${str(d.waited_ms) ?? d.waited_ms ?? "?"}ms)`;
    case "prompt-failed":
      return `→ ${str(d.to) ?? "?"} failed: ${str(d.reason) ?? ""}`;
    case "submit-retries-skipped":
      return `→ ${str(d.to) ?? "?"}: ${str(d.reason) ?? "retries skipped"}`;
    case "agent-spawn":
      return `${str(d.agent) ?? "?"} (${str(d.role) ?? "?"})${d.task ? ` — ${firstLine(str(d.task) ?? "")}` : ""}`;
    case "agent-bind":
      return `${str(d.agent) ?? "?"} bound to pty ${d.pty ?? "?"}`;
    case "task-upsert": {
      const id = str(d.id) ?? "";
      const title = str(d.title) ?? "";
      const status = str(d.status) ?? "";
      return `${id} "${title}"${status ? ` → ${status}` : ""}`;
    }
    case "task-delete":
      return `deleted ${str(d.id) ?? ""}`;
    case "task-reorder":
      return "reordered task board";
    case "group-create":
    case "group-resume":
      return `${str(d.repo) ?? ""} (max ${d.max_agents ?? "?"})`;
    case "state-write":
      return `state.json (${d.bytes ?? "?"} bytes)`;
    default: {
      const compact = JSON.stringify(e.detail ?? {});
      return compact === "{}" || compact === "null"
        ? ""
        : compact.length > 200
          ? compact.slice(0, 200) + "…"
          : compact;
    }
  }
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

  private entries: AuditEntry[] = [];
  private filters: Filters = { actor: "", action: "", agent: "", search: "" };
  /** Entry keys expanded to show full detail (survives re-renders). */
  private expanded = new Set<string>();
  private follow = false;
  private followTimer: number | undefined;
  private disposed = false;
  /** Signature of the last render's data, to skip no-op follow re-renders
   *  (which would otherwise fight the human's scroll/expand). */
  private lastSig = "";

  constructor(
    private groupId: string,
    opts: { onClose: () => void }
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

    const close = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    close.title = "Close (Alt+A)";
    close.addEventListener("click", opts.onClose);
    head.append(close);

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

  /** Called by the pane whenever the overlay is (re)opened. */
  show(): void {
    void this.load();
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
      // bottom) sticks to the newest line.
      this.followTimer = window.setInterval(() => void this.load(), FOLLOW_MS);
      void this.load();
    } else {
      this.stopFollow();
    }
  }

  private stopFollow(): void {
    if (this.followTimer !== undefined) {
      clearInterval(this.followTimer);
      this.followTimer = undefined;
    }
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
    return `${e.ts_ms}|${e.actor}|${e.action}`;
  }

  private render(): void {
    if (this.disposed) return;

    // Refresh filter option lists from the full entry set.
    this.syncSelect(this.actorSel, this.entries.map((e) => e.actor), this.filters.actor);
    this.syncSelect(this.actionSel, this.entries.map((e) => e.action), this.filters.action);
    this.syncSelect(this.agentSel, this.entries.flatMap(entryAgents), this.filters.agent);

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

    this.listEl.replaceChildren();
    if (this.entries.length === 0) {
      this.listEl.appendChild(el("div", "audit-empty", "No audit entries yet for this group."));
      return;
    }
    if (filtered.length === 0) {
      this.listEl.appendChild(el("div", "audit-empty", "No entries match the current filters."));
      return;
    }
    for (const e of filtered) this.listEl.appendChild(this.renderRow(e));

    if (this.follow && nearBottom) this.listEl.scrollTop = this.listEl.scrollHeight;
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
