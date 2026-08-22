// The right-side dock (#1020 item 6, #934): git / files / editor in one
// collapsible panel down the right edge of the workspace, pointed at whichever
// pane is currently active. The pure decisions are in sidedockmodel.ts; the
// argument for the whole shape is doc/design/side-dock.md.
//
// THE ONE STRUCTURAL RULE, and #1150 reversed it. This panel is a FLEX SIBLING
// of `#grid-area` — the mirror of `#sessions` on the other edge — so opening it
// shrinks the grid and the open panes autosize to share the row. It shipped as
// the opposite: `position: absolute`, out of flow, occluding panes precisely so
// that nothing here could ever move a terminal. The human asked for the
// Sessions behaviour instead (#1150, beta1 feedback), and doc/design/side-dock.md
// carries the argument for why that is affordable now and was not before.
//
// The short form, because it is the thing to keep true: a displacing panel used
// to cost one xterm reflow plus one `ResizePseudoConsole` per pane PER FRAME of
// its transition. #1149 moved the coalescing into the fit debounce itself
// (resizeburst.ts), so a whole animated burst collapses into ONE fit per pane at
// the settled geometry. On the TOGGLE this module inherits that by doing nothing
// at all: the panes' own `ResizeObserver`s see the flex row change and the
// shared policy decides the rest. There is no coalescer here and no bracketing
// of the toggle, and anything added would be a second mechanism covering one
// gesture instead of the one mechanism covering every consumer.
//
// "Nothing on the resize path" is NOT true of this module as a whole, and the
// boundary is worth knowing before editing it. `roomObs` (see `onRoomChanged`)
// writes the column's width whenever the ROOM changes, which is what keeps a
// squeezed dock narrow instead of cropped — and a room that is itself animating
// (a `#sessions` slide) therefore re-targets this panel's own 240 ms ease, so
// the composite burst can outlast the coalescer's 400 ms ceiling and take one
// mid-slide fit per pane. Open as #1203, argued in doc/design/side-dock.md,
// deferred rather than fixed blind: the chain is a browser behaviour nothing in
// this repo's test rig can execute.
//
// The one gesture that IS bracketed is the grip drag, and it is bracketed with
// the mechanism that already exists for divider drags (`beginResizeHold` /
// `endResizeHold`, #432) rather than a new one — see `beginResize`.
//
// WHY IT OWNS ITS OWN VIEW INSTANCES. `Pane` already builds a `GitView` and a
// `FileEditView` per pane, for its Alt+G / Alt+F overlays and its #361 embed
// slots. The dock does NOT reach for those: they belong to one pane, and the
// dock belongs to the app — it has to survive the pane it is following being
// closed, and it has to keep showing one folder while the human clicks through
// four panes in three tabs. So it constructs its own, exactly the way
// `Pane.startContent` constructs a content pane's, and the two never interact.

import { FileEditView } from "./fileedit";
import { FileExplorerView } from "./fileexplorer";
import { GitView } from "./gitview";
import { icon } from "./icons";
import { startDragSession } from "./dragsession";
import type { PaneBufferReport } from "./dirtystate";
import {
  DEFAULT_DOCK_PREFS,
  DOCK_PREFS_KEY,
  DOCK_TABS,
  clampDockWidth,
  decideFollow,
  decideViewSync,
  decodeDockPrefs,
  dockBoxes,
  encodeDockPrefs,
  normalizeDockRoot,
  type DockBoxes,
  type DockPrefs,
  type DockTab,
} from "./sidedockmodel";

/** How long the dock waits after an active-pane change before re-rooting.
 *
 *  Trailing-edge, and the value is chosen for a human walking the grid with
 *  Alt+arrow: each step fires `Grid.setActive`, and rebuilding a git view per
 *  keystroke would be both slow and pointless, since only where they STOP
 *  matters. The repo's other gesture-paced debounces (`fileedit.ts`'s search at
 *  300ms) sit in the same range. */
