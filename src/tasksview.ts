// Task-board overlay for orchestrator panes: the human's live window into
// the group's work queue (tasks.json, maintained by the orchestrator via
// MCP tools and edited here). Supports status changes, inline title edits,
// notes, reordering, add, and delete. Every human edit is audited
// backend-side and (except reorders) surfaced to the orchestrator as a
// typed notice.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { swapIfConnected } from "./domutil";
import {
  canApprove,
  canProceed,
  doneCount,
  isAwaitingHuman,
  REQUEST_CHANGES_STATUS,
  retainExisting,
  STATUSES,
  taskActivityState,
} from "./taskboard";
import { approveTask, groupSummary, workflowStatus, type WorkflowStatus } from "./orchestration";
import { normalizeComment } from "./autonomy";
import { approveWillMerge, gateExitsMessage } from "./workflowstatus";

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
  private clearDoneBtn: HTMLButtonElement;
  private clearDoneTimer: number | undefined;
  private deleteSelectedBtn: HTMLButtonElement;
  private deleteSelectedTimer: number | undefined;
  private toastEl: HTMLElement;
  private toastTimer: number | undefined;
  private tasks: OrchTask[] = [];
  /** Ids of the group's currently-live agents (#339 refinement) — an
   *  assignee not in this set reads as history, not active work, however
   *  recently its task was touched. Refreshed alongside `tasks`; best-effort
   *  (an empty set just means everything reads as idle until the next
   *  successful refresh, never a broken board). */
  private liveAgentIds = new Set<string>();
  /** The group's live workflow-mode status (#316), for the gate-aware Approve
   *  label below — refetched alongside `tasks` on every board refresh. `null`
   *  before the first successful read, or if that read fails; either way
   *  `approveWillMerge` treats it as "no gate known" (Approve reads plain)
   *  rather than guessing a warning it can't back up. */
  private workflow: WorkflowStatus | null = null;
  /** Task ids with their notes section expanded (survives re-renders). */
  private expanded = new Set<string>();
  /** Task ids the human has ticked for batch delete. Frontend-only, so it's
   *  pruned to live rows on every refresh (see retainExisting). */
  private selected = new Set<string>();
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

    // Batch-clear all done tasks in one action. Hidden until there is
    // something to clear (updated in render). Two-click confirm — a mis-click
    // must not wipe the board — mirroring the per-row delete. The backend does
    // this as one operation so the orchestrator gets a single board-change
    // notice for the whole batch, not one per task (#120).
    this.clearDoneBtn = el("button", "pane-btn clear-done", "") as HTMLButtonElement;
    this.clearDoneBtn.hidden = true;
    this.clearDoneBtn.addEventListener("click", () => this.onClearDone());
    head.append(this.clearDoneBtn);

    // Multi-select delete: tick task rows, then clear them in one action. Like
    // "delete all done" it's a single backend call (one coalesced notice #120)
    // with a two-click confirm; hidden until at least one row is selected.
    this.deleteSelectedBtn = el("button", "pane-btn delete-selected", "") as HTMLButtonElement;
    this.deleteSelectedBtn.hidden = true;
    this.deleteSelectedBtn.addEventListener("click", () => this.onDeleteSelected());
    head.append(this.deleteSelectedBtn);

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
    clearTimeout(this.clearDoneTimer);
    clearTimeout(this.deleteSelectedTimer);
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
    try {
      const summary = await groupSummary(this.groupId);
      this.liveAgentIds = new Set(summary.agents.map((a) => a.id));
    } catch {
      // Best-effort enrichment, not core data: the board still renders on
      // the tasks alone, just with every assignee reading as history until
      // the next successful refresh — never a broken board over this.
      this.liveAgentIds = new Set();
    }
    // Best-effort: the gate-aware Approve label (#316) is an enrichment, not
    // the board's primary data — a failed read must not toast-error the whole
    // board (tasks above already succeeded), it just leaves Approve unlabeled.
    this.workflow = await workflowStatus(this.groupId).catch(() => null);
    // Drop any ticked rows that vanished from the board (orchestrator edit,
    // another delete) so the "delete selected" count can't outlive its rows.
    this.selected = retainExisting(this.selected, this.tasks);
    this.render();
  }

  private async addTask(): Promise<void> {
    const title = this.addInput.value.trim();
    if (!title) return;
    this.addInput.value = "";
    await this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, title }));
  }

  /** Reflect the current done-count on the batch-clear button, resetting any
   *  pending confirm — called from render() so the label always matches the
   *  board (and a stale "sure?" can't linger after the set changes). */
  private updateClearDone(): void {
    clearTimeout(this.clearDoneTimer);
    delete this.clearDoneBtn.dataset.confirm;
    const n = doneCount(this.tasks);
    this.clearDoneBtn.hidden = n === 0;
    this.clearDoneBtn.textContent = `🗑 done (${n})`;
    this.clearDoneBtn.title = `Delete all ${n} done task${n === 1 ? "" : "s"} — the orchestrator is notified once`;
  }

  /** Two-click confirm, then delete every done task in one backend call. The
   *  batch is a single operation so the orchestrator gets ONE board-change
   *  notice, not one per task (#120). */
  private onClearDone(): void {
    if (this.clearDoneBtn.dataset.confirm) {
      clearTimeout(this.clearDoneTimer);
      delete this.clearDoneBtn.dataset.confirm;
      void this.mutate(invoke("orch_delete_done_tasks", { groupId: this.groupId }));
      return;
    }
    const n = doneCount(this.tasks);
    this.clearDoneBtn.dataset.confirm = "1";
    this.clearDoneBtn.textContent = `delete ${n}?`;
    this.clearDoneTimer = window.setTimeout(() => this.updateClearDone(), 2500);
  }

  /** Reflect the current selection size on the delete-selected button and reset
   *  any pending confirm — called from render() so the label tracks the (pruned)
   *  selection and a stale "sure?" can't linger after the set changes. */
  private updateDeleteSelected(): void {
    clearTimeout(this.deleteSelectedTimer);
    delete this.deleteSelectedBtn.dataset.confirm;
    const n = this.selected.size;
    this.deleteSelectedBtn.hidden = n === 0;
    this.deleteSelectedBtn.textContent = `🗑 selected (${n})`;
    this.deleteSelectedBtn.title = `Delete the ${n} selected task${n === 1 ? "" : "s"} — the orchestrator is notified once`;
  }

  /** Two-click confirm, then delete every selected task in one backend call —
   *  by id, so exactly the ticked rows go (unknown ids are skipped backend-side
   *  if the board shifted). One coalesced board-change notice for the batch,
   *  mirroring "delete all done" (#120). Selection is cleared here; the refresh
   *  that follows the delete re-prunes it anyway. */
  private onDeleteSelected(): void {
    if (this.deleteSelectedBtn.dataset.confirm) {
      clearTimeout(this.deleteSelectedTimer);
      delete this.deleteSelectedBtn.dataset.confirm;
      const ids = [...this.selected];
      this.selected = new Set();
      void this.mutate(invoke("orch_delete_tasks", { groupId: this.groupId, ids }));
      return;
    }
    const n = this.selected.size;
    this.deleteSelectedBtn.dataset.confirm = "1";
    this.deleteSelectedBtn.textContent = `delete ${n}?`;
    this.deleteSelectedTimer = window.setTimeout(() => this.updateDeleteSelected(), 2500);
  }

  /** Open a task's issue/PR reference in the default browser. */
  private openRef(kind: "issue" | "pr", value: string): void {
    invoke("orch_open_ref", { groupId: this.groupId, kind, value }).catch((err) =>
      this.toast(String(err))
    );
  }

  /** Merge-gate "approve & allow merge": a modal confirm that makes explicit
   *  this AUTHORIZES the merge (a real one-time grant, not just a status flip),
   *  with an optional instructions box delivered to the orchestrator. Empty
   *  comment = plain approve (grant only). The modal step is the confirm — a
   *  bare click never issues a grant. */
  private approveWithComment(t: OrchTask): void {
    if (this.dialogEl) return; // one dialog at a time
    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(
      el("div", "tasks-dialog-title", `${t.pr ? "Approve & allow merge" : "Approve"} — ${t.id}`)
    );
    box.append(
      el(
        "div",
        "tasks-dialog-note",
        t.pr
          ? "This authorizes exactly one merge of this PR (single-use grant, expires in ~30 min) " +
              "and tells the orchestrator to merge. Add optional instructions, or leave empty to just approve."
          : "This marks the item done and tells the orchestrator. No PR is linked, so no merge is " +
              "authorized. Add optional instructions, or leave empty to just approve."
      )
    );
    // #316: a human Approve grant is never what opens a workflow-gated merge
    // (#197/#222) — say so again here, not just on the button label, since a
    // human who clicked through to this dialog is the one about to act on it.
    const gate = this.workflow ? approveWillMerge(this.workflow, t) : { ok: true };
    if (!gate.ok && gate.reason) {
      const sentence = gate.reason[0].toUpperCase() + gate.reason.slice(1);
      box.append(el("div", "tasks-dialog-note gate-warn", `${sentence}. ${gateExitsMessage()}`));
    }

    const ta = document.createElement("textarea");
    ta.className = "dlg-input tasks-dialog-text";
    ta.placeholder = "Optional instructions for the agent — e.g. \"squash-merge and delete the branch\".";
    ta.spellcheck = false;
    ta.rows = 3;

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const confirm = el(
      "button",
      "dlg-btn primary",
      t.pr ? "Approve & allow merge" : "Approve"
    ) as HTMLButtonElement;
    actions.append(cancel, confirm);
    box.append(ta, actions);
    overlay.append(box);

    const close = () => {
      overlay.remove();
      this.dialogEl = null;
    };
    const submit = () => {
      close();
      // Empty/whitespace comment → null (grant only, no note).
      void this.mutate(approveTask(this.groupId, t.id, normalizeComment(ta.value)));
    };
    cancel.addEventListener("click", close);
    confirm.addEventListener("click", submit);
    // Keep keystrokes off the underlying terminal; Esc cancels, Ctrl/⌘+Enter confirms.
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
      // Record the findings, then reopen the task as working (#339
      // refinement) — state honesty: the board must never keep showing the
      // Approve button on a task that just had changes requested on it.
      void this.mutate(
        invoke("orch_request_changes", { groupId: this.groupId, id: t.id, findings }).then(() =>
          invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, status: REQUEST_CHANGES_STATUS })
        )
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
    this.updateClearDone();
    this.updateDeleteSelected();
    this.listEl.replaceChildren();
    if (this.tasks.length === 0) {
      this.listEl.appendChild(el("div", "tasks-empty", "No tasks yet — the orchestrator adds them as work items come in, or add one below."));
      return;
    }
    this.tasks.forEach((t, i) => this.listEl.appendChild(this.renderTask(t, i)));
  }

  private renderTask(t: OrchTask, index: number): HTMLElement {
    const row = el("div", "task-row");
    if (isAwaitingHuman(t.status)) row.classList.add("awaiting-human");
    const activity = taskActivityState(t.status, t.assignee, this.liveAgentIds);
    if (activity) row.classList.add(`task-row-${activity}`);

    // Multi-select: tick to add the row to the batch-delete set. A checkbox
    // (over ctrl/shift-click) keeps the affordance discoverable — the human
    // asked to "multi select the tasks and click a button" (#120). Selection is
    // frontend-only; the checked state is rebuilt from `selected` each render.
    const check = document.createElement("input");
    check.type = "checkbox";
    check.className = "task-select";
    check.checked = this.selected.has(t.id);
    check.title = "Select for batch delete";
    check.addEventListener("change", () => {
      if (check.checked) this.selected.add(t.id);
      else this.selected.delete(t.id);
      this.updateDeleteSelected();
    });

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
    // The first thing the eye should land on: unmistakable, not just a tint.
    if (activity === "active") {
      const badge = el("span", "task-active-badge", `● ACTIVE — ${t.assignee}`);
      badge.title = `${t.assignee} is actively working on this right now`;
      top.appendChild(badge);
    }
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
    ] as const) {
      if (!label) continue;
      const chip = el("button", `task-chip ${cls} link`, label) as HTMLButtonElement;
      chip.title = `Open ${kind === "issue" ? "issue" : "PR"} ${label} in browser`;
      chip.addEventListener("click", () => this.openRef(kind, label));
      top.appendChild(chip);
    }
    // The assignee chip is LIVE or HISTORY (#339 refinement) — an old
    // assignee from a killed/resumed/reassigned session must read as past,
    // never blend in as if the same agent were still sitting there.
    if (t.assignee) {
      const isLive = this.liveAgentIds.has(t.assignee);
      const chip = el("span", `task-chip agent ${isLive ? "live" : "history"}`, t.assignee);
      chip.title = isLive
        ? "Currently live agent"
        : "Assigned in a past session — this agent is not currently live";
      top.appendChild(chip);
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
    // Once changes are requested (see requestChanges below), the status
    // moves off pr/human-testing and canApprove goes false on its own — a
    // reopened task can never keep showing a stale Approve button.
    if (canApprove(t.status)) {
      // #316: an Approve that CANNOT succeed under an armed workflow gate says
      // so up front — the button stays clickable (the grant/note still get
      // recorded, and Approve is the human's own gate regardless — see
      // approveWithComment's dialog note), it just never claims a merge that
      // won't happen. `this.workflow === null` (read failed, or hasn't landed
      // yet) reads as "no gate known", the same conservative default the
      // no-PR case already had.
      const gate = this.workflow ? approveWillMerge(this.workflow, t) : { ok: true };
      const approve = el(
        "button",
        "task-btn approve",
        gate.ok ? (t.pr ? "✓ Approve & allow merge" : "✓ Approve") : `✓ Approve (${gate.reason})`
      ) as HTMLButtonElement;
      approve.title = gate.ok
        ? t.pr
          ? "Authorize the merge: write a one-time grant for this PR and tell the orchestrator to merge " +
            "(optionally with instructions). The grant is single-use and expires in ~30 min."
          : "Approve: mark this item done and tell the orchestrator (optionally with instructions). " +
            "No PR is linked, so no merge is authorized."
        : `This still records your grant/note, but the workflow merge gate will refuse the merge. ${gateExitsMessage()}`;
      if (!gate.ok) approve.classList.add("gated");
      approve.addEventListener("click", () => this.approveWithComment(t));
      const changes = el("button", "task-btn changes", "✎ Changes") as HTMLButtonElement;
      changes.title = "Request changes — send findings back to the orchestrator";
      changes.addEventListener("click", () => this.requestChanges(t));
      top.append(approve, changes);
    }

    // Proceed: the human's promote verdict on a prototype (#147). Flips the item
    // to in-progress and tells the orchestrator to run the full production build.
    // Two-click confirm (like delete) — promoting kicks off real work, so a
    // mis-click shouldn't launch it.
    if (canProceed(t.status)) {
      const proceed = el("button", "task-btn proceed", "▶ Proceed") as HTMLButtonElement;
      proceed.title = "Promote this prototype — tell the orchestrator to build the production version";
      proceed.addEventListener("click", () => {
        if (proceed.dataset.confirm) {
          void this.mutate(invoke("orch_proceed_task", { groupId: this.groupId, id: t.id }));
        } else {
          proceed.dataset.confirm = "1";
          proceed.textContent = "promote?";
          window.setTimeout(() => {
            delete proceed.dataset.confirm;
            proceed.textContent = "▶ Proceed";
          }, 2500);
        }
      });
      top.appendChild(proceed);
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

    row.append(check, order, main);
    return row;
  }
}
