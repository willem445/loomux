// Task-board overlay for orchestrator panes: the human's live window into
// the group's work queue (tasks.json, maintained by the orchestrator via
// MCP tools and edited here). Supports status changes, inline title edits,
// notes, reordering, add, and delete. Every human edit is audited
// backend-side and (except reorders) surfaced to the orchestrator as a
// typed notice.

import { invoke, listen, type UnlistenFn } from "./transport.ts";
import { swapIfConnected } from "./domutil";
import {
  approvableSelection,
  blockedTaskMap,
  blockingAncestor,
  boardMarker,
  boardUsesDeps,
  boardUsesHierarchy,
  canApprove,
  canProceed,
  childCounts,
  clearableCount,
  clearedIds,
  isCleared,
  settledIds,
  depCandidates,
  depState,
  DONE_STATUS,
  doneCount,
  grantableCount,
  hasMissingParent,
  indentLevel,
  isAwaitingHuman,
  isReady,
  KINDS,
  kindCandidates,
  nextPicker,
  parentCandidates,
  pickerIsOpen,
  QUEUED_STATUS,
  reorderWithSubtree,
  REQUEST_CHANGES_STATUS,
  retainExisting,
  siblingPosition,
  STATUSES,
  subtreeAllDone,
  taskActivityState,
  unmetDeps,
  visibleRows,
  withDep,
  withoutDep,
  type BoardMarker,
  type BoardRow,
  type PickerField,
  type PickerTarget,
} from "./taskboard";
import {
  approveTask,
  approveTasks,
  groupSummary,
  questionsList,
  workflowStatus,
  type WorkflowStatus,
} from "./orchestration";
import { isPending, type OrchQuestion } from "./decisions";
import { normalizeComment } from "./autonomy";
import { CoalescingRefresh } from "./refreshgate";
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
  /** The branch `pr` targets (#581), as the orchestrator recorded it. Absent
   *  on every pre-#581 task and on any board that doesn't record it — the
   *  Approve relabel treats that as "base unknown" and stays conservative.
   *  Display metadata: nothing here gates a merge. */
  pr_base?: string | null;
  assignee?: string | null;
  session?: string | null;
  notes: OrchTaskNote[];
  /** Ids of tasks on this board that must be `done` first (#582). Optional
   *  because the backend omits an empty vec entirely
   *  (`skip_serializing_if`) — every pre-#582 board arrives with no key at
   *  all, so the board must never assume the array is there. */
  deps?: string[];
  /** Non-blocking "see also" ids (#582). Rendered read-only here: the human
   *  board's `orch_upsert_task` deliberately takes `deps` but not `related`,
   *  which the orchestrator maintains through its own tools. */
  related?: string[];
  /** The task this one sits inside (#958) — containment, not ordering (that
   *  is still `deps`). Optional on the wire like the link arrays: the backend
   *  skips it when absent, so every pre-#958 board arrives with no key. The
   *  board derives the whole tree from this field; nothing gates on it. */
  parent?: string | null;
  /** Advisory Agile level — one of `KINDS` (#958). Advisory means the board
   *  only labels the row with it: a story directly inside an epic is legal,
   *  and no affordance here changes behaviour based on it. */
  kind?: string | null;
  /** Worktree path where a demo of this row lives (#1091 slice B) — recorded
   *  by the orchestrator on a `prototype`/`human-testing` row so the NEEDS-YOU
   *  panel can tell the human where to go run it. Optional on the wire like
   *  `parent`/`kind`: the backend omits the key entirely when absent, so a
   *  pre-#1091 board arrives without it. Absent means "no path recorded", never
   *  "there is no demo" — nothing here guesses one from an assignee's cwd.
   *  Display metadata: nothing gates on it. */
  demo_path?: string | null;
  /** When the human cleared this row out of their working board view (#1152).
   *  Optional on the wire like `parent`/`kind`/`demo_path`: the backend omits
   *  the key entirely when absent, so a pre-#1152 board arrives without it and
   *  absent means "not cleared". An ARCHIVE marker, never a delete — the row,
   *  its notes and its links stay on the board, and `isCleared` only honours
   *  the stamp while the row is still `done`. */
  cleared_ms?: number | null;
  updated_ms: number;
}

/** The nest picker's "take it back to the top level" option (#958). A sentinel
 *  rather than `""`, because the empty value is already the picker's own
 *  "nothing chosen yet" placeholder; it is translated to the empty string —
 *  which is what `orch_upsert_task` reads as "clear the container" — at the
 *  moment of the write, and never travels to the backend as an id. Ids are
 *  monotonic `t-<n>`, so no row can ever collide with this value. */
const TOP_LEVEL_CHOICE = "__top_level__";

/** The kind picker's "clear the label" option (#958 slice K) — same sentinel
 *  shape as `TOP_LEVEL_CHOICE` above and for the same reason: `""` is already
 *  the picker's own placeholder ("nothing chosen yet"), so the clear needs its
 *  own value. Translated to `""` — which `orch_upsert_task` reads as "clear
 *  the label" — only at the moment of the write, and never sent as a literal
 *  kind. `KINDS` entries are plain words (`epic`, `feature`, …), so no real
 *  kind can ever collide with this value. */