const FOLLOW_DEBOUNCE_MS = 250;

/** What the toggle says when the row cannot seat a readable dock (#1150, review
 *  finding 3). It names both ways out, because "no room" on its own reads as a
 *  failure rather than as a thing the human can do something about. */
const NO_ROOM_REASON =
  "Side dock — not enough room at this window size. Close the session browser, or widen the window.";

/** What the dock needs from the app: the active pane's cwd, pulled on demand.
 *
 *  Pulled, never pushed — the same closure shape (`getCwd: () => this.cwdRaw`)
 *  every other consumer of a pane's directory uses, so the dock can never hold
 *  a stale snapshot of a value that moves. */
export interface SideDockHost {
  /** The active tab's active pane's working directory, or null when it has
   *  none (an SSH pane, whose OSC 7 names a remote path; a welcome pane). */
  activeCwd(): string | null;

  /**
   * Begin coalescing every visible pane's PTY resize for the duration of a
   * gesture, and return the release.
   *
   * Only the grip drag uses this, and only since #1150 made the grip a real
   * divider with terminals on the other side of it. A drag has a start and an
   * end, which is the shape `Pane.beginResizeHold` was built for (#432): xterm
   * keeps re-fitting so the terminal looks right throughout, and the
   * `ResizePseudoConsole` call is withheld until release, collapsing a whole
   * drag into one. The open/close toggle deliberately does NOT use it — a CSS
   * transition has no end to hook, and `resizeburst.ts` already coalesces it
   * without being told.
   *
   * The dock cannot reach panes itself (it is app-level chrome, and which panes
   * exist is the active tab's business), so the host supplies this.
   */
  holdPaneResizes(): () => void;

  /**
   * Report whether the dock can be shown at all in the room the row currently
   * has, so the top-bar toggle can say so instead of appearing dead.
   *
   * Called only when the verdict changes. `reason` is null when it is
   * available; when it is not, it is the sentence to put on the control.
   */
  setToggleAvailability(available: boolean, reason: string | null): void;
}

/** One hosted view, plus the root it was constructed at. */
interface Hosted {
  el: HTMLElement;
  /** The dock root this instance was built for — the input to `decideViewSync`. */
  builtRoot: string;
  dispose(): void;
  /** Unsaved edits? Only the editor can answer yes. */
  dirty(): boolean;
  /** Re-read the repo/disk in place, cheaply. Absent where a refresh would
   *  cost the human their place — see `refreshActiveView`. */
  refresh?(): void;
}

const TAB_LABEL: Record<DockTab, string> = { git: "Git", files: "Files", editor: "Editor" };

/** Tab marks, through the icon registry's documented role mapping — never a hue
 *  picked here (ui-redesign.md maintainability rule 3). `git-graph` is `vcs`,
 *  `folder-open` is `workspace`, `file-pen` is `source`: the same three
 *  questions these tabs answer. */
const TAB_ICON = { git: "git-graph", files: "folder-open", editor: "file-pen" } as const;

export class SideDock {
  /** The dock's own column in `#workspace`'s flex row. */
  readonly el: HTMLElement;

  /** The panel inside that column, at a fixed width the column clips (#1150).
   *  Everything except the grip lives in here: the grip has to stay on the
   *  column's live left edge, which the panel's edge stops being the moment the
   *  column is narrower than the panel. */
  private readonly innerEl: HTMLElement;
  private readonly bodyEl: HTMLElement;
  private readonly rootChipEl: HTMLElement;
  private readonly holdEl: HTMLElement;
  private readonly tabBtns = new Map<DockTab, HTMLButtonElement>();

  private prefs: DockPrefs;
  /** Where the dock is pointed. Null until it first adopts one. Nothing is
   *  recorded here while the dock is closed — see `followActivePane`. */
  private dockRoot: string | null = null;
  private readonly views = new Map<DockTab, Hosted>();
  private followTimer: number | undefined;

