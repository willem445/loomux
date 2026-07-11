// The file-MANAGER surface a file-explorer pane hosts (issue #214). The Explorer /
// Finder / Nautilus equivalent: browse folders, and double-click a file to open it
// in **the OS default application for its extension** — loomux never opens it.
//
// This is deliberately NOT `FileEditView` (the `Alt+F` in-app editor). PR #215
// first shipped the editor embedded in the pane; the human's clarification on #214
// is explicit that a native-style manager is what's wanted, and that the editor
// reuse is "explicitly NOT the preferred direction". The editor is untouched and
// still lives behind `Alt+F`; this is a different surface for a different job.
//
// The class owns only DOM wiring. Everything decidable is in the pure modules it
// composes — `fileexplorermodel.ts` (listing order, the rooted-navigation bound,
// formatting, the inline-edit state machine) and `filematch.ts` (the Go-to-file
// ranking) — and all path safety is enforced backend-side in `filemgr.rs`.

import { open } from "@tauri-apps/plugin-dialog";
import {
  fmList,
  fmNewFolder,
  fmNewFile,
  fmRename,
  fmDelete,
  fmCapabilities,
  fmOpen,
  fmOpenWith,
  fmReveal,
  fmHashStart,
  onHashBatch,
  type HashBatch,
} from "./filemgr";
import {
  planListingHashes,
  rememberDigest,
  algoLabel,
  HASH_ALGOS,
  COLUMN_ALGO,
  SHORT_DIGEST_CHARS,
  type HashAlgo,
  type HashCache,
  type HashCell,
} from "./filehashmodel";
import { writeClipboard } from "./clipboard";
import { buildContextMenu, type FmCaps, type MenuAction } from "./filemenu";
import { showContextMenu, closeContextMenu } from "./contextmenu";
import { modal } from "./modal";
import {
  visibleEntries,
  joinRel,
  parentRel,
  breadcrumbs,
  formatSize,
  formatModified,
  clampSelection,
  nameError,
  canCommit,
  isNoopRename,
  isCreate,
  noEdit,
  activeTarget,
  editMountFor,
  mountBlocker,
  type FmEntry,
  type EditState,
  type ExplorerView,
  type OpTarget,
} from "./fileexplorermodel";
import {
  ftFilesStart,
  onFilesBatch,
  ftSearchCancel,
  nextSearchId,
  errorCode,
  errorMessage,
  type FilesBatch,
} from "./fileapi";
import { rankFileNames, moveSelection, basenameStart, queryTerms, type FileNameHit } from "./filematch";
import { fileIconSvg, folderIconSvg } from "./fileicons";
import { confirmModal } from "./modal";
import { showToast } from "./toast";

/** What the hosting pane provides. */
export interface FileExplorerHost {
  /** The pane's root folder — the boundary. Navigation is bounded by it. */
  getRoot(): string;
  /** The user re-rooted via the folder picker: the pane adopts it (title + the
   *  persisted layout follow), so a restore reopens what was on screen. */
  onRootChanged?(root: string): void;
}

/** Cap on rendered Go-to-file results — same reasoning as the editor's: a jump list
 *  is a target, not a report. Ranking runs over the FULL index first, so the cap
 *  never costs the best hit, and the summary reports the true count. */
const GOTO_RESULT_CAP = 200;

const SVG_UP = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8 12.5V3.5M4 7.5 8 3.5l4 4"/></svg>`;
const SVG_NEW_FILE = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M9 1.9H4.4c-.6 0-1.1.5-1.1 1.1v10c0 .6.5 1.1 1.1 1.1h3.1"/><path d="M9 1.9l3.7 3.7V8"/><path d="M8.8 1.9v3.7h3.9"/><path d="M11.8 9.9v4.2M9.7 12h4.2"/></svg>`;
const SVG_NEW_FOLDER = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M1.9 12.2V4.3c0-.6.5-1.1 1.1-1.1h3l1.4 1.5h5.6c.6 0 1.1.5 1.1 1.1v1.4"/><path d="M11.5 9.5v4.2M9.4 11.6h4.2"/></svg>`;
const SVG_RENAME = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 11.2 10.8 2.9a1.6 1.6 0 0 1 2.3 2.3L4.8 13.5l-3 .7z"/></svg>`;
const SVG_DELETE = `<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2.8 4.3h10.4M6.2 4.3V2.8h3.6v1.5M4.2 4.3l.7 8.9h6.2l.7-8.9M6.6 6.6v4.4M9.4 6.6v4.4"/></svg>`;

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

export class FileExplorerView {
  readonly el: HTMLElement;

  /** The directory currently shown, RELATIVE to the pane's root. "" is the root. */
  private rel = "";
  /** The last listing fetched for `rel`, unsorted/unfiltered as the backend sent it.
   *  Kept raw so flipping "show hidden" re-filters in memory with no round-trip. */
  private entries: FmEntry[] = [];
  /** Index into the RENDERED rows (what `rows()` returns), or -1 for none. */
  private sel = -1;
  private showHidden = false;
  /** Whether a delete recycles or destroys here — probed once, so the confirmation
   *  can promise the truth instead of guessing. */
  private deleteRecycles = false;
  /** The one inline edit that may be in flight (new folder or rename). */
  private edit: EditState = noEdit;
  private disposed = false;
  /** Guards against two overlapping listing fetches painting out of order. */
  private listSeq = 0;

  // Chrome.
  private upBtn: HTMLButtonElement;
  private crumbEl: HTMLElement;
  private renameBtn: HTMLButtonElement;
  private deleteBtn: HTMLButtonElement;
  private listEl: HTMLElement;
  private statusEl: HTMLElement;
  /** The Hidden toggle's checkbox. Held so an op can turn it on (renaming a dotfile
   *  found via Go-to-file) and have the control reflect what's actually showing. */
  private hiddenBox: HTMLInputElement;

  // Hashing (#214). The listing shows a short SHA-256 per file, computed OFF-THREAD and
  // streamed in — opening a directory must never block on reading its files. Cells start
  // "…", fill as results arrive, and are cancelled the moment you navigate away.
  /** rel → what that row's hash cell shows right now. */
  private hashCells = new Map<string, HashCell>();
  /** Digest cache, keyed by (rel, size, mtime) — see hashCacheKey. Per-pane, dropped on
   *  a re-root. A stale hash is worse than no hash, so the key includes both. */
  private hashCache: HashCache = new Map();
  /** The in-flight column run's id, or null. Cancelled on navigate/dispose. */
  private hashRunId: number | null = null;
  private hashUnlisten: (() => void) | null = null;
  /** An on-demand digest we are waiting on (the Hash → submenu, or a click-to-hash cell).
   *  Keyed by the run id so a late batch from an abandoned dialog can't land in a new one. */
  private hashRequests = new Map<number, (r: { digest?: string; error?: string }) => void>();
  /** What the OS can do here. Probed once; the context menu is built against it. */
  private caps: FmCaps = {
    delete_mode: "permanent",
    open_with: false,
    reveal: false,
    reveal_selects: false,
  };

