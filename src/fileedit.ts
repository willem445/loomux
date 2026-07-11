// The per-pane file-editor overlay (issue #174): a lazy file tree on the left, a
// code editor on the right, and a project-wide search-and-replace panel — all
// floating over the terminal, never resizing the PTY (CLAUDE.md constraint #1,
// same overlay mechanics as GitView/AuditView). Available in EVERY pane type.
//
// This class owns only DOM wiring; the testable logic lives in the pure modules
// it composes (filetreemodel, fileicons, dirtystate, searchresults) and the
// backend (fileapi → fileedit.rs). DOM wiring is human-validated — there is no
// DOM simulation in the tests, by house convention.

import { open } from "@tauri-apps/plugin-dialog";
import {
  ftListDir,
  ftReadFile,
  ftWriteFile,
  ftSearchStart,
  ftSearchCancel,
  onSearchBatch,
  nextSearchId,
  ftFilesStart,
  onFilesBatch,
  ftReplace,
  errorCode,
  errorMessage,
  type SearchOpts,
  type SearchBatch,
  type FilesBatch,
} from "./fileapi";
import {
  rankFileNames,
  moveSelection,
  basenameStart,
  queryTerms,
  type FileNameHit,
} from "./filematch";
import {
  groupMatches,
  countSummary,
  toggleFile,
  selectedFiles,
  selectedMatchCount,
  paramsEqual,
  replaceIsCurrent,
  hitCounts,
  firstMatch,
  type FileGroup,
  type SearchParams,
} from "./searchresults";
import {
  idle,
  begin,
  accept,
  isTruncated,
  isSearching,
  enumerationSource,
  RENDER_CAP,
  type SearchState,
} from "./searchsession";
import { gitRepoRoot } from "./git";
import {
  makeRoot,
  mergeChildren,
  flatten,
  findNode,
  ancestorDirs,
  type TreeNode,
} from "./filetreemodel";
import { fileIconSvg, folderIconSvg } from "./fileicons";
import { closeDecision, type ConflictChoice } from "./dirtystate";
import { detectEol, applyEol, textDiffers, type Eol } from "./eol";
import { createEditor, type EditorWidget } from "./editorwidget";
import { showToast } from "./toast";
import { modal } from "./modal";

/** What the hosting pane provides. TWO hosts now (#217): the Alt+F OVERLAY over a
 *  terminal (the original, unchanged), and an EDITOR PANE whose permanent content
 *  this view is. The fork between them is these two optional hooks and nothing
 *  else — the tree, the editor, the #207 streaming search and replace are the same
 *  code in both. */
export interface FileEditHost {
  /** The pane's live working directory (shell-integration cwd / worktree) — or, in
   *  an editor pane, that pane's root. */
  getCwd(): string | null;
  /** Close the overlay and return focus to the terminal. Never called in embedded
   *  mode (there is nothing to close back to). */
  onClose(): void;
  /** True when the root is a running agent's worktree — the view shows a subtle
   *  banner (editing it is legitimate but the agent may also be writing). */
  isAgentWorktree?(): boolean;
  /** EMBEDDED mode: this view is an editor PANE's permanent content, not an overlay
   *  floating over a terminal. There is nothing to close back TO, so the ✕ and the
   *  Esc-to-close binding are dropped — the pane's own ✕ closes it (and asks about
   *  unsaved edits first, via `canDiscard`). This is the only behavioral fork, and
   *  it is one the overlay semantics genuinely don't have an answer for.
   *
   *  (First built in PR #215 round 1 for the #214 pane, reverted with it when that
   *  pane became a file manager, and resurrected here — where the editor-as-pane is
   *  the actual ask.) */
  embedded?: boolean;
  /** The user re-rooted the tree from the header's folder picker. An OVERLAY host
   *  ignores this (the root is view-local by design — browsing must not disturb the
   *  terminal or a running agent); an editor PANE adopts it as the pane's root, so
   *  the title and the persisted layout follow what's actually on screen. */
  onRootChanged?(root: string): void;
}

const TREE_W_KEY = "loomux.fileedit.treeW";
const DEFAULT_TREE_W = 280;
const MIN_TREE_W = 180;
const MAX_TREE_W = 640;
/** Debounce for auto-search while typing, so the tree highlights update
 *  "live" without a full-tree walk on every keystroke. */
const SEARCH_DEBOUNCE_MS = 300;
/** How many "Go to file" results are rendered (#214). The list is a jump target,
 *  not a report: past a screenful you refine the query, you don't scroll. Ranking
 *  runs over the FULL path list first, so the cap never costs you the best hit —
 *  and the summary says how many matched, so a cut is never silent. */
const GOTO_RESULT_CAP = 200;

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

export class FileEditView {
  readonly el: HTMLElement;

  /** The directory the tree is rooted at. Seeded from the pane cwd on show;
   *  can be overridden by the folder picker (view-local — it does NOT cd the
   *  shell, so browsing here never disturbs the terminal or a running agent). */
  private root: string | null = null;
  private treeModel: TreeNode = makeRoot();
  /** The in-flight (or last) load of the root's listing, so `openPath` can wait for
   *  the tree to exist before revealing into it. Resolved when nothing is loading. */
  private treeLoad: Promise<void> = Promise.resolve();

  // Header bits.
  private rootLabel: HTMLElement;
  private fileLabel: HTMLElement;
  private dirtyDot: HTMLElement;
  private saveBtn: HTMLButtonElement;
  private findBtn: HTMLButtonElement;
  private agentBanner: HTMLElement;

  // Body panes.
  private treePane: HTMLElement;
  private treeListEl: HTMLElement;
  private editorPane: HTMLElement;
  private editorHost: HTMLElement;
  private emptyState: HTMLElement;

  // "Go to file" — the file-NAME search (issue #214). Distinct from the content
  // search below it: this matches PATHS and never opens a file. The backend
  // enumerates the path list ONCE per root (`ftFilesStart` — off-thread,
  // streaming, cancellable, same enumeration source as the content search) and we
  // cache it here, so every keystroke is an in-memory rank over `fileList` with
  // zero I/O. That is what makes it instant on a big repo, and it's the "optimized
  // and fast file search" the issue asks for.
  private gotoInput!: HTMLInputElement;
  private gotoListEl!: HTMLElement;
  private gotoSummaryEl!: HTMLElement;
  /** Every path under the root, as enumerated. Empty until the first index. */
  private fileList: string[] = [];
  /** The in-flight enumeration's id (for cancel + batch demux), or null. */
  private fileListId: number | null = null;
  /** True once an enumeration has completed for the CURRENT root + ignore mode. */
  private fileListLoaded = false;
  /** The enumeration hit the backend's file ceiling — the list is incomplete, and
   *  the summary says so rather than quietly under-reporting matches. */
  private fileListTruncated = false;
  /** Unsubscribe for the `ft-files` listener (torn down on dispose). */
  private filesUnlisten: (() => void) | null = null;
  private gotoHits: FileNameHit[] = [];
  /** Index into `gotoHits` of the keyboard-selected row. */
  private gotoSel = 0;
  /** Coalesces repaints while paths stream in, one per animation frame. */
  private gotoRenderScheduled = false;

