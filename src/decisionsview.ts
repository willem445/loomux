// The NEEDS-YOU panel (#1091 slice C, reworked by #1151 slice C): one surface,
// one list, for everything in a group that is waiting on the human.
//
//   QUESTIONS — pending `ask_human` rows. The human picks an option, types a
//   reply, or both, and the composed answer settles the row through
//   `orch_question_answer` and arrives in the orchestrator's pane as an
//   ordinary inbound notice. This panel is the TRUSTED answering surface: the
//   backend hard-codes who answered, so no agent can settle a row by any path.
//
//   NEEDS-YOU ITEMS — first-class rows from `needs-you.json` (#1151 slice A):
//   a demo parked for a look, or an ask for feedback. The item owns the ask;
//   the board row it names is joined live and acted on through the SAME
//   commands the board's own buttons call, so the two surfaces cannot disagree
//   about a task's state. **Resolving an item does not move its task** — it
//   clears the attention row, and every control that offers it says so.
//
// The two are ONE list, urgency-pinned then newest-first, because "what is
// waiting on me" is one question and the old tiering forced the human to scan
// three places for it. Settled rows fall into a faded tail that
// **Clear completed** hides — a per-group watermark, not a delete: nothing
// leaves the disk (needs-you-items.md).
//
// Structure follows `TasksView` deliberately — same overlay/embed mechanics,
// same `CoalescingRefresh` gate, same edit-in-progress deferral — because it
// is another embed on the same pane and a reader who knows one should not have
// to learn a second idiom. All projections and the answer-composition rules
// live in `decisions.ts`, DOM-free and unit-tested; this file is wiring.
//
// CONSTRAINT 1 (no PTY resize for chrome) is satisfied structurally, not by
// care: this is an `EmbedKind`, and the embed engine only ever moves elements
// between an overlay host and a flex slot — see doc/design/embedded-panels.md.
// CONSTRAINT 5 holds too: every backend touch is a typed wrapper in
// `orchestration.ts` or an `invoke` from `transport.ts`, never `@tauri-apps`.