  // Go-to-file (the fast file-NAME search, kept from the first cut — it fits a
  // manager perfectly: an index built once per root, filtered in memory per
  // keystroke, and opening a hit hands it to the default app like any other file).
  private gotoInput: HTMLInputElement;
  private gotoListEl: HTMLElement;
  private gotoSummaryEl: HTMLElement;
  private fileIndex: string[] = [];
  private fileIndexId: number | null = null;
  private fileIndexLoaded = false;
  private fileIndexTruncated = false;
  private filesUnlisten: (() => void) | null = null;
  private gotoHits: FileNameHit[] = [];
  private gotoSel = 0;
  private gotoRenderScheduled = false;

  constructor(private host: FileExplorerHost) {
    this.el = el("div", "fileexp");
    this.el.tabIndex = -1;

    // ---- toolbar: up + breadcrumb + operations ----
    const head = el("div", "fileexp-head");

    this.upBtn = el("button", "pane-btn", "") as HTMLButtonElement;
    this.upBtn.innerHTML = SVG_UP;
    this.upBtn.title = "Up one folder (Backspace / Alt+Left)";
    this.upBtn.addEventListener("click", () => void this.goUp());

    this.crumbEl = el("div", "fileexp-crumbs");

    const newFileBtn = el("button", "pane-btn", "") as HTMLButtonElement;
    newFileBtn.innerHTML = SVG_NEW_FILE;
    newFileBtn.title = "New file (Ctrl+N) — created empty; double-click it to open";
    newFileBtn.addEventListener("click", () => this.beginCreate("new-file"));

    const newBtn = el("button", "pane-btn", "") as HTMLButtonElement;
    newBtn.innerHTML = SVG_NEW_FOLDER;
    newBtn.title = "New folder (Ctrl+Shift+N)";
    newBtn.addEventListener("click", () => this.beginCreate("new-folder"));

    this.renameBtn = el("button", "pane-btn", "") as HTMLButtonElement;
    this.renameBtn.innerHTML = SVG_RENAME;
    this.renameBtn.title = "Rename (F2)";
    this.renameBtn.addEventListener("click", () => this.beginRename());

    this.deleteBtn = el("button", "pane-btn", "") as HTMLButtonElement;
    this.deleteBtn.innerHTML = SVG_DELETE;
    this.deleteBtn.title = "Delete (Del)";
    this.deleteBtn.addEventListener("click", () => void this.deleteSelected());

    // ONE toggle, one mental model: "show me everything". It reveals hidden entries
    // in the listing AND drops the ignore filter from the Go-to-file index — a file
    // manager that couldn't jump to something in `dist/` because git ignores it
    // would be surprising, but so would one that indexes 40k node_modules paths by
    // default. The toggle is the honest place to put that choice.
    const hiddenLabel = document.createElement("label");
    hiddenLabel.className = "fileexp-toggle";
    const hiddenBox = document.createElement("input");
    hiddenBox.type = "checkbox";
    this.hiddenBox = hiddenBox;
    hiddenBox.addEventListener("change", () => {
      this.showHidden = hiddenBox.checked;
      this.sel = -1;
      this.invalidateIndex(); // the index's scope changed too
      this.render();
    });
    const hiddenText = document.createElement("span");
    hiddenText.textContent = "Hidden";
    hiddenLabel.append(hiddenBox, hiddenText);
    hiddenLabel.title =
      "Show hidden files, and index git-ignored paths (node_modules, build output) in Go to file.";

    const rootBtn = el("button", "pane-btn", "📁") as HTMLButtonElement;
    rootBtn.title = "Change the pane's root folder";
    rootBtn.addEventListener("click", () => void this.pickRoot());

    head.append(
      this.upBtn,
      this.crumbEl,
      newFileBtn,
      newBtn,
      this.renameBtn,
      this.deleteBtn,
      hiddenLabel,
      rootBtn
    );

    // ---- Go to file ----
    const gotoForm = el("div", "fileexp-goto-form");
    const gotoRow = el("div", "fileexp-goto-row");
    this.gotoInput = document.createElement("input");
    this.gotoInput.className = "fileexp-input";
    this.gotoInput.placeholder = "Go to file… (by name)";
    this.gotoInput.title =
      "Find a file anywhere under the root by NAME. Space-separated terms must all match. ↑/↓ to pick, Enter opens it in its default app, Esc clears.";
    this.gotoInput.spellcheck = false;
    this.gotoInput.addEventListener("focus", () => this.ensureIndex());
    this.gotoInput.addEventListener("input", () => {
      this.ensureIndex();
      this.gotoSel = 0;
      this.refreshGoto();
    });
    this.gotoInput.addEventListener("keydown", (e) => this.onGotoKey(e));
    this.gotoSummaryEl = el("span", "fileexp-goto-summary", "");
    gotoRow.append(this.gotoInput);
    gotoForm.append(gotoRow, this.gotoSummaryEl);

    // ---- listing / results ----
    this.listEl = el("div", "fileexp-list");
    this.listEl.tabIndex = 0; // so the listing itself takes arrow keys
    this.listEl.addEventListener("keydown", (e) => this.onListKey(e));
    // Right-click on the empty space BELOW the rows: no row to act on, so the menu offers
    // only the creates. A row's own handler stops propagation, so this fires only when the
    // click really did miss every row. The webview's default menu is suppressed only here
    // and on the rows — never over the rest of the pane.
    this.listEl.addEventListener("contextmenu", (e) => this.onEmptyContextMenu(e));
    this.gotoListEl = el("div", "fileexp-goto-list");
    this.gotoListEl.hidden = true;
    // Empty space in the RESULTS list: deliberately no menu. The creates act on the
    // directory being browsed — which the results view isn't showing you — so offering them
    // here would act somewhere the user can't see. A row's menu still works (the rows wire
    // their own); this just stops the webview's default menu appearing in the gap.
    this.gotoListEl.addEventListener("contextmenu", (e) => e.preventDefault());

    this.statusEl = el("div", "fileexp-status", "");

    this.el.append(head, gotoForm, this.listEl, this.gotoListEl, this.statusEl);

    void onFilesBatch((b) => this.onFilesBatch(b)).then((un) => {
      if (this.disposed) un();
      else this.filesUnlisten = un;
    });
    // Ask once what this platform can actually do: what a delete does (so the
    // confirmation can promise the truth), and whether there is an Open-with chooser or a
    // real reveal (so the menu offers exactly what works).
    void fmCapabilities()
      .then((caps) => {
        if (this.disposed) return;
        this.caps = caps;
        this.deleteRecycles = caps.delete_mode === "recycle";
      })
      .catch(() => {});
    void onHashBatch((b) => this.onHashBatch(b)).then((un) => {
      if (this.disposed) un();
      else this.hashUnlisten = un;
    });
  }