  // Open-file state.
  private editor: EditorWidget | null = null;
  private openRel: string | null = null;
  /** Last-saved snapshot, kept as the RAW on-disk text (its original line
   *  endings). Dirty checks compare it to the editor's LF text in an
   *  EOL-normalized space (`textDiffers`), so a CRLF file opened and untouched
   *  reads as clean. */
  private savedContent = "";
  private savedHash = "";
  /** The open file's line-ending style, re-applied on save so writing never
   *  silently converts CRLF↔LF. */
  private openEol: Eol = "\n";

  // Search/replace state.
  private searchInput!: HTMLInputElement;
  private replaceInput!: HTMLInputElement;
  private ciBox!: HTMLInputElement;
  private wwBox!: HTMLInputElement;
  private igBox!: HTMLInputElement;
  private igLabel!: HTMLLabelElement;
  private summaryEl!: HTMLElement;
  private replaceBtn!: HTMLButtonElement;
  private searchGroups: FileGroup[] = [];
  /** rel → match count for the current search, consulted per tree row to
   *  highlight + badge files that contain hits. */
  private hits = new Map<string, number>();
  private searchTimer: number | undefined;
  /** Streaming-search accumulator (issue #207). Its `activeId` is the id whose
   *  `ft-search` batches we currently accept; bumping it (new keystroke) or going
   *  idle (Esc/clear) makes every in-flight batch from the old search a no-op —
   *  the cancellation guarantee. Results never exceed the module's render cap. */
  private session: SearchState = idle();
  /** The params the in-flight search was launched with; promoted to
   *  `searchSnapshot` only when that search *finishes* (so a partial/cancelled
   *  search never enables replace against an incomplete result set). */
  private pendingParams: SearchParams | null = null;
  /** Unsubscribe for the `ft-search` event listener (torn down on dispose). */
  private searchUnlisten: (() => void) | null = null;
  /** Coalesces mid-search tree repaints to one per animation frame so a burst of
   *  batches can't thrash the DOM. */
  private renderScheduled = false;
  /** Whether the current root is inside a git work tree — gates the ignore
   *  toggle (a non-git root has no `.gitignore` to respect). Null until probed. */
  private isGitRoot: boolean | null = null;
  /** Set once `dispose()` runs. Guards the async `onSearchBatch` listener
   *  registration: if the view is disposed mid-round-trip, the arriving unlisten
   *  is called immediately instead of landing on a dead view (mirrors the
   *  `tasksview.ts` disposed-before-`listen`-resolves pattern). */
  private disposed = false;
  /** The query + options the current `searchGroups` were produced with. Replace
   *  applies from THIS, not the live inputs, so a query/option edit after a
   *  search can't make apply diverge from the preview. Nulled when the preview
   *  is invalidated (inputs changed) so a stale preview can't be applied. */
  private searchSnapshot: SearchParams | null = null;

