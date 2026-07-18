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
  primaryWorktree,
  resolveSelection,
  isMissingDir,
  isWritable,
  isDeadWorktree,
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

/** What the hosting pane provides. TWO hosts (#217):
 *
 *   OVERLAY (Alt+G, unchanged) — the view floats over a terminal, in a container
 *     pane.ts sizes from the TERMINAL's height (`overlayClamp`) and closes with
 *     Esc/✕. That sizing model is the only thing that ever assumed a terminal.
 *   PANE (`embedded`) — the view IS a pane's content, filling its content box.
 *     There is no terminal to measure and none to close back to, so the ✕ and the
 *     Esc binding are dropped and the box is sized by the pane's own layout.
 *
 * The view's INNER layout (graph | diff over the changes strip, both dividers)
 * never cared: it has always clamped its sub-panes to `this.el`'s live size via
 * its own ResizeObserver, so it fits whatever box it is handed. That is why
 * hosting it in a pane needed a hook, not a second layout engine. */
export interface GitViewHost {
  /** The pane's live working directory (from shell integration) — or, in a git
   *  pane, that pane's repo. Any directory inside the work tree will do; the view
   *  resolves the top level (and the worktree set) itself. */
  getCwd(): string | null;
  /** Close the git view and return to the terminal. Never called when `embedded`. */
  onClose(): void;
  /** A mutating git action ran (commit, checkout, …) — the host may want to
   *  refresh its own git-derived UI (e.g. the pane's branch chip). */
  onRepoAction?: () => void;
  /** EMBEDDED mode: pane content, not an overlay. Drops the ✕ and the Esc-to-close
   *  binding — the PANE's ✕ is the close affordance. */
  embedded?: boolean;
  /** The view's own header "embed beside the terminal" toggle (#361) — a
   *  THIRD, unrelated sense of "embed" from the `embedded` flag above: that
   *  one means "hosted as a whole content pane, no terminal at all"; this is
   *  "share space with a terminal that's still right there," toggled at
   *  runtime from a still-floating-or-panel git view. Never offered when
   *  `embedded` is already true — a content pane has no terminal to share
   *  space WITH. `setPanelActive` (not `setEmbedded`) is the runtime method
   *  that reflects it, for the same disambiguation reason. */
  onToggleEmbed?: () => void;
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
  // a selected worktree's working dir (#208). `primaryRoot` is the repo's MAIN
  // working tree (the porcelain-first `git worktree list` entry), used to key
  // repo identity and to fall back to — NOT the pane's `--show-toplevel`, which
  // is the pane's own (possibly linked) worktree. `selectedWorktree` is an
  // explicit chip choice by path; null means "follow the pane's worktree".
  private repoRoot: string | null = null;
  private primaryRoot: string | null = null;
  private worktrees: Worktree[] = [];
  private selectedWorktree: string | null = null;
  // Read-only / unlock (#208 human ruling): a non-primary worktree is browse-only
  // until explicitly unlocked. `unlockedWorktree` pins the unlock to one path and
  // is cleared on every switch (see pointViewAt). `writable` is recomputed each
  // refresh and gates every write affordance below.
  private unlockedWorktree: string | null = null;
  private writable = true;
  // Normalized paths of worktrees seen to vanish at runtime (deleted without
  // `git worktree remove`/prune — git 2.29 emits no `prunable` marker). Keeps
  // the selector from re-offering a dead entry. Cleared on repo change.
  private missingWorktrees = new Set<string>();
  private commits: CommitInfo[] = [];
  private status: GitStatus | null = null;
  private selection: Selection = { kind: "working" };
  private selectedFile: FileSel | null = null;
  private limit = LOG_STEP;
  private firstOpen = true;
  // Bumped on every renderCommitFiles call; a call whose token goes stale before
  // its await resolves bails without appending (pre-existing double-append: a
  // commit click and a refresh's renderBottom for the SAME hash could both pass
  // the (kind, hash) staleness guard and both append a files column).
  private commitFilesToken = 0;