  /** The width the dock and the grid share, cached so no per-frame path has to
   *  measure. Infinite until the first measurement — an unmeasured room must
   *  never read as a small one (`dockBoxes`). */
  private room = Number.POSITIVE_INFINITY;
  /** The last geometry actually written, so a room change that moves nothing
   *  writes nothing. */
  private applied: DockBoxes | null = null;
  /** Is the row too small to seat a readable dock? Mirrors `applied.starved`. */
  private starved = false;
  /** Fires on any change to the room — a window resize, or #sessions opening. */
  private readonly roomObs: ResizeObserver;

  /** Is the dock actually on screen? Open is the human's intent; this is the
   *  intent AND the room to honour it, and it is what every "no work while it is
   *  not showing" guard keys off — a starved dock builds no views for the same
   *  reason a closed one does not. */
  private get shown(): boolean {
    return this.prefs.open && !this.starved;
  }

  constructor(
    private readonly workspaceEl: HTMLElement,
    /** `#grid-area` — the sibling the dock shares the row with. Held to measure
     *  that shared room, which is the bound on every width here. */
    private readonly gridEl: HTMLElement,
    private readonly host: SideDockHost
  ) {
    this.prefs = loadPrefs();

    this.el = document.createElement("aside");
    this.el.className = "sidedock";
    this.el.setAttribute("aria-label", "Side dock");

    this.innerEl = document.createElement("div");
    this.innerEl.className = "sidedock-inner";

    const grip = document.createElement("div");
    grip.className = "sidedock-grip";
    grip.title = "Drag to resize";
    grip.addEventListener("mousedown", (e) => this.beginResize(e));

    const head = document.createElement("header");
    head.className = "sidedock-head";
    const tabsEl = document.createElement("div");
    tabsEl.className = "sidedock-tabs";
    tabsEl.setAttribute("role", "tablist");
    for (const tab of DOCK_TABS) {
      const btn = document.createElement("button");
      btn.className = "sidedock-tab";
      btn.type = "button";
      btn.setAttribute("role", "tab");
      btn.innerHTML = `${icon(TAB_ICON[tab])}<span>${TAB_LABEL[tab]}</span>`;
      btn.addEventListener("click", () => this.selectTab(tab));
      this.tabBtns.set(tab, btn);
      tabsEl.appendChild(btn);
    }
    const closeBtn = document.createElement("button");
    closeBtn.className = "sidedock-close";
    closeBtn.type = "button";
    closeBtn.textContent = "✕";
    closeBtn.title = "Close the side dock";
    closeBtn.addEventListener("click", () => this.close());
    head.append(tabsEl, closeBtn);

    this.rootChipEl = document.createElement("div");
    this.rootChipEl.className = "sidedock-root";

    this.holdEl = document.createElement("div");
    this.holdEl.className = "sidedock-hold";
    this.holdEl.hidden = true;

    this.bodyEl = document.createElement("div");
    this.bodyEl.className = "sidedock-body";

    this.innerEl.append(head, this.rootChipEl, this.holdEl, this.bodyEl);
    this.el.append(grip, this.innerEl);
    // LAST in the row, after #grid-area — the dock is the right-hand column and
    // the flex order is the visual one.
    this.workspaceEl.appendChild(this.el);
    // Measure BEFORE the first apply, so a dock restored open into a window too
    // small for it is never painted as a sliver on the way to being hidden.
    this.measureRoom();
    // Before the first paint, so a dock restored open is simply open and a dock
    // restored closed never animates itself shut in front of the human.
    this.applyOpenState();
    this.roomObs = new ResizeObserver(() => this.onRoomChanged());
    this.roomObs.observe(this.gridEl);

    this.syncTabButtons();
    // Boot with whatever pane is active. A dock restored CLOSED returns from
    // this immediately and constructs nothing at all until it is opened.
    this.followActivePane(true);
  }