  constructor(private host: FileEditHost) {
    this.el = el("div", "fileedit");
    this.el.hidden = true;
    this.el.tabIndex = -1;
    // Esc closes the OVERLAY. An embedded (pane-content) view has nothing to close
    // back to, so Esc is left alone there — closing the pane on a stray Escape would
    // be a nasty surprise with unsaved edits in the buffer.
    if (!host.embedded) {
      this.el.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          void this.requestClose();
        }
      });
    }

    // ---- header ----
    const head = el("div", "fileedit-head");

    const rootWrap = el("div", "fileedit-root");
    this.rootLabel = el("span", "fileedit-root-path", "(no folder)");
    const rootBtn = el("button", "pane-btn", "📁") as HTMLButtonElement;
    rootBtn.title = "Change root folder";
    rootBtn.addEventListener("click", () => void this.pickRoot());
    rootWrap.append(this.rootLabel, rootBtn);

    const spacer = el("div", "fileedit-spacer");

    this.fileLabel = el("span", "fileedit-file", "");
    this.dirtyDot = el("span", "fileedit-dirty", "●");
    this.dirtyDot.title = "Unsaved changes";
    this.dirtyDot.hidden = true;

    this.findBtn = el("button", "fileedit-save fileedit-find", "⌕ Find") as HTMLButtonElement;
    this.findBtn.title = "Find in this file — opens an overlay search bar";
    this.findBtn.hidden = true;
    this.findBtn.addEventListener("click", () => this.editor?.openFind());

    this.saveBtn = el("button", "fileedit-save", "Save") as HTMLButtonElement;
    this.saveBtn.title = "Save (Ctrl+S)";
    this.saveBtn.disabled = true;
    this.saveBtn.addEventListener("click", () => void this.save());

    const closeBtn = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    closeBtn.title = "Close (Esc)";
    closeBtn.hidden = !!host.embedded; // pane content — the PANE's ✕ is the close affordance
    closeBtn.addEventListener("click", () => void this.requestClose());

    head.append(rootWrap, spacer, this.fileLabel, this.dirtyDot, this.findBtn, this.saveBtn, closeBtn);

    // ---- agent-worktree banner (subtle, non-blocking) ----
    this.agentBanner = el(
      "div",
      "fileedit-banner",
      "Editing a running agent's worktree — the agent may also be writing here."
    );
    this.agentBanner.hidden = true;

    // ---- body ----
    const body = el("div", "fileedit-body");

    // Left column, top to bottom: the "Go to file" NAME filter (#214), then the
    // content search (#174/#207) which drives the in-tree hit highlighting, then
    // the tree itself. The two searches answer different questions — "where is the
    // file called X" vs "which files mention X" — and the issue asks for the first
    // one explicitly, so it goes first. While the name filter has a query, its
    // result list takes the tree's place; clearing it puts the tree back.
    this.treePane = el("div", "fileedit-tree-pane");
    this.treePane.style.width = `${this.storedTreeW()}px`;
    // Build the content-search form FIRST (it owns `igBox`, which the name filter
    // reads to pick its enumeration mode), but mount the name filter above it.
    const searchForm = el("div", "fileedit-search-form");
    this.buildSearchForm(searchForm);
    const gotoForm = el("div", "fileedit-goto-form");
    this.buildGotoForm(gotoForm);
    this.treeListEl = el("div", "fileedit-tree");
    this.gotoListEl = el("div", "fileedit-goto-list");
    this.gotoListEl.hidden = true;
    this.treePane.append(gotoForm, searchForm, this.treeListEl, this.gotoListEl);

    const divider = el("div", "fileedit-vdivider");
    this.wireTreeDivider(divider);

    this.editorPane = el("div", "fileedit-editor-pane");
    this.editorHost = el("div", "fileedit-editor-host");
    this.editorHost.addEventListener("keydown", (e) => {
      // Keep the editor's typing from firing terminal / app shortcuts (plan's
      // stopPropagation, mirroring auditview). Ctrl+S saves; Ctrl+F opens the
      // in-file find (WebView2 may still grab Ctrl+F — the Find button is the
      // reliable path, validated live).
      e.stopPropagation();
      const mod = e.ctrlKey || e.metaKey;
      if (mod && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void this.save();
      } else if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        this.editor?.openFind();
      }
    });
    this.emptyState = el("div", "fileedit-empty", "Select a file to edit.");
    this.editorPane.append(this.editorHost, this.emptyState);

    body.append(this.treePane, divider, this.editorPane);

    this.el.append(head, this.agentBanner, body);

    // One `ft-search` listener per view; it drops batches whose id isn't the
    // active session (`accept`), so cross-pane/stale events are harmless. If the
    // view is disposed before this IPC round-trip resolves, tear the listener
    // down immediately so it can't outlive the (removed) view.
    void onSearchBatch((b) => this.onSearchBatch(b)).then((un) => {
      if (this.disposed) un();
      else this.searchUnlisten = un;
    });
    // Same discipline for the `ft-files` enumeration stream (#214).
    void onFilesBatch((b) => this.onFilesBatch(b)).then((un) => {
      if (this.disposed) un();
      else this.filesUnlisten = un;
    });
  }

  // ---------- overlay contract ----------

  get visible(): boolean {
    return !this.el.hidden;
  }

  show(): void {
    this.el.hidden = false;
    // Re-root at the pane's current cwd unless the user picked a folder while it
    // was open. A null cwd (rare) shows the empty state.
    const cwd = this.host.getCwd();
    if (this.root === null && cwd) {
      this.root = cwd;
    }
    this.agentBanner.hidden = !(this.host.isAgentWorktree?.() ?? false);
    this.refreshRootLabel();
    if (this.root) {
      // Held so `openPath` (an editor pane opened ON a file, #217) can wait for the
      // root's first listing instead of racing it — reloadTree() replaces treeModel
      // wholesale, and a reveal that ran against the old one would expand nothing.
      this.treeLoad = this.reloadTree();
      void this.treeLoad;
    } else {
      this.treeListEl.replaceChildren(el("div", "fileedit-empty", "Pick a folder to browse."));
    }
    // No focus steal — the terminal below stays the primary input target until
    // the user clicks into the tree/editor.
  }

  /** Open `rel` (root-relative) and reveal it in the tree — the entry point for an
   *  editor PANE opened from the file browser's "Open in file editor pane" (#217),
   *  which creates the pane rooted at the browser's root with one file already up.
   *  Safe to call right after `show()`: it waits for that root's first listing.
   *
   *  A refused open (binary, too large, a dirty buffer the human declined to discard)
   *  leaves `openRel` where it was and is NOT followed by a reveal — same rule as the
   *  Go-to-file jump, so the tree never points at a file the editor didn't take. */
  async openPath(rel: string): Promise<void> {
    await this.treeLoad;
    if (this.disposed) return; // the pane was closed while the root's listing loaded
    await this.openFile(rel);
    if (this.disposed || this.openRel !== rel) return;
    await this.revealPath(rel);
  }

  /** The file currently open, root-relative — or null when none is. Captured into the
   *  persisted layout for an editor pane (#217), so a restore reopens the file the
   *  pane was showing rather than a bare tree with a title naming a file it never
   *  opened. Only the PATH: the buffer is deliberately never persisted (see
   *  panerestore.ts), and the file is re-read from disk on restore. */
  get openPathRel(): string | null {
    return this.openRel;
  }

  /** Unsaved edits in the buffer right now — asked WITHOUT prompting, unlike
   *  `canDiscard()`. The tab-close path needs to know whether a close would destroy
   *  work before it decides how to ask (tabbar's arm/confirm), and a question that
   *  itself pops a modal is no use there. */
  get dirty(): boolean {
    return this.isDirtyNow();
  }

  /** May this view be torn down right now — is there unsaved work in the buffer?
   *  True when it's clean, or when the human confirmed discarding it.
   *
   *  The host PANE calls this before closing an editor pane (#217): an editor pane is
   *  the pane kind where loomux itself owns an unsaved buffer, so a ✕ / dock-chip ✕ /
   *  Ctrl+Shift+W must not drop edits silently. Same guard `requestClose` applies to
   *  the overlay's own Esc/✕ — reached from the pane-close path too. */
  async canDiscard(): Promise<boolean> {
    // The same pure gate the view's own Esc/✕ uses, so "dirty means ask" is stated
    // once, in closeDecision, and cannot drift between the two.
    if (closeDecision(this.isDirtyNow()) === "close") return true;
    return this.confirmDiscard();
  }

  hide(): void {
    this.el.hidden = true;
    // Don't let a queued keystroke start a *new* search once the overlay is
    // hidden. A search already in flight is deliberately left to finish — its
    // worker frees its own registry entry and its results are ready on reopen;
    // its batches paint into the hidden tree harmlessly.
    clearTimeout(this.searchTimer);
  }

  dispose(): void {
    this.disposed = true;
    clearTimeout(this.searchTimer);
    if (this.session.activeId !== null) void ftSearchCancel(this.session.activeId);
    // An enumeration outliving its view would walk a whole repo for nobody (#214).
    // Nulling the id also makes any batch that lands before the unlisten resolves a
    // no-op via the id-mismatch guard — same reason `session` goes idle below.
    if (this.fileListId !== null) void ftSearchCancel(this.fileListId);
    this.fileListId = null;
    // Go idle so any batch that arrives before the listener is torn down (or an
    // unlisten that hasn't resolved yet) can't drive a removed view.
    this.session = idle();
    this.searchUnlisten?.();
    this.filesUnlisten?.();
    this.editor?.dispose();
    this.el.remove();
  }


  // ---------- root / tree ----------

  private refreshRootLabel(): void {
    this.rootLabel.textContent = this.root ? shortenPath(this.root) : "(no folder)";
    this.rootLabel.title = this.root ?? "";
  }

  private async pickRoot(): Promise<void> {
    const picked = await open({ directory: true, title: "Browse folder", defaultPath: this.root ?? undefined });
    if (typeof picked !== "string" || picked === this.root) return;

    // A RE-ROOT ABANDONS THE OPEN FILE, and it must say so out loud.
    //
    // `openRel` is a path *relative to the root*. Carry it across a re-root and it
    // silently re-binds to a different file: with `notes.md` open under C:\A and the
    // root moved to C:\B, a Ctrl+S writes A's buffer to `C:\B\notes.md` — the hash
    // guard fires against a file the user never opened, and "Overwrite" then destroys
    // an unrelated project's file with this one's contents. (The same trap sat in the
    // Alt+F overlay; #217 makes it far easier to reach, because a re-root is now a
    // first-class, persisted operation on an editor pane.)
    //
    // So: ask about unsaved edits FIRST — the human may want to save into the old
    // root, and cancelling here must leave everything exactly as it was — then close
    // the buffer and drop the search state, whose results are paths under a root that
    // is no longer on screen.
    if (!(await this.canDiscard())) return;
    this.root = picked;
    this.closeOpenFile();
    this.clearSearch();
    this.refreshRootLabel();
    // An editor PANE (#217) adopts the new root as ITS root — title and persisted
    // layout follow the tree the user is actually looking at. An overlay host does
    // not implement this: there the root stays view-local by design.
    this.host.onRootChanged?.(picked);
    this.treeLoad = this.reloadTree();
    await this.treeLoad;
  }

  /** Drop the open file: no buffer, no `openRel`, no stale hash — back to the empty
   *  state. Used when the root moves out from under the file (`pickRoot`), where
   *  keeping any of it would leave a save aimed at the wrong path. */
  private closeOpenFile(): void {
    this.openRel = null;
    this.savedContent = "";
    this.savedHash = "";
    this.openEol = "\n";
    this.editor?.setValue("", "");
    this.editorHost.hidden = true;
    this.emptyState.hidden = false;
    this.findBtn.hidden = true;
    this.updateFileLabel();
    this.updateDirty();
  }

  /** (Re)load the root directory into a fresh model and render. */
  private async reloadTree(): Promise<void> {
    if (!this.root) return;
    this.treeModel = makeRoot();
    void this.refreshGitStatus(); // gates the ignore toggle; independent of the tree load
    // The cached path list belongs to the OLD root — drop it, or the name filter
    // would happily offer files that aren't under this tree anymore (#214).
    this.invalidateFileList();
    await this.loadDir(this.treeModel);
    this.renderTree();
  }

  /** Probe whether the root is inside a git work tree and update the ignore
   *  toggle accordingly (a non-git root has no `.gitignore` to respect, so the
   *  toggle is disabled with an explanatory tooltip and the full tree is always
   *  searched). */
  private async refreshGitStatus(): Promise<void> {
    const root = this.root;
    if (!root) {
      this.isGitRoot = null;
      this.updateIgnoreToggle();
      return;
    }
    let git: boolean;
    try {
      git = (await gitRepoRoot(root)) !== null;
    } catch {
      git = false;
    }
    // Discard a probe that resolved after the root changed — assigning its value
    // (even out of order vs a newer probe) would leave `isGitRoot` describing the
    // wrong folder, which a later toggle refresh would then read.
    if (root !== this.root) return;
    this.isGitRoot = git;
    this.updateIgnoreToggle();
  }

  private updateIgnoreToggle(): void {
    const git = this.isGitRoot === true;
    this.igBox.disabled = !git;
    if (!git && this.igBox.checked) this.igBox.checked = false; // no meaning off-git
    this.igLabel.classList.toggle("disabled", !git);
    // Describe the *effective* enumeration (the same choice the backend makes).
    const source = enumerationSource(git, this.igBox.checked);
    this.igLabel.title = !git
      ? "Not a git repository — nothing to ignore; the whole folder is searched."
      : source === "git"
        ? "Respecting .gitignore (node_modules, build output skipped). Check to include ignored files."
        : "Including git-ignored files. Uncheck to respect .gitignore.";
  }

  /** Fetch one directory's children (lazy), merging so expansion survives. */
  private async loadDir(node: TreeNode): Promise<void> {
    if (!this.root) return;
    try {
      const entries = await ftListDir(this.root, node.path);
      node.children = mergeChildren(node.children, node.path, entries);
      node.loaded = true;
    } catch (err) {
      showToast(`Cannot read folder: ${errorMessage(err)}`);
    }
  }

  private renderTree(): void {
    const rows = flatten(this.treeModel);
    // rel → group, built once per render so hit rows are an O(1) lookup instead
    // of an O(files) scan each — the difference between fluid and janky when a
    // big streamed search lights up thousands of files (issue #207).
    const groupByRel = new Map(this.searchGroups.map((g) => [g.rel, g]));
    const frag = document.createDocumentFragment();
    for (const { node, depth } of rows) {
      const row = el("div", "fileedit-row");
      if (node.path === this.openRel) row.classList.add("open");
      row.style.paddingLeft = `${8 + depth * 14}px`;
      const icon = el("span", "fileedit-icon");
      icon.innerHTML = node.isDir ? folderIconSvg(node.expanded) : fileIconSvg(node.name);
      const name = el("span", "fileedit-name", node.name);
      if (node.isSymlink) {
        row.classList.add("symlink");
        name.title = "symlink (not followed)";
      }
      row.append(icon, name);
      // Search-hit highlight + count badge (demo feedback #3): a file with
      // matches gets a highlight and a clickable count that toggles whether it's
      // included in a replace.
      const count = node.isDir ? undefined : this.hits.get(node.path);
      if (count !== undefined) {
        row.classList.add("hit");
        const group = groupByRel.get(node.path);
        const selected = group?.selected ?? true;
        const badge = el("span", `fileedit-hit-badge${selected ? "" : " off"}`, String(count));
        badge.title = selected ? "In replace set — click to exclude" : "Excluded from replace — click to include";
        badge.addEventListener("click", (e) => {
          e.stopPropagation();
          this.searchGroups = toggleFile(this.searchGroups, node.path);
          this.updateReplaceBtn();
          this.renderTree();
        });
        if (!selected) row.classList.add("hit-off");
        row.append(badge);
      }
      row.addEventListener("click", () => void this.onRowClick(node));
      frag.appendChild(row);
    }
    this.treeListEl.replaceChildren(frag);
  }

  private async onRowClick(node: TreeNode): Promise<void> {
    if (node.isSymlink) return; // shown but never followed
    if (node.isDir) {
      node.expanded = !node.expanded;
      if (node.expanded && !node.loaded) await this.loadDir(node);
      this.renderTree();
    } else {
      // Opening a file with hits jumps to its first match (demo feedback #3).
      const first = firstMatch(this.searchGroups, node.path);
      await this.openFile(node.path, first?.line, first?.col);
    }
  }

  // ---------- open / save ----------

  private async openFile(rel: string, line?: number, col?: number): Promise<void> {
    if (this.openRel !== null && this.isDirtyNow()) {
      const ok = await this.confirmDiscard();
      if (!ok) return;
    }
    if (!this.root) return;
    try {
      const fr = await ftReadFile(this.root, rel);
      this.openRel = rel;
      this.savedContent = fr.content;
      this.savedHash = fr.hash;
      this.openEol = detectEol(fr.content);
      await this.mountEditor(fr.content, rel);
      this.emptyState.hidden = true;
      this.editorHost.hidden = false;
      this.findBtn.hidden = false;
      this.updateFileLabel();
      this.updateDirty();
      this.renderTree(); // reflect the .open highlight
      this.applyEditorHighlight(); // light up the project-search matches in-file
      if (line !== undefined) this.editor?.reveal(line, col);
    } catch (err) {
      this.explainOpenError(err);
    }
  }

  private async mountEditor(content: string, filename: string): Promise<void> {
    if (this.editor) {
      this.editor.setValue(content, filename);
    } else {
      this.editor = await createEditor(this.editorHost, content, filename);
      this.editor.onChange(() => this.updateDirty());
    }
  }

  private async save(): Promise<void> {
    if (!this.editor || this.openRel === null || !this.root) return;
    // Write with the file's original line ending; the editor works in LF.
    const content = applyEol(this.editor.getValue(), this.openEol);
    try {
      const res = await ftWriteFile(this.root, this.openRel, content, this.savedHash || null);
      this.savedContent = content;
      this.savedHash = res.hash;
      this.updateDirty();
      showToast("Saved");
    } catch (err) {
      if (errorCode(err) === "conflict") {
        await this.resolveConflict(content);
      } else {
        showToast(`Save failed: ${errorMessage(err)}`);
      }
    }
  }

  /** The file changed on disk since it was opened. Offer overwrite / reload /
   *  cancel; act on the choice. */
  private async resolveConflict(pendingContent: string): Promise<void> {
    if (!this.root || this.openRel === null) return;
    const choice = await this.conflictDialog();
    if (choice === "cancel") return;
    if (choice === "reload") {
      try {
        const fr = await ftReadFile(this.root, this.openRel);
        this.savedContent = fr.content;
        this.savedHash = fr.hash;
        this.openEol = detectEol(fr.content);
        this.editor?.setValue(fr.content, this.openRel);
        this.updateDirty();
      } catch (err) {
        showToast(`Reload failed: ${errorMessage(err)}`);
      }
      return;
    }
    // overwrite: write without an expected hash to bypass the guard.
    try {
      const res = await ftWriteFile(this.root, this.openRel, pendingContent, null);
      this.savedContent = pendingContent;
      this.savedHash = res.hash;
      this.updateDirty();
      showToast("Overwrote on-disk changes");
    } catch (err) {
      showToast(`Save failed: ${errorMessage(err)}`);
    }
  }

  // ---------- dirty tracking ----------

  private isDirtyNow(): boolean {
    return this.editor !== null && textDiffers(this.savedContent, this.editor.getValue());
  }

  private updateDirty(): void {
    const dirty = this.isDirtyNow();
    this.dirtyDot.hidden = !dirty;
    this.saveBtn.disabled = !dirty;
  }

  private updateFileLabel(): void {
    this.fileLabel.textContent = this.openRel ?? "";
    this.fileLabel.title = this.openRel ?? "";
  }

  private async requestClose(): Promise<void> {
    if (this.isDirtyNow() && closeDecision(true) === "confirm") {
      const ok = await this.confirmDiscard();
      if (!ok) return;
    }
    this.host.onClose();
  }

  private explainOpenError(err: unknown): void {
    const code = errorCode(err);
    const msg =
      code === "binary"
        ? "This file isn't text and can't be edited here."
        : code === "too-large"
          ? "This file is too large to open in the editor."
          : `Cannot open file: ${errorMessage(err)}`;
    showToast(msg);
  }

  // ---------- dialogs ----------

  private confirmDiscard(): Promise<boolean> {
    return modal<boolean>((resolve) => ({
      title: "Discard unsaved changes?",
      body: `${this.openRel ?? "This file"} has unsaved edits.`,
      buttons: [
        { label: "Cancel", value: false },
        { label: "Discard", value: true, kind: "danger" },
      ],
      onKey: (k) => (k === "Escape" ? resolve(false) : undefined),
    }));
  }

  private conflictDialog(): Promise<ConflictChoice> {
    return modal<ConflictChoice>((resolve) => ({
      title: "File changed on disk",
      body: `${this.openRel ?? "This file"} was modified since you opened it (by an agent, another tool, or git). Overwrite it with your version, reload the on-disk version (losing your edits), or cancel?`,
      buttons: [
        { label: "Cancel", value: "cancel" },
        { label: "Reload", value: "reload" },
        { label: "Overwrite", value: "overwrite", kind: "danger" },
      ],
      onKey: (k) => (k === "Escape" ? resolve("cancel") : undefined),
    }));
  }

  // ---------- tree width divider ----------

  private storedTreeW(): number {
    const raw = Number(localStorage.getItem(TREE_W_KEY));
    if (!Number.isFinite(raw) || raw <= 0) return DEFAULT_TREE_W;
    return Math.max(MIN_TREE_W, Math.min(MAX_TREE_W, raw));
  }

  private wireTreeDivider(divider: HTMLElement): void {
    divider.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const startX = e.clientX;
      const startW = this.treePane.offsetWidth;
      divider.classList.add("dragging");
      const move = (ev: MouseEvent) => {
        const w = Math.max(MIN_TREE_W, Math.min(MAX_TREE_W, startW + (ev.clientX - startX)));
        this.treePane.style.width = `${w}px`;
      };
      const up = () => {
        divider.classList.remove("dragging");
        localStorage.setItem(TREE_W_KEY, String(this.treePane.offsetWidth));
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    });
  }

  // ---------- "Go to file": the file-NAME search (issue #214) ----------
  //
  // The owner's ask, verbatim: "an optimized and fast file search. It does not need
  // to search into files as we already have the file editor pane that can do that."
  // So: paths only, never contents. The speed comes from the split — enumerate once
  // (backend, off-thread, no file opened), then filter in memory per keystroke. The
  // ranking is pure and lives in filematch.ts (unit-tested); this is the wiring.

  private buildGotoForm(form: HTMLElement): void {
    const row = el("div", "fileedit-search-row");
    this.gotoInput = document.createElement("input");
    this.gotoInput.className = "fileedit-search-input";
    this.gotoInput.placeholder = "Go to file… (by name)";
    this.gotoInput.title =
      "Find a file by NAME (not contents — that's the search below). Space-separated terms must all match. ↑/↓ to pick, Enter to open, Esc to clear.";
    this.gotoInput.spellcheck = false;
    // Index on FOCUS, not on construction: a pane that never uses the box never
    // pays for the walk, and by the time the second character is typed the list is
    // usually already in.
    this.gotoInput.addEventListener("focus", () => this.ensureFileList());
    this.gotoInput.addEventListener("input", () => {
      this.ensureFileList();
      this.gotoSel = 0; // a new query invalidates the old selection
      this.refreshGoto();
    });
    this.gotoInput.addEventListener("keydown", (e) => this.onGotoKey(e));
    this.gotoSummaryEl = el("span", "fileedit-goto-summary", "");
    row.append(this.gotoInput);
    form.append(row, this.gotoSummaryEl);
  }

  private onGotoKey(e: KeyboardEvent): void {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault(); // don't move the text caret
      e.stopPropagation();
      this.gotoSel = moveSelection(this.gotoSel, e.key === "ArrowDown" ? 1 : -1, this.gotoHits.length);
      this.renderGoto();
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      void this.openGotoHit(this.gotoSel);
      return;
    }
    if (e.key === "Escape") {
      // A query in the box: Esc clears it (back to the tree) and is CONSUMED, so a
      // single press doesn't also close the overlay out from under the user. Empty
      // box: let it bubble — Esc then means what it always meant (close the overlay).
      if (this.gotoInput.value !== "") {
        e.stopPropagation();
        this.clearGoto();
      }
      return;
    }
    e.stopPropagation(); // ordinary typing must not reach the terminal / app shortcuts
  }

  /** The enumeration mode the name filter runs in — the SAME toggle the content
   *  search uses, so "Ignored files" means one thing in this view, not two. */
  private includeIgnored(): boolean {
    return this.igBox.checked && !this.igBox.disabled;
  }

  /** Start indexing the root's paths, unless it's already done or in flight. Cheap
   *  to call on every keystroke — the guards make repeats free. */
  private ensureFileList(): void {
    if (!this.root || this.fileListLoaded || this.fileListId !== null) return;
    const id = nextSearchId();
    this.fileListId = id;
    this.fileList = [];
    this.fileListTruncated = false;
    this.updateGotoSummary();
    void ftFilesStart(id, this.root, this.includeIgnored());
  }

  /** Drop the cached path list (the root or the ignore mode changed) and cancel any
   *  enumeration still running for it. Re-indexes immediately if the box is in use,
   *  so the user is never left filtering a list that describes a different tree. */
  private invalidateFileList(): void {
    if (this.fileListId !== null) {
      void ftSearchCancel(this.fileListId); // one registry serves both streams
      this.fileListId = null;
    }
    this.fileList = [];
    this.fileListLoaded = false;
    this.fileListTruncated = false;
    if (this.gotoInput.value.trim()) {
      this.ensureFileList();
      this.refreshGoto();
    } else {
      this.updateGotoSummary();
    }
  }

  /** Fold one streamed batch of paths in. Batches from a superseded/cancelled
   *  enumeration are dropped by id, so this is safe to call for every event. */
  private onFilesBatch(b: FilesBatch): void {
    if (this.disposed) return;
    if (b.id !== this.fileListId) return; // stale / cancelled — ignore
    if (b.error) showToast(`Couldn't list files: ${errorMessage(b.error)}`);
    this.fileList.push(...b.files);
    if (b.truncated) this.fileListTruncated = true;
    if (b.done) {
      this.fileListId = null;
      // Loaded even on error — the toast above already said what went wrong, and
      // leaving it un-loaded would restart the walk (and re-toast) on every
      // subsequent keystroke. A root change or an ignore-toggle flip invalidates and
      // retries, which is the recovery path for the realistic failure (root is gone).
      this.fileListLoaded = true;
      this.refreshGoto();
    } else {
      // Results improve as paths arrive — repaint, but at most once a frame so a
      // burst of 1000-path batches can't thrash the DOM.
      if (this.gotoRenderScheduled) return;
      this.gotoRenderScheduled = true;
      requestAnimationFrame(() => {
        this.gotoRenderScheduled = false;
        if (!this.disposed) this.refreshGoto();
      });
    }
  }

  /** Re-rank and repaint. With a query the result list REPLACES the tree; with an
   *  empty one the tree comes back — the box is a filter over the tree, not a
   *  separate mode you have to leave. */
  private refreshGoto(): void {
    const active = queryTerms(this.gotoInput.value).length > 0;
    this.gotoHits = active ? rankFileNames(this.fileList, this.gotoInput.value, GOTO_RESULT_CAP) : [];
    this.gotoSel = Math.min(this.gotoSel, Math.max(0, this.gotoHits.length - 1));
    this.gotoListEl.hidden = !active;
    this.treeListEl.hidden = active;
    this.renderGoto();
    this.updateGotoSummary();
  }

  private clearGoto(): void {
    this.gotoInput.value = "";
    this.gotoSel = 0;
    this.refreshGoto();
  }

  private renderGoto(): void {
    if (this.gotoListEl.hidden) {
      this.gotoListEl.replaceChildren();
      return;
    }
    if (this.gotoHits.length === 0) {
      const msg = this.fileListId !== null ? "Indexing…" : "No file matches.";
      this.gotoListEl.replaceChildren(el("div", "fileedit-empty", msg));
      return;
    }
    const frag = document.createDocumentFragment();
    this.gotoHits.forEach((hit, i) => {
      const base = basenameStart(hit.rel);
      const row = el("div", "fileedit-goto-row");
      if (i === this.gotoSel) row.classList.add("sel");
      const icon = el("span", "fileedit-icon");
      icon.innerHTML = fileIconSvg(hit.rel.slice(base));
      // Name first, then the dim directory — the quick-open convention: you scan the
      // names, and the path only disambiguates same-named files.
      const name = el("span", "fileedit-goto-name");
      name.append(markUp(hit.rel.slice(base), clipRanges(hit.ranges, base, hit.rel.length)));
      const dir = el("span", "fileedit-goto-dir");
      dir.append(markUp(hit.rel.slice(0, base), clipRanges(hit.ranges, 0, base)));
      row.title = hit.rel;
      row.append(icon, name, dir);
      row.addEventListener("click", () => void this.openGotoHit(i));
      frag.appendChild(row);
    });
    this.gotoListEl.replaceChildren(frag);
    this.gotoListEl.querySelector(".fileedit-goto-row.sel")?.scrollIntoView({ block: "nearest" });
  }

  private updateGotoSummary(): void {
    const s = this.gotoSummaryEl;
    const indexing = this.fileListId !== null;
    const active = queryTerms(this.gotoInput.value).length > 0;
    if (!active) {
      // Idle: say nothing while nothing is indexed, so the box doesn't nag. Once a
      // list exists, its size is genuinely useful ("this root has 3,412 files").
      s.textContent = indexing
        ? `Indexing… ${this.fileList.length}`
        : this.fileListLoaded
          ? `${this.fileList.length} files${this.fileListTruncated ? " (truncated)" : ""}`
          : "";
      s.classList.toggle("truncated", this.fileListTruncated);
      return;
    }
    const capped = this.gotoHits.length >= GOTO_RESULT_CAP;
    const n = this.gotoHits.length;
    s.textContent = indexing
      ? `Indexing… ${n} of ${this.fileList.length} so far`
      : n === 0
        ? "No file matches"
        : `${n}${capped ? "+" : ""} of ${this.fileList.length} files${this.fileListTruncated ? " (truncated)" : ""}`;
    // Flag anything the user is NOT seeing: a capped result list or an incomplete
    // index. A silent cut would read as "that file doesn't exist" (house rule).
    s.classList.toggle("truncated", capped || this.fileListTruncated);
  }

  /** Open the selected result: the file lands in the editor, the filter clears, and
   *  the tree comes back with that file revealed and highlighted where it lives —
   *  so "jump to a file" leaves you oriented, not in a detached result list. */
  private async openGotoHit(i: number): Promise<void> {
    const hit = this.gotoHits[i];
    if (!hit) return;
    const rel = hit.rel;
    await this.openFile(rel);
    // The open can decline: a binary/oversize file toasts, and a dirty buffer whose
    // discard the human refuses leaves `openRel` where it was. Only THEN clear the
    // query and follow the tree — bailing here keeps the user's typed filter intact
    // so they can pick a different file instead of retyping it.
    if (this.openRel !== rel) return;
    this.clearGoto();
    await this.revealPath(rel);
  }

  /** Expand every directory on the way to `rel` so an opened file is visible in the
   *  tree. Shallow → deep, so each parent is loaded before its child is needed —
   *  the single-path twin of `revealHits`. */
  private async revealPath(rel: string): Promise<void> {
    for (const path of ancestorDirs(rel)) {
      const node = findNode(this.treeModel, path);
      if (!node || !node.isDir) continue;
      if (!node.loaded) await this.loadDir(node);
      node.expanded = true;
    }
    this.renderTree();
  }

  // ---------- search + replace (drives the in-tree hit highlighting) ----------

  private buildSearchForm(form: HTMLElement): void {
    const searchRow = el("div", "fileedit-search-row");
    this.searchInput = document.createElement("input");
    this.searchInput.className = "fileedit-search-input";
    this.searchInput.placeholder = "Search in files…";
    this.searchInput.spellcheck = false;
    this.searchInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.stopPropagation();
        clearTimeout(this.searchTimer);
        this.startSearch();
      } else if (e.key === "Escape") {
        if (isSearching(this.session)) {
          // A search is actively running: Esc cancels it (keeping the partial
          // results) and is consumed here so it doesn't also close the overlay.
          e.stopPropagation();
          clearTimeout(this.searchTimer);
          this.cancelSearch();
        }
        // Nothing running (idle *or* a finished search): let Escape bubble to the
        // overlay's own handler (`this.el` keydown → requestClose) so a single
        // press closes the editor.
      } else {
        e.stopPropagation();
      }
    });
    // Live highlight: debounce a search as the user types so tree hits update
    // without a full-tree walk on every keystroke. Editing also invalidates the
    // current preview immediately so a stale replace can't be applied (finding #1).
    this.searchInput.addEventListener("input", () => {
      this.invalidateIfStale();
      this.scheduleSearch();
    });
    searchRow.append(this.searchInput);

    const replaceRow = el("div", "fileedit-search-row");
    this.replaceInput = document.createElement("input");
    this.replaceInput.className = "fileedit-search-input";
    this.replaceInput.placeholder = "Replace with…";
    this.replaceInput.spellcheck = false;
    this.replaceInput.addEventListener("keydown", (e) => e.stopPropagation());
    this.replaceBtn = el("button", "fileedit-save", "Replace") as HTMLButtonElement;
    this.replaceBtn.disabled = true;
    this.replaceBtn.addEventListener("click", () => void this.runReplace());
    replaceRow.append(this.replaceInput, this.replaceBtn);

    const opts = el("div", "fileedit-search-opts");
    const [ciLabel, ciBox] = checkboxLabel("Ignore case");
    const [wwLabel, wwBox] = checkboxLabel("Whole word");
    // The gitignore toggle (issue #207): off by default (searches respect
    // .gitignore); on includes ignored files (node_modules, build output).
    const [igLabel, igBox] = checkboxLabel("Ignored files");
    this.ciBox = ciBox;
    this.wwBox = wwBox;
    this.igBox = igBox;
    this.igLabel = igLabel;
    const onOpt = () => {
      this.invalidateIfStale();
      this.scheduleSearch();
    };
    ciBox.addEventListener("change", onOpt);
    wwBox.addEventListener("change", onOpt);
    igBox.addEventListener("change", () => {
      this.updateIgnoreToggle(); // refresh the effective-mode tooltip
      // The toggle governs the name filter's enumeration too — same meaning, same
      // backend `plan_enumeration` — so its cached list is now stale (#214).
      this.invalidateFileList();
      onOpt();
    });
    this.summaryEl = el("span", "fileedit-search-summary", "");
    opts.append(ciLabel, wwLabel, igLabel, this.summaryEl);

    form.append(searchRow, replaceRow, opts);
  }

  /** The live search parameters from the inputs. A disabled ignore toggle
   *  (non-git root) always reads as false, so `include_ignored` never sneaks on
   *  where it has no meaning. */
  private currentParams(): SearchParams {
    return {
      query: this.searchInput.value,
      caseInsensitive: this.ciBox.checked,
      wholeWord: this.wwBox.checked,
      includeIgnored: this.igBox.checked && !this.igBox.disabled,
    };
  }

  private static paramsToOpts(p: SearchParams): SearchOpts {
    return {
      case_insensitive: p.caseInsensitive,
      whole_word: p.wholeWord,
      // One past what the UI will render: enough to detect+flag overflow, but the
      // walk stops there instead of scanning thousands of matches the UI drops.
      max_results: RENDER_CAP + 1,
      include_ignored: p.includeIgnored,
    };
  }

  private scheduleSearch(): void {
    clearTimeout(this.searchTimer);
    this.searchTimer = window.setTimeout(() => this.startSearch(), SEARCH_DEBOUNCE_MS);
  }

  /** If the live inputs no longer match the snapshot the current hits were
   *  produced with, drop the preview so a stale replace can't apply (finding #1
   *  from the prior review) — the debounced search re-establishes it. */
  private invalidateIfStale(): void {
    if (this.searchSnapshot && !paramsEqual(this.searchSnapshot, this.currentParams())) {
      this.searchSnapshot = null;
      this.updateReplaceBtn();
    }
  }

  /** Launch a streaming search (issue #207). Cancels any in-flight one, opens a
   *  fresh session under a new id, and kicks the backend walk off-thread — results
   *  arrive via `onSearchBatch`. Non-blocking: this returns immediately and the UI
   *  stays live (Esc/typing keep working) while the walk runs. */
  private startSearch(): void {
    const params = this.currentParams();
    // Cancel whatever's running; its late batches will be dropped by id anyway,
    // but telling the backend stops a big walk from grinding on in the background.
    if (this.session.activeId !== null) void ftSearchCancel(this.session.activeId);
    if (!this.root || params.query === "") {
      this.clearSearch();
      return;
    }
    const id = nextSearchId();
    this.session = begin(id);
    this.pendingParams = params;
    // The preview is invalid until this search *finishes*: a replace mustn't apply
    // against a partial (or about-to-be-superseded) result set.
    this.searchGroups = [];
    this.hits = new Map();
    this.searchSnapshot = null;
    this.updateReplaceBtn();
    this.summaryEl.textContent = "Searching…";
    this.summaryEl.classList.remove("truncated");
    this.renderTree();
    void ftSearchStart(id, this.root, params.query, FileEditView.paramsToOpts(params));
  }

  /** Fold one streamed batch into the session and reflect it in the UI. Batches
   *  from a superseded/cancelled search are dropped by `accept` (id mismatch), so
   *  this is safe to call for every event regardless of which search it belongs
   *  to. Live batches update the tree (throttled) and the running count; the
   *  terminal `done` batch finalizes the preview + reveal. */
  private onSearchBatch(b: SearchBatch): void {
    if (this.disposed) return; // view is gone; never touch its DOM
    if (b.id !== this.session.activeId) return; // stale / cancelled — ignore
    if (b.error && errorCode(b.error) !== "empty-query") {
      showToast(`Search failed: ${errorMessage(b.error)}`);
    }
    this.session = accept(this.session, b);
    this.searchGroups = groupMatches(this.session.matches);
    this.hits = hitCounts(this.searchGroups);
    if (this.session.done) {
      // Promote the snapshot only now the full result set is in — replace applies
      // from this, exactly what was previewed (the preview→apply guarantee).
      this.searchSnapshot = this.pendingParams;
      void this.finishSearch();
    } else {
      this.updateLiveSummary();
      this.scheduleRender();
    }
  }

  /** Finalize a completed search: expand the branches leading to hits, paint, and
   *  light up the matches inside any open file. Guarded so a search that gets
   *  superseded *during* the async reveal doesn't paint stale results. */
  private async finishSearch(): Promise<void> {
    const id = this.session.activeId;
    this.updateSearchSummary(isTruncated(this.session));
    await this.revealHits();
    if (id !== this.session.activeId) return; // superseded mid-reveal
    this.renderTree();
    this.updateReplaceBtn();
    this.applyEditorHighlight();
  }

  /** Cancel the in-flight search but keep whatever was found so far, freezing it
   *  as the (non-replaceable) result set. Going idle-but-done means no later batch
   *  can land, and no partial preview enables replace. */
  private cancelSearch(): void {
    if (this.session.activeId === null) return;
    void ftSearchCancel(this.session.activeId);
    this.session = { ...this.session, activeId: null, done: true };
    this.updateSearchSummary(isTruncated(this.session));
    this.renderTree();
  }

  /** Coalesce mid-search repaints to one per frame. */
  private scheduleRender(): void {
    if (this.renderScheduled) return;
    this.renderScheduled = true;
    requestAnimationFrame(() => {
      this.renderScheduled = false;
      this.renderTree();
    });
  }

  private updateLiveSummary(): void {
    const { files, matches } = countSummary(this.searchGroups);
    this.summaryEl.textContent = `Searching… ${matches} in ${files} file${files === 1 ? "" : "s"}`;
    this.summaryEl.classList.remove("truncated");
  }

  private clearSearch(): void {
    if (this.session.activeId !== null) void ftSearchCancel(this.session.activeId);
    this.session = idle();
    this.pendingParams = null;
    this.searchGroups = [];
    this.hits = new Map();
    this.searchSnapshot = null;
    this.summaryEl.textContent = "";
    this.summaryEl.classList.remove("truncated");
    this.updateReplaceBtn();
    this.renderTree();
    this.applyEditorHighlight(); // clears the in-file highlight
  }

  /** Push the active project-search query into the open editor so its matches
   *  are highlighted inside the file (demo feedback #4). No active search →
   *  clears the highlight. The in-file Find button (CM6 overlay) uses the same
   *  query state, so it opens pre-filled. */
  private applyEditorHighlight(): void {
    if (!this.editor) return;
    const s = this.searchSnapshot;
    if (s && s.query) {
      this.editor.setHighlightQuery(s.query, s.caseInsensitive, s.wholeWord);
    } else {
      this.editor.setHighlightQuery("", false, false);
    }
  }

  private updateSearchSummary(truncated: boolean): void {
    const { files, matches } = countSummary(this.searchGroups);
    this.summaryEl.textContent =
      matches === 0
        ? "No matches"
        : `${matches} in ${files} file${files === 1 ? "" : "s"}${truncated ? " (truncated)" : ""}`;
    this.summaryEl.classList.toggle("truncated", truncated);
  }

  /** Load + expand every directory on the path to a hit, so highlighted files
   *  aren't hidden inside collapsed folders. Shallow → deep so parents load
   *  before their children are needed. */
  private async revealHits(): Promise<void> {
    const dirs = new Set<string>();
    for (const g of this.searchGroups) for (const d of ancestorDirs(g.rel)) dirs.add(d);
    // Shallowest first (fewest separators) so each parent is loaded in turn.
    const ordered = [...dirs].sort((a, b) => a.split("/").length - b.split("/").length);
    for (const path of ordered) {
      const node = findNode(this.treeModel, path);
      if (!node || !node.isDir) continue;
      if (!node.loaded) await this.loadDir(node);
      node.expanded = true;
    }
  }

  private updateReplaceBtn(): void {
    // Label stays a plain "Replace" (no count — it was wrapping); the count lives
    // in the summary line and the per-file badges instead.
    const n = this.searchSnapshot ? selectedMatchCount(this.searchGroups) : 0;
    this.replaceBtn.disabled = n === 0;
    this.replaceBtn.textContent = "Replace";
  }

  private async runReplace(): Promise<void> {
    if (!this.root) return;
    // Apply from the snapshot the preview was built with — NOT the live inputs —
    // so what's applied is exactly what was previewed (finding #1). A null
    // snapshot means the preview was invalidated; require a fresh search.
    const snap = this.searchSnapshot;
    // Belt-and-braces on top of the search seq guard: never apply unless the
    // snapshot still matches the live inputs, so a stale resolution or an untimed
    // edit can't replace a query the preview never showed. (The `!snap` also
    // narrows the type for the `snap.query` use below.)
    if (!snap || !replaceIsCurrent(snap, this.currentParams())) {
      showToast("Search again before replacing.", "info");
      return;
    }
    const files = selectedFiles(this.searchGroups);
    if (files.length === 0) return;
    const replacement = this.replaceInput.value;
    const ok = await this.confirmReplace(selectedMatchCount(this.searchGroups), files.length);
    if (!ok) return;
    try {
      const res = await ftReplace(
        this.root,
        snap.query,
        replacement,
        files,
        FileEditView.paramsToOpts(snap)
      );
      const changedMatches = res.changed.reduce((s, c) => s + c.replacements, 0);
      let msg = `Replaced ${changedMatches} in ${res.changed.length} file${res.changed.length === 1 ? "" : "s"}`;
      if (res.skipped.length > 0) msg += `, skipped ${res.skipped.length}`;
      showToast(msg, "info");
      // If the open file was among those changed, reload it so the buffer isn't
      // stale against disk.
      if (this.openRel && res.changed.some((c) => c.rel === this.openRel)) {
        await this.reloadOpenFile();
      }
      this.startSearch(); // re-run against the new contents (streams in)
    } catch (err) {
      showToast(`Replace failed: ${errorMessage(err)}`);
    }
  }

  private async reloadOpenFile(): Promise<void> {
    if (!this.root || this.openRel === null) return;
    // Don't silently discard unsaved edits (finding #2): if the open buffer is
    // dirty, confirm before overwriting it with the just-replaced disk content.
    // Declining keeps the user's edits — the on-disk hash now differs, so the
    // next save hits the conflict guard rather than losing anything silently.
    if (this.isDirtyNow()) {
      const discard = await this.confirmDiscard();
      if (!discard) {
        showToast("Kept your unsaved edits — saving will flag the on-disk change.", "info");
        return;
      }
    }
    try {
      const fr = await ftReadFile(this.root, this.openRel);
      this.savedContent = fr.content;
      this.savedHash = fr.hash;
      this.openEol = detectEol(fr.content);
      this.editor?.setValue(fr.content, this.openRel);
      this.updateDirty();
    } catch {
      /* file may have been removed; leave the buffer as-is */
    }
  }

  private confirmReplace(matches: number, files: number): Promise<boolean> {
    return modal<boolean>((resolve) => ({
      title: "Replace across files?",
      body: `Replace ${matches} occurrence${matches === 1 ? "" : "s"} in ${files} file${files === 1 ? "" : "s"}. Each file is written atomically; this can't be undone from here.`,
      buttons: [
        { label: "Cancel", value: false },
        { label: "Replace", value: true, kind: "primary" },
      ],
      onKey: (k) => (k === "Escape" ? resolve(false) : undefined),
    }));
  }
}