  private refreshing = false;
  private refreshQueued = false;
  private lastRefresh = 0;
  private throttleTimer: number | undefined;
  private toastTimer: number | undefined;
  private disposed = false;

  private headTitleEl: HTMLElement;
  private headWorktreeEl: HTMLElement;
  private headLockEl: HTMLButtonElement;
  private headBranchEl: HTMLElement;
  private pullBtn: HTMLButtonElement;
  private pushBtn: HTMLButtonElement;
  private embedBtn: HTMLButtonElement;
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
    // Esc closes the OVERLAY. Embedded (pane content) there is nothing to close back
    // to, so Esc is left alone — it belongs to whatever inner control has focus.
    if (!host.embedded) {
      this.el.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          this.host.onClose();
        }
      });
    }

    // -- graph column --
    const graph = el("div", "git-graph");
    this.graphEl = graph;
    const head = el("div", "git-graph-head");
    this.headTitleEl = el("span", "git-head-title");
    this.headWorktreeEl = el("span", "git-head-worktree clickable");
    this.headWorktreeEl.hidden = true;
    this.headWorktreeEl.title = "Switch worktree";
    this.headWorktreeEl.addEventListener("click", () => this.openWorktreeMenu());
    // Read-only indicator + per-selection unlock toggle (non-primary only).
    this.headLockEl = el("button", "git-head-lock") as HTMLButtonElement;
    this.headLockEl.hidden = true;
    this.headLockEl.addEventListener("click", () => this.toggleUnlock());
    this.headBranchEl = el("span", "git-head-branch clickable");
    this.headBranchEl.title = "Switch branch";
    this.headBranchEl.addEventListener("click", () => {
      if (this.writable) void this.openBranchMenu();
    });

    const pullBtn = el("button", "pane-btn", "↓") as HTMLButtonElement;
    this.pullBtn = pullBtn;
    pullBtn.title = "Pull (fast-forward only)";
    pullBtn.addEventListener("click", () =>
      void this.runOp(pullBtn, () => gitPull(this.repoRoot!), "Pulled")
    );
    const pushBtn = el("button", "pane-btn", "↑") as HTMLButtonElement;
    this.pushBtn = pushBtn;
    pushBtn.title = "Push current branch";
    pushBtn.addEventListener("click", () => void this.push(pushBtn));
    const fetchBtn = el("button", "pane-btn", "↻") as HTMLButtonElement;
    fetchBtn.title = "Fetch from remotes & refresh";
    fetchBtn.addEventListener("click", () =>
      void this.runOp(fetchBtn, () => gitFetch(this.repoRoot!), "Fetched")
    );
    // Embed toggle (#361): only offered when this view has a terminal to
    // share space WITH at all — never on a content pane (`host.embedded`),
    // which has none. See `GitViewHost.onToggleEmbed` above for the naming
    // disambiguation from the unrelated `embedded` flag.
    this.embedBtn = el("button", "pane-btn embed", "⬒") as HTMLButtonElement;
    this.embedBtn.hidden = !!host.embedded;
    this.embedBtn.addEventListener("click", () => this.host.onToggleEmbed?.());
    const closeBtn = el("button", "pane-btn close", "✕");
    closeBtn.title = "Back to terminal (Esc)";
    closeBtn.hidden = !!host.embedded; // pane content — the PANE's ✕ is the close affordance
    closeBtn.addEventListener("click", () => this.host.onClose());
    head.append(
      this.headTitleEl,
      this.headWorktreeEl,
      this.headLockEl,
      this.headBranchEl,
      pullBtn,
      pushBtn,
      fetchBtn,
      this.embedBtn,
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
    this.setPanelActive(false);
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

  /** Reflect whether the pane currently has this view in its embed-panel
   *  slot (#361) — pure display state on the header's toggle button; the
   *  pane owns the actual move. `setPanelActive`, not `setEmbedded`: see
   *  `GitViewHost.onToggleEmbed`'s comment for why the two must not share a
   *  name on this class. */
  setPanelActive(active: boolean): void {
    this.embedBtn.classList.toggle("active", active);
    this.embedBtn.textContent = active ? "⬓" : "⬒";
    this.embedBtn.title = active
      ? "Un-embed — back to a floating overlay"
      : "Embed beside the terminal (resizes this pane)";
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

  /** Point the view at `root` (primary or a worktree). Resets the graph/diff
   *  view state only when the root actually changed — the worktree *selection*
   *  itself is owned by the caller and left untouched. */
  private pointViewAt(root: string): void {
    if (root === this.repoRoot) return;
    this.repoRoot = root;
    // Every switch drops the unlock — re-selecting a worktree is read-only again.
    this.unlockedWorktree = null;
    this.commits = [];
    this.selection = { kind: "working" };
    this.selectedFile = null;
    this.limit = LOG_STEP;
    this.firstOpen = true;
  }

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
      // The pane's own top level. Inside a linked worktree this is that
      // worktree, not the main checkout — so it's the view's default target,
      // not the repo identity (see below).
      let paneTop: string | null;
      try {
        paneTop = await gitRepoRoot(cwd);
      } catch (err) {
        this.setBlank(
          String(err) === "git-not-found"
            ? "git was not found on PATH."
            : `git error: ${String(err)}`
        );
        return;
      }
      if (!paneTop) {
        this.setBlank(`Not a git repository:\n${cwd}`);
        return;
      }

      // Enumerate the worktree set from the pane's tree (any member lists them
      // all). The MAIN working tree — the porcelain-first entry — is the repo's
      // stable identity and fall-back, so cd'ing between a repo's own worktrees
      // (same set) never counts as changing repos. `enumOk` is false only if the
      // command errors outright — then we can't confirm primary-ness and fail
      // read-only (below), rather than defaulting to writable when state is least
      // known.
      let enumOk = true;
      try {
        this.worktrees = parseWorktrees(await gitWorktreeList(paneTop));
      } catch {
        this.worktrees = [];
        enumOk = false;
      }
      if (this.disposed) return;
      const primary = primaryWorktree(this.worktrees)?.path ?? paneTop;
      if (primary !== this.primaryRoot) {
        // Entered a different repo: drop the selection and the dead-entry memo.
        this.primaryRoot = primary;
        this.selectedWorktree = null;
        this.missingWorktrees.clear();
      }

      // Settle on which worktree the view targets. With no explicit chip choice
      // the view follows the pane (paneTop); an explicit choice wins and fails
      // soft if it was pruned/removed. An empty listing means enumeration failed
      // — keep the selection and just view the primary this pass, not drop it.
      let root = primary;
      if (this.worktrees.length > 0) {
        const res = resolveSelection(this.worktrees, this.selectedWorktree, paneTop);
        if (res.fellBack && this.selectedWorktree !== null) {
          this.toast("Selected worktree is gone — showing the primary repo.");
        }
        this.selectedWorktree = res.selected;
        if (res.active) root = res.active.path;
      }

      this.pointViewAt(root);

      let commits: CommitInfo[];
      let status: GitStatus;
      try {
        [commits, status] = await Promise.all([gitLog(root, this.limit), gitStatus(root)]);
        // Loaded fine — if this root was previously flagged dead (recreated),
        // clear it so the selector offers it again.
        this.missingWorktrees.delete(normalizePath(root));
      } catch (err) {
        // A selected worktree whose directory was deleted without git knowing
        // (manual rm -rf, crashed teardown) is still listed — resolveSelection
        // can't see that, so the git call is the first to notice ("no such
        // directory"). Only a non-primary root can vanish under us like this; a
        // missing primary means the whole repo is gone, so let that surface.
        const offPrimary = normalizePath(root) !== normalizePath(primary);
        if (!offPrimary || !isMissingDir(err)) throw err;
        // Memoize the dead path so the selector disables it, drop the selection,
        // and land where the default resolves in ONE step — the pane's own
        // worktree if the pane sits in a live one, else the primary — instead of
        // bouncing via primary this refresh and pane-following on the next.
        this.missingWorktrees.add(normalizePath(root));
        this.selectedWorktree = null;
        const fallback =
          this.worktrees.length > 0
            ? resolveSelection(this.worktrees, null, paneTop).active?.path ?? primary
            : primary;
        this.toast(
          normalizePath(fallback) === normalizePath(primary)
            ? "Selected worktree is gone — showing the primary repo."
            : "Selected worktree is gone — showing this pane's worktree."
        );
        root = fallback;
        this.pointViewAt(root);
        [commits, status] = await Promise.all([gitLog(root, this.limit), gitStatus(root)]);
        this.missingWorktrees.delete(normalizePath(root));
      }
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

      // Non-primary worktrees are read-only until unlocked (#208 ruling); every
      // render below gates its write affordances on this. If enumeration failed
      // we can't confirm primary-ness, so isWritable fails closed (read-only).
      this.writable = isWritable(this.activeWorktree(), this.unlockedWorktree, enumOk);

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

  /** Display label for the working-row dirty badge: the exact count, or a
   *  "N+" form when the untracked list was capped (#399) — the true count of
   *  untracked files can be far higher than what `dirtyCount()` sums, so the
   *  badge must say so rather than quietly showing a number that looks exact
   *  but isn't. */
  private dirtyCountLabel(): string {
    const dirty = this.dirtyCount();
    return this.status?.untracked_truncated ? `${dirty}+` : String(dirty);
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

  /** Right-click actions for a commit. On a read-only worktree only the copy
   *  actions remain — every history op mutates the checked-out branch. */
  private commitMenu(x: number, y: number, c: CommitInfo): void {
    if (!this.repoRoot) return;
    const short = c.hash.slice(0, 7);
    const branch = this.status?.branch ?? "HEAD";
    const items: MenuItem[] = [];
    if (this.writable) {
      items.push(
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
        { label: "-" }
      );
    }
    items.push(
      { label: "Copy commit hash", onClick: () => void copyText(c.hash) },
      { label: "Copy subject", onClick: () => void copyText(c.subject) }
    );
    showMenu(x, y, items);
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
    this.renderLockToggle();
    // Branch switching is a write — only offer it (and the affordance) when
    // writable; otherwise the name is shown read-only.
    this.headBranchEl.classList.toggle("clickable", this.writable);
    this.headBranchEl.title = this.writable ? "Switch branch" : "Read-only worktree";
    // Pull/push mutate; disable them off a writable tree (fetch stays — it only
    // refreshes remote-tracking refs, touching no working copy).
    this.pullBtn.disabled = !this.writable;
    this.pushBtn.disabled = !this.writable;
    const s = this.status;
    this.headBranchEl.textContent = s?.branch
      ? s.branch
      : s?.detached
        ? `detached @ ${this.headHash().slice(0, 7)}`
        : "";
  }

  /** The read-only badge + unlock toggle, shown only when a non-primary worktree
   *  is active (the primary is always writable, so it needs no control). */
  private renderLockToggle(): void {
    const active = this.activeWorktree();
    const show = active !== null && !active.primary;
    this.headLockEl.hidden = !show;
    if (!show) return;
    const unlocked = this.writable;
    this.headLockEl.textContent = unlocked ? "🔓 writable" : "🔒 read-only";
    this.headLockEl.title = unlocked
      ? "This worktree is unlocked for writes — click to lock (read-only)."
      : "This worktree is read-only so a live agent isn't disturbed — click to unlock writes.";
    this.headLockEl.classList.toggle("unlocked", unlocked);
  }

  /** Flip the active non-primary worktree between read-only and writable. The
   *  unlock is pinned to this worktree's path and cleared on any switch. */
  private toggleUnlock(): void {
    const active = this.activeWorktree();
    if (!active || active.primary) return;
    this.unlockedWorktree = this.writable ? null : active.path;
    void this.refresh();
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
      const dead = isDeadWorktree(w, this.missingWorktrees);
      const name = w.primary ? `${worktreeLabel(w)} (primary)` : worktreeLabel(w);
      const detail = w.bare
        ? "  (bare)"
        : dead
          ? "  (missing)"
          : w.branch
            ? `  ${w.branch}`
            : w.detached
              ? "  (detached)"
              : "";
      items.push({
        // A bare tree has no working copy to inspect; a dead one's dir is gone —
        // whether git flagged it prunable (≥ 2.31) or we saw it vanish at
        // runtime (2.29 has no marker). List both, disabled.
        label: `${isActive ? "● " : "  "}${name}${detail}`,
        disabled: isActive || dead,
        onClick: () => this.selectWorktree(w),
      });
    }
    const r = this.headWorktreeEl.getBoundingClientRect();
    showMenu(r.left, r.bottom + 3, items);
  }

  /** Record `w` as the explicit selection and refresh. Always store the path —
   *  including the primary — because a null selection means "follow the pane's
   *  worktree", which for a pane inside a linked worktree is NOT the primary. */
  private selectWorktree(w: Worktree): void {
    this.selectedWorktree = w.path;
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
      if (dirty > 0) row.appendChild(el("span", "git-count", this.dirtyCountLabel()));
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
          // Checkout is a write — the chip is inert on a read-only worktree.
          if (this.writable && (r.kind === "branch" || r.kind === "remote" || r.kind === "tag")) {
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

    // Read-only worktree: browse the same file lists (click to diff) but with no
    // stage/unstage/discard affordances and no commit box (#208 ruling).
    const rw = this.writable;
    this.changesEl.append(
      this.filesColumn("Staged", s.staged, {
        allLabel: rw ? "unstage all" : "",
        onAll: () =>
          void this.act(() =>
            gitUnstage(this.repoRoot!, s.staged.map((f) => f.path), s.empty)
          ),
        row: rw
          ? (f) => [
              this.actionBtn("−", "Unstage", () =>
                void this.act(() => gitUnstage(this.repoRoot!, [f.path], s.empty))
              ),
            ]
          : () => [],
        mode: () => "staged",
      }),
      this.filesColumn("Changes", changes, {
        allLabel: rw ? "stage all" : "",
        onAll: () =>
          void this.act(() => gitStage(this.repoRoot!, changes.map((f) => f.path))),
        row: rw
          ? (f) => [
              this.actionBtn("+", "Stage", () =>
                void this.act(() => gitStage(this.repoRoot!, [f.path]))
              ),
              this.discardBtn(f),
            ]
          : () => [],
        mode: (f) => (f.status === "?" ? "untracked" : "worktree"),
      })
    );

    if (s.untracked_truncated) {
      // #399: an unbounded untracked pile (a build dir, node_modules before
      // .gitignore catches it) is capped backend-side rather than rendered one
      // DOM row per file — never silently, so this says so.
      this.changesEl.appendChild(
        el(
          "div",
          "git-changes-note",
          `Showing the first ${s.untracked.length.toLocaleString()} untracked files — narrow with .gitignore to see the rest.`
        )
      );
    }

    if (!rw) {
      this.changesEl.appendChild(
        el(
          "div",
          "git-changes-note",
          "Read-only worktree — unlock (🔒 in the header) to stage, commit, or discard."
        )
      );
      return;
    }

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
    const token = ++this.commitFilesToken;

    let files: FileEntry[] = [];
    try {
      files = await gitCommitFiles(this.repoRoot, hash);
    } catch (err) {
      this.toast(String(err));
    }
    if (
      this.disposed ||
      this.selection.kind !== "commit" ||
      this.selection.hash !== hash ||
      token !== this.commitFilesToken
    ) {
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
