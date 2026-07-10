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
  gitFetch,
  gitPush,
  gitPull,
  gitTag,
  gitBranchCreate,
  gitCherryPick,
  gitRevert,
  gitMerge,
  gitRebase,
  gitBranches,
  gitWorktreeList,
  type BranchInfo,
  type CommitInfo,
  type FileEntry,
  type GitStatus,
  type DiffMode,
} from "./git";
import {
  parseWorktrees,
  resolveSelection,
  worktreeLabel,
  normalizePath,
  type Worktree,
} from "./gitworktree";
import { computeLanes, renderRowSvg } from "./gitgraph";
import { shortRev, fmtWhen, fmtWhenFull, authorLine } from "./gitformat";
import { renderDiff } from "./diffrender";
import {
  GRAPH_MIN,
  DIFF_MIN,
  CHANGES_MIN,
  TOP_MIN,
  DIVIDER_PX,
  DEFAULT_GRAPH_W,
  DEFAULT_CHANGES_H,
  KEY_GRAPH_W,
  KEY_CHANGES_H,
  clampPaneSize,
  parseStoredSize,
} from "./gitlayout";

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

interface MenuItem {
  /** "-" renders a separator. */
  label: string;
  danger?: boolean;
  disabled?: boolean;
  onClick?: () => void;
}

let activeMenu: HTMLElement | null = null;

function closeMenu(): void {
  if (!activeMenu) return;
  activeMenu.remove();
  activeMenu = null;
  window.removeEventListener("pointerdown", onMenuPointer, true);
  window.removeEventListener("keydown", onMenuKey, true);
  window.removeEventListener("resize", closeMenu);
}

function onMenuPointer(e: PointerEvent): void {
  if (activeMenu && !activeMenu.contains(e.target as Node)) closeMenu();
}

function onMenuKey(e: KeyboardEvent): void {
  if (e.key === "Escape") {
    e.stopPropagation();
    closeMenu();
  }
}