// ---------- helpers ----------

/** The parts of `ranges` (which index into the whole path) that fall inside
 *  `[from, to)`, rebased to `from` — so a path's highlight spans can be painted
 *  separately onto its name and its directory parts (#214). */
function clipRanges(
  ranges: readonly [number, number][],
  from: number,
  to: number
): [number, number][] {
  const out: [number, number][] = [];
  for (const [start, end] of ranges) {
    const a = Math.max(start, from);
    const b = Math.min(end, to);
    if (a < b) out.push([a - from, b - from]);
  }
  return out;
}

/** `text` with `<mark>` over each range. Builds real text nodes — never innerHTML:
 *  a file name is untrusted input (it's whatever is on disk), and this is the one
 *  place a path is rendered as markup-adjacent content. */
function markUp(text: string, ranges: readonly [number, number][]): DocumentFragment {
  const frag = document.createDocumentFragment();
  let at = 0;
  for (const [start, end] of ranges) {
    if (start > at) frag.append(text.slice(at, start));
    const m = document.createElement("mark");
    m.className = "fileedit-goto-mark";
    m.textContent = text.slice(start, end);
    frag.append(m);
    at = end;
  }
  if (at < text.length) frag.append(text.slice(at));
  return frag;
}

/** A `<label>` wrapping a checkbox + caption, returned with the input so the
 *  caller can read `.checked`. */
function checkboxLabel(text: string): [HTMLLabelElement, HTMLInputElement] {
  const label = document.createElement("label");
  const box = document.createElement("input");
  box.type = "checkbox";
  const span = document.createElement("span");
  span.textContent = text;
  label.append(box, span);
  return [label, box];
}

/** Abbreviate a long absolute path for the header (keep the tail, which is the
 *  meaningful folder name). */
function shortenPath(p: string): string {
  const norm = p.replace(/\\/g, "/").replace(/\/+$/, "");
  const parts = norm.split("/");
  if (parts.length <= 2) return norm;
  return `…/${parts.slice(-2).join("/")}`;
}