  get open(): boolean {
    return this.prefs.open;
  }

  toggle(): void {
    if (this.prefs.open) this.close();
    else this.show();
  }

  show(): void {
    if (this.prefs.open) {
      this.syncActiveView();
      return;
    }
    this.prefs.open = true;
    this.applyOpenState();
    savePrefs(this.prefs);
    // `open` is set FIRST, because every follow path is guarded on it: this is
    // the call that pulls the live cwd for the first time and adopts it. Reading
    // it now, rather than replaying a root captured while hidden, is why the
    // dock needs no pending-root state at all.
    this.followActivePane(true);
    this.syncActiveView();
  }

  close(): void {
    if (!this.prefs.open) return;
    this.prefs.open = false;
    this.applyOpenState();
    savePrefs(this.prefs);
    // Nothing is disposed. Closing is hiding: it must not destroy the editor's
    // buffer, and it should not throw away a git view's loaded log either. The
    // views stop being asked to do anything (`syncActiveView` is only reached
    // through paths that require `open`), which is the whole of "no work when
    // closed".
    this.clearFollowTimer();
  }

  /**
   * The active pane changed (or the active TAB did) — re-point the dock.
   *
   * Debounced, because `Grid.setActive` fires on far more than a deliberate
   * click: walking the grid with Alt+arrow, closing a pane and inheriting its
   * neighbour, and finishing a drag all reach it. `immediate` is for the two
   * moments where a delay would be visible as a flicker rather than felt as
   * smoothness: construction, and the human opening the dock.
   */
  followActivePane(immediate = false): void {
    this.clearFollowTimer();
    // A dock that is not showing does nothing at all — it does not even arm a
    // timer, and it records no pending root. `show()` pulls the live cwd, which
    // is a better answer than replaying one captured while it was hidden, and
    // the same holds for a starved dock coming back into a room that fits.
    if (!this.shown) return;
    if (immediate) {
      this.applyFollow();
      return;
    }
    this.followTimer = window.setTimeout(() => this.applyFollow(), FOLLOW_DEBOUNCE_MS);
  }

  /**
   * The dock's unsaved-work report for the app-quit guard (#219).
   *
   * The dock's editor is a buffer holder that lives OUTSIDE every pane, so the
   * quit sweep — which walks tabs, then panes — cannot reach it on its own. A
   * quit that misses a holder is a quit that silently destroys it, so the sweep
   * concatenates this (main.ts's `unsavedBuffers`). Null when the dock has
   * never built an editor.
   */
  bufferReport(): PaneBufferReport | null {
    const hosted = this.views.get("editor");
    if (!hosted) return null;
    const view = this.editorView;
    if (!view) return null;
    const report = view.bufferReport();
    if (!report) return null;
    return {
      tab: hosted.builtRoot,
      pane: "editor",
      host: "sidedock",
      file: report.file,
      dirty: report.dirty,
    };
  }

  dispose(): void {
    this.roomObs.disconnect();
    this.clearFollowTimer();
    for (const hosted of this.views.values()) hosted.dispose();
    this.views.clear();
    this.el.remove();
  }

  // ---------- internals ----------

  /** The live `FileEditView`, if one is built. Held separately from `Hosted`
   *  because it is the only view with a typed question worth asking. */
  private editor: FileEditView | null = null;

  private get editorView(): FileEditView | null {
    return this.views.has("editor") ? this.editor : null;
  }

  private clearFollowTimer(): void {
    if (this.followTimer !== undefined) {
      clearTimeout(this.followTimer);
      this.followTimer = undefined;
    }
  }