  // ---------- lifecycle ----------

  /** Load the root listing. Called once, when the pane is built. */
  show(): void {
    void this.navigate("");
  }

  dispose(): void {
    this.disposed = true;
    if (this.fileIndexId !== null) void ftSearchCancel(this.fileIndexId);
    this.fileIndexId = null;
    // A hash run outliving its view would read a whole directory for nobody.
    this.cancelHashRun();
    this.filesUnlisten?.();
    this.hashUnlisten?.();
    closeContextMenu();
    this.el.remove();
  }

  focus(): void {
    this.listEl.focus();
  }

  // ---------- navigation ----------

  /** Show the directory `rel` (relative to the pane's root) and select nothing.
   *  `selectName` optionally lands the selection on an entry once it's listed —
   *  used after a rename/new-folder, and when a Go-to-file hit navigates you to the
   *  file's folder, so the thing you just acted on is the thing that's highlighted. */
  private async navigate(rel: string, selectName?: string): Promise<void> {
    const seq = ++this.listSeq;
    this.edit = noEdit;
    try {
      const entries = await fmList(this.host.getRoot(), rel);
      if (this.disposed || seq !== this.listSeq) return; // superseded by a newer navigation
      this.rel = rel;
      this.entries = entries;
      this.sel = selectName ? this.rows().findIndex((e) => e.name === selectName) : -1;
      this.startHashRun(); // off-thread; the render below does NOT wait on it
      this.render();
    } catch (err) {
      if (this.disposed || seq !== this.listSeq) return;
      showToast(`Cannot open folder: ${errorMessage(err)}`);
      // A folder that vanished under us (deleted in another app) must not strand the
      // pane on a dead listing — fall back to the root, which the pane guarantees.
      if (rel !== "") void this.navigate("");
    }
  }

  /** Re-fetch the current directory. Every mutating op ends here, so the listing is
   *  always what's actually on disk rather than a guess about what we just did. */
  private refresh(selectName?: string): Promise<void> {
    return this.navigate(this.rel, selectName);
  }

  private async goUp(): Promise<void> {
    const up = parentRel(this.rel);
    if (up === null) return; // at the root — the pane's boundary
    // Land the selection on the folder we just came out of, so Up/Enter is reversible
    // without hunting for where you were.
    const cameFrom = this.rel.split("/").pop();
    await this.navigate(up, cameFrom);
  }

  private async pickRoot(): Promise<void> {
    const picked = await open({
      directory: true,
      title: "Choose a folder to browse",
      defaultPath: this.host.getRoot(),
    });
    if (typeof picked !== "string") return;
    this.host.onRootChanged?.(picked);
    this.invalidateIndex();
    await this.navigate("");
  }

  /** Open an entry: a folder navigates in, a file goes to the OS default app. This
   *  is the double-click / Enter behavior, and it's the heart of the feature. */
  private async openEntry(entry: FmEntry): Promise<void> {
    const rel = joinRel(this.rel, entry.name);
    if (entry.is_dir && !entry.is_symlink) {
      await this.navigate(rel);
      return;
    }
    await this.openWithDefaultApp(rel);
  }

  private async openWithDefaultApp(rel: string): Promise<void> {
    try {
      await fmOpen(this.host.getRoot(), rel);
    } catch (err) {
      showToast(explainOpError(err, "open"));
    }
  }

  // ---------- rows, and what an op acts on ----------

  /** The rendered listing rows: hidden entries dropped unless asked for, folders first. */
  private rows(): FmEntry[] {
    return visibleEntries(this.entries, this.showHidden);
  }

  /** True while the Go-to-file results are what's on screen (the listing is hidden). */
  private get filtering(): boolean {
    return queryTerms(this.gotoInput.value).length > 0;
  }

  /** The view the user is ACTUALLY LOOKING AT. Ops resolve against this, never against
   *  the listing unconditionally — conflating the two is the bug the human hit. */
  private view(): ExplorerView {
    return this.filtering
      ? { kind: "results", dir: this.rel, hits: this.gotoHits, sel: this.gotoSel }
      : { kind: "listing", dir: this.rel, rows: this.rows(), sel: this.sel };
  }

  /** What an operation invoked RIGHT NOW would act on — a path, captured by identity.
   *  Immune to the lists changing underneath it afterwards (see fileexplorermodel). */
  private target(): OpTarget | null {
    return activeTarget(this.view());
  }

  /** Leave the filtered results and go back to the listing.
   *
   *  Any op that needs the LISTING to be visible calls this first. Creating a new
   *  folder/file, and editing a name inline, both render a row into the listing — and
   *  rendering into a hidden list is exactly how "the click did nothing" happened. */
  private exitFilter(): void {
    if (!this.filtering) return;
    this.gotoInput.value = "";
    this.gotoSel = 0;
    this.refreshGoto();
  }

  // ---------- operations ----------

  /** New folder / new file — the same interaction, so the same code path; only the
   *  backend call differs, at the single point where it has to. */
  private beginCreate(kind: "new-folder" | "new-file"): void {
    // The new entry lands in the directory being BROWSED, so the listing has to be the
    // thing on screen — otherwise its editor row renders into a hidden list.
    this.exitFilter();
    this.edit = { kind, draft: "" };
    this.render();
  }

  private beginRename(): void {
    const target = this.target();
    if (!target) {
      showToast("Select an item to rename.", "info");
      return;
    }
    this.beginRenameOn(target);
  }

  /** Rename a target that the caller has ALREADY bound — the toolbar/keyboard resolve it
   *  from the current view, the context menu carries the one it captured at menu-open.
   *  Both land here, so there is exactly one rename implementation and the
   *  editMountFor/mountBlocker discipline can't be forgotten by a second call site. */
  private beginRenameOn(target: OpTarget): void {
    // What the VIEW must become before an editor can mount on this target (pure, and
    // tested: editMountFor). Getting the TARGET right was only half the bug — the other
    // half is where the editor LANDS. `exitFilter` is the load-bearing bit: render()
    // ends in refreshGoto(), which re-hides the listing while a query is still set, so
    // without it the editor mounts inside a display:none list and its focus call no-ops
    // — the "F2 does nothing" symptom, rebuilt on the very path added to kill it.
    const mount = editMountFor(target, this.view());
    if (mount.exitFilter) this.exitFilter();
    if (!mount.navigate) {
      this.openRenameEditor(target);
      return;
    }
    // The file lives in another folder (a Go-to-file hit can be anywhere under the
    // root). Go to where it actually is and select it, so the user SEES the file they
    // picked, in context, with the cursor in its name. (Hosting the editor in the
    // results list instead would put a focused input in a list that re-renders on every
    // streaming index batch — it would eat keystrokes.)
    void this.navigate(mount.dir, target.name).then(() => {
      if (this.disposed) return;
      this.openRenameEditor(target);
    });
  }