const CLEAR_KIND_CHOICE = "__clear_kind__";

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
  /** Archive every `done` row out of the working view (#1152) — non-destructive,
   *  so no confirm step. */
  private clearDoneBtn: HTMLButtonElement;
  /** Show/hide the archive, and bring all of it back (#1152). */
  private showClearedBtn: HTMLButtonElement;
  private restoreClearedBtn: HTMLButtonElement;
  /** The DESTRUCTIVE batch: delete every `done` row, archive included (#120). */
  private deleteDoneBtn: HTMLButtonElement;
  private deleteDoneTimer: number | undefined;
  private deleteSelectedBtn: HTMLButtonElement;
  private deleteSelectedTimer: number | undefined;
  private approveSelectedBtn: HTMLButtonElement;
  private toastEl: HTMLElement;
  private toastTimer: number | undefined;
  private tasks: OrchTask[] = [];
  /** Pending + settled questions (#1091 slice G) — read alongside `tasks` so
   *  the board can derive the decision-blocked marker. Best-effort like
   *  `liveAgentIds`/`workflow` below: a failed read just means no marker
   *  until the next successful refresh, never a broken board. */
  private questions: OrchQuestion[] = [];
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
  /** Containers the human has collapsed (#958). Frontend-only and deliberately
   *  not persisted — the same shape as `expanded` above: a view preference
   *  that survives re-renders but never becomes board data (the board is the
   *  orchestrator's queue, not this window's UI state). Pruned to live rows on
   *  every refresh, like `selected`. */
  private collapsed = new Set<string>();
  /** Whether the archived (cleared) rows are on screen right now (#1152).
   *  Frontend-only and deliberately not persisted, exactly like `collapsed`
   *  above: WHICH rows are archived is durable board data (`cleared_ms`, written
   *  by the human's own command), but whether this particular window is
   *  currently looking at them is a view preference and belongs nowhere else. */
  private showCleared = false;
  /** The task whose picker is open, if any (#582, #958) — one at a time across
   *  EVERY picker, and kept here rather than in the DOM so a background
   *  refresh re-renders it instead of silently closing it mid-choice. `field`
   *  says which one: a dependency (ordering), a container (nesting), or an
   *  Agile level (#958 slice K). */
  private picking: PickerTarget | null = null;
  /** The picker was just opened by a click, so it should take focus on this
   *  render. Cleared once consumed: a later refresh must re-render the open
   *  picker without stealing focus back from wherever the human has moved. */
  private pickingFocus = false;
  /** A refresh arrived while the human was mid-edit; run it on blur. */
  private pendingRefresh = false;
  /** Single-flight + trailing-edge merge for this view's refresh (#743 S5).
   *  `orch-tasks-changed` fires on EVERY `write_tasks`, and agents write in
   *  bursts (a plan posting ten tasks, a batch status sweep), so an ungated
   *  handler cost one full refetch — three backend commands and a whole-board
   *  re-render — per write, per open board. A burst of N now costs the run
   *  already in flight plus exactly one trailing run, which reads the final
   *  state, so nothing is lost by coalescing. */
  private readonly refresher = new CoalescingRefresh(() => this.refreshNow());
  /** The open request-changes modal, if any (kept to one at a time). */
  private dialogEl: HTMLElement | null = null;
  private unlistenTasks: UnlistenFn | null = null;
  /** #1091 slice G: the board's marker chip needs `orch-questions-changed`
   *  too — a question being asked, answered, or withdrawn changes whether a
   *  row is decision-blocked, independently of any `orch-tasks-changed`. */
  private unlistenQuestions: UnlistenFn | null = null;
  private disposed = false;

  private embedBtn: HTMLButtonElement;
  private closeBtn: HTMLButtonElement;

  constructor(
    private groupId: string,
    private opts: {
      onClose: () => void;
      onEmbedMenu: (anchor: HTMLElement) => void;
      /** Drain any focus request parked for the BOARD (#1091 slice C), called
       *  once per render. Non-null only when something asked for a specific
       *  row while the board was closed or unbuilt — a NEEDS-YOU card citing
       *  `t-N` is the first caller. `undefined` when the host wires no focus. */
      takeFocus?: () => string | null;
      /** The board-to-panel direction of the focus hook (#1091 slice G): ask
       *  the pane to open the NEEDS-YOU panel at `id` — a `q-N` for a
       *  decision-blocked row, this row's own `t-N` for a demo-gated one (see
       *  `boardMarker` in `taskboard.ts`). Returns whether the pane could
       *  route it, mirroring `decisionsview.ts`'s `onFocusTask` the other way. */
      onFocusDecision: (id: string) => boolean;
    }
  ) {
    this.el = el("div", "tasks-view");

    const head = el("div", "tasks-head");
    head.append(el("span", "tasks-title", "task board"));
    head.append(el("span", "tasks-group", groupId));

    // Archive every done row out of the working view in one action (#1152).
    // NOTHING is deleted — the rows keep their notes, links and place in
    // tasks.json, the action is audited, and the 👁 toggle beside this brings
    // them back — so unlike its destructive neighbour below there is no
    // two-click confirm to stand between the human and a reversible action.
    this.clearDoneBtn = el("button", "pane-btn clear-done", "") as HTMLButtonElement;
    this.clearDoneBtn.hidden = true;
    this.clearDoneBtn.addEventListener("click", () => this.onClearDone());
    head.append(this.clearDoneBtn);

    // Show/hide the archive. A per-window view preference, so it lives in
    // `showCleared` and is not persisted; hidden entirely while there is
    // nothing archived to look at.
    this.showClearedBtn = el("button", "pane-btn show-cleared", "") as HTMLButtonElement;
    this.showClearedBtn.hidden = true;
    this.showClearedBtn.addEventListener("click", () => {
      this.showCleared = !this.showCleared;
      this.render();
    });
    head.append(this.showClearedBtn);

    // Bulk un-archive, the counterpart to one clear click. Only shown while the
    // archive is on screen: a bulk restore the human cannot see the effect of
    // would be the same kind of blind batch the clear button avoids by being
    // reversible in the first place.
    this.restoreClearedBtn = el("button", "pane-btn restore-cleared", "") as HTMLButtonElement;
    this.restoreClearedBtn.hidden = true;
    this.restoreClearedBtn.addEventListener("click", () => this.onRestoreAll());
    head.append(this.restoreClearedBtn);

    // Batch-DELETE all done tasks in one action. Hidden until there is
    // something to delete (updated in render). Two-click confirm — a mis-click
    // must not wipe the board — mirroring the per-row delete. The backend does
    // this as one operation so the orchestrator gets a single board-change
    // notice for the whole batch, not one per task (#120).
    this.deleteDoneBtn = el("button", "pane-btn delete-done", "") as HTMLButtonElement;
    this.deleteDoneBtn.hidden = true;
    this.deleteDoneBtn.addEventListener("click", () => this.onDeleteDone());
    head.append(this.deleteDoneBtn);

    // Multi-select delete: tick task rows, then clear them in one action. Like
    // "delete all done" it's a single backend call (one coalesced notice #120)
    // with a two-click confirm; hidden until at least one row is selected.
    this.deleteSelectedBtn = el("button", "pane-btn delete-selected", "") as HTMLButtonElement;
    this.deleteSelectedBtn.hidden = true;
    this.deleteSelectedBtn.addEventListener("click", () => this.onDeleteSelected());
    head.append(this.deleteSelectedBtn);

    // Multi-select approve (#507): tick several merge-gate rows and authorize
    // them in one action. Each PR still gets its own one-time grant — what the
    // batch saves is the orchestrator receiving N separate prompts. No
    // two-click confirm here (unlike delete): the modal below IS the confirm,
    // exactly as it is for a single Approve, because a grant is an
    // authorization the human should read before issuing.
    this.approveSelectedBtn = el("button", "pane-btn approve-selected", "") as HTMLButtonElement;
    this.approveSelectedBtn.hidden = true;
    this.approveSelectedBtn.addEventListener("click", () => this.onApproveSelected());
    head.append(this.approveSelectedBtn);

    // Embed side-picker: switch between the floating overlay and any of the
    // pane's (up to three) embed slots (#361) — a discrete, user-initiated
    // layout change, like a split (see doc/design/embedded-panels.md).
    // setPanelActive() below keeps the icon/tooltip in sync with whether the
    // pane currently has this docked, regardless of which side.
    this.embedBtn = el("button", "pane-btn embed", "⬒") as HTMLButtonElement;
    this.embedBtn.addEventListener("click", () => opts.onEmbedMenu(this.embedBtn));
    head.append(this.embedBtn);

    this.closeBtn = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    this.closeBtn.title = "Close (Alt+T)";
    this.closeBtn.addEventListener("click", opts.onClose);
    head.append(this.closeBtn);
    // Now that both buttons `setPanelActive` touches exist.
    this.setPanelActive(false);

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
        if (this.pendingRefresh && !this.isEditing()) this.refresh();
      }, 0);
    });

    void listen<{ group_id: string }>("orch-tasks-changed", ({ payload }) => {
      if (payload.group_id === this.groupId) this.refresh();
    }).then((u) => {
      if (this.disposed) u();
      else this.unlistenTasks = u;
    });
    // #1091 slice G: the marker chip's decision signal depends on the
    // questions list too, not just the board — both listeners share the one
    // `refresher` gate above, so a simultaneous burst on both streams still
    // coalesces to one refresh, exactly like the NEEDS-YOU panel's own pair
    // (decisionsview.ts).
    void listen<{ group_id: string }>("orch-questions-changed", ({ payload }) => {
      if (payload.group_id === this.groupId) this.refresh();
    }).then((u) => {
      if (this.disposed) u();
      else this.unlistenQuestions = u;
    });
  }

  /** Called by the pane whenever the view is (re)opened, in either mode. */
  show(): void {
    this.refresh();
  }

  /** Reflect which mode the pane currently has this view mounted in —
   *  called on creation and on every embed/un-embed. Pure display state; the
   *  pane owns the actual toggle (it has to move the view between an overlay
   *  host and the pane's single embed-panel slot). Named `setPanelActive`,
   *  not `setEmbedded`, across every embeddable view (#361 generalization) —
   *  `GitView` already has an unrelated ctor option called `embedded`
   *  (#217: is this view hosted as a whole content PANE?), and reusing that
   *  word for a same-named but different-meaning method on the same class
   *  would read as the two being connected when they aren't. */
  setPanelActive(active: boolean): void {
    this.embedBtn.classList.toggle("active", active);
    this.embedBtn.textContent = active ? "⬓" : "⬒";
    this.embedBtn.title = active
      ? "Un-embed — back to a floating overlay"
      : "Embed beside the terminal (resizes this pane)";
    // The overlay toggle (this button, the pane header's own board button)
    // is disabled while docked (#361 user-demo finding — see embedtoggle.ts):
    // only un-embedding closes a docked board now.
    this.closeBtn.disabled = active;
    this.closeBtn.title = active ? "Docked — un-embed it (side menu) to close" : "Close (Alt+T)";
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.toastTimer);
    clearTimeout(this.deleteDoneTimer);
    clearTimeout(this.deleteSelectedTimer);
    this.unlistenTasks?.();
    this.unlistenQuestions?.();
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
      this.refresh(); // resync UI with reality after a failed edit
    }
  }

  /** True while the human is typing in an inline editor inside the list
   *  (title rename, note input) — re-rendering would destroy their edit. */
  private isEditing(): boolean {
    const a = document.activeElement;
    return !!a && this.listEl.contains(a) && (a.tagName === "INPUT" || a.tagName === "TEXTAREA");
  }

  /** Ask for a refresh. Coalesced — see `refresher`. The gate lives here rather
   *  than at each call site so a future caller cannot forget it, and so the
   *  event handler and the human's own actions share one in-flight run. */
  private refresh(): void {
    this.refresher.request();
  }

  /** One board refresh. Only `refresher` calls this. */
  private async refreshNow(): Promise<void> {
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
      // #904: `null` when the backend refuses the group id — the same
      // best-effort degrade the `catch` below already takes, since an empty
      // live set reads every assignee as history rather than breaking the board.
      this.liveAgentIds = new Set(summary?.agents.map((a) => a.id) ?? []);
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
    // Best-effort like liveAgentIds/workflow above (#1091 slice G): the
    // decision-blocked marker is an enrichment on top of the tasks the board
    // already has, not core data — a failed read just means no marker until
    // the next successful refresh.
    this.questions = await questionsList(this.groupId).catch(() => []);
    // Drop any ticked rows that vanished from the board (orchestrator edit,
    // another delete) so the "delete selected" count can't outlive its rows.
    this.selected = retainExisting(this.selected, this.tasks);
    // Same for collapsed containers (#958) — housekeeping rather than
    // correctness (ids are monotonic, so a stale flag can never match a later
    // row): the set is kept to what the board actually holds.
    this.collapsed = retainExisting(this.collapsed, this.tasks);
    // Same for an open picker whose row has gone (#582, #958).
    if (this.picking && !this.tasks.some((t) => t.id === this.picking?.id)) this.picking = null;
    this.render();
  }

  private async addTask(): Promise<void> {
    const title = this.addInput.value.trim();
    if (!title) return;
    this.addInput.value = "";
    await this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, title }));
  }

  /** Reflect the archive on the three cleared-item buttons (#1152) — called
   *  from render() so every label matches the board it sits above.
   *
   *  The two counts are deliberately different questions: the clear button
   *  offers what it would archive NOW (`clearableCount` — done, not already
   *  archived), while the toggle names what is actually being hidden
   *  (`clearedIds` — the closure, so a cleared container still holding live work
   *  is not counted as out of sight when it isn't). */
  private updateCleared(): void {
    const clearable = clearableCount(this.tasks);
    this.clearDoneBtn.hidden = clearable === 0;
    this.clearDoneBtn.textContent = `📥 clear done (${clearable})`;
    this.clearDoneBtn.title =
      `Clear ${clearable} done item${clearable === 1 ? "" : "s"} out of this list. ` +
      `Nothing is deleted: they stay in the board file and the audit log, and 👁 brings them back.`;

    const archived = clearedIds(this.tasks).size;
    this.showClearedBtn.hidden = archived === 0;
    this.showClearedBtn.classList.toggle("active", this.showCleared);
    this.showClearedBtn.textContent = this.showCleared
      ? `👁 hide cleared (${archived})`
      : `👁 show cleared (${archived})`;
    this.showClearedBtn.title = this.showCleared
      ? `Hide the ${archived} cleared item${archived === 1 ? "" : "s"} again`
      : `Show the ${archived} cleared item${archived === 1 ? "" : "s"} — they are still on the board, just out of the way`;

    // Only offered while they are on screen — see the ctor comment.
    this.restoreClearedBtn.hidden = archived === 0 || !this.showCleared;
    this.restoreClearedBtn.textContent = `↩ restore all (${archived})`;
    // "All" means the rows the toggle beside it is hiding — the same set, so
    // the two counts can never disagree about what this button acts on. A
    // cleared container still holding live work is not in that set (it never
    // left the list); its own ↩ is how it comes back.
    this.restoreClearedBtn.title =
      `Bring back all ${archived} item${archived === 1 ? "" : "s"} the 👁 toggle hides`;
  }

  /** Archive every clearable done row in one backend call. Non-destructive and
   *  reversible, so no confirm step: the rows stay in `tasks.json` with a
   *  `cleared_ms` stamp, the write is audited, and 👁 / ↩ undo it. */
  private onClearDone(): void {
    void this.mutate(invoke("orch_clear_done_tasks", { groupId: this.groupId }));
  }

  /** Un-archive everything currently hidden, in one backend call. */
  private onRestoreAll(): void {
    const ids = [...clearedIds(this.tasks)];
    if (ids.length === 0) return;
    void this.mutate(invoke("orch_restore_cleared_tasks", { groupId: this.groupId, ids }));
  }

  /** Reflect the current done-count on the batch-delete button, resetting any
   *  pending confirm — called from render() so the label always matches the
   *  board (and a stale "sure?" can't linger after the set changes). */
  private updateDeleteDone(): void {
    clearTimeout(this.deleteDoneTimer);
    delete this.deleteDoneBtn.dataset.confirm;
    const n = doneCount(this.tasks);
    this.deleteDoneBtn.hidden = n === 0;
    this.deleteDoneBtn.textContent = `🗑 done (${n})`;
    // Every done row, cleared ones included — the archive is not a safe place
    // this button spares, and a count that quietly excluded it would understate
    // what one confirm click is about to destroy.
    this.deleteDoneBtn.title =
      `Delete all ${n} done task${n === 1 ? "" : "s"}, cleared ones included — permanent, ` +
      `and the orchestrator is notified once`;
  }

  /** Two-click confirm, then delete every done task in one backend call. The
   *  batch is a single operation so the orchestrator gets ONE board-change
   *  notice, not one per task (#120). */
  private onDeleteDone(): void {
    if (this.deleteDoneBtn.dataset.confirm) {
      clearTimeout(this.deleteDoneTimer);
      delete this.deleteDoneBtn.dataset.confirm;
      void this.mutate(invoke("orch_delete_done_tasks", { groupId: this.groupId }));
      return;
    }
    const n = doneCount(this.tasks);
    this.deleteDoneBtn.dataset.confirm = "1";
    this.deleteDoneBtn.textContent = `delete ${n}?`;
    this.deleteDoneTimer = window.setTimeout(() => this.updateDeleteDone(), 2500);
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

  /** Reflect the *approvable* part of the selection on the bulk-approve button
   *  — ticking a `queued` row must not inflate a count of merge grants the
   *  human is about to issue (#507). Hidden when nothing ticked is at the
   *  gate, so the affordance only appears when it can actually do something. */
  private updateApproveSelected(): void {
    const picked = approvableSelection(this.selected, this.tasks);
    const n = picked.length;
    // The grant count is the LINKED-PR count, not the selection size — a gate
    // row with no PR is approved but never granted, so a tooltip promising one
    // grant per selected row would claim authority the backend won't issue.
    const grants = grantableCount(picked);
    this.approveSelectedBtn.hidden = n === 0;
    this.approveSelectedBtn.textContent = `✓ Approve selected (${n})`;
    this.approveSelectedBtn.title =
      `Approve the ${n} selected merge-gate task${n === 1 ? "" : "s"}: ` +
      (grants === 0
        ? "none has a PR linked, so no merge is authorized"
        : `${grants} one-time merge grant${grants === 1 ? "" : "s"} ` +
          `(one per linked PR, single-use, ~30 min)`) +
      `, and the orchestrator is notified once for the batch`;
  }

  /** Bulk merge-gate approve: one modal listing exactly what is about to be
   *  authorized, with an optional per-task note on each row, then ONE backend
   *  call. Same authority as clicking Approve on each row — N per-PR one-time
   *  grants — delivered as a single consolidated notice (#507). */
  private onApproveSelected(): void {
    if (this.dialogEl) return; // one dialog at a time
    const picked = approvableSelection(this.selected, this.tasks);
    if (picked.length === 0) return;
    const withPr = grantableCount(picked);

    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(
      el("div", "tasks-dialog-title", `Approve ${picked.length} selected — allow ${withPr} merge${withPr === 1 ? "" : "s"}`)
    );
    box.append(
      el(
        "div",
        "tasks-dialog-note",
        `${withPr === 0
          ? "None of these has a PR linked, so no merge is authorized — they are marked done and the orchestrator is told."
          : `This authorizes exactly one merge of each of the ${withPr} linked PR${withPr === 1 ? "" : "s"} ` +
            "(a separate single-use grant per PR, each expiring in ~30 min)."} ` +
          "The orchestrator is notified once for the whole batch. Notes below are optional."
      )
    );
    // #316, same as the single-approve dialog: an armed workflow gate refuses
    // the merge whatever the human grants here. Say which of the selected rows
    // that applies to, so a batch can't hide it behind a count.
    const gated = this.workflow
      ? picked.filter((t) => !approveWillMerge(this.workflow as WorkflowStatus, t).ok)
      : [];
    if (gated.length > 0) {
      box.append(
        el(
          "div",
          "tasks-dialog-note gate-warn",
          `The workflow merge gate will refuse ${gated.length} of these (${gated
            .map((t) => t.id)
            .join(", ")}). ${gateExitsMessage()}`
        )
      );
    }

    // One row per task: what it is, and its own note. Per-task rather than one
    // shared note because the notes ride to the orchestrator attached to their
    // PR ("squash this one", "rebase that one") — a single box for a batch
    // would force the human to write the attribution by hand.
    const list = el("div", "tasks-dialog-list");
    const inputs = new Map<string, HTMLInputElement>();
    for (const t of picked) {
      const row = el("div", "tasks-dialog-row");
      row.append(el("div", "tasks-dialog-row-title", `${t.id} — ${t.title}${t.pr ? ` (PR ${t.pr})` : " (no PR)"}`));
      const input = document.createElement("input");
      input.type = "text";
      input.className = "dlg-input";
      input.placeholder = "Optional note for this one — e.g. \"squash-merge\".";
      input.spellcheck = false;
      inputs.set(t.id, input);
      row.append(input);
      list.append(row);
    }

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const confirm = el("button", "dlg-btn primary", `Approve ${picked.length}`) as HTMLButtonElement;
    actions.append(cancel, confirm);
    box.append(list, actions);
    overlay.append(box);

    const close = () => {
      overlay.remove();
      this.dialogEl = null;
    };
    const submit = () => {
      close();
      const items = picked.map((t) => ({
        id: t.id,
        comment: normalizeComment(inputs.get(t.id)?.value ?? ""),
      }));
      // Untick only what was approved: rows ticked for a delete but not at the
      // gate stay selected, so the batch-delete count survives this action.
      for (const t of picked) this.selected.delete(t.id);
      void this.mutate(approveTasks(this.groupId, items));
    };
    cancel.addEventListener("click", close);
    confirm.addEventListener("click", submit);
    // Keep keystrokes off the underlying terminal; Esc cancels, Ctrl/⌘+Enter
    // confirms — same as the single-approve dialog, and plain Enter is left
    // inert on purpose: with one note field per row it would be ambiguous
    // which row the human meant to finish, and the action issues real grants.
    box.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
    });
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });

    this.dialogEl = overlay;
    this.el.appendChild(overlay);
    inputs.values().next().value?.focus();
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
    this.updateCleared();
    this.updateDeleteDone();
    this.updateDeleteSelected();
    this.updateApproveSelected();
    this.listEl.replaceChildren();
    if (this.tasks.length === 0) {
      this.listEl.appendChild(el("div", "tasks-empty", "No tasks yet — the orchestrator adds them as work items come in, or add one below."));
      return;
    }
    // Whether this board uses dependencies at all (#582) — computed once for
    // the whole render, since the "ready" mark is a board-level decision (see
    // boardUsesDeps): on a board with no links every queued row is trivially
    // ready and marking them all would say nothing.
    const usesDeps = boardUsesDeps(this.tasks);
    // Same board-level gate for the nesting chrome (#958): a board that nests
    // nothing keeps exactly the rows it has today, with no collapse gutter.
    const usesHierarchy = boardUsesHierarchy(this.tasks);
    // t-N → q-N for every row a PENDING question cites (#1091 slice G) —
    // computed once for the whole render, like usesDeps/usesHierarchy above.
    // `isPending` is decisions.ts's own rule (never re-spelled here — see
    // `blockedTaskMap`'s doc), so a settled question can never leave a stale
    // marker on a row it no longer holds up.
    const blocked = blockedTaskMap(this.questions.filter(isPending));
    // Display order is DERIVED (#958, #1152) — the board array stays flat and
    // its order stays the human's priority order. What is derived from it: the
    // tree (from `parent`), finished subtrees sinking below the live work of
    // their own sibling group, and the archived rows dropping out until 👁.
    // Board-level, computed once for the whole render like `usesDeps`/`blocked`
    // above: which rows are finished subtrees (#1152). Each row's ▲/▼ needs it,
    // and re-deriving it per row would walk the tree once per row.
    const settled = settledIds(this.tasks);
    const rows = visibleRows(this.tasks, this.collapsed, this.showCleared);
    if (rows.length === 0) {
      // The board is not empty — everything on it is archived. Say that,
      // rather than showing the "no tasks yet" line for a board with 400 rows
      // in it, which would read as data loss.
      this.listEl.appendChild(
        el(
          "div",
          "tasks-empty",
          "Every item on this board is cleared. Nothing was deleted — use 👁 show cleared above to bring it back."
        )
      );
      return;
    }
    for (const row of rows) {
      this.listEl.appendChild(this.renderTask(row, usesDeps, usesHierarchy, blocked, settled));
    }
    this.drainFocus();
  }

  /** Bring a requested row into view and flash it (#1091 slice C).
   *
   *  Drained LAST in a render, once the rows exist, and consumed — so an
   *  ordinary refresh never yanks the viewport back to a row the human has
   *  already scrolled away from.
   *
   *  A target that names no row on this render used to be a silent no-op, which
   *  conflates two different situations (#1152 review round 1, finding 4). The
   *  request is CONSUMED either way, so a deep link from the NEEDS-YOU panel
   *  onto a row that is merely off-screen simply appeared to do nothing:
   *  already true for a row inside a collapsed container (#958), and #1152 adds
   *  a second way — a cleared row, hidden until 👁. So the two cases are now
   *  told apart by the board itself rather than by the human's guess:
   *
   *  - the id names NO row on the board — still a silent no-op, because the
   *    task really can be deleted between the request and the render, and that
   *    is not worth an error;
   *  - the id names a row that IS on the board but is not rendered — say so,
   *    and say which affordance brings it back.
   *
   *  A toast rather than an auto-reveal: flipping the human's view (expanding a
   *  container, turning the archive on) as a side effect of a click elsewhere
   *  is a bigger behaviour than the problem, and this at least makes the state
   *  legible. Covers the pre-existing collapsed case too, not just the new one. */
  private drainFocus(): void {
    const target = this.opts.takeFocus?.() ?? null;
    if (!target) return;
    const row = this.listEl.querySelector<HTMLElement>(`[data-item-id="${CSS.escape(target)}"]`);
    if (!row) {
      const known = this.tasks.find((t) => t.id === target);
      if (known) {
        this.toast(
          isCleared(known)
            ? `${target} is cleared — use 👁 show cleared above to bring it back into view.`
            : `${target} is on the board but hidden right now — expand the container it sits in.`
        );
      }
      return;
    }
    row.scrollIntoView({ block: "nearest" });
    row.classList.add("task-row-focused");
    window.setTimeout(() => row.classList.remove("task-row-focused"), 1600);
  }

  /** The dependency line under a task's row (#582): what is blocking it, what
   *  it is merely related to, and — when open — the picker for adding another
   *  dep. Returns null (no line at all) when the task has neither links nor an
   *  open picker, so a board that uses no dependencies keeps exactly the row
   *  height it has today.
   *
   *  Every edit here sends the WHOLE `deps` array through the board's existing
   *  `orch_upsert_task` path (the backend's array args are replace-or-untouched,
   *  never a delta), so it inherits that path's validation and error surfacing:
   *  a cycle, or an id that stopped existing between render and click, is
   *  refused backend-side and lands in this view's toast rather than being
   *  second-guessed here. */
  private renderLinks(t: OrchTask): HTMLElement | null {
    const deps = t.deps ?? [];
    const related = t.related ?? [];
    const picking = this.picking?.id === t.id;
    if (deps.length === 0 && related.length === 0 && !picking) return null;

    const line = el("div", "task-links");

    if (deps.length > 0) {
      line.appendChild(el("span", "task-links-label", "blocked by"));
      for (const id of deps) {
        // met / unmet / missing — the third is only reachable from a
        // hand-edited tasks.json (the backend validates ids on write and
        // strips them on delete), and it reads as its own state because the
        // fix is different: not "wait", but "this link names nothing".
        const state = depState(id, this.tasks);
        const dep = this.tasks.find((x) => x.id === id);
        const chip = el("span", `task-chip dep ${state}`);
        chip.appendChild(
          el("span", "dep-mark", state === "met" ? "✓" : state === "missing" ? "⚠" : "✗")
        );
        chip.appendChild(el("span", "dep-id", id));
        chip.title =
          state === "met"
            ? `${id} "${dep?.title ?? ""}" is done — this dependency is satisfied`
            : state === "missing"
              ? `${id} names no task on this board, so it counts as unmet and this task ` +
                `can never read as ready. Remove the link (✕) or re-create that task.`
              : `${id} "${dep?.title ?? ""}" is ${dep?.status ?? "?"} — this task waits ` +
                `until it is done`;
        const rm = el("button", "task-dep-remove", "✕") as HTMLButtonElement;
        rm.title = `Remove the dependency on ${id}`;
        rm.addEventListener("click", () =>
          void this.mutate(
            invoke("orch_upsert_task", {
              groupId: this.groupId,
              id: t.id,
              deps: withoutDep(t.deps, id),
            })
          )
        );
        chip.appendChild(rm);
        line.appendChild(chip);
      }
    }

    // Read-only: `orch_upsert_task` deliberately takes `deps` and not
    // `related` — the orchestrator maintains see-also links through its own
    // tools, and the board renders them so the human can see the annotation
    // without it silently affecting anything.
    if (related.length > 0) {
      line.appendChild(el("span", "task-links-label", "see also"));
      for (const id of related) {
        const rel = this.tasks.find((x) => x.id === id);
        const chip = el("span", "task-chip related", id);
        chip.title = rel
          ? `${id} "${rel.title}" (${rel.status}) — a non-blocking link set by the orchestrator; ` +
            `it never affects whether this task is ready`
          : `${id} names no task on this board`;
        line.appendChild(chip);
      }
    }

    if (picking) {
      const field = this.picking?.field;
      line.appendChild(
        field === "parent"
          ? this.renderParentPicker(t)
          : field === "kind"
            ? this.renderKindPicker(t)
            : this.renderDepPicker(t)
      );
    }
    return line;
  }

  /** Open (or close) one of the row pickers. One at a time across the whole
   *  board and across every field, so the human is never choosing a
   *  dependency, a container, and an Agile level at the same time in three
   *  places. */
  private togglePicker(id: string, field: PickerField): void {
    this.picking = nextPicker(this.picking, id, field);
    // Focus only when this click OPENED one — a close has nothing to focus.
    this.pickingFocus = this.picking !== null;
    this.render();
  }

  /** A picker's own deferred close (blur/Esc). Every picker calls THIS rather
   *  than each re-deriving the condition — the original two copies (dep,
   *  parent) had already drifted apart from `togglePicker`'s, and a close that
   *  reads fewer signals than the button that opens swallows a click exactly
   *  the width of the difference (see `pickerIsOpen`). One rule, one place —
   *  which is what let the kind picker (#958 slice K) become a third caller of
   *  this same close for free, rather than a third copy of the condition. */
  private closePicker(id: string, field: PickerField): void {
    if (!pickerIsOpen(this.picking, id, field)) return;
    this.picking = null;
    this.render();
  }

  /** The "⤵ nest under…" picker (#958): every other row, plus a top-level
   *  escape when this one is already inside something.
   *
   *  Like the dep picker it does NOT pre-filter the choices the backend would
   *  refuse — its own descendants (a cycle) or a pick that would bust the
   *  depth cap. That rule lives in one authoritative place, inside the
   *  backend's lock, and its error names the path; a second copy here could
   *  only ever disagree with it, and it surfaces through the same toast. */
  private renderParentPicker(t: OrchTask): HTMLElement {
    const options = parentCandidates(t, this.tasks);
    if (options.length === 0 && !t.parent) {
      return el("span", "task-links-label", "no other task to nest under");
    }
    const sel = document.createElement("select");
    sel.className = "task-dep-picker parent";
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "⤵ nest under…";
    sel.appendChild(placeholder);
    if (t.parent) {
      // The clear. `orch_upsert_task` reads an EMPTY parent as "take it to the
      // top level" (omitting the field means "leave it alone"), so the sentinel
      // below is mapped to "" on the way out — never sent as a literal id.
      const top = document.createElement("option");
      top.value = TOP_LEVEL_CHOICE;
      top.textContent = "↥ top level (leave its container)";
      sel.appendChild(top);
    }
    for (const c of options) {
      const opt = document.createElement("option");
      opt.value = c.id;
      opt.textContent = `${c.id} — ${c.title}`;
      sel.appendChild(opt);
    }
    sel.value = "";

    const close = () => this.closePicker(t.id, "parent");
    sel.addEventListener("change", () => {
      const pick = sel.value;
      if (!pick) return;
      const parent = pick === TOP_LEVEL_CHOICE ? "" : pick;
      this.picking = null;
      // Close on our own rather than waiting for the board-change event: if the
      // write is refused (a cycle, the depth cap), mutate() toasts the
      // backend's own error and resyncs.
      this.render();
      void this.mutate(
        invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, parent })
      );
    });
    // Keep keystrokes off the terminal underneath; Esc backs out unwritten.
    sel.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
    });
    sel.addEventListener("blur", () => window.setTimeout(close, 0));
    if (this.pickingFocus) {
      this.pickingFocus = false;
      window.setTimeout(() => sel.focus(), 0);
    }
    return sel;
  }

  /** The "🏷 set kind…" picker (#958 slice K): the three Agile levels this row
   *  doesn't already carry, plus a clear option once it carries one. `kind` is
   *  advisory-only (§2 of doc/design/task-hierarchy.md) — this picker changes
   *  a label and nothing else, same as the badge it sits under says. Unlike
   *  the nest/dep pickers there is no authoritative backend rule this could
   *  disagree with: `kindCandidates` and the backend's own `TASK_KINDS` check
   *  are both just "one of the four known levels", so nothing is deliberately
   *  left unfiltered here. */
  private renderKindPicker(t: OrchTask): HTMLElement {
    const options = kindCandidates(t);
    const sel = document.createElement("select");
    sel.className = "task-dep-picker kind";
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "🏷 set kind…";
    sel.appendChild(placeholder);
    if (t.kind) {
      // The clear. `orch_upsert_task` reads an EMPTY kind as "clear the
      // label" (omitting the field means "leave it alone"), so the sentinel
      // below is mapped to "" on the way out — never sent as a literal kind.
      const clear = document.createElement("option");
      clear.value = CLEAR_KIND_CHOICE;
      clear.textContent = "— clear (plain task)";
      sel.appendChild(clear);
    }
    for (const k of options) {
      const opt = document.createElement("option");
      opt.value = k;
      opt.textContent = k;
      sel.appendChild(opt);
    }
    sel.value = "";

    const close = () => this.closePicker(t.id, "kind");
    sel.addEventListener("change", () => {
      const pick = sel.value;
      if (!pick) return;
      const kind = pick === CLEAR_KIND_CHOICE ? "" : pick;
      this.picking = null;
      // Close on our own rather than waiting for the board-change event: if
      // the write is refused (an out-of-vocabulary value — unreachable from
      // this picker, but mutate() still resyncs on any backend error),
      // mutate() toasts the backend's own error.
      this.render();
      void this.mutate(invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, kind }));
    });
    // Keep keystrokes off the terminal underneath; Esc backs out unwritten.
    sel.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
    });
    sel.addEventListener("blur", () => window.setTimeout(close, 0));
    if (this.pickingFocus) {
      this.pickingFocus = false;
      window.setTimeout(() => sel.focus(), 0);
    }
    return sel;
  }

  /** The "＋ depends on…" picker: every other task on the board, minus the ones
   *  this one already depends on. Cycle-closing choices are deliberately NOT
   *  filtered out here — the backend rejects those inside its lock with an
   *  error naming the path, and duplicating that walk frontend-side would be a
   *  second copy of a rule that could only ever disagree with it. */
  private renderDepPicker(t: OrchTask): HTMLElement {
    const options = depCandidates(t, this.tasks);
    if (options.length === 0) {
      return el("span", "task-links-label", "no other task to depend on");
    }
    const sel = document.createElement("select");
    sel.className = "task-dep-picker";
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "＋ depends on…";
    sel.appendChild(placeholder);
    for (const c of options) {
      const opt = document.createElement("option");
      opt.value = c.id;
      opt.textContent = `${c.id} — ${c.title}`;
      sel.appendChild(opt);
    }
    sel.value = "";

    const close = () => this.closePicker(t.id, "dep");
    sel.addEventListener("change", () => {
      const pick = sel.value;
      if (!pick) return;
      const deps = withDep(t.deps, pick);
      this.picking = null;
      // Close the picker on our own rather than waiting for the board-change
      // event the write will raise: if the write is refused, mutate() toasts
      // and resyncs, and either way the picker has done its job.
      this.render();
      void this.mutate(
        invoke("orch_upsert_task", { groupId: this.groupId, id: t.id, deps })
      );
    });
    // Keep keystrokes off the terminal underneath (every inline editor in this
    // view does this); Esc backs out without writing anything.
    sel.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
    });
    sel.addEventListener("blur", () => window.setTimeout(close, 0));
    // Focus only on the render that opened it — a later background refresh
    // re-renders the open picker, and must not yank focus back from wherever
    // the human has moved since.
    if (this.pickingFocus) {
      this.pickingFocus = false;
      window.setTimeout(() => sel.focus(), 0);
    }
    return sel;
  }

  private renderTask(
    boardRow: BoardRow<OrchTask>,
    usesDeps: boolean,
    usesHierarchy: boolean,
    blocked: ReadonlyMap<string, string>,
    settled: ReadonlySet<string>
  ): HTMLElement {
    const t = boardRow.task;
    const row = el("div", "task-row");
    // The anchor the focus hook scrolls to (#1091 slice C). A data attribute
    // rather than an id: several boards can be open at once across panes, and
    // duplicate DOM ids would make `querySelector` pick an arbitrary one.
    row.dataset.itemId = t.id;
    // Nesting depth (#958). Clamped: the backend caps writes at depth 4, but a
    // hand-edited tasks.json can be deeper and an unbounded indent would walk
    // the row off the right edge of the overlay.
    if (boardRow.depth > 0) row.classList.add(`task-depth-${indentLevel(boardRow.depth)}`);
    if (isAwaitingHuman(t.status)) row.classList.add("awaiting-human");
    const activity = taskActivityState(t.status, t.assignee, this.liveAgentIds);
    if (activity) row.classList.add(`task-row-${activity}`);
    // An archived row recedes further still, so the working items keep the eye
    // even in the show-cleared view. Usually that IS the show-cleared view —
    // but not only: a cleared container still holding live work is never
    // hidden (`clearedIds` is a whole-subtree closure), so it can carry this
    // class with the toggle off. See `BoardRow.cleared`.
    if (boardRow.cleared) row.classList.add("task-row-cleared");
    // A queued task waiting on an unfinished dep recedes, so it can't be read
    // as work anyone could pick up right now (#582's core ask: blocked-queued
    // must not look like plain queued). Deliberately no new accent color —
    // the chips below name the blockers, and the amber/blue accents already
    // mean "waiting on YOU" and "live work".
    //
    // A row whose CONTAINER is the one waiting recedes identically (#958 slice
    // R): it is unstartable for the same reason and must not read as startable.
    // No chip of its own — the container carries the ✗ chips that say what is
    // holding it, and a row is only ever visible when every container above it
    // is expanded, so the explanation is already on screen, one line up.
    const unmet = unmetDeps(t, this.tasks);
    const depBlocked =
      t.status === QUEUED_STATUS &&
      (unmet.length > 0 || blockingAncestor(t, this.tasks) !== null);
    if (depBlocked) row.classList.add("task-row-dep-blocked");

    // Multi-select: tick to add the row to the batch-delete set. A checkbox
    // (over ctrl/shift-click) keeps the affordance discoverable — the human
    // asked to "multi select the tasks and click a button" (#120). Selection is
    // frontend-only; the checked state is rebuilt from `selected` each render.
    const check = document.createElement("input");
    check.type = "checkbox";
    check.className = "task-select";
    check.checked = this.selected.has(t.id);
    check.title = "Select for a batch action (delete, or approve if it's at the merge gate)";
    check.addEventListener("change", () => {
      if (check.checked) this.selected.add(t.id);
      else this.selected.delete(t.id);
      this.updateDeleteSelected();
      this.updateApproveSelected();
    });

    // Reorder: board order is the priority order the orchestrator follows.
    // Sibling-scoped since #958 — a row moves among the rows sharing its
    // container, and a container carries its whole subtree with it, so
    // priority edits can never silently re-home a task. A step is one step
    // among the rows the human can SEE above/below it (#1152), so the sunk
    // finished rows in between are not positions a click can land on.
    const order = el("div", "task-order");
    const up = el("button", "task-btn", "▲") as HTMLButtonElement;
    const down = el("button", "task-btn", "▼") as HTMLButtonElement;
    const pos = siblingPosition(this.tasks, t.id, settled);
    up.disabled = pos.index <= 0;
    down.disabled = pos.index < 0 || pos.index === pos.count - 1;
    // A settled row reports {-1, 0} above, so both buttons are already off —
    // its place at the bottom is derived (most recently updated first) and a manual
    // step there would contradict the order the board just told the human it
    // was using. Say so rather than leaving two dead arrows unexplained.
    up.title = boardRow.settled
      ? "Finished work sits at the bottom, most recently updated first — reopen it to give it a priority again"
      : boardRow.depth > 0
        ? "Higher priority within its container"
        : "Higher priority";
    down.title = boardRow.settled
      ? "Finished work sits at the bottom, most recently updated first — reopen it to give it a priority again"
      : boardRow.depth > 0
        ? "Lower priority within its container"
        : "Lower priority";
    const move = (delta: number) => {
      const ids = reorderWithSubtree(this.tasks, t.id, delta);
      void this.mutate(invoke("orch_reorder_tasks", { groupId: this.groupId, ids }));
    };
    up.addEventListener("click", () => move(-1));
    down.addEventListener("click", () => move(1));
    order.append(up, down);

    const main = el("div", "task-main");
    const top = el("div", "task-top");
    // Collapse chevron (#958), leftmost so every row's text starts at the same
    // place whether or not it contains anything. Containers only — a leaf gets
    // an inert spacer of the same width rather than a button that does
    // nothing, so the affordance means "there is something inside here".
    // One scan for the two places this row states its child counts (the
    // chevron's tooltip and the rollup chip below) — they are the same two
    // numbers, and computing them twice scanned the board twice per container.
    const counts = boardRow.hasChildren ? childCounts(t.id, this.tasks) : null;
    if (boardRow.hasChildren && counts) {
      const chevron = el(
        "button",
        "task-collapse",
        boardRow.collapsed ? "▸" : "▾"
      ) as HTMLButtonElement;
      chevron.title = boardRow.collapsed
        ? `Show what is inside (${counts.total} task${counts.total === 1 ? "" : "s"})`
        : "Hide what is inside";
      chevron.addEventListener("click", () => {
        if (this.collapsed.has(t.id)) this.collapsed.delete(t.id);
        else this.collapsed.add(t.id);
        this.render();
      });
      top.appendChild(chevron);
    } else if (usesHierarchy) {
      // The gutter only exists on a board that nests something — see
      // boardUsesHierarchy. A flat board renders exactly the row it always has.
      top.appendChild(el("span", "task-collapse-spacer"));
    }
    top.appendChild(el("span", "task-id", t.id));

    // Unmistakable, not just a tint — but it sits AFTER the id, not in front of
    // it (#1152, human beta feedback). `.task-top` is a flex row, so a badge in
    // the leading slot pushed the id and everything after it right by its own
    // width, and an active row read as INDENTED — indistinguishable at a glance
    // from a row nested inside a container, which is a real and different thing
    // here (#958's `.task-depth-N` on the row itself). Every row's left edge is
    // now decided by its depth alone. The glow, pulse and left accent on the row
    // are what carry "active" the moment the eye lands on it; the badge names
    // WHO, one position further in.
    if (activity === "active") {
      const badge = el("span", "task-active-badge", `● ACTIVE — ${t.assignee}`);
      badge.title = `${t.assignee} is actively working on this right now`;
      top.appendChild(badge);
    }

    // Board marker + deep-link (#1091 slice G): an obvious chip on a row
    // that is blocked on a human decision or gated on a demo, routing
    // through the pane's focus hook to open the NEEDS-YOU panel at that
    // item. Placed right after the id, ahead of every other chip, so it is
    // the first chip the eye lands on after "which row is this". (On a live
    // row the ACTIVE badge above precedes it — that one is louder still, and
    // both sit behind the id so no row's left edge moves.)
    const marker: BoardMarker | null = boardMarker(t, blocked);
    if (marker) {
      const chip = el(
        "button",
        "task-chip marker",
        marker.kind === "decision" ? "❓ needs a decision" : "👀 needs a look"
      ) as HTMLButtonElement;
      chip.title =
        marker.kind === "decision"
          ? "A pending question is holding this up — open it in the NEEDS-YOU panel"
          : "Parked for a demo — open it in the NEEDS-YOU panel";
      chip.addEventListener("click", () => {
        if (!this.opts.onFocusDecision(marker.target)) {
          this.toast("The NEEDS-YOU panel isn't available on this pane.");
        }
      });
      top.appendChild(chip);
    }

    // Archived (#1152) — the row's own stamp, so it appears on every rendered
    // cleared row: the ones the 👁 toggle just revealed, and the cleared
    // container that was never hidden because live work sits inside it. It says
    // WHY the row is dimmed, and the ↩ below is its undo. See `BoardRow.cleared`.
    if (boardRow.cleared) {
      const chip = el("span", "task-chip cleared", "📥 cleared");
      chip.title = `Cleared from the working list on ${fmtTime(t.cleared_ms ?? 0)} — still on the board, nothing was deleted`;
      top.appendChild(chip);
    }

    // Advisory Agile level (#958): a label, nothing more — no affordance on
    // this board reads it, and the backend gates nothing on it either.
    if (t.kind) {
      const known = (KINDS as readonly string[]).includes(t.kind);
      const kind = el("span", `task-chip kind k-${known ? t.kind : "unknown"}`, t.kind);
      kind.title = known
        ? `Agile level: ${t.kind} — advisory only, it labels this row and nothing more`
        : `${t.kind} is not one of ${KINDS.join(" | ")} — only a hand-edited tasks.json can hold it`;
      top.appendChild(kind);
    }

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

    // Child rollup (#958). DIRECT children only, because these are the same
    // two numbers the orchestrator's `list_tasks` row carries (`children` /
    // `children_done`) and the two readers must not be shown different counts
    // for the same thing.
    if (counts) {
      const chip = el("span", "task-chip children", `${counts.done}/${counts.total}`);
      chip.title =
        `${counts.done} of ${counts.total} task${counts.total === 1 ? "" : "s"} directly inside ` +
        `this one ${counts.total === 1 ? "is" : "are"} done`;
      top.appendChild(chip);
      // The nudge (#958): everything underneath is finished but this row's own
      // status hasn't caught up. A PROMPT, never a write — a derived status
      // write-back is exactly the wedge that keeping `ready` derived avoids,
      // and status here has two authors (the human and the orchestrator).
      // Whole subtree, not just the direct children, so the claim it makes is
      // one that can't be false with an open grandchild.
      if (t.status !== DONE_STATUS && subtreeAllDone(t.id, this.tasks)) {
        const nudge = el("span", "task-chip rollup-done", "⤴ all inside done");
        nudge.title =
          `Every task under ${t.id} is done, but ${t.id} itself is ${t.status}. ` +
          `Nothing has been changed — set its status yourself if that's right.`;
        top.appendChild(nudge);
      }
    }
    // A container that names no row on the board (#958) — only reachable by
    // hand-editing tasks.json, since the backend validates on write and
    // re-homes survivors on delete. The row renders at top level, and this
    // says why rather than leaving it looking like ordinary top-level work.
    if (hasMissingParent(t, this.tasks)) {
      const chip = el("span", "task-chip parent-missing", `⚠ in ${t.parent}`);
      chip.title =
        `${t.parent} names no task on this board, so this row shows at the top level. ` +
        `Re-nest it with ⤵, or move it to the top level from the same picker.`;
      top.appendChild(chip);
    }

    // "ready" (#582): this queued item's dependencies are all done, so it can
    // start now. Only on a board that actually uses deps — see boardUsesDeps —
    // and it sits next to Start because that is the action it enables.
    if (usesDeps && isReady(t, this.tasks)) {
      const ready = el("span", "task-chip ready", "▸ ready");
      ready.title =
        (t.deps?.length ?? 0) > 0
          ? "Every task this depends on is done — this one can start now"
          : "Nothing blocks this one — it can start now";
      top.appendChild(ready);
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

    // Add a dependency (#582): toggles the picker on the links line below.
    // One entry point, always in the same place, whether or not the task has
    // links yet — the links line itself stays absent on a link-free row so a
    // dep-free board keeps exactly the row height it has today.
    const linkBtn = el("button", "task-btn deplink", "🔗") as HTMLButtonElement;
    linkBtn.title = "Add a dependency — this task waits until the one you pick is done";
    linkBtn.addEventListener("click", () => this.togglePicker(t.id, "dep"));
    top.appendChild(linkBtn);

    // Nest (#958): put this row inside another one, or take it back to the top
    // level. Containment, not ordering — deliberately a separate control from
    // 🔗 above, since a container never blocks anything by being a container.
    const nestBtn = el("button", "task-btn nest", "⤵") as HTMLButtonElement;
    nestBtn.title = t.parent
      ? `Move this task into a different container, or back to the top level (it is in ${t.parent})`
      : "Move this task inside another one — grouping only, it changes nothing about what blocks it";
    nestBtn.addEventListener("click", () => this.togglePicker(t.id, "parent"));
    top.appendChild(nestBtn);

    // Set kind (#958 slice K): the Agile-level picker. Always present, like
    // 🔗/⤵ above — one entry point in the same place whether or not the row
    // already carries a label, rather than making the badge itself (absent on
    // most rows) the only way in.
    const kindBtn = el("button", "task-btn kindpick", "🏷") as HTMLButtonElement;
    kindBtn.title = t.kind
      ? `Change this row's Agile level (currently ${t.kind}) — advisory only, a label and nothing more`
      : "Set this row's Agile level (epic / feature / story / task) — advisory only, a label and nothing more";
    kindBtn.addEventListener("click", () => this.togglePicker(t.id, "kind"));
    top.appendChild(kindBtn);

    const notesBtn = el("button", "task-btn notes", `🗨 ${t.notes.length}`) as HTMLButtonElement;
    notesBtn.title = "Notes";
    notesBtn.addEventListener("click", () => {
      if (this.expanded.has(t.id)) this.expanded.delete(t.id);
      else this.expanded.add(t.id);
      this.render();
    });
    top.appendChild(notesBtn);

    // Per-row un-archive (#1152). No confirm: it puts a row back into a list,
    // which is the reversible direction of a reversible action.
    if (boardRow.cleared) {
      const restore = el("button", "task-btn restore", "↩") as HTMLButtonElement;
      restore.title = "Bring this one back into the working list";
      restore.addEventListener("click", () =>
        void this.mutate(
          invoke("orch_restore_cleared_tasks", { groupId: this.groupId, ids: [t.id] })
        )
      );
      top.appendChild(restore);
    }

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

    const links = this.renderLinks(t);
    if (links) main.appendChild(links);

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
