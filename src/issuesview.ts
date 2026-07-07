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
  type GhIssue,
  type GhAuth,
} from "./issues";
import {
  TOGGLEABLE_LABELS,
  AGENT_MANAGED,
  isLabeledForAgents,
  filterAndSortIssues,
  labelDelta,
  validateNewIssue,
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
  private auth: GhAuth | null = null;
  private query = "";
  /** Issue numbers with a label-write in flight (buttons disabled meanwhile). */
  private busy = new Set<number>();

  private refreshing = false;
  private disposed = false;
  private toastTimer: number | undefined;

  private headTitleEl: HTMLElement;
  private headRepoEl: HTMLElement;
  private filterInput: HTMLInputElement;
  private listEl: HTMLElement;
  private blankEl: HTMLElement;
  private toastEl: HTMLElement;
  private createBtn: HTMLButtonElement;
  private refreshBtn: HTMLButtonElement;
  /** The open create-issue form, if any (kept to one at a time). */
  private formEl: HTMLElement | null = null;

  constructor(private host: IssuesViewHost) {
    this.el = el("div", "pane-issues");
    this.el.hidden = true;
    this.el.tabIndex = -1;
    this.el.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        // A mid-edit Esc should close the form, not the whole view.
        if (this.formEl) this.closeForm();
        else this.host.onClose();
      }
    });

    // -- header: title, repo, filter, actions --
    const head = el("div", "issues-head");
    this.headTitleEl = el("span", "issues-head-title", "Issues");
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
      this.headTitleEl,
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
    this.el.hidden = true;
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
        // Entered a different repo: reset view state.
        this.repoRoot = root;
        this.issues = [];
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

      let issues: GhIssue[];
      try {
        issues = await ghIssueList(root);
      } catch (err) {
        this.setBlank(`Could not list issues:\n${ghErrText(err)}`);
        return;
      }
      if (this.disposed) return;
      this.issues = issues;
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
    this.createBtn.disabled = !ready;
    this.filterInput.disabled = !ready;
  }

  private renderList(): void {
    this.listEl.replaceChildren();
    if (!this.repoRoot) return;

    const shown = filterAndSortIssues(this.issues, this.query);
    if (shown.length === 0) {
      const empty = el(
        "div",
        "issues-empty",
        this.issues.length === 0
          ? "No open issues."
          : "No issues match the filter."
      );
      this.listEl.appendChild(empty);
      return;
    }

    for (const issue of shown) {
      this.listEl.appendChild(this.renderRow(issue));
    }
  }

  private renderRow(issue: GhIssue): HTMLElement {
    const row = el("div", "issues-row");
    if (isLabeledForAgents(issue)) row.classList.add("agent-labeled");

    const num = el("span", "issues-num", `#${issue.number}`);

    const main = el("div", "issues-row-main");
    const title = el("span", "issues-title", issue.title);
    title.title = issue.title;
    main.appendChild(title);

    // Existing labels (all of them, read-only chips) + the updated time.
    const meta = el("div", "issues-row-meta");
    for (const label of issue.labels) {
      const known = label in LABEL_SHORT;
      const chip = el(
        "span",
        `issues-label${known ? " agent" : ""}`,
        known ? LABEL_SHORT[label] : label
      );
      chip.title = label;
      meta.appendChild(chip);
    }
    const when = el("span", "issues-when", fmtUpdated(issue.updated_at));
    when.title = issue.updated_at;
    meta.appendChild(when);
    main.appendChild(meta);

    // Toggle buttons for the go-signal labels + copy-URL.
    const actions = el("div", "issues-row-actions");
    for (const label of TOGGLEABLE_LABELS) {
      const on = issue.labels.includes(label);
      const btn = el(
        "button",
        `issues-toggle${on ? " on" : ""}`,
        LABEL_SHORT[label] ?? label
      ) as HTMLButtonElement;
      btn.title = on ? `Remove ${label}` : `Add ${label}`;
      btn.disabled = this.busy.has(issue.number);
      btn.addEventListener("click", () => void this.toggleLabel(issue, label, !on));
      actions.appendChild(btn);
    }
    // agent-managed is orchestrator-owned; show it read-only when present so the
    // human sees an orchestrator already claimed the issue (never toggled here).
    if (issue.labels.includes(AGENT_MANAGED)) {
      const chip = el("span", "issues-toggle managed", LABEL_SHORT[AGENT_MANAGED]);
      chip.title = "Owned by an orchestrator";
      actions.appendChild(chip);
    }
    const copyBtn = el("button", "issues-copy", "⧉") as HTMLButtonElement;
    copyBtn.title = "Copy issue URL";
    copyBtn.addEventListener("click", () => {
      void copyText(issue.url);
      this.toast(`Copied #${issue.number} URL`, "ok");
    });
    actions.appendChild(copyBtn);

    row.append(num, main, actions);
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