  /** Open the inline rename editor on `target`. The target's PATH is what's stored, so
   *  the commit acts on the file the user picked no matter what the listing does next.
   *
   *  The row has to actually BE in the listing, though, and `mountBlocker` says whether
   *  it is. Two ways it isn't — both reachable from a Go-to-file hit, and both of which
   *  would otherwise mount no editor AND leave `edit` set with no input to Escape from,
   *  deadening the listing's keyboard until some unrelated path reset it. */
  private openRenameEditor(target: OpTarget): void {
    const block = mountBlocker(target, this.entries, this.showHidden);
    if (block.kind === "missing") {
      // Gone between capture and now — an agent, or another app, deleted it.
      showToast(`"${target.name}" no longer exists.`);
      this.edit = noEdit;
      this.render();
      return;
    }
    if (block.kind === "hidden") {
      // The Go-to-file index reaches files the listing HIDES (every dotfile on
      // macOS/Linux; hidden-attribute files on Windows). The user asked to rename a file
      // they can plainly see in the results — hiding it from them now would be a
      // non-sequitur. So turn Hidden on for them, and SAY so: a listing that silently
      // sprouts dotfiles is its own small mystery.
      this.showHidden = true;
      this.hiddenBox.checked = true;
      this.invalidateIndex(); // the index's scope follows the toggle
      showToast(`Showing hidden files so "${target.name}" can be renamed.`, "info");
    }
    this.edit = { kind: "rename", rel: target.rel, original: target.name, draft: target.name };
    this.render();
  }

  /** Commit the inline edit. The inline validation is a courtesy that answers while the
   *  user types; the backend re-checks everything and stays authoritative. */
  private async commitEdit(): Promise<void> {
    const state = this.edit;
    if (state.kind === "none") return;
    const siblings = this.rows().map((e) => e.name);
    if (!canCommit(state, siblings)) {
      // The row already shows the reason; don't also toast it.
      return;
    }
    const name = state.draft.trim();
    // A rename to the same name: nothing to do, and no reason to make the call.
    if (isNoopRename(state)) {
      this.edit = noEdit;
      this.render();
      return;
    }
    // Latch BEFORE the await, so a double-Enter can't fire the op twice.
    this.edit = noEdit;
    try {
      if (state.kind === "new-folder") {
        await fmNewFolder(this.host.getRoot(), this.rel, name);
      } else if (state.kind === "new-file") {
        // Created empty and NOT opened — the user's double-click is what decides which
        // app it belongs to. Loomux having an opinion about that is the thing this pane
        // exists not to do.
        await fmNewFile(this.host.getRoot(), this.rel, name);
      } else {
        // `state.rel` — the path captured when the editor opened, not a re-resolved
        // index. This is the fix for the demo bug: whatever the lists did in between,
        // the rename lands on the file the user actually picked.
        await fmRename(this.host.getRoot(), state.rel, name);
      }
      this.invalidateIndex(); // the tree changed under the Go-to-file index
      await this.refresh(name); // re-list from disk, selecting what we just made/renamed
    } catch (err) {
      showToast(
        isCreate(state)
          ? `Couldn't create ${state.kind === "new-file" ? "file" : "folder"}: ${errorMessage(err)}`
          : explainOpError(err, "rename")
      );
      await this.refresh();
    }
  }

  private cancelEdit(): void {
    this.edit = noEdit;
    this.render();
    this.listEl.focus();
  }

  private async deleteSelected(): Promise<void> {
    // Captured BEFORE the confirm dialog, from the view on screen. The dialog is an
    // await: without capturing, the target would be re-resolved afterwards against
    // whatever list is current by then. Delete is not undoable with an "oh, nothing
    // happened" — this is the op where getting the target wrong costs the most.
    const target = this.target();
    if (!target) {
      showToast("Select an item to delete.", "info");
      return;
    }
    await this.deleteTarget(target);
  }

  /** Delete a target the caller has ALREADY bound (see `beginRenameOn` for why). */
  private async deleteTarget(target: OpTarget): Promise<void> {
    const what = target.isDir ? "folder" : "file";
    // Name the file in the dialog, and say what will ACTUALLY happen: promising the
    // Recycle Bin on a platform that has none is a lie the user only discovers when
    // they go looking for the file.
    const ok = await confirmModal(
      this.deleteRecycles ? `Move this ${what} to the Recycle Bin?` : `Permanently delete this ${what}?`,
      this.deleteRecycles
        ? `"${target.rel}" will go to the Recycle Bin — you can restore it from there.`
        : `"${target.rel}" will be deleted permanently. This cannot be undone${target.isDir ? ", and it will take everything inside it" : ""}.`,
      this.deleteRecycles ? "Move to Recycle Bin" : "Delete permanently",
      !this.deleteRecycles
    );
    if (!ok) return;
    try {
      await fmDelete(this.host.getRoot(), target.rel); // the captured path, not a re-lookup
      this.invalidateIndex();
      await this.refresh();
    } catch (err) {
      showToast(explainOpError(err, "delete"));
      await this.refresh();
    }
  }

  // ---------- hashing (#214) ----------
  //
  // The listing's SHA-256 column. The rule that shapes all of this: opening a directory
  // must NEVER block on reading its files. So the render paints a placeholder immediately
  // and the digests arrive afterwards, from a worker thread, and stop the instant you
  // leave. A sync command would have run on Tauri's main thread and frozen the window.

  /** Plan and launch the column's hashes for the current listing. The pure model decides
   *  WHAT to hash (`planListingHashes`: never a directory or a symlink, cached hits for
   *  free, over-threshold files only on request); this just runs it. */
  private startHashRun(): void {
    this.cancelHashRun();
    const { cells, toHash } = planListingHashes(this.entries, this.rel, this.hashCache);
    this.hashCells = cells;
    if (toHash.length === 0) return;
    const id = nextSearchId();
    this.hashRunId = id;
    void fmHashStart(id, this.host.getRoot(), toHash, COLUMN_ALGO);
  }

  /** Stop the in-flight column run. Called on every navigate and on dispose: a run that
   *  outlives its listing is reading a whole directory for a view nobody is looking at. */
  private cancelHashRun(): void {
    if (this.hashRunId !== null) {
      void ftSearchCancel(this.hashRunId); // one registry serves search, index AND hashing
      this.hashRunId = null;
    }
  }

