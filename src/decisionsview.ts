// The NEEDS-YOU panel (#1091 slice C): one surface for everything in a group
// that is waiting on the human, in two tiers.
//
//   DECISIONS — pending `ask_human` questions. The human picks an option,
//   types a reply, or both, and the composed answer settles the row through
//   `orch_question_answer` and arrives in the orchestrator's pane as an
//   ordinary inbound notice. This panel is the TRUSTED answering surface: the
//   backend hard-codes who answered, so no agent can settle a row by any path.
//
//   DEMOS — board rows parked in `prototype`/`human-testing`, projected from
//   the SAME `tasks.json` the board renders and acted on through the SAME
//   commands the board's own buttons call. There is no second record, so the
//   two surfaces cannot disagree about a demo's state.
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
import { answerQuestion, questionsList } from "./orchestration";
import { CoalescingRefresh } from "./refreshgate";
import {
  answerFor,
  citedTask,
  EMPTY_DRAFT,
  freeTextAllowed,
  isUrgent,
  needsYouCount,
  normalizeOptions,
  projectDemos,
  projectQuestions,
  retainDrafts,
  selectMode,
  setFreeText,
  submitBlock,
  toggleChoice,
  type AnswerDraft,
  type DemoItem,
  type OrchQuestion,
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
  private disposed = false;

  private embedBtn: HTMLButtonElement;
  private closeBtn: HTMLButtonElement;

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

    // TWO producers, one panel: a question mutation and a board write both
    // change what is waiting on the human. Both are declared in the #743 perf
    // manifest (test/perfpolicy.test.ts) and both land on the same coalescing
    // gate, so a burst on either costs one trailing refresh, not one per event.
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
    // Both reads degrade to empty backend-side rather than rejecting (an
    // unreadable file, a group id the backend refuses), so a failure here is a
    // transport-level one — report it and keep the last good render rather
    // than blanking the panel.
    try {
      [this.questions, this.tasks] = await Promise.all([
        questionsList(this.groupId),
        invoke<OrchTask[]>("orch_tasks", { groupId: this.groupId }),
      ]);
    } catch (err) {
      this.toast(String(err));
      return;
    }
    // Housekeeping, in the board's idiom: a draft or an expanded card whose
    // question was answered elsewhere or withdrawn by its asker must not come
    // back holding stale input.
    this.drafts = retainDrafts(this.drafts, this.questions);
    const answerable = new Set(this.questions.filter((q) => q.status === "pending").map((q) => q.id));
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
    const { pending, settled, omitted } = projectQuestions(this.questions);
    const demos = projectDemos(this.tasks);

    const count = needsYouCount(this.questions, this.tasks);
    this.countEl.textContent = String(count);
    this.countEl.hidden = count === 0;
    this.countEl.title = `${pending.length} decision${pending.length === 1 ? "" : "s"}, ${demos.length} demo${demos.length === 1 ? "" : "s"}`;

    const list = el("div", "decisions-body");

    if (count === 0 && settled.length === 0) {
      list.append(el("div", "tasks-empty", "Nothing is waiting on you."));
    }

    if (pending.length > 0) {
      list.append(this.section("decisions", pending.length));
      for (const q of pending) list.append(this.questionCard(q));
    }
    if (demos.length > 0) {
      list.append(this.section("demos", demos.length));
      for (const d of demos) list.append(this.demoCard(d));
    }
    if (settled.length > 0) {
      list.append(this.section("answered", settled.length));
      for (const q of settled) list.append(this.settledCard(q));
      if (omitted > 0) {
        // Never let a capped tail read as the whole history.
        list.append(el("div", "decisions-omitted", `${omitted} older decision${omitted === 1 ? "" : "s"} not shown`));
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

  // ---------- decision cards ----------

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

  private settledCard(q: OrchQuestion): HTMLElement {
    const card = el("div", "decisions-card settled");
    card.dataset.itemId = q.id;
    const head = el("div", "decisions-card-head");
    head.append(el("span", "decisions-id", q.id));
    head.append(el("span", "decisions-status", q.status));
    if (q.settled_ms) head.append(el("span", "decisions-when", fmtTime(q.settled_ms)));
    card.append(head);
    card.append(el("div", "decisions-text", q.text));
    if (q.answer) card.append(el("div", "decisions-answer", q.answer));
    return card;
  }

  // ---------- demo cards ----------

  private demoCard(d: DemoItem): HTMLElement {
    const card = el("div", "decisions-card demo");
    card.dataset.itemId = d.id;

    const head = el("div", "decisions-card-head");
    head.append(this.taskLink(d.id, d.id));
    head.append(el("span", "decisions-status", d.status));
    if (d.assignee) head.append(el("span", "decisions-asker", d.assignee));
    card.append(head);
    card.append(el("div", "decisions-text", d.title));

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
    card.append(meta);

    const actions = el("div", "decisions-card-actions");
    if (d.canProceed) {
      // The SAME command the board's own Proceed button calls, guarded the
      // same way — two surfaces, one gesture, so demo state cannot fork.
      const go = el("button", "dlg-btn primary", "Proceed") as HTMLButtonElement;
      go.addEventListener("click", () => {
        go.disabled = true;
        invoke("orch_proceed_task", { groupId: this.groupId, id: d.id }).catch((err) => {
          go.disabled = false;
          this.toast(String(err));
        });
      });
      actions.append(go);
    }
    const changes = el("button", "dlg-btn", "Feedback") as HTMLButtonElement;
    changes.addEventListener("click", () => this.requestChanges(d));
    actions.append(changes);
    card.append(actions);
    return card;
  }

  /** Demo feedback: the board's own request-changes gesture, on the same two
   *  commands — record the findings for the orchestrator, then reopen the task
   *  as working, so the panel cannot keep offering Proceed on a row that just
   *  had changes asked of it. */
  private requestChanges(d: DemoItem): void {
    if (this.el.querySelector(".tasks-dialog")) return; // one at a time
    const overlay = el("div", "tasks-dialog");
    const box = el("div", "tasks-dialog-box");
    box.append(el("div", "tasks-dialog-title", `Feedback on ${d.id}`));

    const ta = document.createElement("textarea");
    ta.className = "dlg-input tasks-dialog-text";
    ta.placeholder = "What did you find? This goes to the orchestrator.";
    ta.spellcheck = false;
    ta.rows = 4;

    const actions = el("div", "dlg-actions");
    const cancel = el("button", "dlg-btn", "Cancel") as HTMLButtonElement;
    const send = el("button", "dlg-btn primary", "Send") as HTMLButtonElement;
    actions.append(cancel, send);
    box.append(ta, actions);
    overlay.append(box);

    const close = () => overlay.remove();
    const submit = () => {
      const findings = ta.value.trim();
      if (!findings) {
        ta.focus();
        return;
      }
      close();
      invoke("orch_request_changes", { groupId: this.groupId, id: d.id, findings })
        .then(() =>
          invoke("orch_upsert_task", { groupId: this.groupId, id: d.id, status: REQUEST_CHANGES_STATUS })
        )
        .catch((err) => this.toast(String(err)));
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