  private applyFollow(): void {
    this.followTimer = undefined;
    const action = decideFollow({
      open: this.shown,
      dockRoot: this.dockRoot,
      paneCwd: this.host.activeCwd(),
    });
    if (action.kind === "none") {
      // Same folder (or nothing to follow) — no rebuild. But coming back to a
      // pane you just committed in should show the commit, so the active view
      // gets a cheap live refresh rather than staying the snapshot it was built
      // as (#1097 rev-767 N2). Guarded on `open` by `followActivePane`, and
      // `refresh` is a no-op for every view that cannot do it without cost.
      this.refreshActiveView();
      return;
    }
    this.dockRoot = action.root;
    this.renderRootChip();
    this.syncActiveView();
  }

  /**
   * Ask the active view to re-read the repo/disk, without rebuilding it.
   *
   * Only the git tab has one, and deliberately: `GitView.notifyPrompt()` is the
   * throttled (500ms) refresh `Pane` already drives from OSC 7, and it no-ops
   * unless the view is visible. The explorer and the editor have none — for
   * them "refresh" would mean re-navigating to the root or reloading the tree,
   * which throws away the human's place in it. Those keep their own explicit
   * refresh affordances, which is the right shape for a destructive reload.
   */
  private refreshActiveView(): void {
    if (!this.shown) return;
    this.views.get(this.prefs.tab)?.refresh?.();
  }

  private selectTab(tab: DockTab): void {
    this.prefs.tab = tab;
    savePrefs(this.prefs);
    this.syncTabButtons();
    this.syncActiveView();
    // Selecting a tab whose view is already at the right root would otherwise
    // just unhide a snapshot (#1097 rev-767 N2).
    this.refreshActiveView();
  }

  private syncTabButtons(): void {
    for (const [tab, btn] of this.tabBtns) {
      const active = tab === this.prefs.tab;
      btn.classList.toggle("active", active);
      btn.setAttribute("aria-selected", String(active));
    }
  }

  /**
   * Bring the ACTIVE tab's view in line with the dock's root, and show only it.
   *
   * Only the active tab is ever synced. An inactive tab's view is left exactly
   * as it was — which is what makes a hidden dirty editor safe, and what keeps a
   * root change from rebuilding three views the human is not looking at. Each
   * one catches up the next time its tab is selected, which is also the moment
   * a previously-held editor gets to re-ask whether it is clean now.
   */
  private syncActiveView(): void {
    if (!this.shown) return;
    const tab = this.prefs.tab;
    const hosted = this.views.get(tab);
    const action = decideViewSync({
      dockRoot: this.dockRoot,
      builtRoot: hosted?.builtRoot ?? null,
      dirty: hosted?.dirty() ?? false,
    });

    if (action === "rebuild") {
      hosted?.dispose();
      this.views.delete(tab);
      if (tab === "editor") this.editor = null;
    }
    if (action === "build" || action === "rebuild") {
      const root = normalizeDockRoot(this.dockRoot);
      if (root !== null) this.views.set(tab, this.buildView(tab, root));
    }

    for (const [key, v] of this.views) v.el.hidden = key !== tab;
    this.renderHoldNotice(action === "hold");
    this.renderRootChip();
  }