  /** Fold one streamed batch in. Batches from a superseded run are dropped by id — which
   *  is what stops a digest computed for the PREVIOUS directory landing in a row of this
   *  one that happens to share a name. */
  private onHashBatch(b: HashBatch): void {
    if (this.disposed) return;

    // An on-demand request (Hash submenu, or a click-to-hash cell) waiting on this id.
    const waiter = this.hashRequests.get(b.id);
    if (waiter) {
      const r = b.results[0];
      if (r) {
        this.hashRequests.delete(b.id);
        waiter({ digest: r.digest, error: r.error });
      } else if (b.done) {
        // Done with no result: cancelled, or the file vanished. Never leave a dialog
        // spinning on a promise nobody will resolve.
        this.hashRequests.delete(b.id);
        waiter({ error: "hashing was cancelled" });
      }
      return;
    }

    if (b.id !== this.hashRunId) return; // stale column run — its directory is gone
    for (const r of b.results) {
      if (r.digest) {
        this.hashCells.set(r.rel, {
          kind: "done",
          full: r.digest,
          short: r.digest.slice(0, SHORT_DIGEST_CHARS),
        });
        // Cache against the size+mtime it was computed from, so re-entering this folder is
        // free — and so an EDIT to the file invalidates it (the key moves with the file).
        rememberDigest(this.hashCache, r.rel, this.entries, this.rel, r.digest);
      } else {
        this.hashCells.set(r.rel, { kind: "error", message: r.error ?? "hash failed" });
      }
    }
    if (b.done) this.hashRunId = null;
    this.paintHashCells();
  }

  /** Update the hash column IN PLACE. Deliberately not a re-render: results stream in
   *  while the user is selecting, scrolling, or typing in a rename editor, and blowing the
   *  listing away underneath them for a cosmetic column would be its own bug. */
  private paintHashCells(): void {
    for (const cellEl of Array.from(this.listEl.querySelectorAll<HTMLElement>(".fileexp-hash"))) {
      const rel = cellEl.dataset.rel;
      if (rel) this.fillHashCell(cellEl, this.hashCells.get(rel) ?? { kind: "pending" });
    }
  }

  private fillHashCell(cellEl: HTMLElement, cell: HashCell): void {
    cellEl.className = "fileexp-hash " + cell.kind;
    cellEl.onclick = null;
    if (cell.kind === "none") {
      cellEl.textContent = "";
      cellEl.title = "";
    } else if (cell.kind === "pending") {
      cellEl.textContent = "…";
      cellEl.title = "Hashing…";
    } else if (cell.kind === "on-demand") {
      // Over the auto threshold: the read is not free, so the user gets to ask for it.
      cellEl.textContent = "hash";
      cellEl.title = "This file is large — click to hash it (SHA-256).";
      cellEl.onclick = (e) => {
        e.stopPropagation();
        const rel = cellEl.dataset.rel;
        if (rel) void this.hashOnDemandIntoCell(rel);
      };
    } else if (cell.kind === "error") {
      cellEl.textContent = "—";
      cellEl.title = cell.message;
    } else {
      cellEl.textContent = cell.short;
      cellEl.title = "SHA-256: " + cell.full + "\nClick to copy";
      cellEl.onclick = (e) => {
        e.stopPropagation();
        void copyDigest(cell.full, "SHA-256");
      };
    }
  }

  /** Hash ONE file on demand. Same worker path as the column — there is one place hashing
   *  can be wrong, and it is the one the published-vector tests cover. */
  private hashOne(rel: string, algo: HashAlgo): Promise<{ digest?: string; error?: string }> {
    const id = nextSearchId();
    return new Promise((resolve) => {
      this.hashRequests.set(id, resolve);
      void fmHashStart(id, this.host.getRoot(), [rel], algo).catch((e) => {
        this.hashRequests.delete(id);
        resolve({ error: errorMessage(e) });
      });
    });
  }

  /** The click-to-hash cell for an over-threshold file. */
  private async hashOnDemandIntoCell(rel: string): Promise<void> {
    this.hashCells.set(rel, { kind: "pending" });
    this.paintHashCells();
    const r = await this.hashOne(rel, COLUMN_ALGO);
    if (this.disposed) return;
    this.hashCells.set(
      rel,
      r.digest
        ? { kind: "done", full: r.digest, short: r.digest.slice(0, SHORT_DIGEST_CHARS) }
        : { kind: "error", message: r.error ?? "hash failed" }
    );
    if (r.digest) rememberDigest(this.hashCache, rel, this.entries, this.rel, r.digest);
    this.paintHashCells();
  }

  /** The Hash submenu: compute one digest and show it in a copyable dialog. */
  private async showHashDialog(target: OpTarget, algo: HashAlgo): Promise<void> {
    const label = algoLabel(algo);
    const r = await this.hashOne(target.rel, algo);
    if (this.disposed) return;
    if (!r.digest) {
      showToast(explainOpError(r.error ?? "", "hash"));
      return;
    }
    const digest = r.digest;
    const copy = await modal<boolean>((resolve) => ({
      title: label + " — " + target.name,
      body: digest,
      bodyMono: true,
      buttons: [
        { label: "Close", value: false },
        { label: "Copy", value: true, kind: "primary" },
      ],
      onKey: (k) => (k === "Escape" ? resolve(false) : undefined),
    }));
    if (copy) await copyDigest(digest, label);
  }

  // ---------- context menu (#214) ----------

  /** Right-click on a ROW, in EITHER view.
   *
   *  Called from the listing's rows and the Go-to-file results' rows alike (see
   *  `wireRowAffordances`, the one place a row's behaviours are attached — the results view
   *  being the forgotten twin of the listing is a mistake this codebase has now made three
   *  times, so the two no longer get separate wiring to forget).
   *
   *  `rowIndex` indexes whichever list is on screen. Selecting it FIRST matters: the menu
   *  must bind the row under the cursor, not whatever was highlighted before — you can
   *  right-click a row you never selected.
   *
   *  The target is then resolved from the view on screen and BOUND HERE; every action the
   *  menu fires carries that path. A context menu is the identity-vs-index trap with a
   *  longer fuse: it is built now and clicked seconds later, by which time the lists have
   *  had every chance to re-rank, re-sort, or be replaced by search results. */
  private onRowContextMenu(e: MouseEvent, rowIndex: number): void {
    e.preventDefault();
    e.stopPropagation();
    if (this.filtering) {
      this.gotoSel = rowIndex;
      this.renderGoto();
      this.syncOpButtons();
    } else {
      this.sel = rowIndex;
      this.render();
    }
    this.openMenuAt(e, this.target());
  }

  /** Right-click on the empty space below the rows: there is no row, so only the
   *  directory-scoped creates apply. */
  private onEmptyContextMenu(e: MouseEvent): void {
    e.preventDefault();
    e.stopPropagation();
    this.openMenuAt(e, null);
  }

  private openMenuAt(e: MouseEvent, target: OpTarget | null): void {
    showContextMenu(e.clientX, e.clientY, buildContextMenu(target, this.caps, HASH_ALGOS), (a) =>
      this.runMenuAction(a)
    );
  }

