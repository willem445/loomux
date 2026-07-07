// Per-pane GitHub issues view: lists the repo's open issues, creates new ones,
// and toggles the orchestrator go-signal labels (agent-ready /
// agent-investigation). Toggled over the terminal like the git view; owns no PTY
// state and NEVER resizes it (hard constraint 1). All GitHub access goes
// through the typed src/issues.ts wrappers against the repo root resolved from
// the pane's live cwd — the same availability model as the git view.
//
// "Start work" here is just *labelling*: applying agent-ready is what a running
// orchestrator's poll loop pulls onto its board, so this view needs zero
// orchestrator coupling (see doc/design/orchestration.md and the plan on #82).

import { gitRepoRoot } from "./git";
import {
  ghAuthStatus,
  ghIssueList,
  ghIssueCreate,
  ghIssueSetLabels,
  ghIssueView,
  ghIssueComment,
  ghPrList,
  ghPrView,
  ghPrComment,
  type GhIssue,
  type GhPr,
  type GhDetail,
  type GhAuth,
} from "./issues";
import {
  TOGGLEABLE_LABELS,
  AGENT_MANAGED,
  isLabeledForAgents,
  filterAndSortIssues,
  labelDelta,
  validateNewIssue,
  validateComment,
  type ViewMode,
} from "./issuesmodel";

// gh.rs surfaces its bare "gh-not-found" sentinel from every command, not just
// auth status (a gh removed mid-session) — map it to the install hint wherever
// an error is rendered, mirroring the git-not-found handling in refresh().
function ghErrText(err: unknown): string {
  return String(err) === "gh-not-found"
    ? "GitHub CLI (gh) was not found on PATH — install it from https://cli.github.com."
    : String(err);
}

export interface IssuesViewHost {
  /** The pane's live working directory (from shell integration). */
  getCwd(): string | null;
  /** Close the issues view and return to the terminal. */
  onClose(): void;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls?: string,
  text?: string
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** Short human label for a label chip. */
const LABEL_SHORT: Record<string, string> = {
  "agent-ready": "ready",
  "agent-investigation": "investigate",
  "agent-managed": "managed",
};

/** Relative "updated N ago" from an ISO timestamp; falls back to the raw
 *  string if it doesn't parse. */
function fmtUpdated(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const secs = Math.max(0, Math.round((Date.now() - t) / 1000));
  if (secs < 60) return "just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.round(hrs / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(t).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** Copy to clipboard, with a legacy fallback for locked-down webviews.
 *  (Mirrors gitview.ts — the issue URL is copyable since browser-open is a
 *  group-scoped backend path not available here in v1.) */
async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    /* fall through */
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try {
    document.execCommand("copy");
  } finally {
    ta.remove();
  }
}

export class IssuesView {
  readonly el: HTMLElement;

  private repoRoot: string | null = null;
  private issues: GhIssue[] = [];
  private prs: GhPr[] = [];
  /** Which list is showing — issues or pull requests. */
  private mode: ViewMode = "issues";
  private auth: GhAuth | null = null;
  private query = "";
  /** Issue numbers with a label-write in flight (buttons disabled meanwhile). */
  private busy = new Set<number>();

  private refreshing = false;
  private disposed = false;
  private toastTimer: number | undefined;

  private issuesTabBtn: HTMLButtonElement;
  private prsTabBtn: HTMLButtonElement;
  private headRepoEl: HTMLElement;
  private filterInput: HTMLInputElement;
  private listEl: HTMLElement;
  private blankEl: HTMLElement;
  private toastEl: HTMLElement;
  private createBtn: HTMLButtonElement;
  private refreshBtn: HTMLButtonElement;
  /** The open create-issue form, if any (kept to one at a time). */
  private formEl: HTMLElement | null = null;
  /** The open detail pane, if any (issue or PR). */
  private detailEl: HTMLElement | null = null;
  /** What the open detail pane is showing, so a comment posts to the right thread. */
  private detailCtx: { kind: ViewMode; number: number } | null = null;
  /** The detail pane's thread region (description + comments), rebuilt per load. */
  private detailThreadEl: HTMLElement | null = null;
  private detailTitleEl: HTMLElement | null = null;
  private detailStateEl: HTMLElement | null = null;