import { invoke, listen, type UnlistenFn } from "./transport.ts";
import {
  answerQuestion,
  clearNeedsYou,
  needsYouList,
  questionsList,
  resolveNeedsYou,
} from "./orchestration";
import { CoalescingRefresh } from "./refreshgate";
import {
  answerFor,
  citedTask,
  EMPTY_DRAFT,
  EMPTY_VIEW,
  feedbackSubmitStep,
  freeTextAllowed,
  isPending,
  isUrgent,
  itemTask,
  needsYouCount,
  normalizeOptions,
  projectPanel,
  resolveBlock,
  resolveNote,
  RESOLUTION_MAX,
  retainDrafts,
  selectMode,
  setFreeText,
  submitBlock,
  toggleChoice,
  type AnswerDraft,
  type FeedbackSubmitState,
  type LinkedTask,
  type NeedsYouItem,
  type NeedsYouView,
  type OpenRow,
  type OrchQuestion,
  type SettledRow,
  type SubmitBlock,
} from "./decisions";
import { REQUEST_CHANGES_STATUS } from "./taskboard";
import type { OrchTask } from "./tasksview";

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const fmtTime = (ms: number): string =>
  new Date(ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

/** What to say when the submit button is disabled. A sentence rather than a
 *  dead button: the over-cap case especially is one a human can reach without
 *  doing anything obviously wrong, and discovering it as a backend rejection
 *  after the click is the experience this avoids. */
const BLOCK_HINT: Record<SubmitBlock, string> = {
  empty: "Type an answer, or pick an option.",
  "no-choice": "Pick one of the options — this question takes no free text.",
  "too-long": "Too long — the answer must be 2000 characters or fewer.",
};

export class DecisionsView {
  readonly el: HTMLElement;
  private listEl: HTMLElement;
  private countEl: HTMLElement;
  private toastEl: HTMLElement;
  private toastTimer: number | undefined;

  private questions: OrchQuestion[] = [];
  private tasks: OrchTask[] = [];
  /** The item registry's rows AND the clear-completed watermark, from ONE
   *  read — see `NeedsYouView`. Held together because the watermark decides
   *  which of these rows are hidden, and a stamp from a different read could
   *  flash back a row the human had just cleared. */
  private view: NeedsYouView = EMPTY_VIEW;
  /** Half-composed answers, keyed by question id, so a re-render (a burst of
   *  board writes, another question arriving) never destroys what the human
   *  has typed. Frontend-only, so they are pruned to still-answerable rows on
   *  every refresh — see `retainDrafts`. */
  private drafts = new Map<string, AnswerDraft>();
  /** Question ids whose card is expanded into its answering form. Collapsed by
   *  default so a queue of pending decisions reads as a queue rather than as a
   *  wall of textareas. */
  private open = new Set<string>();
  /** A refresh arrived while the human was mid-answer; run it on blur. */
  private pendingRefresh = false;
  /** Single-flight + trailing-edge merge, the same gate the board uses (#743
   *  S5): BOTH of this panel's source events are agent-driven and bursty, and
   *  each refresh costs two backend reads plus a re-render. A burst of N now
   *  costs the run in flight plus exactly one trailing run, which reads the
   *  final state, so nothing is lost by coalescing. */
  private readonly refresher = new CoalescingRefresh(() => this.refreshNow());
  private unlistenQuestions: UnlistenFn | null = null;
  private unlistenTasks: UnlistenFn | null = null;
  private unlistenItems: UnlistenFn | null = null;
  private disposed = false;

  private embedBtn: HTMLButtonElement;
  private closeBtn: HTMLButtonElement;
  private clearBtn: HTMLButtonElement;

  constructor(
    private groupId: string,
    private opts: {
      onClose: () => void;
      onEmbedMenu: (anchor: HTMLElement) => void;
      /** Ask the pane to open its task board at `taskId` (#1091 slice C's
       *  focus hook). The panel does not know the board exists beyond this —
       *  it hands the pane an intent, and the pane owns which embed serves it.
       *  Returns whether the pane could route it, so a card can degrade to a
       *  plain label instead of offering a link that goes nowhere. */
      onFocusTask: (taskId: string) => boolean;
      /** Drain any focus request parked for THIS panel, called once per
       *  render. Non-null only when something asked for a specific card while
       *  the panel was closed or unbuilt — #1091 slice G's board marker is the
       *  first caller. `undefined` when the host wires no focus at all. */
      takeFocus?: () => string | null;
    }
  ) {
    this.el = el("div", "tasks-view decisions-view");

    const head = el("div", "tasks-head");
    head.append(el("span", "tasks-title", "needs you"));
    // The count rides IN the header, never hover-only: a panel whose whole job
    // is "what is waiting on you" must say how much at a glance (the
    // queuebadge.ts lesson).
    this.countEl = el("span", "decisions-count", "");
    this.countEl.hidden = true;
    head.append(this.countEl);
    head.append(el("span", "tasks-group", groupId));

    // Clear completed. Hidden — not merely disabled — while there is no tail,
    // because a control that can never do anything on this render is chrome
    // the eye has to re-dismiss every time. No confirm: it stamps a watermark
    // and deletes nothing, which the title says in both halves so the human
    // does not have to trust the word "clear" (needs-you-items.md).
    // A text-labelled header button in the board's own idiom (`clear-done`),
    // not a new one: `.tasks-head .pane-btn` is always opaque, and the width
    // override the label needs already exists there for its two siblings.
    this.clearBtn = el("button", "pane-btn decisions-clear", "Clear completed") as HTMLButtonElement;
    this.clearBtn.title =
      "Hide the settled rows below — they stay on disk, and nothing still open is touched";
    this.clearBtn.hidden = true;
    this.clearBtn.addEventListener("click", () => void this.clearCompleted());
    head.append(this.clearBtn);

    this.embedBtn = el("button", "pane-btn embed", "⬒") as HTMLButtonElement;
    this.embedBtn.addEventListener("click", () => opts.onEmbedMenu(this.embedBtn));
    head.append(this.embedBtn);

    this.closeBtn = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    this.closeBtn.addEventListener("click", opts.onClose);
    head.append(this.closeBtn);
    this.setPanelActive(false);

    this.listEl = el("div", "tasks-list decisions-list");
    this.toastEl = el("div", "git-toast");
    this.toastEl.hidden = true;
    this.el.append(head, this.listEl, this.toastEl);

    // Deferred refreshes run once the human's answer box loses focus.
    this.listEl.addEventListener("focusout", () => {
      window.setTimeout(() => {
        if (this.pendingRefresh && !this.isEditing()) this.refresh();
      }, 0);
    });

    // THREE producers, one panel: a question mutation, an item mutation and a
    // board write all change what is waiting on the human — and a single board
    // write can be all three at once, since the demo-gate hook raises or
    // resolves an item inside `upsert_task`. All three are declared in the #743
    // perf manifest (test/perfpolicy.test.ts) and all three land on the SAME
    // coalescing gate, so that write costs one trailing refresh rather than
    // three, and a burst on any of them costs the run in flight plus one.
    void listen<{ group_id: string }>("orch-questions-changed", ({ payload }) => {
      if (payload.group_id === this.groupId) this.refresh();
    }).then((u) => {
      if (this.disposed) u();
      else this.unlistenQuestions = u;
    });
    void listen<{ group_id: string }>("orch-tasks-changed", ({ payload }) => {
      if (payload.group_id === this.groupId) this.refresh();
    }).then((u) => {
      if (this.disposed) u();
      else this.unlistenTasks = u;
    });
    void listen<{ group_id: string }>("orch-needs-you-changed", ({ payload }) => {
      if (payload.group_id === this.groupId) this.refresh();
    }).then((u) => {
      if (this.disposed) u();
      else this.unlistenItems = u;
    });
  }

  /** Called by the pane whenever the view is (re)opened, in either mode. */
  show(): void {
    this.refresh();
  }

  /** Reflect which mode the pane currently has this view mounted in — the
   *  `setPanelActive` contract every embeddable view implements (#361). */
  setPanelActive(active: boolean): void {
    this.embedBtn.classList.toggle("active", active);
    this.embedBtn.textContent = active ? "⬓" : "⬒";
    this.embedBtn.title = active
      ? "Un-embed — back to a floating overlay"
      : "Embed beside the terminal (resizes this pane)";
    this.closeBtn.disabled = active;
    this.closeBtn.title = active ? "Docked — un-embed it (side menu) to close" : "Close (Alt+Q)";
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.toastTimer);
    this.unlistenQuestions?.();
    this.unlistenTasks?.();
    this.unlistenItems?.();
    this.el.remove();
  }

  private toast(msg: string): void {
    this.toastEl.textContent = msg;
    this.toastEl.hidden = false;
    clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(() => (this.toastEl.hidden = true), 4000);
  }

  /** True while the human is typing an answer — re-rendering would destroy it.
   *  The drafts map survives a re-render on its own, but the caret and the
   *  selection do not, so a refresh mid-sentence still has to wait. */
  private isEditing(): boolean {
    const a = document.activeElement;
    return !!a && this.listEl.contains(a) && (a.tagName === "INPUT" || a.tagName === "TEXTAREA");
  }

  private refresh(): void {
    this.refresher.request();
  }

  private async refreshNow(): Promise<void> {
    if (this.disposed) return;
    if (this.isEditing()) {
      this.pendingRefresh = true;
      return;
    }
    this.pendingRefresh = false;
    // All three reads degrade to empty backend-side rather than rejecting (an
    // unreadable file, a group id the backend refuses), so a failure here is a
    // transport-level one — report it and keep the last good render rather
    // than blanking the panel. One `Promise.all`, so the three land together
    // and the render never mixes this second's items with last second's board.
    try {
      [this.questions, this.tasks, this.view] = await Promise.all([
        questionsList(this.groupId),
        invoke<OrchTask[]>("orch_tasks", { groupId: this.groupId }),
        needsYouList(this.groupId),
      ]);
    } catch (err) {
      this.toast(String(err));
      return;
    }
    // Housekeeping, in the board's idiom: a draft or an expanded card whose
    // question was answered elsewhere or withdrawn by its asker must not come
    // back holding stale input.
    this.drafts = retainDrafts(this.drafts, this.questions);
    // `isPending`, not a second inline status test: the draft map and the
    // expanded set must be pruned by exactly the same rule, and two spellings
    // of "answerable" is how they drift apart.
    const answerable = new Set(this.questions.filter(isPending).map((q) => q.id));
    for (const id of [...this.open]) if (!answerable.has(id)) this.open.delete(id);
    this.render();
  }

  private draftFor(id: string): AnswerDraft {
    return this.drafts.get(id) ?? EMPTY_DRAFT;
  }

  private setDraft(id: string, d: AnswerDraft): void {
    this.drafts.set(id, d);
  }

  // ---------- render ----------

  private render(): void {
    const { open, settled, omitted } = projectPanel(this.view, this.questions, this.tasks);

    // `needsYouCount` rather than `open.length` — the same number by
    // construction, and the relation is pinned in test/decisions.test.ts so the
    // header chip and the list cannot come to disagree about what is waiting.
    const count = needsYouCount(this.view, this.questions);
    const decisions = open.filter((r) => r.source === "question").length;
    this.countEl.textContent = String(count);
    this.countEl.hidden = count === 0;
    this.countEl.title = `${decisions} decision${decisions === 1 ? "" : "s"}, ${count - decisions} item${count - decisions === 1 ? "" : "s"}`;

    // Only offered when there is something to clear — see the constructor.
    this.clearBtn.hidden = settled.length === 0;

    const list = el("div", "decisions-body");

    if (count === 0 && settled.length === 0) {
      list.append(el("div", "tasks-empty", "Nothing is waiting on you."));
    }

    // ONE list, urgency-pinned then newest-first. No per-source tier: a human
    // asking "what needs me" wants one queue, and the source is a badge on the
    // card rather than a heading to scan past.
    if (open.length > 0) {
      list.append(this.section("waiting", open.length));
      for (const row of open) list.append(this.openCard(row));
    }
    if (settled.length > 0) {
      list.append(this.section("settled", settled.length));
      for (const row of settled) list.append(this.settledCard(row));
      if (omitted > 0) {
        // Never let a capped tail read as the whole history. Counts only what
        // the CAP dropped — rows the human cleared are not "not shown", they
        // are handled, and reporting them back would undo the gesture.
        list.append(el("div", "decisions-omitted", `${omitted} older row${omitted === 1 ? "" : "s"} not shown`));
      }
    }

    this.listEl.replaceChildren(list);

    // Drain any parked focus request LAST, once the rows it names exist — the
    // whole reason the request is parked rather than delivered directly (see
    // embedfocus.ts). Once per render, and consumed, so an ordinary refresh
    // never yanks the viewport back.
    const target = this.opts.takeFocus?.() ?? null;
    if (target) this.focusItem(target);
  }

  private section(label: string, n: number): HTMLElement {
    const s = el("div", "decisions-section");
    s.append(el("span", "decisions-eyebrow", label));
    s.append(el("span", "decisions-section-n", String(n)));
    return s;
  }

  /** Scroll the card for `id` (a `q-N` or a `t-N`) into view and flash it.
   *  A no-op when nothing on this render carries that id — the row may have
   *  settled between the request and the render, and a missing target is not
   *  worth an error. */
  private focusItem(id: string): void {
    const card = this.listEl.querySelector<HTMLElement>(`[data-item-id="${CSS.escape(id)}"]`);
    if (!card) return;
    card.scrollIntoView({ block: "nearest" });
    card.classList.add("decisions-focused");
    window.setTimeout(() => card.classList.remove("decisions-focused"), 1600);
  }

  // ---------- cards ----------

  /** One open row, whichever registry it came from. The `anchor` — what a
   *  deep-link targets — is the projection's, never re-derived here: an open
   *  demo item anchors on its TASK id because that is what the board's marker
   *  chip emits (#1091 slice G), and getting that rule wrong in a second place
   *  is how the board's "👀 needs a look" chip would quietly stop landing. */
  private openCard(row: OpenRow): HTMLElement {
    const card = row.source === "question" ? this.questionCard(row.question) : this.itemCard(row);
    card.dataset.itemId = row.anchor;
    return card;
  }

  private questionCard(q: OrchQuestion): HTMLElement {
    const card = el("div", "decisions-card");
    card.dataset.itemId = q.id;
    if (isUrgent(q)) card.classList.add("urgent");

    const head = el("div", "decisions-card-head");
    head.append(el("span", "decisions-id", q.id));
    if (isUrgent(q)) head.append(el("span", "decisions-urgent", "urgent"));
    head.append(el("span", "decisions-asker", q.asker));
    if (q.created_ms) head.append(el("span", "decisions-when", fmtTime(q.created_ms)));

    // The panel-to-board direction (#1091 scope add 3): a question that names
    // the task it is holding up links straight to that row, routed through the
    // pane's focus hook. Rendered as plain text — not a link — when the pane
    // cannot route it, so the affordance never lies about where it goes.
    const cited = citedTask(q);
    if (cited) head.append(this.taskLink(cited, `holding ${cited}`));
    card.append(head);

    const text = el("div", "decisions-text", q.text);
    text.addEventListener("click", () => this.toggleOpen(q.id));
    card.append(text);

    if (this.open.has(q.id)) card.append(this.answerForm(q));
    else {
      const answerBtn = el("button", "dlg-btn primary decisions-answer-btn", "Answer") as HTMLButtonElement;
      answerBtn.addEventListener("click", () => this.toggleOpen(q.id));
      const actions = el("div", "decisions-card-actions");
      actions.append(answerBtn);
      card.append(actions);
    }
    return card;
  }

  private toggleOpen(id: string): void {
    if (this.open.has(id)) this.open.delete(id);
    else this.open.add(id);
    this.render();
  }

  /** A `t-N` reference as a click-through to the board, or as an inert chip
   *  when this pane cannot host the board. `onFocusTask` reports which. */
  private taskLink(taskId: string, label: string): HTMLElement {
    const btn = el("button", "decisions-tasklink", label) as HTMLButtonElement;
    btn.title = `Show ${taskId} on the task board`;
    btn.addEventListener("click", () => {
      if (!this.opts.onFocusTask(taskId)) this.toast("The task board isn't available on this pane.");
    });
    return btn;
  }

  private answerForm(q: OrchQuestion): HTMLElement {
    const form = el("div", "decisions-form");
    const options = normalizeOptions(q.options);
    const mode = selectMode(q);
    const draft = this.draftFor(q.id);

    if (options.length > 0) {
      const hint = el(
        "div",
        "decisions-hint",
        mode === "multi" ? "Pick any that apply" : "Pick one"
      );
      form.append(hint);
      const opts = el("div", "decisions-options");
      options.forEach((o, i) => {
        const b = el("button", "decisions-option") as HTMLButtonElement;
        b.append(el("span", "decisions-option-label", o.label));
        // The reasoning under a label is the whole point of the richer ask
        // shape (#1091 slice A) — showing only the labels would throw away
        // what the orchestrator wrote to help the human choose.
        if (o.description) b.append(el("span", "decisions-option-desc", o.description));
        if (draft.chosen.includes(i)) b.classList.add("chosen");
        b.setAttribute("aria-pressed", String(draft.chosen.includes(i)));
        b.addEventListener("click", () => {
          this.setDraft(q.id, toggleChoice(this.draftFor(q.id), i, mode, options.length));
          this.render();
        });
        opts.append(b);
      });
      form.append(opts);
    }

    let ta: HTMLTextAreaElement | null = null;
    if (freeTextAllowed(q)) {
      ta = document.createElement("textarea");
      ta.className = "dlg-input decisions-freetext";
      ta.rows = 2;
      ta.spellcheck = false;
      ta.value = draft.freeText;
      ta.placeholder = options.length > 0 ? "…or say something else" : "Your answer";
      // Keystrokes must not reach the terminal underneath.
      ta.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Escape") this.toggleOpen(q.id);
        if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) void this.submit(q);
      });
      // Update the draft WITHOUT re-rendering: a re-render on every keystroke
      // would rebuild the textarea and lose the caret.
      ta.addEventListener("input", () => {
        this.setDraft(q.id, setFreeText(this.draftFor(q.id), ta!.value));
        this.syncSubmit(form, q);
      });
      form.append(ta);
    }

    const actions = el("div", "dlg-actions decisions-card-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    cancel.addEventListener("click", () => this.toggleOpen(q.id));
    const send = el("button", "dlg-btn primary decisions-submit", "Send") as HTMLButtonElement;
    send.addEventListener("click", () => void this.submit(q));
    actions.append(el("span", "decisions-block-hint"), cancel, send);
    form.append(actions);
    this.syncSubmit(form, q);
    if (ta) window.setTimeout(() => ta!.focus(), 0);
    return form;
  }

  /** Re-evaluate the submit gate against the live draft, without a re-render.
   *  Same `submitBlock` the click path uses, so the button and the send can
   *  never disagree about whether an answer is sendable. */
  private syncSubmit(form: HTMLElement, q: OrchQuestion): void {
    const block = submitBlock(q, this.draftFor(q.id));
    const send = form.querySelector<HTMLButtonElement>(".decisions-submit");
    const hint = form.querySelector<HTMLElement>(".decisions-block-hint");
    if (send) send.disabled = block !== null;
    if (hint) hint.textContent = block ? BLOCK_HINT[block] : "";
  }

  private async submit(q: OrchQuestion): Promise<void> {
    const draft = this.draftFor(q.id);
    if (submitBlock(q, draft) !== null) return;
    const answer = answerFor(q, draft);
    // Close the card and drop the draft optimistically: the row is about to
    // settle, and `orch-questions-changed` will re-render from the truth. A
    // failure restores nothing but the panel — the registry is unchanged, and
    // the toast says so — which beats leaving a card that looks submitted.
    this.open.delete(q.id);
    this.drafts.delete(q.id);
    try {
      await answerQuestion(this.groupId, q.id, answer);
    } catch (err) {
      this.setDraft(q.id, draft);
      this.open.add(q.id);
      this.toast(String(err));
    }
    this.refresh();
  }

  /** The faded tail: a settled question or a resolved item, rendered from the
   *  same union the projection built so the two cannot drift apart in order.
   *  History, not work — no action of any kind is offered here. */
  private settledCard(row: SettledRow): HTMLElement {
    const card = el("div", "decisions-card settled");
    card.dataset.itemId = row.anchor;
    const head = el("div", "decisions-card-head");
    if (row.source === "question") {
      const q = row.question;
      head.append(el("span", "decisions-id", q.id));
      head.append(el("span", "decisions-status", q.status));
      if (q.settled_ms) head.append(el("span", "decisions-when", fmtTime(q.settled_ms)));
      card.append(head);
      card.append(el("div", "decisions-text", q.text));
      if (q.answer) card.append(el("div", "decisions-answer", q.answer));
      return card;
    }
    const item = row.item;
    head.append(el("span", "decisions-id", item.id));
    head.append(el("span", "decisions-kind", item.kind));
    // WHO closed it, verbatim from the record: `webview` (the human looked),
    // `board:<status>` (the work moved on) or `withdrawn:<agent>` (the raiser
    // took it back). The three are kept distinguishable backend-side precisely
    // so a reader can tell them apart, so the panel shows the tag rather than
    // flattening all three to "resolved".
    if (item.resolved_by) head.append(el("span", "decisions-status", item.resolved_by));
    if (item.resolved_ms) head.append(el("span", "decisions-when", fmtTime(item.resolved_ms)));
    const linked = itemTask(item);
    if (linked) head.append(el("span", "decisions-asker", linked));
    card.append(head);
    card.append(el("div", "decisions-text", item.text));
    if (item.resolution) card.append(el("div", "decisions-answer", item.resolution));
    return card;
  }

  // ---------- item cards ----------

  /** One open needs-you item: the ask, plus whatever the linked board row
   *  currently says.
   *
   *  **The join can be `null`, and the card still works.** A task can be pruned
   *  or renamed under an open item, and a `feedback` ask may name no task at
   *  all — so the board affordances (path, PR, Proceed, Feedback) are the part
   *  that disappears, never Resolve. An item the human can see but cannot clear
   *  would be a permanent row on their queue. */
  private itemCard(row: Extract<OpenRow, { source: "item" }>): HTMLElement {
    const { item, task } = row;
    const card = el("div", "decisions-card demo");

    const head = el("div", "decisions-card-head");
    head.append(el("span", "decisions-id", item.id));
    head.append(el("span", "decisions-kind", item.kind));
    if (row.urgent) head.append(el("span", "decisions-urgent", "urgent"));
    head.append(el("span", "decisions-asker", item.raiser));
    if (item.created_ms) head.append(el("span", "decisions-when", fmtTime(item.created_ms)));
    const linked = itemTask(item);
    if (task) {
      head.append(this.taskLink(task.id, task.id));
      head.append(el("span", "decisions-status", task.status));
      if (task.assignee) head.append(el("span", "decisions-asker", task.assignee));
    } else if (linked) {
      // The join degraded. SAY which row is missing rather than dropping the
      // reference — "t-12 is not on the board" is the fact the human needs to
      // decide whether this row is stale; a silently task-less card is not.
      head.append(el("span", "decisions-nopath", `${linked} is not on the board`));
    }
    if (row.urgent) card.classList.add("urgent");
    card.append(head);

    card.append(el("div", "decisions-text", item.text));
    // The board's own title, when there is a row to read it from: the item's
    // text is what the RAISER wrote, and the two are different claims.
    if (task) card.append(el("div", "decisions-subtext", task.title));

    if (task) card.append(this.taskMeta(task));

    const actions = el("div", "decisions-card-actions");
    if (task?.canProceed) {
      // The SAME command the board's own Proceed button calls, guarded the
      // same way — two surfaces, one gesture, so demo state cannot fork.
      const go = el("button", "dlg-btn primary", "Proceed") as HTMLButtonElement;
      go.title = "Promote this prototype — the board action, not a close-out";
      go.addEventListener("click", () => {
        go.disabled = true;
        invoke("orch_proceed_task", { groupId: this.groupId, id: task.id }).catch((err) => {
          go.disabled = false;
          this.toast(String(err));
        });
      });
      actions.append(go);
    }
    if (task) {
      // Always offered once there is a row to act on, on BOTH demo statuses —
      // but not always the same call. A `prototype` is the #147 demo gate, so
      // it is exactly the row where "I looked, and this is not right" must be
      // sayable; what changes is the verb the backend will accept for it (see
      // `feedbackRoute`).
      const changes = el("button", "dlg-btn", "Feedback") as HTMLButtonElement;
      changes.addEventListener("click", () => this.giveFeedback(task));
      actions.append(changes);
    }
    // ALWAYS offered, join or no join. Two claims in the label's title,
    // because "resolve" alone would read as a decision about the work: it
    // clears the attention row and deliberately leaves the task where it is.
    const resolve = el("button", "dlg-btn decisions-resolve", "Resolve") as HTMLButtonElement;
    resolve.title = "Clear this from needs-you — the task stays exactly where it is";
    resolve.addEventListener("click", () => this.resolveItem(item));
    actions.append(resolve);
    card.append(actions);
    return card;
  }

  /** The joined board row's demo affordances: where to run it, and its PR. */
  private taskMeta(d: LinkedTask): HTMLElement {
    const meta = el("div", "decisions-meta");
    if (d.path) {
      // The path is the point of a demo card: the human goes and runs it.
      // Mono, with a copy affordance, because it is a thing to paste — never a
      // link, since nothing here can open a shell for them.
      const p = el("code", "decisions-path", d.path);
      p.title = "Click to copy";
      p.addEventListener("click", () => {
        void navigator.clipboard
          .writeText(d.path!)
          .then(() => this.toast("Demo path copied."))
          .catch(() => this.toast("Could not copy the path."));
      });
      meta.append(p);
    } else {
      // Explicit beats inferred: with no recorded path the panel SAYS so
      // rather than guessing one from an assignee's cwd, because a caption is
      // a claim (D7).
      meta.append(el("span", "decisions-nopath", "no demo path recorded"));
    }
    if (d.pr) {
      const pr = el("button", "decisions-tasklink", d.pr) as HTMLButtonElement;
      pr.title = "Open the PR";
      pr.addEventListener("click", () => {
        invoke("orch_open_ref", { groupId: this.groupId, kind: "pr", value: d.pr }).catch((err) =>
          this.toast(String(err))
        );
      });
      meta.append(pr);
    }
    return meta;
  }

  /** Close out an item: the human says "I have seen this".
   *
   *  **A dialog rather than a bare ✕, and the note is optional in it.** Two
   *  reasons, both about what the gesture MEANS: an accidental click on a row
   *  the human has not looked at costs them the record of the ask, and the
   *  dialog is where "this does not move the task" can be said in a sentence
   *  rather than only in a tooltip nobody hovers. An empty box sends `null`,
   *  which the backend treats as the quiet tidy it is — no pane notice — while
   *  a note is delivered to the orchestrator (`resolveNote`).
   *
   *  There is no MCP resolve at all: this dialog is the only path, which is the
   *  same no-self-served-gate boundary answering a question has. */
  private resolveItem(item: NeedsYouItem): void {
    if (this.el.querySelector(".tasks-dialog")) return; // one at a time
    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(el("div", "tasks-dialog-title", `Resolve ${item.id}`));
    const linked = itemTask(item);
    box.append(
      el(
        "div",
        "decisions-hint",
        linked
          ? `Clears this from needs-you. ${linked} keeps whatever status it has — resolving is not a board move.`
          : "Clears this from needs-you. Nothing else changes."
      )
    );

    const ta = document.createElement("textarea");
    ta.className = "dlg-input tasks-dialog-text";
    ta.placeholder = "Optional note back to the orchestrator — leave empty to just clear it.";
    ta.spellcheck = false;
    ta.rows = 3;

    const actions = el("div", "dlg-actions");
    const hint = el("span", "decisions-block-hint");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const send = el("button", "dlg-btn primary", "Resolve") as HTMLButtonElement;
    actions.append(hint, cancel, send);
    box.append(ta, actions);
    overlay.append(box);

    const close = () => overlay.remove();
    let inFlight = false;
    // The cap is the backend's (`validate_resolution` REJECTS over it rather
    // than truncating), mirrored here so the human is stopped before the click
    // rather than losing the paste to a rejection after it.
    const sync = () => {
      const block = resolveBlock(ta.value);
      send.disabled = block !== null || inFlight;
      hint.textContent = block
        ? `Too long — the note must be ${RESOLUTION_MAX} characters or fewer.`
        : "";
    };
    const submit = () => {
      if (inFlight || resolveBlock(ta.value) !== null) return;
      inFlight = true;
      sync();
      // Closes on SUCCESS, never before the write — the same rule the feedback
      // dialog learned: a rejection that had already closed the dialog would
      // take the human's typed note with it.
      resolveNeedsYou(this.groupId, item.id, resolveNote(ta.value))
        .then(() => close())
        .catch((err) => {
          inFlight = false;
          sync();
          this.toast(String(err));
          ta.focus();
        })
        .finally(() => this.refresh());
    };
    ta.addEventListener("input", sync);
    cancel.addEventListener("click", close);
    send.addEventListener("click", submit);
    ta.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
    });
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });

    this.el.appendChild(overlay);
    sync();
    ta.focus();
  }

  /** Hide the settled tail. Stamps a per-group watermark; **deletes nothing**,
   *  which is why it takes no confirm.
   *
   *  The new stamp comes back from the command, so it is applied without a
   *  second read — `clear_needs_you` writes only the marker and emits no
   *  `orch-needs-you-changed`, so nothing else would tell this panel. */
  private async clearCompleted(): Promise<void> {
    this.clearBtn.disabled = true;
    try {
      const cleared = await clearNeedsYou(this.groupId);
      this.view = { ...this.view, cleared_ms: cleared };
      this.render();
    } catch (err) {
      this.toast(String(err));
    } finally {
      this.clearBtn.disabled = false;
    }
  }

  /** Demo feedback, routed to the verb this row's status actually admits.
   *
   *  At the merge gate (`human-testing`) this is the board's own
   *  request-changes gesture on the same two commands — record the findings,
   *  then reopen the task as working so no surface keeps offering a gate
   *  action on a row that just had changes asked of it.
   *
   *  On a `prototype` it is a plain board note instead, because
   *  `orch_request_changes` REFUSES that status (`ensure_at_merge_gate`) and
   *  sending it there would reject every time. The note reaches the
   *  orchestrator all the same — but as the generic board-edit ping, NOT with
   *  the findings inline the way the merge-gate verb delivers them: the
   *  orchestrator sees `note_count` move and reads the text with `get_task`,
   *  identically to every other human board note. It deliberately does not move
   *  the status either: a
   *  prototype that has had feedback is still a prototype until it is promoted
   *  or re-dispatched, and flipping it here would silently consume the #147
   *  demo gate.
   *
   *  **The dialog closes on SUCCESS, never before the write.** An earlier
   *  shape closed first, so any rejection destroyed everything the human had
   *  typed with no way back — the same shape the question-answer path already
   *  avoids by restoring its draft on failure. */
  private giveFeedback(d: LinkedTask): void {
    if (this.el.querySelector(".tasks-dialog")) return; // one at a time
    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(el("div", "tasks-dialog-title", `Feedback on ${d.id}`));

    const ta = document.createElement("textarea");
    ta.className = "dlg-input tasks-dialog-text";
    ta.placeholder =
      d.feedback === "merge-gate"
        ? "What needs to change? These findings go to the orchestrator."
        : "What did you find? This goes to the orchestrator as a note on the demo.";
    ta.spellcheck = false;
    ta.rows = 4;

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const send = el("button", "dlg-btn primary", "Send") as HTMLButtonElement;
    actions.append(cancel, send);
    box.append(ta, actions);
    overlay.append(box);

    const close = () => overlay.remove();
    // Because the dialog now outlives its own write, `submit` is reachable more
    // than once — the keydown path below calls it whatever state `send` is in.
    // The state is held here; what a press MEANS is `feedbackSubmitStep`'s, so
    // that the guard is pinned by `test/decisions.test.ts` rather than by this
    // untested wiring.
    //
    // `state` is per-dialog by construction, which bounds what this closes: a
    // second PRESS, not a second DIALOG. `close()` does not cancel an in-flight
    // write, so dismissing mid-flight (Escape, or the overlay click below) and
    // reopening mints fresh state, and a Send there is a real second write.
    // Two deliberate gestures, not the habitual double-tap this fixes — see
    // `feedbackSubmitStep`'s doc, which carries the same bound.
    const state: FeedbackSubmitState = { inFlight: false, findingsLanded: false };
    const submit = () => {
      const findings = ta.value.trim();
      if (!findings) {
        ta.focus();
        return;
      }
      const step = feedbackSubmitStep(d.feedback, state);
      if (step === "ignore") return;
      // Disable rather than close: the text stays on screen and recoverable
      // until the write is known to have landed.
      send.disabled = true;
      state.inFlight = true;
      const flipStatus = () =>
        invoke("orch_upsert_task", {
          groupId: this.groupId,
          id: d.id,
          status: REQUEST_CHANGES_STATUS,
        });
      const write =
        step === "note"
          ? invoke("orch_upsert_task", { groupId: this.groupId, id: d.id, note: findings })
          : step === "status-only"
            ? flipStatus()
            : invoke("orch_request_changes", { groupId: this.groupId, id: d.id, findings })
                .then(() => {
                  // Recorded — a retry after a failed flip must not say it twice.
                  state.findingsLanded = true;
                })
                .then(flipStatus);
      write
        .then(() => close())
        .catch((err) => {
          send.disabled = false;
          state.inFlight = false;
          this.toast(String(err));
          ta.focus();
        });
    };
    cancel.addEventListener("click", close);
    send.addEventListener("click", submit);
    ta.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") close();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
    });
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });

    this.el.appendChild(overlay);
    ta.focus();
  }
}