  /** Attach every behaviour a ROW has, in whichever view it lives.
   *
   *  THE POINT OF THIS FUNCTION IS THAT THERE IS ONLY ONE OF IT. Three rounds running, an
   *  affordance was built for the listing and quietly omitted from the Go-to-file results:
   *  rename bound the wrong row (round 4), the rename editor mounted into the hidden list
   *  (round 5), and the context menu simply wasn't wired there at all (round 6). Each time
   *  the fix was correct and each time the NEXT affordance forgot again — because the two
   *  views had two sets of listeners and nothing tied them together.
   *
   *  Now a row's behaviours are attached in exactly one place, so a new one lands in both
   *  views by construction rather than by remembering. `ROW_AFFORDANCES` (the pure model)
   *  is the declarative half of the same guard: a new affordance must state whether it
   *  works in the results view, and the parity test fails until it does. */
  private wireRowAffordances(row: HTMLElement, index: number, onOpen: () => void): void {
    row.addEventListener("click", () => this.selectRow(index));
    row.addEventListener("dblclick", () => onOpen());
    row.addEventListener("contextmenu", (e) => this.onRowContextMenu(e, index));
  }

  /** Move the selection to `index` in whichever view is on screen. */
  private selectRow(index: number): void {
    if (this.filtering) {
      this.gotoSel = index;
      this.renderGoto();
      this.syncOpButtons();
    } else {
      this.sel = index;
      this.render();
      this.listEl.focus();
    }
  }

  /** Execute a bound menu action. Every one routes through the SAME op layer the toolbar
   *  and the keyboard use — `beginRenameOn` (and thus editMountFor / mountBlocker),
   *  `beginCreate`, the same fm_* calls. The menu is a second way to REACH the ops, never
   *  a second copy of them: that is why the round-4 rename-from-results fix applies here
   *  for free rather than needing to be remembered again. */
  private runMenuAction(a: MenuAction): void {
    switch (a.kind) {
      case "new-folder":
        this.beginCreate("new-folder");
        return;
      case "new-file":
        this.beginCreate("new-file");
        return;
      case "open":
        void this.openTarget(a.target);
        return;
      case "open-with":
        void this.openWithTarget(a.target);
        return;
      case "reveal":
        void this.revealTarget(a.target);
        return;
      case "rename":
        this.beginRenameOn(a.target);
        return;
      case "delete":
        void this.deleteTarget(a.target);
        return;
      case "hash":
        void this.showHashDialog(a.target, a.algo);
        return;
    }
  }

  private async openTarget(target: OpTarget): Promise<void> {
    if (target.isDir) {
      await this.navigate(target.rel);
      return;
    }
    await this.openWithDefaultApp(target.rel);
  }

  private async openWithTarget(target: OpTarget): Promise<void> {
    try {
      await fmOpenWith(this.host.getRoot(), target.rel);
    } catch (err) {
      showToast(explainOpError(err, "open"));
    }
  }

  private async revealTarget(target: OpTarget): Promise<void> {
    try {
      await fmReveal(this.host.getRoot(), target.rel);
    } catch (err) {
      showToast(explainOpError(err, "reveal"));
    }
  }

  // ---------- keyboard ----------

  private onListKey(e: KeyboardEvent): void {
    // Never let the listing's keys reach the terminal / app shortcut layer.
    if (this.edit.kind !== "none") return; // the inline input owns the keyboard
    const rows = this.rows();
    switch (e.key) {
      case "ArrowDown":
      case "ArrowUp":
        e.preventDefault();
        e.stopPropagation();
        this.sel = clampSelection(this.sel, e.key === "ArrowDown" ? 1 : -1, rows.length);
        this.render();
        return;
      case "Enter": {
        e.preventDefault();
        e.stopPropagation();
        const entry = this.rows()[this.sel];
        if (entry) void this.openEntry(entry);
        return;
      }
      case "Backspace":
        e.preventDefault();
        e.stopPropagation();
        void this.goUp();
        return;
      case "F2":
        e.preventDefault();
        e.stopPropagation();
        this.beginRename();
        return;
      case "Delete":
        e.preventDefault();
        e.stopPropagation();
        void this.deleteSelected();
        return;
    }
    if (e.key === "ArrowLeft" && e.altKey) {
      e.preventDefault();
      e.stopPropagation();
      void this.goUp();
      return;
    }
    if (e.key.toLowerCase() === "n" && e.ctrlKey) {
      // Pane-local, and only while the listing has focus — so neither can collide with
      // an app shortcut (matchShortcut claims no Ctrl+N or Ctrl+Shift+N).
      e.preventDefault();
      e.stopPropagation();
      this.beginCreate(e.shiftKey ? "new-folder" : "new-file");
    }
  }

  // ---------- render ----------

  private render(): void {
    if (this.disposed) return;
    this.renderCrumbs();
    this.upBtn.disabled = parentRel(this.rel) === null;
    this.renderList();
    this.renderStatus();
    this.refreshGoto(); // re-ranks, swaps the visible list, and syncs the op buttons
  }

  /** Enable/disable Rename + Delete from the ACTIVE view's target.
   *
   *  They used to read the listing's selection unconditionally — which is how Delete
   *  stayed live, while the results were on screen, pointed at a file the user could
   *  not even see. Now: no target in whichever list is showing → the buttons are dead. */
  private syncOpButtons(): void {
    const target = this.target();
    this.renameBtn.disabled = !target;
    this.deleteBtn.disabled = !target;
  }

  private renderCrumbs(): void {
    const rootName = shortName(this.host.getRoot());
    const frag = document.createDocumentFragment();
    const crumbs = breadcrumbs(rootName, this.rel);
    crumbs.forEach((c, i) => {
      if (i > 0) frag.append(el("span", "fileexp-crumb-sep", "›"));
      const btn = el("button", "fileexp-crumb", c.label) as HTMLButtonElement;
      btn.title = c.rel || rootName;
      if (i === crumbs.length - 1) btn.classList.add("current");
      btn.addEventListener("click", () => void this.navigate(c.rel));
      frag.appendChild(btn);
    });
    this.crumbEl.replaceChildren(frag);
  }

