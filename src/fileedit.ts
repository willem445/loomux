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
  errorCode,
  errorMessage,
} from "./fileapi";
import {
  makeRoot,
  mergeChildren,
  flatten,
  type TreeNode,
} from "./filetreemodel";
import { fileIconSvg, folderIconSvg } from "./fileicons";
import { isDirty, closeDecision, type ConflictChoice } from "./dirtystate";
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

type Mode = "tree" | "search";

const TREE_W_KEY = "loomux.fileedit.treeW";
const DEFAULT_TREE_W = 240;
const MIN_TREE_W = 140;
const MAX_TREE_W = 560;

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

export class FileEditView {
  readonly el: HTMLElement;

  private mode: Mode = "tree";
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
  private searchPane: HTMLElement;

  // Open-file state.
  private editor: EditorWidget | null = null;
  private openRel: string | null = null;
  private savedContent = "";
  private savedHash = "";

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

    const tabs = el("div", "fileedit-tabs");
    const treeTab = el("button", "fileedit-tab active", "Files") as HTMLButtonElement;
    const searchTab = el("button", "fileedit-tab", "Search") as HTMLButtonElement;
    treeTab.addEventListener("click", () => this.setMode("tree", treeTab, searchTab));
    searchTab.addEventListener("click", () => this.setMode("search", treeTab, searchTab));
    tabs.append(treeTab, searchTab);

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

    this.findBtn = el("button", "pane-btn", "⌕") as HTMLButtonElement;
    this.findBtn.title = "Find in file";
    this.findBtn.hidden = true;
    this.findBtn.addEventListener("click", () => this.editor?.openFind());

    this.saveBtn = el("button", "fileedit-save", "Save") as HTMLButtonElement;
    this.saveBtn.title = "Save (Ctrl+S)";
    this.saveBtn.disabled = true;
    this.saveBtn.addEventListener("click", () => void this.save());

    const closeBtn = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    closeBtn.title = "Close (Esc)";
    closeBtn.addEventListener("click", () => void this.requestClose());

    head.append(tabs, rootWrap, spacer, this.fileLabel, this.dirtyDot, this.findBtn, this.saveBtn, closeBtn);

    // ---- agent-worktree banner (subtle, non-blocking) ----
    this.agentBanner = el(
      "div",
      "fileedit-banner",
      "Editing a running agent's worktree — the agent may also be writing here."
    );
    this.agentBanner.hidden = true;

    // ---- body ----
    const body = el("div", "fileedit-body");

    this.treePane = el("div", "fileedit-tree-pane");
    this.treePane.style.width = `${this.storedTreeW()}px`;
    this.treeListEl = el("div", "fileedit-tree");
    this.treePane.appendChild(this.treeListEl);

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

    this.searchPane = el("div", "fileedit-search-pane");
    this.buildSearchPane(this.searchPane); // logic wired in step 6

    body.append(this.treePane, divider, this.editorPane, this.searchPane);

    this.el.append(head, this.agentBanner, body);
    this.applyModeClass();
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
    this.editor?.dispose();
    this.el.remove();
  }

  // ---------- mode ----------

  private setMode(mode: Mode, treeTab: HTMLElement, searchTab: HTMLElement): void {
    this.mode = mode;
    treeTab.classList.toggle("active", mode === "tree");
    searchTab.classList.toggle("active", mode === "search");
    this.applyModeClass();
  }

  private applyModeClass(): void {
    this.el.classList.toggle("mode-tree", this.mode === "tree");
    this.el.classList.toggle("mode-search", this.mode === "search");
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
    await this.loadDir(this.treeModel);
    this.renderTree();
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
      await this.openFile(node.path);
    }
  }

  // ---------- open / save ----------

  private async openFile(rel: string): Promise<void> {
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
      await this.mountEditor(fr.content, rel);
      this.emptyState.hidden = true;
      this.editorHost.hidden = false;
      this.findBtn.hidden = false;
      this.updateFileLabel();
      this.updateDirty();
      this.renderTree(); // reflect the .open highlight
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
    const content = this.editor.getValue();
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
    return this.editor !== null && isDirty(this.savedContent, this.editor.getValue());
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

  // ---------- search pane (wired in step 6) ----------

  private buildSearchPane(_pane: HTMLElement): void {
    // Placeholder until the search+replace panel lands (issue #174, step 6).
    _pane.appendChild(el("div", "fileedit-empty", "Search coming up."));
  }
}

// ---------- helpers ----------

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