/** Floating context menu at viewport coordinates (x, y). */
function showMenu(x: number, y: number, items: MenuItem[]): void {
  closeMenu();
  const menu = el("div", "git-menu");
  for (const it of items) {
    if (it.label === "-") {
      menu.appendChild(el("div", "git-menu-sep"));
      continue;
    }
    const row = el("button", `git-menu-item${it.danger ? " danger" : ""}`, it.label);
    (row as HTMLButtonElement).disabled = !!it.disabled;
    if (!it.disabled && it.onClick) {
      row.addEventListener("click", () => {
        closeMenu();
        it.onClick!();
      });
    }
    menu.appendChild(row);
  }
  menu.style.visibility = "hidden";
  document.body.appendChild(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(4, Math.min(x, window.innerWidth - r.width - 6))}px`;
  menu.style.top = `${Math.max(4, Math.min(y, window.innerHeight - r.height - 6))}px`;
  menu.style.visibility = "visible";
  activeMenu = menu;
  // Defer so the opening click doesn't immediately dismiss it.
  setTimeout(() => {
    if (!activeMenu) return;
    window.addEventListener("pointerdown", onMenuPointer, true);
    window.addEventListener("keydown", onMenuKey, true);
    window.addEventListener("resize", closeMenu);
  }, 0);
}

/** Read a persisted divider size, tolerating a webview with no/blocked
 *  localStorage (returns null, so the caller falls back to a default). */
function readSize(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

/** Persist a divider size (rounded to whole px), ignoring storage failures. */
function writeSize(key: string, value: number): void {
  try {
    localStorage.setItem(key, String(Math.round(value)));
  } catch {
    /* ignore — resizing still works this session, just won't persist */
  }
}

/** Copy to clipboard, with a legacy fallback for locked-down webviews. */
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

export class GitView {
  readonly el: HTMLElement;

  // `repoRoot` is the directory git commands run against — the primary repo, or
  // the selected worktree's working dir (#208). `primaryRoot` is always the
  // pane's main repo, used to enumerate worktrees and to fall back to. A
  // worktree is selected by its path (`selectedWorktree`); null = primary.
  private repoRoot: string | null = null;
  private primaryRoot: string | null = null;
  private worktrees: Worktree[] = [];
  private selectedWorktree: string | null = null;
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
  private headWorktreeEl: HTMLElement;
  private headBranchEl: HTMLElement;
  private graphListEl: HTMLElement;
  private diffHeadEl: HTMLElement;
  private diffBodyEl: HTMLElement;
  private changesEl: HTMLElement;
  private blankEl: HTMLElement;
  private toastEl: HTMLElement;

  // Resizable sub-panes. `topEl` is the graph|diff row; `graphEl` is the left
  // column whose width the vertical divider drives; `changesEl` height is what
  // the horizontal divider drives. Sizes persist per-divider in localStorage
  // and are re-clamped to the container on every layout (the overlay's outer
  // bounds — and thus the PTY — never change; only this inner distribution).
  private topEl: HTMLElement;
  private graphEl: HTMLElement;
  private resizeObs: ResizeObserver;

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
    this.graphEl = graph;
    const head = el("div", "git-graph-head");
    this.headTitleEl = el("span", "git-head-title");
    this.headWorktreeEl = el("span", "git-head-worktree clickable");
    this.headWorktreeEl.hidden = true;
    this.headWorktreeEl.title = "Switch worktree";
    this.headWorktreeEl.addEventListener("click", () => this.openWorktreeMenu());
    this.headBranchEl = el("span", "git-head-branch clickable");
    this.headBranchEl.title = "Switch branch";
    this.headBranchEl.addEventListener("click", () => void this.openBranchMenu());

    const pullBtn = el("button", "pane-btn", "↓") as HTMLButtonElement;
    pullBtn.title = "Pull (fast-forward only)";
    pullBtn.addEventListener("click", () =>
      void this.runOp(pullBtn, () => gitPull(this.repoRoot!), "Pulled")
    );
    const pushBtn = el("button", "pane-btn", "↑") as HTMLButtonElement;
    pushBtn.title = "Push current branch";
    pushBtn.addEventListener("click", () => void this.push(pushBtn));
    const fetchBtn = el("button", "pane-btn", "↻") as HTMLButtonElement;
    fetchBtn.title = "Fetch from remotes & refresh";
    fetchBtn.addEventListener("click", () =>
      void this.runOp(fetchBtn, () => gitFetch(this.repoRoot!), "Fetched")
    );
    const closeBtn = el("button", "pane-btn close", "✕");
    closeBtn.title = "Back to terminal (Esc)";
    closeBtn.addEventListener("click", () => this.host.onClose());
    head.append(
      this.headTitleEl,
      this.headWorktreeEl,
      this.headBranchEl,
      pullBtn,
      pushBtn,
      fetchBtn,
      closeBtn
    );
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

    // -- layout: [ graph | vdiv | diff ] / hdiv / changes --
    // A flex column (top row + bottom strip) with two draggable dividers. The
    // vertical divider moves the graph|diff boundary; the horizontal one moves
    // the top|changes boundary. Same drag mechanics as the overlay divider in
    // pane.ts (mousedown + window move/up + a `dragging` class), but this only
    // redistributes space *inside* .pane-git — its outer box keeps its size,
    // so the terminal's PTY is never resized.
    this.topEl = el("div", "git-top");
    const vDiv = this.makeDivider(
      "vertical",
      () => this.graphEl.offsetWidth,
      (start, delta) => this.applyGraphWidth(start + delta, true)
    );
    this.topEl.append(graph, vDiv, diff);
    const hDiv = this.makeDivider(
      "horizontal",
      () => this.changesEl.offsetHeight,
      // The divider sits above the changes strip: dragging down (delta > 0)
      // grows the top row and shrinks changes.
      (start, delta) => this.applyChangesHeight(start - delta, true)
    );

    this.el.append(this.topEl, hDiv, this.changesEl, this.blankEl, this.toastEl);

    // Seed initial sizes from storage (container not yet measurable here, so no
    // clamp); relayout() re-clamps once the overlay is shown and on any resize.
    this.graphEl.style.flex = `0 0 ${parseStoredSize(readSize(KEY_GRAPH_W)) ?? DEFAULT_GRAPH_W}px`;
    this.changesEl.style.flex = `0 0 ${parseStoredSize(readSize(KEY_CHANGES_H)) ?? DEFAULT_CHANGES_H}px`;
    this.resizeObs = new ResizeObserver(() => this.relayout());
    this.resizeObs.observe(this.el);
  }

  get visible(): boolean {
    return !this.el.hidden;
  }

  show(): void {
    this.el.hidden = false;
    // Now measurable: clamp the (possibly stale) stored sizes to the container.
    this.relayout();
    // No focus steal: the terminal below stays the primary input target.
    void this.refresh();
  }

  hide(): void {
    closeMenu();
    this.el.hidden = true;
  }

  // ---------- resizable sub-panes ----------

  /** Build a draggable divider. `orientation` picks the axis (vertical = a
   *  column splitter dragged along X; horizontal = a row splitter dragged along
   *  Y). `measure()` returns the neighbored pane's size at drag start; `onDrag`
   *  receives that start size plus the signed pixel delta on every move and on
   *  release. Mirrors makeOverlayDivider in pane.ts, but never touches the
   *  overlay's outer size — only the inner flex distribution. */
  private makeDivider(
    orientation: "vertical" | "horizontal",
    measure: () => number,
    onDrag: (start: number, delta: number) => void
  ): HTMLElement {
    const div = el("div", `git-divider ${orientation}`);
    div.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const origin = orientation === "vertical" ? e.clientX : e.clientY;
      const start = measure();
      div.classList.add("dragging");
      const move = (ev: MouseEvent) => {
        const cur = orientation === "vertical" ? ev.clientX : ev.clientY;
        onDrag(start, cur - origin);
      };
      const up = () => {
        div.classList.remove("dragging");
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    });
    return div;
  }

  /** Set the graph column's width (px), clamped so neither it nor the diff drops
   *  below its minimum; optionally persist. No-op until the pane is measurable. */
  private applyGraphWidth(proposed: number, persist: boolean): void {
    const total = this.topEl.clientWidth - DIVIDER_PX;
    if (total <= 0) return;
    const w = clampPaneSize(proposed, { total, min: GRAPH_MIN, otherMin: DIFF_MIN });
    this.graphEl.style.flex = `0 0 ${w}px`;
    if (persist) writeSize(KEY_GRAPH_W, w);
  }

  /** Set the changes strip's height (px), clamped so neither it nor the top row
   *  drops below its minimum; optionally persist. No-op until measurable. */
  private applyChangesHeight(proposed: number, persist: boolean): void {
    const total = this.el.clientHeight - DIVIDER_PX;
    if (total <= 0) return;
    const h = clampPaneSize(proposed, { total, min: CHANGES_MIN, otherMin: TOP_MIN });
    this.changesEl.style.flex = `0 0 ${h}px`;
    if (persist) writeSize(KEY_CHANGES_H, h);
  }

  /** Re-apply the stored (or default) sizes, clamped to the current container.
   *  Called on show and whenever the overlay is resized (its outer bounds may
   *  shrink around the panes), so a pane grows back toward its stored preference
   *  when room returns and never collapses below its minimum when room is scarce.
   *  Preference-preserving because it derives from storage, not the live size. */
  private relayout(): void {
    if (this.disposed) return;
    this.applyGraphWidth(parseStoredSize(readSize(KEY_GRAPH_W)) ?? DEFAULT_GRAPH_W, false);
    this.applyChangesHeight(parseStoredSize(readSize(KEY_CHANGES_H)) ?? DEFAULT_CHANGES_H, false);
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
    closeMenu();
    this.resizeObs.disconnect();
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
      let primary: string | null;
      try {
        primary = await gitRepoRoot(cwd);
      } catch (err) {
        this.setBlank(
          String(err) === "git-not-found"
            ? "git was not found on PATH."
            : `git error: ${String(err)}`
        );
        return;
      }
      if (!primary) {
        this.setBlank(`Not a git repository:\n${cwd}`);
        return;
      }
      if (primary !== this.primaryRoot) {
        // Entered a different repo: drop the worktree selection with the rest.
        this.primaryRoot = primary;
        this.selectedWorktree = null;
        this.worktrees = [];
      }

      // Enumerate the worktree set from the primary and settle on which one the
      // view is pointed at. A pruned/removed selection fails soft back to the
      // primary (#208). Enumeration failing (very old git, etc.) degrades to
      // primary-only rather than breaking the whole view.
      try {
        this.worktrees = parseWorktrees(await gitWorktreeList(primary));
      } catch {
        this.worktrees = [];
      }
      if (this.disposed) return;
      // Only reconcile the selection against a real listing. An empty result
      // means enumeration failed (or an unusual git) — keep the selection and
      // just view the primary this pass, rather than dropping it on a blip.
      let root = primary;
      if (this.worktrees.length > 0) {
        const res = resolveSelection(this.worktrees, this.selectedWorktree);
        if (res.fellBack && this.selectedWorktree !== null) {
          this.toast("Selected worktree is gone — showing the primary repo.");
        }
        this.selectedWorktree = res.selected;
        if (res.active) root = res.active.path;
      }

      if (root !== this.repoRoot) {
        // Entered a different worktree (or repo): reset the graph/diff view
        // state, but keep the worktree selection itself.
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

  private setBusy(btn: HTMLButtonElement, on: boolean): void {
    btn.disabled = on;
    btn.classList.toggle("busy", on);
  }

  /** Run a remote op with button feedback (spin + disable), a result toast,
   *  and a refresh. */
  private async runOp(
    btn: HTMLButtonElement,
    fn: () => Promise<void>,
    okMsg: string
  ): Promise<void> {
    if (!this.repoRoot) return;
    this.setBusy(btn, true);
    try {
      await fn();
      this.toast(okMsg, "ok");
    } catch (err) {
      this.toast(String(err));
    } finally {
      this.setBusy(btn, false);
      await this.refresh();
      this.host.onRepoAction?.();
    }
  }

  /** Push; if the branch has no upstream, offer to publish it. */
  private async push(btn: HTMLButtonElement): Promise<void> {
    if (!this.repoRoot) return;
    this.setBusy(btn, true);
    try {
      try {
        await gitPush(this.repoRoot, false);
        this.toast("Pushed", "ok");
      } catch (err) {
        const msg = String(err);
        if (!/upstream/i.test(msg)) {
          this.toast(msg);
          return;
        }
        this.setBusy(btn, false);
        const ok = await this.confirm(
          "Publish branch",
          "This branch has no upstream yet. Publish it to the remote and set tracking?",
          "Publish"
        );
        if (!ok) return;
        this.setBusy(btn, true);
        await gitPush(this.repoRoot, true);
        this.toast("Published", "ok");
      }
    } catch (err) {
      this.toast(String(err));
    } finally {
      this.setBusy(btn, false);
      await this.refresh();
      this.host.onRepoAction?.();
    }
  }

  private async openBranchMenu(): Promise<void> {
    if (!this.repoRoot) return;
    let branches: BranchInfo[];
    try {
      branches = await gitBranches(this.repoRoot);
    } catch (err) {
      this.toast(String(err));
      return;
    }
    const items: MenuItem[] = [];
    const locals = branches.filter((b) => b.kind === "local");
    const remotes = branches.filter((b) => b.kind === "remote");
    if (locals.length === 0 && remotes.length === 0) {
      items.push({ label: "No branches", disabled: true });
    }
    for (const b of locals) {
      items.push({
        label: `${b.current ? "● " : " "}${b.name}`,
        disabled: b.current,
        onClick: () => void this.checkout(b.name, false),
      });
    }
    if (remotes.length > 0) {
      items.push({ label: "-" });
      for (const b of remotes) {
        items.push({
          label: b.name,
          onClick: () => void this.checkout(b.name, true),
        });
      }
    }
    const r = this.headBranchEl.getBoundingClientRect();
    showMenu(r.left, r.bottom + 3, items);
  }

  /** Right-click actions for a commit. */
  private commitMenu(x: number, y: number, c: CommitInfo): void {
    if (!this.repoRoot) return;
    const short = c.hash.slice(0, 7);
    const branch = this.status?.branch ?? "HEAD";
    showMenu(x, y, [
      {
        label: `Checkout ${short} (detached)`,
        onClick: () => void this.checkout(c.hash, false),
      },
      { label: "Create branch here…", onClick: () => void this.createBranch(c.hash) },
      { label: "Create tag here…", onClick: () => void this.createTag(c.hash) },
      { label: "-" },
      {
        label: `Cherry-pick onto ${branch}`,
        onClick: () =>
          void this.confirmOp(
            "Cherry-pick",
            `Apply commit ${short} onto ${branch}?`,
            "Cherry-pick",
            () => gitCherryPick(this.repoRoot!, c.hash)
          ),
      },
      {
        label: `Revert ${short}`,
        danger: true,
        onClick: () =>
          void this.confirmOp(
            "Revert commit",
            `Create a commit on ${branch} that undoes ${short}?`,
            "Revert",
            () => gitRevert(this.repoRoot!, c.hash)
          ),
      },
      {
        label: `Merge ${short} into ${branch}`,
        danger: true,
        onClick: () =>
          void this.confirmOp(
            "Merge",
            `Merge commit ${short} into ${branch}?`,
            "Merge",
            () => gitMerge(this.repoRoot!, c.hash)
          ),
      },
      {
        label: `Rebase ${branch} onto ${short}`,
        danger: true,
        onClick: () =>
          void this.confirmOp(
            "Rebase",
            `Rebase ${branch} onto ${short}? This rewrites history on ${branch}.`,
            "Rebase",
            () => gitRebase(this.repoRoot!, c.hash)
          ),
      },
      { label: "-" },
      { label: "Copy commit hash", onClick: () => void copyText(c.hash) },
      { label: "Copy subject", onClick: () => void copyText(c.subject) },
    ]);
  }

  private async createBranch(hash: string): Promise<void> {
    const name = await this.promptName("Create branch", "branch name");
    if (!name) return;
    await this.act(() => gitBranchCreate(this.repoRoot!, name, hash, true));
  }

  private async createTag(hash: string): Promise<void> {
    const name = await this.promptName("Create tag", "tag name");
    if (!name) return;
    await this.act(() => gitTag(this.repoRoot!, name, hash));
  }

  private async confirmOp(
    title: string,
    detail: string,
    label: string,
    fn: () => Promise<void>
  ): Promise<void> {
    if (await this.confirm(title, detail, label)) await this.act(fn);
  }

  // ---------- modals ----------

  /** A two-button confirm overlay scoped to the git view. Resolves true on OK. */
  private confirm(title: string, detail: string, confirmLabel: string): Promise<boolean> {
    return new Promise((resolve) => {
      const backdrop = el("div", "git-modal-backdrop");
      const box = el("div", "git-modal");
      box.append(el("div", "git-modal-title", title), el("div", "git-modal-detail", detail));
      const actions = el("div", "git-modal-actions");
      const cancel = el("button", "git-modal-btn", "Cancel");
      const ok = el("button", "git-modal-btn primary", confirmLabel);
      actions.append(cancel, ok);
      box.appendChild(actions);
      backdrop.appendChild(box);
      const onKey = (e: KeyboardEvent) => {
        e.stopPropagation();
        if (e.key === "Escape") done(false);
        else if (e.key === "Enter") done(true);
      };
      const done = (v: boolean) => {
        backdrop.remove();
        window.removeEventListener("keydown", onKey, true);
        resolve(v);
      };
      cancel.addEventListener("click", () => done(false));
      ok.addEventListener("click", () => done(true));
      backdrop.addEventListener("pointerdown", (e) => {
        if (e.target === backdrop) done(false);
      });
      window.addEventListener("keydown", onKey, true);
      this.el.appendChild(backdrop);
      ok.focus();
    });
  }

  /** A single-line text prompt overlay. Resolves the trimmed value, or null. */
  private promptName(title: string, placeholder: string): Promise<string | null> {
    return new Promise((resolve) => {
      const backdrop = el("div", "git-modal-backdrop");
      const box = el("div", "git-modal");
      box.appendChild(el("div", "git-modal-title", title));
      const input = document.createElement("input");
      input.className = "git-modal-input";
      input.placeholder = placeholder;
      box.appendChild(input);
      const actions = el("div", "git-modal-actions");
      const cancel = el("button", "git-modal-btn", "Cancel");
      const ok = el("button", "git-modal-btn primary", "OK");
      actions.append(cancel, ok);
      box.appendChild(actions);
      backdrop.appendChild(box);
      const done = (v: string | null) => {
        backdrop.remove();
        resolve(v);
      };
      const submit = () => {
        const v = input.value.trim();
        done(v.length > 0 ? v : null);
      };
      input.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Enter") submit();
        else if (e.key === "Escape") done(null);
      });
      cancel.addEventListener("click", () => done(null));
      ok.addEventListener("click", submit);
      backdrop.addEventListener("pointerdown", (e) => {
        if (e.target === backdrop) done(null);
      });
      this.el.appendChild(backdrop);
      input.focus();
    });
  }

  private renderHead(): void {
    // Title stays the primary repo name even when viewing a worktree, so the
    // repo identity is stable; the worktree chip names which tree we're in.
    const root = this.primaryRoot ?? this.repoRoot ?? "";
    this.headTitleEl.textContent = root.split(/[\\/]/).filter(Boolean).pop() ?? "";
    this.renderWorktreeChip();
    const s = this.status;
    this.headBranchEl.textContent = s?.branch
      ? s.branch
      : s?.detached
        ? `detached @ ${this.headHash().slice(0, 7)}`
        : "";
  }

  /** The worktree currently being viewed (matched by path), or null. */
  private activeWorktree(): Worktree | null {
    const root = normalizePath(this.repoRoot ?? "");
    return this.worktrees.find((w) => normalizePath(w.path) === root) ?? null;
  }

  /** Show a clickable chip naming the viewed worktree whenever there's more
   *  than one to switch between (or a non-primary one is selected, so it's
   *  obvious you're off the main tree). Hidden for a plain single-worktree repo. */
  private renderWorktreeChip(): void {
    const active = this.activeWorktree();
    const show = this.worktrees.length > 1 || (active !== null && !active.primary);
    this.headWorktreeEl.hidden = !show;
    if (!show) return;
    const label = active ? (active.primary ? "primary" : worktreeLabel(active)) : "primary";
    this.headWorktreeEl.textContent = `⧉ ${label} ▾`;
    this.headWorktreeEl.classList.toggle("non-primary", active !== null && !active.primary);
    this.headWorktreeEl.title = active
      ? `Worktree: ${active.path}${active.branch ? ` (${active.branch})` : ""}\nClick to switch`
      : "Switch worktree";
  }

  private openWorktreeMenu(): void {
    const active = normalizePath(this.repoRoot ?? "");
    const items: MenuItem[] = [];
    if (this.worktrees.length === 0) {
      items.push({ label: "No worktrees", disabled: true });
    }
    for (const w of this.worktrees) {
      const isActive = normalizePath(w.path) === active;
      const name = w.primary ? `${worktreeLabel(w)} (primary)` : worktreeLabel(w);
      const detail = w.bare
        ? "  (bare)"
        : w.branch
          ? `  ${w.branch}`
          : w.detached
            ? "  (detached)"
            : "";
      items.push({
        // A bare tree has no working copy to inspect — list it, but disabled.
        label: `${isActive ? "● " : "  "}${name}${detail}`,
        disabled: isActive || w.bare,
        onClick: () => this.selectWorktree(w),
      });
    }
    const r = this.headWorktreeEl.getBoundingClientRect();
    showMenu(r.left, r.bottom + 3, items);
  }

  /** Point the view at `w` (primary → clear the selection) and refresh. */
  private selectWorktree(w: Worktree): void {
    this.selectedWorktree = w.primary ? null : w.path;
    void this.refresh();
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
          if (r.kind === "branch" || r.kind === "remote" || r.kind === "tag") {
            const remote = r.kind === "remote";
            chip.title = `Right-click for actions · double-click to checkout ${r.name}`;
            chip.addEventListener("dblclick", (e) => {
              e.stopPropagation();
              void this.checkout(r.name, remote);
            });
            chip.addEventListener("contextmenu", (e) => {
              e.preventDefault();
              e.stopPropagation();
              showMenu(e.clientX, e.clientY, [
                { label: `Checkout ${r.name}`, onClick: () => void this.checkout(r.name, remote) },
              ]);
            });
          }
          refs.appendChild(chip);
        }
        row.appendChild(refs);
      }

      const subject = el("span", "git-subject", c.subject);
      subject.title = `${c.subject}\n${authorLine(c.author, c.committer, c.timestamp)}\n${c.hash}`;

      // Committer · short rev · date+time, all ellipsizing/dim so a row stays
      // scannable at the default graph width; full values live in tooltips.
      const committer = el("span", "git-committer", c.committer);
      committer.title = c.committer;
      const rev = el("span", "git-rev", shortRev(c.hash));
      rev.title = c.hash;
      const when = el("span", "git-when", fmtWhen(c.timestamp));
      when.title = fmtWhenFull(c.timestamp);
      row.append(subject, committer, rev, when);
      row.addEventListener("click", () => this.select({ kind: "commit", hash: c.hash }));
      row.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        this.select({ kind: "commit", hash: c.hash });
        this.commitMenu(e.clientX, e.clientY, c);
      });
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
        el("div", "git-meta-line", authorLine(commit.author, commit.committer, commit.timestamp))
      );
      const hashLine = el("div", "git-meta-line hash", commit.hash);
      hashLine.title = commit.hash;
      meta.appendChild(hashLine);
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