  /** Construct one tab's view at `root`, attached and shown.
   *
   *  ATTACH, THEN `show()`. `GitView.show()` clamps its own sub-panes against
   *  its container's live size, so showing it before it is in the document
   *  measures a zero-width box — the same ordering `Pane.startContent` calls
   *  out at its own `appendChild`. */
  private buildView(tab: DockTab, root: string): Hosted {
    if (tab === "git") {
      // `embedded: true` drops the view's own ✕ and its Escape-to-close
      // binding: the dock owns closing, and a second close affordance inside a
      // panel that has one in its header is how the #361 demo found a dead
      // empty rectangle. `onClose` is consequently never called.
      const view = new GitView({ getCwd: () => root, onClose: () => {}, embedded: true });
      this.bodyEl.appendChild(view.el);
      view.show();
      return {
        el: view.el,
        builtRoot: root,
        dispose: () => view.dispose(),
        dirty: () => false,
        // Throttled (500ms) and a no-op while hidden — the same call Pane makes
        // from OSC 7 for its own instance.
        refresh: () => view.notifyPrompt(),
      };
    }
    if (tab === "files") {
      const view = new FileExplorerView({
        getRoot: () => root,
        // The explorer's own 📁 picker re-roots the DOCK, not just itself —
        // otherwise the next active-pane change would silently yank the human
        // back out of the folder they just chose, and they would have no way to
        // tell which of the two roots the git tab was showing.
        onRootChanged: (picked) => this.adoptRoot(picked, true),
        // "Open in editor pane" inside the dock means the DOCK's editor, not a
        // whole new pane: the dock is the small-surface answer, and spawning a
        // pane from it would be answering a different question than the one the
        // human asked by opening a sidebar.
        onOpenEditorPane: (req) => this.openInDockEditor(req),
      });
      this.bodyEl.appendChild(view.el);
      view.show();
      return { el: view.el, builtRoot: root, dispose: () => view.dispose(), dirty: () => false };
    }
    const view = new FileEditView({
      getCwd: () => root,
      onClose: () => {},
      embedded: true,
      onRootChanged: (picked) => this.adoptRoot(picked, true),
    });
    this.editor = view;
    this.bodyEl.appendChild(view.el);
    view.show();
    return {
      el: view.el,
      builtRoot: root,
      dispose: () => {
        view.dispose();
        if (this.editor === view) this.editor = null;
      },
      dirty: () => view.dirty,
    };
  }

  /**
   * Re-root the whole dock.
   *
   * `byActiveView` is true when the ACTIVE tab's own view is what re-rooted —
   * its 📁 picker, which re-loads that view internally before telling us. That
   * view is therefore already correct, and re-syncing it would rebuild it for
   * nothing, so its `builtRoot` is stamped to match. Every OTHER view is now
   * stale and catches up when its tab is next selected.
   *
   * It is false when the dock is re-rooted around a view that did NOT move —
   * "open this SUBFOLDER in the editor" from the files tab's context menu, where
   * the explorer stays listing the parent. Stamping there would record a root
   * the view is not actually at, and `decideViewSync` would then never rebuild
   * it: the files tab would sit on the parent folder forever, claiming to be on
   * the child.
   */
  private adoptRoot(picked: string, byActiveView: boolean): void {
    const root = normalizeDockRoot(picked);
    if (root === null || root === this.dockRoot) return;
    this.dockRoot = root;
    if (byActiveView) {
      const hosted = this.views.get(this.prefs.tab);
      if (hosted) hosted.builtRoot = root;
    }
    this.renderRootChip();
  }

  /** The files tab asked to edit a file — open it in the dock's editor tab. */
  private openInDockEditor(req: { root: string; file: string | null }): void {
    this.adoptRoot(req.root, false);
    this.prefs.tab = "editor";
    savePrefs(this.prefs);
    this.syncTabButtons();
    this.syncActiveView();
    if (!req.file) return;
    // `req.file` is relative to `req.root`. If the editor is HOLDING (a dirty
    // buffer kept it at some earlier root, `decideViewSync`), resolving that
    // relative path against the root it is actually on would open a different
    // file — or silently fail — so the open is skipped rather than guessed. The
    // hold notice already explains why the tab is not where the human expected.
    const hosted = this.views.get("editor");
    if (!hosted || hosted.builtRoot !== this.dockRoot) return;
    // `openPath` awaits the editor's own tree load internally, so calling it
    // straight after the view is built is safe.
    void this.editorView?.openPath(req.file);
  }

  private renderRootChip(): void {
    const root = this.dockRoot;
    if (root === null) {
      this.rootChipEl.innerHTML = `${icon("folder", 12)}<span class="sidedock-root-none">no folder yet</span>`;
      this.rootChipEl.title = "The active pane has no local working directory";
      return;
    }
    const leaf = root.split(/[\\/]/).filter(Boolean).pop() ?? root;
    this.rootChipEl.innerHTML = `${icon("folder", 12)}<span></span>`;
    this.rootChipEl.querySelector("span")!.textContent = leaf;
    this.rootChipEl.title = root;
  }

