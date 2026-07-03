// Per-pane git view: commit graph (left), diff preview (right), working
// changes + commit box (bottom). Toggled over the terminal; owns no PTY
// state. All git access goes through the Rust backend against the repo
// root resolved from the pane's live cwd.

import {
  gitRepoRoot,
  gitLog,
  gitStatus,
  gitDiff,
  gitCommitFiles,
  gitStage,
  gitUnstage,
  gitCommit,
  gitCheckout,
  gitDiscard,
  type CommitInfo,
  type FileEntry,
  type GitStatus,
  type DiffMode,
} from "./git";
import { computeLanes, renderRowSvg } from "./gitgraph";
import { renderDiff } from "./diffrender";

export interface GitViewHost {
  /** The pane's live working directory (from shell integration). */
  getCwd(): string | null;
  /** Close the git view and return to the terminal. */
  onClose(): void;
  /** A mutating git action ran (commit, checkout, …) — the host may want to
   *  refresh its own git-derived UI (e.g. the pane's branch chip). */
  onRepoAction?: () => void;
}

type Selection = { kind: "working" } | { kind: "commit"; hash: string };

interface FileSel {
  path: string;
  mode: DiffMode;
  hash?: string;
  label: string;
}

const ROW_H = 26;
const LANE_W = 12;
const MAX_LANES = 12;
const LOG_STEP = 300;

function relTime(unixSec: number): string {
  const s = Math.max(0, Date.now() / 1000 - unixSec);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 604800) return `${Math.floor(s / 86400)}d`;
  return new Date(unixSec * 1000).toLocaleDateString();
}

/** Display letter for a working-tree entry ("?" untracked → U like VS Code,
 *  backend conflict U → !). */