  private renderList(): void {
    const rows = this.rows();
    const now = Date.now();
    const frag = document.createDocumentFragment();

    // The new-folder / new-file editor is a row at the TOP of the listing, so it is
    // visible without scrolling. On commit the entry is re-listed from disk and
    // selected, so a new FILE landing further down (files sort after folders) is
    // scrolled to rather than lost.
    if (isCreate(this.edit)) {
      frag.appendChild(this.editRow(this.edit.kind === "new-folder"));
    }

    let renamedRowRendered = false;
    rows.forEach((entry, i) => {
      if (this.edit.kind === "rename" && this.edit.original === entry.name) {
        frag.appendChild(this.editRow(entry.is_dir && !entry.is_symlink));
        renamedRowRendered = true;
        return;
      }
      const row = el("div", "fileexp-row");
      if (i === this.sel) row.classList.add("sel");
      const isDir = entry.is_dir && !entry.is_symlink;

      const icon = el("span", "fileexp-icon");
      icon.innerHTML = isDir ? folderIconSvg(false) : fileIconSvg(entry.name);
      const name = el("span", "fileexp-name", entry.name);
      if (entry.is_symlink) {
        row.classList.add("symlink");
        // Say the whole truth: it is inert, not merely un-followed. Every op on it is
        // refused (see `explainOpError`), and a tooltip that only said "never
        // followed" would leave the user surprised when Del/F2 bounced.
        name.title = "Symlink — shown, but loomux won't follow, open, rename or delete it.";
      }
      if (entry.is_hidden) row.classList.add("hidden-entry");
      const size = el("span", "fileexp-size", formatSize(entry));
      const mtime = el("span", "fileexp-mtime", formatModified(entry.modified_ms, now));
      // The SHA-256 column. Painted from the cell map, which streams in from the worker —
      // the row renders NOW and the digest catches up.
      const rel = joinRel(this.rel, entry.name);
      const hash = el("span", "fileexp-hash");
      hash.dataset.rel = rel;
      this.fillHashCell(hash, this.hashCells.get(rel) ?? { kind: "pending" });

      row.append(icon, name, hash, size, mtime);
      // Click / double-click (a folder goes in, a file goes to the OS) / right-click — all
      // from the ONE place a row's behaviours are attached, so the results rows below get
      // exactly the same set. See wireRowAffordances.
      this.wireRowAffordances(row, i, () => void this.openEntry(entry));
      frag.appendChild(row);
    });

    if (rows.length === 0 && !isCreate(this.edit)) {
      frag.appendChild(el("div", "fileexp-empty", "This folder is empty."));
    }
    this.listEl.replaceChildren(frag);
    this.listEl.querySelector(".fileexp-row.sel")?.scrollIntoView({ block: "nearest" });

    // SELF-HEAL. A rename edit whose row we did not render has no input to type in and
    // no Escape to press — and `onListKey` would stay deadened by it (`edit.kind !==
    // "none"` swallows every key) until some unrelated path happened to reset it.
    // `openRenameEditor`'s mountBlocker check should mean this never fires; this is here
    // so that no FUTURE path can reintroduce the whole class by forgetting to make the
    // row visible first. Belt and braces on the bug that has now been built twice.
    if (this.edit.kind === "rename" && !renamedRowRendered) {
      this.edit = noEdit;
    }
  }

  /** The inline input row shared by "new folder" and "rename" — one interaction, so
   *  one implementation. */
  private editRow(isDir: boolean): HTMLElement {
    const state = this.edit;
    const row = el("div", "fileexp-row editing");
    const icon = el("span", "fileexp-icon");
    icon.innerHTML = isDir ? folderIconSvg(false) : fileIconSvg(state.kind === "rename" ? state.original : "new.txt");
    const input = document.createElement("input");
    input.className = "fileexp-input fileexp-edit-input";
    input.spellcheck = false;
    input.value = state.kind === "none" ? "" : state.draft;
    input.placeholder =
      state.kind === "new-folder" ? "New folder name…" : state.kind === "new-file" ? "New file name…" : "";

    const err = el("span", "fileexp-edit-err", "");
    const paint = () => {
      // Every sibling name, the entry's own included: `nameError` knows a rename to
      // one's own name is a no-op rather than a self-collision (it has `original`),
      // so the caller doesn't have to pre-filter the list and get that wrong.
      const msg = nameError(this.edit, this.rows().map((e) => e.name));
      err.textContent = msg ?? "";
      input.classList.toggle("invalid", msg !== null && input.value.trim() !== "");
    };

    input.addEventListener("input", () => {
      if (this.edit.kind === "none") return;
      this.edit = { ...this.edit, draft: input.value };
      paint(); // live validation — answer while they type, not after a round-trip
    });
    input.addEventListener("keydown", (e) => {
      e.stopPropagation(); // typing here must never reach app shortcuts
      if (e.key === "Enter") {
        e.preventDefault();
        void this.commitEdit();
      } else if (e.key === "Escape") {
        e.preventDefault();
        this.cancelEdit();
      }
    });
    // Clicking away abandons the edit — the same "no half-finished state" rule the
    // pane rename (panerename.ts) follows.
    input.addEventListener("blur", () => {
      if (this.edit.kind !== "none" && input.isConnected) this.cancelEdit();
    });

    row.append(icon, input, err);
    // Focus + select after it's in the DOM (the caller appends it this tick).
    queueMicrotask(() => {
      if (!input.isConnected) return;
      input.focus();
      // Rename pre-selects the stem, not the extension — you almost always want to
      // change the name and keep the `.ts`.
      const stem = input.value.lastIndexOf(".");
      if (state.kind === "rename" && stem > 0) input.setSelectionRange(0, stem);
      else input.select();
    });
    paint();
    return row;
  }

  private renderStatus(): void {
    const rows = this.rows();
    const hiddenCount = this.entries.length - this.entries.filter((e) => !e.is_hidden).length;
    const folders = rows.filter((e) => e.is_dir && !e.is_symlink).length;
    const files = rows.length - folders;
    const bits = [`${folders} folder${folders === 1 ? "" : "s"}`, `${files} file${files === 1 ? "" : "s"}`];
    if (!this.showHidden && hiddenCount > 0) bits.push(`${hiddenCount} hidden`);
    this.statusEl.textContent = bits.join(" · ");
  }

  // ---------- Go to file (name index) ----------

  private ensureIndex(): void {
    const root = this.host.getRoot();
    if (!root || this.fileIndexLoaded || this.fileIndexId !== null) return;
    const id = nextSearchId();
    this.fileIndexId = id;
    this.fileIndex = [];
    this.fileIndexTruncated = false;
    this.updateGotoSummary();
    // include_ignored follows the Hidden toggle — see the toggle's own comment.
    void ftFilesStart(id, root, this.showHidden);
  }

  private invalidateIndex(): void {
    if (this.fileIndexId !== null) {
      void ftSearchCancel(this.fileIndexId);
      this.fileIndexId = null;
    }
    this.fileIndex = [];
    this.fileIndexLoaded = false;
    this.fileIndexTruncated = false;
    if (this.gotoInput.value.trim()) {
      this.ensureIndex();
      this.refreshGoto();
    } else {
      this.updateGotoSummary();
    }
  }

  private onFilesBatch(b: FilesBatch): void {
    if (this.disposed || b.id !== this.fileIndexId) return;
    if (b.error) showToast(`Couldn't index files: ${errorMessage(b.error)}`);
    this.fileIndex.push(...b.files);
    if (b.truncated) this.fileIndexTruncated = true;
    if (b.done) {
      this.fileIndexId = null;
      this.fileIndexLoaded = true;
      this.refreshGoto();
    } else if (!this.gotoRenderScheduled) {
      this.gotoRenderScheduled = true;
      requestAnimationFrame(() => {
        this.gotoRenderScheduled = false;
        if (!this.disposed) this.refreshGoto();
      });
    }
  }

