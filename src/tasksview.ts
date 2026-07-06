// Task-board overlay for orchestrator panes: the human's live window into
// the group's work queue (tasks.json, maintained by the orchestrator via
// MCP tools and edited here). Supports status changes, inline title edits,
// notes, reordering, add, and delete. Every human edit is audited
// backend-side and (except reorders) surfaced to the orchestrator as a
// typed notice.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { swapIfConnected } from "./domutil";

export interface OrchTaskNote {
  ts_ms: number;
  author: string;
  text: string;
}

export interface OrchTask {
  id: string;
  title: string;
  status: string;
  issue?: string | null;
  pr?: string | null;
  assignee?: string | null;
  session?: string | null;
  notes: OrchTaskNote[];
  updated_ms: number;
}

/** Mirrors TASK_STATUSES in the backend (validated there). */
const STATUSES = ["queued", "in-progress", "review", "pr", "human-testing", "done", "blocked"];

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const fmtTime = (ms: number): string =>
  new Date(ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

export class TasksView {
  readonly el: HTMLElement;
  private listEl: HTMLElement;
  private addInput: HTMLInputElement;
  private toastEl: HTMLElement;
  private toastTimer: number | undefined;
  private tasks: OrchTask[] = [];
  /** Task ids with their notes section expanded (survives re-renders). */
  private expanded = new Set<string>();
  /** A refresh arrived while the human was mid-edit; run it on blur. */
  private pendingRefresh = false;
  /** The open request-changes modal, if any (kept to one at a time). */
  private dialogEl: HTMLElement | null = null;
  private unlisten: UnlistenFn | null = null;
  private disposed = false;

  constructor(
    private groupId: string,
    opts: { onClose: () => void }
  ) {
    this.el = el("div", "tasks-view");

    const head = el("div", "tasks-head");
    head.append(el("span", "tasks-title", "task board"));
    head.append(el("span", "tasks-group", groupId));
    const close = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    close.title = "Close (Alt+T)";
    close.addEventListener("click", opts.onClose);
    head.append(close);

    this.listEl = el("div", "tasks-list");

    const foot = el("div", "tasks-add");
    this.addInput = document.createElement("input");
    this.addInput.className = "dlg-input";
    this.addInput.placeholder = "Add a task — the orchestrator is notified";
    this.addInput.spellcheck = false;
    this.addInput.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") void this.addTask();
    });
    const addBtn = el("button", "dlg-btn primary", "Add") as HTMLButtonElement;
    addBtn.addEventListener("click", () => void this.addTask());
    foot.append(this.addInput, addBtn);

    this.toastEl = el("div", "git-toast");
    this.toastEl.hidden = true;

    this.el.append(head, this.listEl, foot, this.toastEl);

    // Deferred refreshes (see refresh()) run once the editor loses focus.
    this.listEl.addEventListener("focusout", () => {
      window.setTimeout(() => {
        if (this.pendingRefresh && !this.isEditing()) void this.refresh();
      }, 0);
    });

    void listen<{ group_id: string }>("orch-tasks-changed", ({ payload }) => {
      if (payload.group_id === this.groupId) void this.refresh();
    }).then((u) => {
      if (this.disposed) u();
      else this.unlisten = u;
    });
  }

  /** Called by the pane whenever the overlay is (re)opened. */
  show(): void {
    void this.refresh();
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.toastTimer);
    this.unlisten?.();
    this.el.remove();
  }

  private toast(msg: string): void {
    this.toastEl.textContent = msg;
    this.toastEl.hidden = false;
    clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(() => (this.toastEl.hidden = true), 4000);
  }

  /** Run a mutation; the resulting orch-tasks-changed event re-renders. */
  private async mutate(action: Promise<unknown>): Promise<void> {
    try {
      await action;
    } catch (err) {
      this.toast(String(err));
      void this.refresh(); // resync UI with reality after a failed edit
    }
  }

  /** True while the human is typing in an inline editor inside the list
   *  (title rename, note input) — re-rendering would destroy their edit. */
  private isEditing(): boolean {
    const a = document.activeElement;
    return !!a && this.listEl.contains(a) && (a.tagName === "INPUT" || a.tagName === "TEXTAREA");
  }

  private async refresh(): Promise<void> {
    if (this.disposed) return;
    if (this.isEditing()) {
      // Orchestrator updates mustn't clobber a human's half-typed edit;
      // the focusout handler re-runs this once they're done.
      this.pendingRefresh = true;
      return;
    }
    this.pendingRefresh = false;
    try {
      this.tasks = await invoke<OrchTask[]>("orch_tasks", { groupId: this.groupId });
    } catch (err) {
      this.toast(String(err));
      return;
    }
    this.render();
  }

  private async addTask(): Promise<void> {
    const title = this.addInput.value.trim();
    if (!title) return;
    this.addInput.value = "";
    await this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, title }));
  }

  /** Open a task's issue/PR reference in the default browser. */
  private openRef(kind: "issue" | "pr", value: string): void {
    invoke("orch_open_ref", { groupId: this.groupId, kind, value }).catch((err) =>
      this.toast(String(err))
    );
  }

  /** Merge-gate "request changes": collect findings in a modal, then hand
   *  them to the orchestrator (which routes them back to a worker). */
  private requestChanges(t: OrchTask): void {
    if (this.dialogEl) return; // one at a time
    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(el("div", "tasks-dialog-title", `Request changes on ${t.id}`));

    const ta = document.createElement("textarea");
    ta.className = "dlg-input tasks-dialog-text";
    ta.placeholder = "What needs to change? These findings go to the orchestrator.";
    ta.spellcheck = false;
    ta.rows = 4;

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const send = el("button", "dlg-btn primary", "Send") as HTMLButtonElement;
    actions.append(cancel, send);
    box.append(ta, actions);
    overlay.append(box);

    const close = () => {
      overlay.remove();
      this.dialogEl = null;
    };
    const submit = () => {
      const findings = ta.value.trim();
      if (!findings) {
        ta.focus();
        return;
      }
      close();
      void this.mutate(
        invoke("orch_request_changes", { groupId: this.groupId, id: t.id, findings })
      );
    };
    cancel.addEventListener("click", close);
    send.addEventListener("click", submit);
    // Keep keystrokes off the underlying terminal; Esc cancels, Ctrl/⌘+Enter sends.
    ta.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
    });
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });

    this.dialogEl = overlay;
    this.el.appendChild(overlay);
    ta.focus();
  }

  private render(): void {
    this.listEl.replaceChildren();
    if (this.tasks.length === 0) {
      this.listEl.appendChild(el("div", "tasks-empty", "No tasks yet — the orchestrator adds them as work items come in, or add one below."));
      return;
    }
    this.tasks.forEach((t, i) => this.listEl.appendChild(this.renderTask(t, i)));
  }

  /** Statuses where only the human can move the item forward — highlighted on
   *  the board so what is waiting on you stands out (attention routing #6). */
  private static AWAITING_HUMAN = new Set(["pr", "human-testing", "blocked"]);

  private renderTask(t: OrchTask, index: number): HTMLElement {
    const row = el("div", "task-row");
    if (TasksView.AWAITING_HUMAN.has(t.status)) row.classList.add("awaiting-human");

    // Reorder: board order is the priority order the orchestrator follows.
    const order = el("div", "task-order");
    const up = el("button", "task-btn", "▲") as HTMLButtonElement;
    const down = el("button", "task-btn", "▼") as HTMLButtonElement;
    up.disabled = index === 0;
    down.disabled = index === this.tasks.length - 1;
    up.title = "Higher priority";
    down.title = "Lower priority";
    const move = (delta: number) => {
      const ids = this.tasks.map((x) => x.id);
      ids.splice(index, 1);
      ids.splice(index + delta, 0, t.id);
      void this.mutate(invoke("orch_reorder_tasks", { groupId: this.groupId, ids }));
    };
    up.addEventListener("click", () => move(-1));
    down.addEventListener("click", () => move(1));
    order.append(up, down);

    const main = el("div", "task-main");
    const top = el("div", "task-top");
    top.appendChild(el("span", "task-id", t.id));

    const status = document.createElement("select");
    status.className = `task-status st-${t.status}`;
    for (const s of STATUSES) {
      const opt = document.createElement("option");
      opt.value = s;
      opt.textContent = s;
      status.appendChild(opt);
    }
    status.value = t.status;
    status.addEventListener("change", () =>
      void this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, status: status.value }))
    );
    top.appendChild(status);

    // Title: double-click to edit in place.
    const title = el("span", "task-title", t.title);
    title.title = "Double-click to edit";
    title.addEventListener("dblclick", () => {
      const input = document.createElement("input");
      input.className = "dlg-input task-title-input";
      input.value = t.title;
      title.replaceWith(input);
      input.focus();
      input.select();
      const commit = (save: boolean) => {
        // Enter/Escape/click all commit, and detaching the focused input fires
        // blur → a second commit; swapIfConnected keeps that redundant call (or
        // a background re-render having already removed the row) from throwing
        // NotFoundError out into the app-wide error banner.
        if (!swapIfConnected(input, title)) return;
        const v = input.value.trim();
        if (save && v && v !== t.title) {
          void this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, title: v }));
        }
      };
      input.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Enter") commit(true);
        if (e.key === "Escape") commit(false);
      });
      input.addEventListener("blur", () => commit(true));
    });
    top.appendChild(title);

    // Meta chips: issue / PR / assignee / resumable session. Issue and PR
    // refs are clickable — they open in the browser (see openRef).
    for (const [cls, label, kind] of [
      ["issue", t.issue, "issue"],
      ["pr", t.pr, "pr"],
      ["agent", t.assignee, null],
    ] as const) {
      if (!label) continue;
      if (kind) {
        const chip = el("button", `task-chip ${cls} link`, label) as HTMLButtonElement;
        chip.title = `Open ${kind === "issue" ? "issue" : "PR"} ${label} in browser`;
        chip.addEventListener("click", () => this.openRef(kind, label));
        top.appendChild(chip);
      } else {
        const chip = el("span", `task-chip ${cls}`, label);
        chip.title = "Assigned agent";
        top.appendChild(chip);
      }
    }
    if (t.session) {
      const chip = el("span", "task-chip session", `⟲ ${t.session.slice(0, 8)}`);
      chip.title = `Resumable session ${t.session} — the orchestrator can reopen this task's agent for follow-ups`;
      top.appendChild(chip);
    }

    // Start: the human's nudge to begin a queued item now. Delivers a prompt
    // to the orchestrator (which assigns a worker and flips the status then);
    // shown only on queued items, where starting is meaningful.
    if (t.status === "queued") {
      const start = el("button", "task-btn start", "▶ Start") as HTMLButtonElement;
      start.title = "Tell the orchestrator to begin work on this task now";
      start.addEventListener("click", () => {
        // Start doesn't flip the status, so — unlike Approve — the button
        // isn't removed by the mutation, and mutate() doesn't re-render on
        // success. Disable on click so an accidental double-click can't fire
        // two nudges (two prompts + two identical notes) for one intent; it
        // stays disabled until the board refresh (triggered by the note write,
        // or by mutate's resync on error) rebuilds this row.
        start.disabled = true;
        void this.mutate(invoke("orch_start_task", { groupId: this.groupId, id: t.id }));
      });
      top.appendChild(start);
    }

    // Merge-gate actions: the human's approve / request-changes touchpoints,
    // shown only where they belong — on items awaiting the merge decision.
    if (t.status === "pr" || t.status === "human-testing") {
      const approve = el("button", "task-btn approve", "✓ Approve") as HTMLButtonElement;
      approve.title = "Mark done and tell the orchestrator to merge";
      approve.addEventListener("click", () =>
        void this.mutate(invoke("orch_approve_task", { groupId: this.groupId, id: t.id }))
      );
      const changes = el("button", "task-btn changes", "✎ Changes") as HTMLButtonElement;
      changes.title = "Request changes — send findings back to the orchestrator";
      changes.addEventListener("click", () => this.requestChanges(t));
      top.append(approve, changes);
    }

    const notesBtn = el("button", "task-btn notes", `🗨 ${t.notes.length}`) as HTMLButtonElement;
    notesBtn.title = "Notes";
    notesBtn.addEventListener("click", () => {
      if (this.expanded.has(t.id)) this.expanded.delete(t.id);
      else this.expanded.add(t.id);
      this.render();
    });
    top.appendChild(notesBtn);

    // Delete with a two-click confirm, mirroring the git view's pattern.
    const del = el("button", "task-btn danger", "✕") as HTMLButtonElement;
    del.title = "Delete task";
    del.addEventListener("click", () => {
      if (del.dataset.confirm) {
        void this.mutate(invoke("orch_delete_task", { groupId: this.groupId, id: t.id }));
      } else {
        del.dataset.confirm = "1";
        del.textContent = "sure?";
        window.setTimeout(() => {
          delete del.dataset.confirm;
          del.textContent = "✕";
        }, 2500);
      }
    });
    top.appendChild(del);
    main.appendChild(top);

    if (this.expanded.has(t.id)) {
      const notes = el("div", "task-notes");
      for (const n of t.notes) {
        const line = el("div", "task-note");
        line.append(
          el("span", "task-note-meta", `${n.author} · ${fmtTime(n.ts_ms)}`),
          el("span", "task-note-text", n.text)
        );
        notes.appendChild(line);
      }
      const addRow = el("div", "task-note-add");
      const input = document.createElement("input");
      input.className = "dlg-input";
      input.placeholder = "Add a note…";
      input.spellcheck = false;
      const submit = () => {
        const text = input.value.trim();
        if (!text) return;
        input.value = "";
        void this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, note: text }));
      };
      input.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Enter") submit();
      });
      const btn = el("button", "dlg-btn", "Note") as HTMLButtonElement;
      btn.addEventListener("click", submit);
      addRow.append(input, btn);
      notes.appendChild(addRow);
      main.appendChild(notes);
    }

    row.append(order, main);
    return row;
  }
}