  constructor(private host: IssuesViewHost) {
    this.el = el("div", "pane-issues");
    this.el.hidden = true;
    this.el.tabIndex = -1;
    this.el.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        // Peel back one layer at a time: detail → form → whole view.
        if (this.detailEl) this.closeDetail();
        else if (this.formEl) this.closeForm();
        else this.host.onClose();
      }
    });

    // -- header: mode toggle (Issues ⇄ PRs), repo, filter, actions --
    const head = el("div", "issues-head");
    const modeToggle = el("div", "issues-mode");
    this.issuesTabBtn = el("button", "issues-mode-tab on", "Issues") as HTMLButtonElement;
    this.issuesTabBtn.title = "Show issues";
    this.issuesTabBtn.addEventListener("click", () => this.setMode("issues"));
    this.prsTabBtn = el("button", "issues-mode-tab", "PRs") as HTMLButtonElement;
    this.prsTabBtn.title = "Show pull requests";
    this.prsTabBtn.addEventListener("click", () => this.setMode("prs"));
    modeToggle.append(this.issuesTabBtn, this.prsTabBtn);
    this.headRepoEl = el("span", "issues-head-repo");

    this.filterInput = document.createElement("input");
    this.filterInput.className = "issues-filter";
    this.filterInput.placeholder = "Filter by number, title, or label…";
    this.filterInput.addEventListener("input", () => {
      this.query = this.filterInput.value;
      this.renderList();
    });
    // Keep the shell/app shortcuts from firing while typing here.
    this.filterInput.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") {
        if (this.filterInput.value) {
          this.filterInput.value = "";
          this.query = "";
          this.renderList();
        } else {
          this.filterInput.blur();
        }
      }
    });

    this.createBtn = el("button", "pane-btn", "＋") as HTMLButtonElement;
    this.createBtn.title = "New issue";
    this.createBtn.addEventListener("click", () => this.openForm());

    this.refreshBtn = el("button", "pane-btn", "↻") as HTMLButtonElement;
    this.refreshBtn.title = "Refresh";
    this.refreshBtn.addEventListener("click", () => void this.refresh());

    const closeBtn = el("button", "pane-btn close", "✕");
    closeBtn.title = "Back to terminal (Esc)";
    closeBtn.addEventListener("click", () => this.host.onClose());

    head.append(
      modeToggle,
      this.headRepoEl,
      this.filterInput,
      this.createBtn,
      this.refreshBtn,
      closeBtn
    );

    // -- list + overlays --
    this.listEl = el("div", "issues-list");
    this.blankEl = el("div", "git-blank");
    this.blankEl.hidden = true;
    this.toastEl = el("div", "git-toast");
    this.toastEl.hidden = true;

    this.el.append(head, this.listEl, this.blankEl, this.toastEl);
  }

  get visible(): boolean {
    return !this.el.hidden;
  }

  show(): void {
    this.el.hidden = false;
    void this.refresh();
  }

  hide(): void {
    this.closeForm();
    this.closeDetail();
    this.el.hidden = true;
  }

  /** Switch between the issues and PR lists. No-op if already there; otherwise
   *  closes any open form/detail, updates the header, and refetches. */
  private setMode(mode: ViewMode): void {
    if (mode === this.mode) return;
    this.closeForm();
    this.closeDetail();
    this.mode = mode;
    this.issuesTabBtn.classList.toggle("on", mode === "issues");
    this.prsTabBtn.classList.toggle("on", mode === "prs");
    this.renderHead();
    // Show the already-fetched list for this mode immediately, then refresh.
    this.renderList();
    void this.refresh();
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.toastTimer);
    this.el.remove();
  }

  // ---------- data ----------

  private async refresh(): Promise<void> {
    if (this.disposed || this.refreshing) return;
    this.refreshing = true;
    this.setBusyBtn(this.refreshBtn, true);
    try {
      const cwd = this.host.getCwd();
      if (!cwd) {
        this.setBlank("Waiting for the shell to report its folder…");
        return;
      }
      let root: string | null;
      try {
        root = await gitRepoRoot(cwd);
      } catch (err) {
        this.setBlank(
          String(err) === "git-not-found"
            ? "git was not found on PATH."
            : `git error: ${String(err)}`
        );
        return;
      }
      if (!root) {
        this.setBlank(`Not a git repository:\n${cwd}`);
        return;
      }
      if (root !== this.repoRoot) {
        // Entered a different repo: reset view state (both lists).
        this.repoRoot = root;
        this.issues = [];
        this.prs = [];
      }

      // gh presence/auth gates everything — one cheap check up front so a
      // missing or unauthenticated gh yields a clear hint, not a stack of
      // failed list calls.
      let auth: GhAuth;
      try {
        auth = await ghAuthStatus();
      } catch (err) {
        this.setBlank(`Could not run gh: ${String(err)}`);
        return;
      }
      if (this.disposed) return;
      this.auth = auth;
      if (!auth.installed) {
        this.setBlank(
          "GitHub CLI (gh) was not found on PATH.\n\n" +
            "Install it from https://cli.github.com, then reopen this view."
        );
        return;
      }
      if (!auth.authenticated) {
        this.setBlank(
          "gh is installed but not authenticated.\n\n" +
            "Run  gh auth login  in a terminal, then reopen this view."
        );
        return;
      }

      const mode = this.mode;
      try {
        if (mode === "issues") {
          this.issues = await ghIssueList(root);
        } else {
          this.prs = await ghPrList(root);
        }
      } catch (err) {
        this.setBlank(
          `Could not list ${mode === "issues" ? "issues" : "pull requests"}:\n${ghErrText(err)}`
        );
        return;
      }
      if (this.disposed) return;
      // A mode switch mid-fetch makes this response stale — drop it.
      if (this.mode !== mode) return;
      this.setBlank(null);
      this.renderHead();
      this.renderList();
    } finally {
      this.refreshing = false;
      this.setBusyBtn(this.refreshBtn, false);
    }
  }

  // ---------- rendering ----------

  private setBlank(message: string | null): void {
    this.blankEl.hidden = message === null;
    if (message !== null) {
      this.blankEl.textContent = message;
      this.listEl.replaceChildren();
      this.renderHead();
    }
  }

  private toast(message: string, kind: "err" | "ok" = "err"): void {
    this.toastEl.textContent = message;
    this.toastEl.className = `git-toast ${kind}`;
    this.toastEl.hidden = false;
    clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(
      () => (this.toastEl.hidden = true),
      kind === "ok" ? 2500 : 6000
    );
  }

  private setBusyBtn(btn: HTMLButtonElement, on: boolean): void {
    btn.disabled = on;
    btn.classList.toggle("busy", on);
  }

  private renderHead(): void {
    const root = this.repoRoot ?? "";
    const name = root.split(/[\\/]/).filter(Boolean).pop() ?? "";
    this.headRepoEl.textContent = name;
    this.headRepoEl.title = root;
    // Create/filter only make sense once gh is authenticated and we're in a repo.
    const ready = !!this.repoRoot && !!this.auth?.authenticated;
    // Creating is issue-only — the PR mode is read-only (list + comment).
    this.createBtn.hidden = this.mode !== "issues";
    this.createBtn.disabled = !ready;
    this.filterInput.disabled = !ready;
    this.filterInput.placeholder =
      this.mode === "issues"
        ? "Filter by number, title, or label…"
        : "Filter PRs by number, title, or label…";
  }

  private renderList(): void {
    this.listEl.replaceChildren();
    if (!this.repoRoot) return;

    const isIssues = this.mode === "issues";
    const items = isIssues ? this.issues : this.prs;
    const shown = filterAndSortIssues(items, this.query);
    if (shown.length === 0) {
      const noun = isIssues ? "issues" : "pull requests";
      const empty = el(
        "div",
        "issues-empty",
        items.length === 0
          ? `No open ${noun}.`
          : `No ${noun} match the filter.`
      );
      this.listEl.appendChild(empty);
      return;
    }

    for (const item of shown) {
      this.listEl.appendChild(
        isIssues ? this.renderIssueRow(item as GhIssue) : this.renderPrRow(item as GhPr)
      );
    }
  }

  /** The shared skeleton for a list row: number, title, read-only label chips,
   *  updated-time, and an empty actions container. The whole row opens the detail
   *  pane on click (each caller wires the right kind); action buttons inside must
   *  `stopPropagation` so they don't also trigger the detail. */
  private rowShell(
    item: GhIssue | GhPr,
    kind: ViewMode
  ): { row: HTMLElement; meta: HTMLElement; actions: HTMLElement } {
    const row = el("div", "issues-row clickable");
    const num = el("span", "issues-num", `#${item.number}`);

    const main = el("div", "issues-row-main");
    const title = el("span", "issues-title", item.title);
    title.title = item.title;
    main.appendChild(title);

    const meta = el("div", "issues-row-meta");
    for (const label of item.labels) {
      const known = label in LABEL_SHORT;
      const chip = el(
        "span",
        `issues-label${known ? " agent" : ""}`,
        known ? LABEL_SHORT[label] : label
      );
      chip.title = label;
      meta.appendChild(chip);
    }
    const when = el("span", "issues-when", fmtUpdated(item.updated_at));
    when.title = item.updated_at;
    meta.appendChild(when);
    main.appendChild(meta);

    const actions = el("div", "issues-row-actions");
    row.append(num, main, actions);
    row.addEventListener("click", () => this.openDetail(kind, item.number, item.title));
    return { row, meta, actions };
  }

  /** A copy-URL button that never bubbles a click up to the row's detail-open. */
  private copyButton(url: string, number: number): HTMLButtonElement {
    const copyBtn = el("button", "issues-copy", "⧉") as HTMLButtonElement;
    copyBtn.title = "Copy URL";
    copyBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      void copyText(url);
      this.toast(`Copied #${number} URL`, "ok");
    });
    return copyBtn;
  }

  private renderIssueRow(issue: GhIssue): HTMLElement {
    const { row, actions } = this.rowShell(issue, "issues");
    if (isLabeledForAgents(issue)) row.classList.add("agent-labeled");

    // Toggle buttons for the go-signal labels.
    for (const label of TOGGLEABLE_LABELS) {
      const on = issue.labels.includes(label);
      const btn = el(
        "button",
        `issues-toggle${on ? " on" : ""}`,
        LABEL_SHORT[label] ?? label
      ) as HTMLButtonElement;
      btn.title = on ? `Remove ${label}` : `Add ${label}`;
      btn.disabled = this.busy.has(issue.number);
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        void this.toggleLabel(issue, label, !on);
      });
      actions.appendChild(btn);
    }
    // agent-managed is orchestrator-owned; show it read-only when present so the
    // human sees an orchestrator already claimed the issue (never toggled here).
    if (issue.labels.includes(AGENT_MANAGED)) {
      const chip = el("span", "issues-toggle managed", LABEL_SHORT[AGENT_MANAGED]);
      chip.title = "Owned by an orchestrator";
      actions.appendChild(chip);
    }
    actions.appendChild(this.copyButton(issue.url, issue.number));
    return row;
  }

  private renderPrRow(pr: GhPr): HTMLElement {
    const { row, meta, actions } = this.rowShell(pr, "prs");
    // PRs are read-only here: no label toggles, no merge/approve — just the head
    // branch as context and copy-URL. Comments are added from the detail pane.
    if (pr.head_ref) {
      const branch = el("span", "issues-branch", pr.head_ref);
      branch.title = `head: ${pr.head_ref}`;
      meta.insertBefore(branch, meta.firstChild);
    }
    actions.appendChild(this.copyButton(pr.url, pr.number));
    return row;
  }

  // ---------- label toggle ----------

  private async toggleLabel(issue: GhIssue, label: string, desired: boolean): Promise<void> {
    if (!this.repoRoot || this.busy.has(issue.number)) return;
    const delta = labelDelta(issue.labels, label, desired);
    if (delta.add.length === 0 && delta.remove.length === 0) return; // no-op
    this.busy.add(issue.number);
    this.renderList();
    try {
      await ghIssueSetLabels(this.repoRoot, issue.number, delta.add, delta.remove);
      // Reflect the change locally so the row updates without a full refetch;
      // the next refresh reconciles with GitHub's truth.
      issue.labels = issue.labels
        .filter((l) => !delta.remove.includes(l))
        .concat(delta.add);
      this.toast(
        desired ? `Applied ${label} to #${issue.number}` : `Removed ${label} from #${issue.number}`,
        "ok"
      );
      if (desired && isLabeledForAgents(issue) && !this.hasLiveOrchestratorHint()) {
        // Durable-on-GitHub reassurance when no orchestrator is watching.
        this.toast(
          `Labeled #${issue.number} — picked up when an orchestrator runs on this repo.`,
          "ok"
        );
      }
    } catch (err) {
      this.toast(ghErrText(err));
    } finally {
      this.busy.delete(issue.number);
      this.renderList();
    }
  }

  /** v1 has no live-group awareness in this view (it's not group-scoped), so we
   *  always surface the durable-label reassurance. Kept as a seam for phase 2's
   *  "push to board now" when a live orchestrator owns the pane's repo. */
  private hasLiveOrchestratorHint(): boolean {
    return false;
  }

  // ---------- detail pane ----------

  /** Open the detail pane over the list for issue/PR `number`. `kind` selects
   *  the view + comment backend; `title` seeds the header immediately (refined
   *  by the fetched detail). One pane at a time. */
  private openDetail(kind: ViewMode, number: number, title: string): void {
    if (!this.repoRoot) return;
    this.closeForm();
    this.closeDetail();
    this.detailCtx = { kind, number };

    const pane = el("div", "issues-detail");
    pane.tabIndex = -1;

    const head = el("div", "issues-detail-head");
    const back = el("button", "pane-btn", "←") as HTMLButtonElement;
    back.title = "Back to the list (Esc)";
    back.addEventListener("click", () => this.closeDetail());
    const num = el("span", "issues-detail-num", `#${number}`);
    this.detailTitleEl = el("span", "issues-detail-title", title);
    this.detailTitleEl.title = title;
    this.detailStateEl = el("span", "issues-detail-state");
    this.detailStateEl.hidden = true;
    head.append(back, num, this.detailTitleEl, this.detailStateEl);

    const scroll = el("div", "issues-detail-scroll");
    this.detailThreadEl = el("div", "issues-detail-thread");
    scroll.appendChild(this.detailThreadEl);

    // Composer — the one write PR mode allows too (gh {issue,pr} comment).
    const composer = el("div", "issues-detail-composer");
    const box = document.createElement("textarea");
    box.className = "issues-detail-input";
    box.placeholder = "Add a comment…  (Ctrl+Enter to post)";
    const submit = el("button", "git-modal-btn primary", "Comment") as HTMLButtonElement;
    box.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter" && e.ctrlKey) {
        e.preventDefault();
        void this.postComment(box, submit);
      } else if (e.key === "Escape") {
        // Keep a typed draft on the first Esc (blur to the pane so a second Esc
        // closes it via the view-level handler); close outright when empty.
        if (box.value) pane.focus();
        else this.closeDetail();
      }
    });
    submit.addEventListener("click", () => void this.postComment(box, submit));
    composer.append(box, submit);

    pane.append(head, scroll, composer);
    this.detailEl = pane;
    this.el.appendChild(pane);
    pane.focus();

    this.setThreadNote("Loading…");
    void this.loadDetail();
  }

  private closeDetail(): void {
    this.detailEl?.remove();
    this.detailEl = null;
    this.detailThreadEl = null;
    this.detailTitleEl = null;
    this.detailStateEl = null;
    this.detailCtx = null;
  }

  /** Replace the thread region with a single muted note (loading / error / empty). */
  private setThreadNote(message: string): void {
    if (!this.detailThreadEl) return;
    this.detailThreadEl.replaceChildren(el("div", "issues-detail-note", message));
  }

  /** Fetch and render the open detail pane's description + comment thread. */
  private async loadDetail(): Promise<void> {
    const ctx = this.detailCtx;
    if (!ctx || !this.repoRoot) return;
    const repo = this.repoRoot;
    let detail: GhDetail;
    try {
      detail =
        ctx.kind === "issues"
          ? await ghIssueView(repo, ctx.number)
          : await ghPrView(repo, ctx.number);
    } catch (err) {
      // Ignore if the pane was closed or swapped while the fetch was in flight.
      if (this.detailCtx !== ctx) return;
      this.setThreadNote(`Could not load #${ctx.number}:\n${ghErrText(err)}`);
      return;
    }
    if (this.disposed || this.detailCtx !== ctx) return;
    this.renderDetail(detail);
  }

  private renderDetail(detail: GhDetail): void {
    if (this.detailTitleEl) {
      this.detailTitleEl.textContent = detail.title;
      this.detailTitleEl.title = detail.title;
    }
    if (this.detailStateEl) {
      this.detailStateEl.textContent = detail.state.toLowerCase();
      this.detailStateEl.className = `issues-detail-state ${detail.state.toLowerCase()}`;
      this.detailStateEl.hidden = false;
    }
    const thread = this.detailThreadEl;
    if (!thread) return;
    thread.replaceChildren();

    // Description (GitHub-authored markdown — textContent only, never innerHTML;
    // the #129 XSS boundary). Rendered pre-wrap so newlines survive.
    thread.appendChild(this.renderComment(detail.author, null, detail.body, "description"));

    // Comment thread.
    if (detail.comments.length === 0) {
      thread.appendChild(el("div", "issues-detail-note", "No comments yet."));
    } else {
      for (const c of detail.comments) {
        thread.appendChild(this.renderComment(c.author, c.created_at, c.body, "comment"));
      }
    }
  }

  /** One authored block (the description or a comment). Every piece of text is
   *  set via `textContent` — no innerHTML on GitHub data (the #129 boundary). */
  private renderComment(
    author: string | null,
    createdAt: string | null,
    body: string,
    kind: "description" | "comment"
  ): HTMLElement {
    const block = el("div", `issues-detail-item ${kind}`);
    const head = el("div", "issues-detail-item-head");
    head.appendChild(el("span", "issues-detail-author", author ?? "(unknown)"));
    if (createdAt) {
      const when = el("span", "issues-detail-when", fmtUpdated(createdAt));
      when.title = createdAt;
      head.appendChild(when);
    }
    block.appendChild(head);
    const text = body.trim();
    const bodyEl = el(
      "div",
      text ? "issues-detail-body" : "issues-detail-body empty",
      text || (kind === "description" ? "No description provided." : "(empty comment)")
    );
    block.appendChild(bodyEl);
    return block;
  }

  private async postComment(
    box: HTMLTextAreaElement,
    submit: HTMLButtonElement
  ): Promise<void> {
    const ctx = this.detailCtx;
    if (!ctx || !this.repoRoot) return;
    const valid = validateComment(box.value);
    if (!valid.ok) {
      this.toast(valid.error);
      box.focus();
      return;
    }
    const repo = this.repoRoot;
    this.setBusyBtn(submit, true);
    try {
      if (ctx.kind === "issues") {
        await ghIssueComment(repo, ctx.number, valid.body);
      } else {
        await ghPrComment(repo, ctx.number, valid.body);
      }
      // The pane may have closed/swapped while posting — bail cleanly.
      if (this.detailCtx !== ctx) return;
      box.value = "";
      this.toast(`Comment posted to #${ctx.number}`, "ok");
      // Reflect the new comment: refetch the thread from GitHub's truth.
      await this.loadDetail();
    } catch (err) {
      this.toast(ghErrText(err));
    } finally {
      if (this.detailCtx === ctx) this.setBusyBtn(submit, false);
    }
  }

  // ---------- create form ----------

  private openForm(): void {
    if (!this.repoRoot || this.formEl) return;
    const backdrop = el("div", "issues-form-backdrop");
    const box = el("div", "issues-form");
    box.appendChild(el("div", "issues-form-title", "New issue"));

    const titleInput = document.createElement("input");
    titleInput.className = "issues-form-input";
    titleInput.placeholder = "Title";

    const bodyInput = document.createElement("textarea");
    bodyInput.className = "issues-form-body";
    bodyInput.placeholder = "Body (optional)";

    const actions = el("div", "issues-form-actions");
    const cancel = el("button", "git-modal-btn", "Cancel") as HTMLButtonElement;
    const submit = el("button", "git-modal-btn primary", "Create") as HTMLButtonElement;
    actions.append(cancel, submit);
    box.append(titleInput, bodyInput, actions);
    backdrop.appendChild(box);

    const stop = (e: KeyboardEvent) => {
      e.stopPropagation();
      if (e.key === "Escape") this.closeForm();
      // Ctrl+Enter submits from either field.
      if (e.key === "Enter" && e.ctrlKey) void this.submitForm(titleInput, bodyInput, submit);
    };
    titleInput.addEventListener("keydown", (e) => {
      stop(e);
      if (e.key === "Enter" && !e.ctrlKey) {
        e.preventDefault();
        bodyInput.focus();
      }
    });
    bodyInput.addEventListener("keydown", stop);
    cancel.addEventListener("click", () => this.closeForm());
    submit.addEventListener("click", () => void this.submitForm(titleInput, bodyInput, submit));
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) this.closeForm();
    });

    this.formEl = backdrop;
    this.el.appendChild(backdrop);
    titleInput.focus();
  }

  private closeForm(): void {
    this.formEl?.remove();
    this.formEl = null;
  }

  private async submitForm(
    titleInput: HTMLInputElement,
    bodyInput: HTMLTextAreaElement,
    submit: HTMLButtonElement
  ): Promise<void> {
    if (!this.repoRoot) return;
    const valid = validateNewIssue({ title: titleInput.value, body: bodyInput.value });
    if (!valid.ok) {
      this.toast(valid.error);
      titleInput.focus();
      return;
    }
    this.setBusyBtn(submit, true);
    try {
      const created = await ghIssueCreate(this.repoRoot, valid.title, valid.body);
      this.closeForm();
      this.toast(`Created #${created.number}`, "ok");
      await this.refresh();
    } catch (err) {
      this.toast(ghErrText(err));
      this.setBusyBtn(submit, false);
    }
  }
}