function statusLetter(status: string, untracked: boolean): string {
  if (untracked) return "U";
  if (status === "U") return "!";
  return status;
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

export class GitView {
  readonly el: HTMLElement;

  private repoRoot: string | null = null;
  private commits: CommitInfo[] = [];
  private status: GitStatus | null = null;
  private selection: Selection = { kind: "working" };
  private selectedFile: FileSel | null = null;
  private limit = LOG_STEP;
  private firstOpen = true;

  private refreshing = false;
  private refreshQueued = false;
  private lastRefresh = 0;
  private throttleTimer: number | undefined;
  private toastTimer: number | undefined;
  private disposed = false;

  private headTitleEl: HTMLElement;
  private headBranchEl: HTMLElement;
  private graphListEl: HTMLElement;
  private diffHeadEl: HTMLElement;
  private diffBodyEl: HTMLElement;
  private changesEl: HTMLElement;
  private blankEl: HTMLElement;
  private toastEl: HTMLElement;

  constructor(private host: GitViewHost) {
    this.el = el("div", "pane-git");
    this.el.hidden = true;
    this.el.tabIndex = -1;
    this.el.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        this.host.onClose();
      }
    });

    // -- graph column --
    const graph = el("div", "git-graph");
    const head = el("div", "git-graph-head");
    this.headTitleEl = el("span", "git-head-title");
    this.headBranchEl = el("span", "git-head-branch");
    const refreshBtn = el("button", "pane-btn", "↻");
    refreshBtn.title = "Refresh";
    refreshBtn.addEventListener("click", () => void this.refresh());
    const closeBtn = el("button", "pane-btn close", "✕");
    closeBtn.title = "Back to terminal (Esc)";
    closeBtn.addEventListener("click", () => this.host.onClose());
    head.append(this.headTitleEl, this.headBranchEl, refreshBtn, closeBtn);
    this.graphListEl = el("div", "git-graph-list");
    graph.append(head, this.graphListEl);

    // -- diff panel --
    const diff = el("div", "git-diff");
    this.diffHeadEl = el("div", "git-diff-head");
    this.diffBodyEl = el("div", "git-diff-body");
    diff.append(this.diffHeadEl, this.diffBodyEl);

    // -- changes strip --
    this.changesEl = el("div", "git-changes");

    // -- overlays --
    this.blankEl = el("div", "git-blank");
    this.blankEl.hidden = true;
    this.toastEl = el("div", "git-toast");
    this.toastEl.hidden = true;

    this.el.append(graph, diff, this.changesEl, this.blankEl, this.toastEl);
  }

  get visible(): boolean {
    return !this.el.hidden;
  }

  show(): void {
    this.el.hidden = false;
    // No focus steal: the terminal below stays the primary input target.
    void this.refresh();
  }

  hide(): void {
    this.el.hidden = true;
  }

  /** Called on every shell prompt; refreshes at most twice a second. */
  notifyPrompt(): void {
    if (!this.visible || this.disposed) return;
    const since = Date.now() - this.lastRefresh;
    if (since >= 500) {
      void this.refresh();
    } else if (this.throttleTimer === undefined) {
      this.throttleTimer = window.setTimeout(() => {
        this.throttleTimer = undefined;
        if (this.visible && !this.disposed) void this.refresh();
      }, 500 - since);
    }
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.throttleTimer);
    clearTimeout(this.toastTimer);
    this.el.remove();
  }

  // ---------- data ----------

  private async refresh(): Promise<void> {
    if (this.disposed) return;
    if (this.refreshing) {
      this.refreshQueued = true;
      return;
    }
    this.refreshing = true;
    this.lastRefresh = Date.now();
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
        this.commits = [];
        this.selection = { kind: "working" };
        this.selectedFile = null;
        this.limit = LOG_STEP;
        this.firstOpen = true;
      }

      const [commits, status] = await Promise.all([
        gitLog(root, this.limit),
        gitStatus(root),
      ]);
      if (this.disposed) return;
      this.commits = commits;
      this.status = status;
      this.setBlank(null);

      // Keep the selection valid across refreshes.
      if (this.selection.kind === "commit") {
        const hash = this.selection.hash;
        if (!commits.some((c) => c.hash === hash)) this.selection = { kind: "working" };
      }
      if (this.firstOpen) {
        this.firstOpen = false;
        this.selection =
          this.dirtyCount() > 0 || commits.length === 0
            ? { kind: "working" }
            : { kind: "commit", hash: commits[0].hash };
      }

      this.renderHead();
      this.renderGraph();
      await this.renderBottom();
      await this.refreshDiff();
    } catch (err) {
      this.toast(String(err));
    } finally {
      this.refreshing = false;
      if (this.refreshQueued) {
        this.refreshQueued = false;
        void this.refresh();
      }
    }
  }

  private dirtyCount(): number {
    const s = this.status;
    if (!s) return 0;
    return s.staged.length + s.unstaged.length + s.untracked.length;
  }

  /** Run a mutating git action, surface errors, refresh either way. */
  private async act(fn: () => Promise<void>): Promise<void> {
    try {
      await fn();
    } catch (err) {
      this.toast(String(err));
    }
    await this.refresh();
    this.host.onRepoAction?.();
  }

  // ---------- rendering ----------

  private setBlank(message: string | null): void {
    this.blankEl.hidden = message === null;
    if (message !== null) {
      this.blankEl.textContent = message;
      this.repoRoot = null;
    }
  }

  private toast(message: string): void {
    this.toastEl.textContent = message;
    this.toastEl.hidden = false;
    clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(() => (this.toastEl.hidden = true), 6000);
  }

  private renderHead(): void {
    const root = this.repoRoot ?? "";
    this.headTitleEl.textContent = root.split(/[\\/]/).filter(Boolean).pop() ?? "";
    const s = this.status;
    this.headBranchEl.textContent = s?.branch
      ? s.branch
      : s?.detached
        ? `detached @ ${this.headHash().slice(0, 7)}`
        : "";
  }

  private headHash(): string {
    return (
      this.commits.find((c) => c.refs.some((r) => r.kind === "head"))?.hash ??
      this.commits[0]?.hash ??
      ""
    );
  }

  private renderGraph(): void {
    this.graphListEl.replaceChildren();
    const dirty = this.dirtyCount();

    if (dirty > 0 || this.commits.length === 0) {
      const row = el("div", "git-row uncommitted");
      if (this.selection.kind === "working") row.classList.add("selected");
      const dot = el("span", "git-wip-dot", "●");
      const label = el(
        "span",
        "git-subject",
        this.commits.length === 0 && dirty === 0
          ? "No commits yet"
          : `Uncommitted changes`
      );
      row.append(dot, label);
      if (dirty > 0) row.appendChild(el("span", "git-count", String(dirty)));
      row.addEventListener("click", () => this.select({ kind: "working" }));
      this.graphListEl.appendChild(row);
    }

    const lanes = computeLanes(this.commits);
    this.commits.forEach((c, i) => {
      const row = el("div", "git-row");
      if (this.selection.kind === "commit" && this.selection.hash === c.hash) {
        row.classList.add("selected");
      }
      row.appendChild(renderRowSvg(lanes[i], ROW_H, LANE_W, MAX_LANES));

      if (c.refs.length > 0) {
        const refs = el("span", "git-refs");
        for (const r of c.refs) {
          if (r.kind === "head" && c.refs.some((o) => o.kind === "branch")) continue;
          const chip = el("span", `git-chip ${r.kind}`, r.name);
          if (r.kind === "branch" || r.kind === "remote") {
            chip.title = `Double-click to checkout ${r.name}`;
            chip.addEventListener("dblclick", (e) => {
              e.stopPropagation();
              void this.checkout(r.name, r.kind === "remote");
            });
          }
          refs.appendChild(chip);
        }
        row.appendChild(refs);
      }

      const subject = el("span", "git-subject", c.subject);
      subject.title = `${c.subject}\n${c.author} · ${new Date(c.timestamp * 1000).toLocaleString()}\n${c.hash}`;
      const when = el("span", "git-when", relTime(c.timestamp));
      when.title = new Date(c.timestamp * 1000).toLocaleString();
      row.append(subject, when);
      row.addEventListener("click", () => this.select({ kind: "commit", hash: c.hash }));
      this.graphListEl.appendChild(row);
    });

    if (this.commits.length >= this.limit) {
      const more = el("button", "git-more", "Load more…");
      more.addEventListener("click", () => {
        this.limit += LOG_STEP;
        void this.refresh();
      });
      this.graphListEl.appendChild(more);
    }
  }

  private select(sel: Selection): void {
    this.selection = sel;
    this.selectedFile = null;
    this.renderGraph();
    void this.renderBottom().then(() => this.refreshDiff());
  }

  private async renderBottom(): Promise<void> {
    if (this.selection.kind === "working") this.renderWorking();
    else await this.renderCommitFiles(this.selection.hash);
  }

  // -- working mode: staged / changes / commit box --

  private renderWorking(): void {
    this.changesEl.replaceChildren();
    const s = this.status;
    if (!s || !this.repoRoot) return;

    const changes: FileEntry[] = [
      ...s.unstaged,
      ...s.untracked.map((path) => ({ path, orig_path: null, status: "?" })),
    ];

    this.changesEl.append(
      this.filesColumn("Staged", s.staged, {
        allLabel: "unstage all",
        onAll: () =>
          void this.act(() =>
            gitUnstage(this.repoRoot!, s.staged.map((f) => f.path), s.empty)
          ),
        row: (f) => [
          this.actionBtn("−", "Unstage", () =>
            void this.act(() => gitUnstage(this.repoRoot!, [f.path], s.empty))
          ),
        ],
        mode: () => "staged",
      }),
      this.filesColumn("Changes", changes, {
        allLabel: "stage all",
        onAll: () =>
          void this.act(() => gitStage(this.repoRoot!, changes.map((f) => f.path))),
        row: (f) => [
          this.actionBtn("+", "Stage", () =>
            void this.act(() => gitStage(this.repoRoot!, [f.path]))
          ),
          this.discardBtn(f),
        ],
        mode: (f) => (f.status === "?" ? "untracked" : "worktree"),
      })
    );

    // -- commit box --
    const box = el("div", "git-commit-box");
    const msg = document.createElement("textarea");
    msg.className = "git-msg";
    msg.placeholder = "Commit message";
    const btn = el("button", "git-commit-btn", "Commit");
    const update = () => {
      btn.disabled = s.staged.length === 0 || msg.value.trim().length === 0;
    };
    update();
    msg.addEventListener("input", update);
    msg.addEventListener("keydown", (e) => {
      e.stopPropagation(); // keep Esc/shortcuts from closing while typing
      if (e.key === "Enter" && e.ctrlKey && !btn.disabled) btn.click();
      if (e.key === "Escape") msg.blur();
    });
    btn.addEventListener("click", () => {
      const message = msg.value.trim();
      if (!message) return;
      void this.act(() => gitCommit(this.repoRoot!, message));
    });
    const hint = el(
      "div",
      "git-commit-hint",
      s.staged.length === 0 ? "Stage files to commit" : `${s.staged.length} file(s) staged`
    );
    box.append(msg, btn, hint);
    this.changesEl.appendChild(box);
  }

  private filesColumn(
    title: string,
    files: FileEntry[],
    opts: {
      allLabel: string;
      onAll: () => void;
      row: (f: FileEntry) => HTMLElement[];
      mode: (f: FileEntry) => DiffMode;
    }
  ): HTMLElement {
    const col = el("div", "git-files-col");
    const head = el("div", "git-files-head");
    head.appendChild(el("span", "", `${title} (${files.length})`));
    if (files.length > 0 && opts.allLabel) {
      const all = el("button", "git-link", opts.allLabel);
      all.addEventListener("click", opts.onAll);
      head.appendChild(all);
    }
    const list = el("div", "git-files-list");
    for (const f of files) {
      const untracked = f.status === "?";
      const row = el("div", "git-file-row");
      const label = f.orig_path ? `${f.orig_path} → ${f.path}` : f.path;
      if (this.selectedFile?.path === f.path && this.selectedFile.mode === opts.mode(f)) {
        row.classList.add("selected");
      }
      const letter = statusLetter(f.status, untracked);
      const st = el("span", `git-st st-${letter}`, letter);
      const name = el("span", "git-file-name", label);
      name.title = label;
      row.append(st, name, ...opts.row(f));
      row.addEventListener("click", () => {
        this.selectedFile = { path: f.path, mode: opts.mode(f), label };
        void this.renderBottom().then(() => this.refreshDiff());
      });
      list.appendChild(row);
    }
    if (files.length === 0) list.appendChild(el("div", "git-files-none", "none"));
    col.append(head, list);
    return col;
  }

  private actionBtn(glyph: string, tip: string, fn: () => void): HTMLElement {
    const btn = el("button", "git-file-btn", glyph);
    btn.title = tip;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      fn();
    });
    return btn;
  }

  /** Discard with a two-click inline confirm (native confirm dialogs are
   *  unreliable inside the Tauri webview). */
  private discardBtn(f: FileEntry): HTMLElement {
    const btn = el("button", "git-file-btn danger", "✕");
    btn.title = "Discard changes";
    let armed = false;
    let timer: number | undefined;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (!armed) {
        armed = true;
        btn.textContent = "sure?";
        btn.classList.add("confirm");
        timer = window.setTimeout(() => {
          armed = false;
          btn.textContent = "✕";
          btn.classList.remove("confirm");
        }, 3000);
        return;
      }
      clearTimeout(timer);
      void this.act(() => gitDiscard(this.repoRoot!, f.path, f.status === "?"));
    });
    return btn;
  }

  // -- commit mode: file list + metadata --

  private async renderCommitFiles(hash: string): Promise<void> {
    this.changesEl.replaceChildren();
    if (!this.repoRoot) return;
    const commit = this.commits.find((c) => c.hash === hash);

    let files: FileEntry[] = [];
    try {
      files = await gitCommitFiles(this.repoRoot, hash);
    } catch (err) {
      this.toast(String(err));
    }
    if (this.disposed || this.selection.kind !== "commit" || this.selection.hash !== hash) {
      return;
    }

    this.changesEl.appendChild(
      this.filesColumn(`Files in ${hash.slice(0, 7)}`, files, {
        allLabel: "",
        onAll: () => {},
        row: () => [],
        mode: () => "commit",
      })
    );

    if (commit) {
      const meta = el("div", "git-commit-meta");
      meta.appendChild(el("div", "git-meta-subject", commit.subject));
      meta.appendChild(
        el(
          "div",
          "git-meta-line",
          `${commit.author} · ${new Date(commit.timestamp * 1000).toLocaleString()}`
        )
      );
      meta.appendChild(el("div", "git-meta-line hash", commit.hash));
      this.changesEl.appendChild(meta);
    }
  }

  // -- diff panel --

  private async refreshDiff(): Promise<void> {
    const sel = this.selectedFile;
    this.diffHeadEl.replaceChildren();
    if (!sel || !this.repoRoot) {
      this.diffBodyEl.replaceChildren(el("div", "git-empty", "Select a file to preview."));
      return;
    }
    this.diffHeadEl.appendChild(el("span", "git-diff-path", sel.label));
    this.diffHeadEl.appendChild(el("span", `git-diff-mode ${sel.mode}`, sel.mode));
    const hash = this.selection.kind === "commit" ? this.selection.hash : undefined;
    try {
      const raw = await gitDiff(this.repoRoot, sel.path, sel.mode, hash);
      if (this.disposed || this.selectedFile !== sel) return;
      renderDiff(raw, this.diffBodyEl);
    } catch (err) {
      this.diffBodyEl.replaceChildren(el("div", "git-empty", String(err)));
    }
  }

  // -- checkout --

  private async checkout(name: string, remote: boolean): Promise<void> {
    await this.act(() => gitCheckout(this.repoRoot!, name, remote));
  }
}