  /** The "this tab stopped following" notice — see `decideViewSync`'s `hold`. */
  private renderHoldNotice(held: boolean): void {
    this.holdEl.hidden = !held;
    if (!held) return;
    const at = this.views.get(this.prefs.tab)?.builtRoot ?? "";
    this.holdEl.textContent = `Unsaved edits — still showing ${at}. Save or discard to follow the active pane.`;
  }

  private beginResize(e: MouseEvent): void {
    e.preventDefault();
    const startX = e.clientX;
    const startW = this.el.offsetWidth;
    this.el.classList.add("resizing");
    // The grip is a DIVIDER now (#1150): the grid is on the other side of it, so
    // every mousemove re-fits every pane. Bracketed exactly the way grid.ts
    // brackets its split divider — xterm keeps fitting so the terminals track
    // the drag, and the ConPTY resize is withheld until release. Without it a
    // drag costs one ResizePseudoConsole per pane per FIT_MAX_WAIT_MS (the
    // coalescer's ceiling, which is what a gesture with no settled geometry
    // resolves to) for as long as the human holds the mouse.
    //
    // The release is captured ONCE here rather than re-derived in `onEnd`, so
    // begin/end stay balanced 1:1 per pane even if the pane set changes
    // mid-drag — the same reason grid.ts captures its pane list up front.
    const release = this.host.holdPaneResizes();
    startDragSession({
      // Dragging LEFT widens: the dock is pinned to the right edge.
      onMove: (ev) =>
        this.applyWidth(clampDockWidth(startW + (startX - ev.clientX), this.room)),
      // Fires on mouseup OR on a drag that ends without one (window blur,
      // Escape) — `startDragSession`'s whole job — so the hold cannot strand.
      onEnd: () => {
        this.el.classList.remove("resizing");
        release();
        // Persist on release only, never per mousemove — the same discipline
        // every other divider in this codebase already keeps.
        savePrefs(this.prefs);
      },
    });
  }

  private applyWidth(px: number): void {
    this.prefs.width = px;
    this.applyBoxes();
  }

  /**
   * The width the dock and the terminal grid SHARE — the quantity every clamp
   * here is against, and never the workspace's own width.
   *
   * `#sessions` is a `flex: none` sibling of both, so it takes its 344px off the
   * top and the two of them divide what is left. That sum does not move as the
   * divider between them moves, which is what makes it safe to measure once and
   * hold for the length of a drag, and what stops writing the dock's own width
   * from feeding back into the measurement (see `onRoomChanged`).
   *
   * Measured with `offsetWidth` on both, so a border counts as the room it
   * occupies.
   */
  private measureRoom(): void {
    this.room = this.gridEl.offsetWidth + this.el.offsetWidth;
  }

  /**
   * Push the dock's two widths into the DOM: the column's, and the width of the
   * panel the column clips.
   *
   * ONE place, and both from ONE `dockBoxes` call, because the column and its
   * contents have to agree about what "the dock's width" is.
   *
   * Two style writes and nothing else, because the drag calls this on every
   * mousemove: the open/closed half below is the part that must not be on a
   * per-frame path. It reads the CACHED room rather than measuring, so a drag
   * costs no layout reads at all.
   */
  private applyBoxes(): DockBoxes {
    const boxes = dockBoxes(this.prefs.open, this.prefs.width, this.room);
    this.applied = boxes;
    this.starved = boxes.starved;
    this.el.style.width = `${boxes.columnPx}px`;
    this.innerEl.style.width = `${boxes.contentPx}px`;
    return boxes;
  }

