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
  ftReplace,
  errorCode,
  errorMessage,
  type SearchOpts,
  type SearchBatch,
} from "./fileapi";
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
  enumerationSource,
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

/** What the hosting pane provides to the overlay. */
export interface FileEditHost {
  /** The pane's live working directory (shell-integration cwd / worktree). */
  getCwd(): string | null;
  /** Close the overlay and return focus to the terminal. */
  onClose(): void;
  /** True when the root is a running agent's worktree — the view shows a subtle
   *  banner (editing it is legitimate but the agent may also be writing). */
  isAgentWorktree?(): boolean;
}

const TREE_W_KEY = "loomux.fileedit.treeW";
const DEFAULT_TREE_W = 280;
const MIN_TREE_W = 180;
const MAX_TREE_W = 640;
/** Debounce for auto-search while typing, so the tree highlights update
 *  "live" without a full-tree walk on every keystroke. */
const SEARCH_DEBOUNCE_MS = 300;

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
  /** The query + options the current `searchGroups` were produced with. Replace
   *  applies from THIS, not the live inputs, so a query/option edit after a
   *  search can't make apply diverge from the preview. Nulled when the preview
   *  is invalidated (inputs changed) so a stale preview can't be applied. */
  private searchSnapshot: SearchParams | null = null;

  constructor(private host: FileEditHost) {
    this.el = el("div", "fileedit");
    this.el.hidden = true;
    this.el.tabIndex = -1;
    this.el.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        void this.requestClose();
      }
    });

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

    // Left column: the search box sits ABOVE the tree (demo feedback) and drives
    // the in-tree hit highlighting; the tree fills the rest.
    this.treePane = el("div", "fileedit-tree-pane");
    this.treePane.style.width = `${this.storedTreeW()}px`;
    const searchForm = el("div", "fileedit-search-form");
    this.buildSearchForm(searchForm);
    this.treeListEl = el("div", "fileedit-tree");
    this.treePane.append(searchForm, this.treeListEl);

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
    // active session (`accept`), so cross-pane/stale events are harmless.
    void onSearchBatch((b) => this.onSearchBatch(b)).then((un) => {
      this.searchUnlisten = un;
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
      void this.reloadTree();
    } else {
      this.treeListEl.replaceChildren(el("div", "fileedit-empty", "Pick a folder to browse."));
    }
    // No focus steal — the terminal below stays the primary input target until
    // the user clicks into the tree/editor.
  }

  hide(): void {
    this.el.hidden = true;
  }

  dispose(): void {
    clearTimeout(this.searchTimer);
    if (this.session.activeId !== null) void ftSearchCancel(this.session.activeId);
    this.searchUnlisten?.();
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
    if (typeof picked === "string") {
      this.root = picked;
      this.refreshRootLabel();
      await this.reloadTree();
    }
  }

  /** (Re)load the root directory into a fresh model and render. */
  private async reloadTree(): Promise<void> {
    if (!this.root) return;
    this.treeModel = makeRoot();
    void this.refreshGitStatus(); // gates the ignore toggle; independent of the tree load
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
    } else {
      try {
        this.isGitRoot = (await gitRepoRoot(root)) !== null;
      } catch {
        this.isGitRoot = false;
      }
      if (root !== this.root) return; // root changed while probing — stale result
    }
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
      } else if (e.key === "Escape" && this.session.activeId !== null) {
        // Esc cancels an in-flight search (keeping the partial results); only
        // when nothing is running does Esc fall through to close the overlay.
        e.stopPropagation();
        clearTimeout(this.searchTimer);
        this.cancelSearch();
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
      max_results: 0,
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

interface ModalButton<T> {
  label: string;
  value: T;
  kind?: "danger" | "primary";
}
interface ModalSpec<T> {
  title: string;
  body: string;
  buttons: ModalButton<T>[];
  onKey?: (key: string) => void;
}

/** Minimal confirm/choice modal reusing the `.agent-dialog` / `.dlg-*` kit
 *  (same look as editorConfigDialog). Resolves with the chosen button value. */
function modal<T>(build: (resolve: (v: T) => void) => ModalSpec<T>): Promise<T> {
  return new Promise<T>((resolve) => {
    let settled = false;
    const done = (v: T) => {
      if (settled) return;
      settled = true;
      overlay.remove();
      resolve(v);
    };
    const spec = build(done);

    const overlay = el("div", "launcher-overlay visible");
    const dlg = el("div", "agent-dialog");
    dlg.append(el("h2", "", spec.title), el("div", "dlg-hint", spec.body));
    const actions = el("div", "dlg-actions");
    for (const b of spec.buttons) {
      const btn = el("button", `dlg-btn${b.kind === "danger" ? " danger" : b.kind === "primary" ? " primary" : ""}`, b.label);
      btn.addEventListener("click", () => done(b.value));
      actions.appendChild(btn);
    }
    dlg.appendChild(actions);
    overlay.appendChild(dlg);
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay && spec.onKey) spec.onKey("Escape");
    });
    overlay.addEventListener("keydown", (e) => {
      e.stopPropagation();
      spec.onKey?.(e.key);
    });
    document.body.appendChild(overlay);
    (dlg.querySelector(".dlg-btn:last-child") as HTMLElement | null)?.focus();
  });
}