  /** With a query, the result list REPLACES the listing; clearing it puts the folder
   *  back. The box filters the whole tree under the root, so it's a jump, not a
   *  filter of the current folder — which is the point. */
  private refreshGoto(): void {
    const active = queryTerms(this.gotoInput.value).length > 0;
    this.gotoHits = active ? rankFileNames(this.fileIndex, this.gotoInput.value, GOTO_RESULT_CAP) : [];
    this.gotoSel = Math.min(this.gotoSel, Math.max(0, this.gotoHits.length - 1));
    this.gotoListEl.hidden = !active;
    this.listEl.hidden = active;
    this.renderGoto();
    this.updateGotoSummary();
    this.syncOpButtons();
  }

  private onGotoKey(e: KeyboardEvent): void {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      e.stopPropagation();
      this.gotoSel = moveSelection(this.gotoSel, e.key === "ArrowDown" ? 1 : -1, this.gotoHits.length);
      this.renderGoto();
      this.syncOpButtons(); // the op target just moved to a different file
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      void this.openGotoHit(this.gotoSel);
      return;
    }
    if (e.key === "F2") {
      // Rename the highlighted RESULT, without having to leave the search box first.
      // Delete is deliberately NOT bound here: inside a text input, Del means "delete
      // the character under the caret", and stealing that would be its own bug.
      e.preventDefault();
      e.stopPropagation();
      this.beginRename();
      return;
    }
    if (e.key === "Escape") {
      if (this.gotoInput.value !== "") {
        e.stopPropagation();
        this.gotoInput.value = "";
        this.gotoSel = 0;
        this.refreshGoto();
        this.listEl.focus();
      }
      return;
    }
    e.stopPropagation();
  }

  private renderGoto(): void {
    if (this.gotoListEl.hidden) {
      this.gotoListEl.replaceChildren();
      return;
    }
    if (this.gotoHits.length === 0) {
      const msg = this.fileIndexId !== null ? "Indexing…" : "No file matches.";
      this.gotoListEl.replaceChildren(el("div", "fileexp-empty", msg));
      return;
    }
    const frag = document.createDocumentFragment();
    this.gotoHits.forEach((hit, i) => {
      const base = basenameStart(hit.rel);
      const row = el("div", "fileexp-goto-item");
      if (i === this.gotoSel) row.classList.add("sel");
      const icon = el("span", "fileexp-icon");
      icon.innerHTML = fileIconSvg(hit.rel.slice(base));
      const name = el("span", "fileexp-goto-name");
      name.append(markUp(hit.rel.slice(base), clipRanges(hit.ranges, base, hit.rel.length)));
      const dir = el("span", "fileexp-goto-dir");
      dir.append(markUp(hit.rel.slice(0, base), clipRanges(hit.ranges, 0, base)));
      row.title = hit.rel;
      row.append(icon, name, dir);
      // The SAME wiring the listing rows get — click, double-click, and (the round-6 gap)
      // right-click. A result is a row; every affordance a row has, it has.
      this.wireRowAffordances(row, i, () => void this.openGotoHit(i));
      frag.appendChild(row);
    });
    this.gotoListEl.replaceChildren(frag);
    this.gotoListEl.querySelector(".fileexp-goto-item.sel")?.scrollIntoView({ block: "nearest" });
  }

  private updateGotoSummary(): void {
    const s = this.gotoSummaryEl;
    const indexing = this.fileIndexId !== null;
    const active = queryTerms(this.gotoInput.value).length > 0;
    if (!active) {
      s.textContent = indexing
        ? `Indexing… ${this.fileIndex.length}`
        : this.fileIndexLoaded
          ? `${this.fileIndex.length} files indexed${this.fileIndexTruncated ? " (truncated)" : ""}`
          : "";
      s.classList.toggle("truncated", this.fileIndexTruncated);
      return;
    }
    const n = this.gotoHits.length;
    const capped = n >= GOTO_RESULT_CAP;
    s.textContent = indexing
      ? `Indexing… ${n} of ${this.fileIndex.length} so far`
      : n === 0
        ? "No file matches"
        : `${n}${capped ? "+" : ""} of ${this.fileIndex.length} files${this.fileIndexTruncated ? " (truncated)" : ""}`;
    // Never a silent cut: a capped list or an incomplete index is flagged, or the
    // user reads "no such file" when there is one.
    s.classList.toggle("truncated", capped || this.fileIndexTruncated);
  }

  /** Open a Go-to-file hit: hand it to the OS default app (the manager's open — NOT
   *  an in-app editor), and navigate to its folder with it selected, so you end up
   *  oriented rather than back where you started. */
  private async openGotoHit(i: number): Promise<void> {
    const hit = this.gotoHits[i];
    if (!hit) return;
    const rel = hit.rel;
    this.gotoInput.value = "";
    this.gotoSel = 0;
    await this.openWithDefaultApp(rel);
    const dir = parentRel(rel) ?? "";
    await this.navigate(dir, rel.slice(basenameStart(rel)));
    this.listEl.focus();
  }
}

// ---------- helpers ----------

/** Turn a backend error into something worth reading, for a toast.
 *
 *  One case needs the translation. Every operation on a SYMLINK (or a Windows
 *  junction) is refused — not just navigation *through* one, but `open`, `rename`
 *  and `delete` on the link entry itself: `safe_resolve`'s `ensure_no_symlink`
 *  lstats the final component too. That is the right safety call — a junction
 *  pointing outside the root is exactly how a recursive delete would escape, and
 *  refusing the op outright means the question "does FO_DELETE recurse through a
 *  junction" never gets to be asked — but the raw error says "refusing to traverse
 *  symlink", which is both jargon and, for a link you were trying to *delete*,
 *  simply the wrong verb. Say what actually happened instead. */
function explainOpError(err: unknown, verb: string): string {
  if (errorCode(err) === "symlink") {
    return `Loomux won't ${verb} a symlink — it's shown here, but it's left alone. Use your OS file manager for links and junctions.`;
  }
  return `Couldn't ${verb}: ${errorMessage(err)}`;
}

/** Put a digest on the clipboard and say so. A hash you can see but can't copy is a hash
 *  you have to transcribe by hand, which for 128 hex characters is not a feature. */
async function copyDigest(digest: string, label: string): Promise<void> {
  const ok = await writeClipboard(digest);
  showToast(ok ? `${label} copied` : "Copy failed — the clipboard is unavailable.");
}

/** The last segment of a path — the root folder's own name, for the breadcrumb. */
function shortName(p: string): string {
  const parts = p.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

/** The parts of `ranges` inside `[from, to)`, rebased to `from`. */
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

/** `text` with `<mark>` over each range. Real text nodes, never innerHTML — a file
 *  name is untrusted input (it is whatever is on disk). */
function markUp(text: string, ranges: readonly [number, number][]): DocumentFragment {
  const frag = document.createDocumentFragment();
  let at = 0;
  for (const [start, end] of ranges) {
    if (start > at) frag.append(text.slice(at, start));
    const m = document.createElement("mark");
    m.className = "fileexp-goto-mark";
    m.textContent = text.slice(start, end);
    frag.append(m);
    at = end;
  }
  if (at < text.length) frag.append(text.slice(at));
  return frag;
}