  /**
   * Everything that changes when the dock opens, closes, or crosses the starve
   * threshold — the widths above, the collapse class, the accessibility half,
   * and the toggle button's availability. Deliberately NOT on the drag path or
   * on every room measurement: re-writing `aria-hidden` sixty times a second
   * during a resize is churn on the two gestures this change already made more
   * expensive.
   *
   * **A hidden dock is `inert`, not `hidden`.** It used to be `el.hidden`, which
   * is `display: none` — nothing to tab into, nothing for a screen reader. A
   * zero-width column with `overflow: hidden` is only VISUALLY empty: without
   * this, the dock's buttons and its editor's buffer would still be in the tab
   * order and still be announced, which is a regression the human would hit long
   * before they worked out why. Applied immediately rather than on
   * `transitionend`: a panel on its way out should stop taking input at the
   * moment it is dismissed, and nothing has to be scheduled or cleaned up.
   *
   * The three states collapse to one question — is there a column? — so a dock
   * the human opened into a room too small for it is inert for the same reason
   * and by the same code as one they closed.
   */
  private applyOpenState(): void {
    this.applyShownState(this.applyBoxes());
  }

  /** The open/closed half, for geometry that has already been written. Split
   *  out so `onRoomChanged` can write the widths on every delivery and this
   *  only when the answer to "is there a column?" actually changed — and so the
   *  whole class derives its geometry from ONE `dockBoxes` call. */
  private applyShownState(boxes: DockBoxes): void {
    const showing = boxes.columnPx > 0;
    this.el.classList.toggle("collapsed", !showing);
    this.el.inert = !showing;
    this.el.setAttribute("aria-hidden", String(!showing));
    // A toggle that silently does nothing is the other way to look broken, so
    // the button says why instead of pretending it can help.
    this.host.setToggleAvailability(!boxes.starved, boxes.starved ? NO_ROOM_REASON : null);
  }

  /**
   * The room changed — the window was resized, or `#sessions` was toggled.
   *
   * Two reasons this exists rather than leaving the layout to CSS. The panel
   * inside the column is laid out at a FIXED width, so it cannot adapt to a
   * column the flex row squeezed; only this can re-lay it out. And the starve
   * verdict is a threshold, so nothing in CSS can express it.
   *
   * **It cannot oscillate**, which is the hazard with any observer that writes
   * geometry: the measured room is `grid + dock`, which is invariant to how wide
   * this makes the dock, so a write here produces at most one more delivery that
   * measures the same room, computes the same boxes, and returns without
   * writing.
   *
   * The open/closed half runs only when the starve verdict actually flips, so a
   * window drag that never crosses the threshold costs two style writes per
   * frame at most — and nothing at all once the widths stop moving.
   */
  private onRoomChanged(): void {
    const before = this.applied;
    this.measureRoom();
    const boxes = this.applyBoxes();
    if (before !== null && before.starved === boxes.starved) return;
    this.applyShownState(boxes);
    // Crossing back into a room that fits is the same work `show()` does: while
    // starved the dock builds nothing, so it has a root and views to catch up on.
    if (this.shown) {
      this.followActivePane(true);
      this.syncActiveView();
    }
  }
}

// ---------- persistence ----------

/** localStorage is unavailable under `node --test`, and this module is imported
 *  by nothing that runs there — but the same defensive shape `agents.ts` uses is
 *  cheap, and a storage-disabled webview should degrade to "the dock works,
 *  it just forgets" rather than to a boot-time throw. */
function loadPrefs(): DockPrefs {
  try {
    return decodeDockPrefs(localStorage.getItem(DOCK_PREFS_KEY));
  } catch {
    return { ...DEFAULT_DOCK_PREFS };
  }
}

function savePrefs(p: DockPrefs): void {
  try {
    localStorage.setItem(DOCK_PREFS_KEY, encodeDockPrefs(p));
  } catch {
    /* storage disabled — the dock still works, it just forgets. */
  }
}
