// A Workspace: one project tab's Grid + its own minimize dock, wrapped in a
// container that lives in the workspace stack (#63, Option A). The tab model and
// switching logic are in the DOM-free tabs.ts; this is the Grid/DOM half.
//
// Switching tabs shows/hides these containers with `display:none` — inheriting
// the maximize no-resize guarantee: a hidden container's panes report zero width
// and so issue no PTY resize (panefit.ts `shouldResizePty`; CLAUDE.md
// constraint 1). Panes are NEVER detached on hide — detaching would lose
// scrollback beyond the backend's ring — only hidden.

import { Grid, type GridLayoutNode } from "./grid";
import type { ManagedWorkspace } from "./tabs";
import { PreviewBudget, type PreviewNode } from "./tabroute";
import type { PersistedLayoutNode, PersistedPane } from "./tabstore";
import type { TabPaneInfo } from "./tabcounts";
import type { PaneBufferReport } from "./dirtystate";

/** Cap on panes serialized per preview refresh — serializing every pane every
 *  ~700ms would get expensive on a huge grid. Panes past the cap render as a
 *  titled placeholder (#63). */
const PREVIEW_PANE_CAP = 8;

export class Workspace implements ManagedWorkspace {
  /** Container in the workspace stack; hidden with display:none when inactive. */
  readonly el: HTMLElement;
  readonly grid: Grid;
  private _name = "";
  private _color: string | null = null;
  /** While tearing down, suppress the grid's "never empty" respawn so closing
   *  the last pane doesn't resurrect one mid-dispose. */
  private disposed = false;

  /** @param onEmpty invoked when the tab's grid goes empty (last pane closed)
   *  so the caller can keep the grid non-empty — the per-tab mirror of the
   *  app-wide "never leave the grid empty" rule.
   *  @param onChange invoked when this tab's pane set / layout changes (open or
   *  close) so the host can re-render the tab strip's live agent counter and
   *  re-persist the layout (#194 P4). */
  constructor(
    readonly id: string,
    onEmpty: (ws: Workspace) => void,
    onChange: (ws: Workspace) => void = () => {}
  ) {
    this.el = document.createElement("div");
    this.el.className = "workspace";
    this.el.dataset.wsId = id;

    // Each tab carries its own grid root + dock, structural clones of the
    // single ones index.html used to hold. Classes (not ids) — there are now N.
    const gridRoot = document.createElement("main");
    gridRoot.className = "grid-root";
    const dock = document.createElement("div");
    dock.className = "pane-dock";
    dock.hidden = true;
    dock.setAttribute("aria-label", "Minimized panes");
    this.el.append(gridRoot, dock);

    this.grid = new Grid(
      gridRoot,
      dock,
      () => {
        if (!this.disposed) onEmpty(this);
      },
      () => {
        if (!this.disposed) onChange(this);
      }
    );
  }

  get name(): string {
    return this._name;
  }
  set name(v: string) {
    this._name = v;
  }

  get color(): string | null {
    return this._color;
  }
  set color(v: string | null) {
    this._color = v;
  }

  setVisible(visible: boolean): void {
    this.el.style.display = visible ? "" : "none";
    // Delegate the WebGL drop/reload to the grid, which also applies it to any
    // pane opened later while still hidden (a background orchestrator spawn) —
    // so a hidden tab holds no GL contexts regardless of when its panes appear
    // (#63 GL policy). Rendering-only: PTY + buffer untouched, no resize, no
    // scrollback loss. The `display:none` above is what zeroes pane width and
    // thus suppresses PTY resizes (the no-resize invariant).
    this.grid.setHidden(!visible);
  }

  focus(): void {
    this.grid.activePane?.focus();
  }

  /** Composite the whole tab: the split tree with every pane's serialized-HTML
   *  viewport at the leaves, capped at PREVIEW_PANE_CAP panes (see
   *  ManagedWorkspace). Reads the in-memory buffers, so it's valid while hidden. */
  previewLayout(): PreviewNode | null {
    const tree = this.grid.layoutSnapshot();
    if (!tree) return null;
    const budget = new PreviewBudget(PREVIEW_PANE_CAP);
    const map = (n: GridLayoutNode): PreviewNode => {
      if (n.kind === "split") {
        return { kind: "split", dir: n.dir, weight: n.weight, children: n.children.map(map) };
      }
      const render = budget.take();
      const html = render ? n.pane.serializeViewportHtml() : "";
      return { kind: "leaf", weight: n.weight, title: n.pane.name, html, capped: !render };
    };
    return map(tree);
  }

  /** Capture this tab's whole pane layout as a serializable tree (#194 P4): the
   *  split tree with each live pane reduced to a PersistedPane (Pane.capture()).
   *  A welcome (setup) pane captures null and is pruned — a split that loses a
   *  child collapses to its surviving child, and a tab that is nothing but a
   *  welcome pane captures null (nothing worth restoring). Reads the in-memory
   *  tree + flex weights only (no geometry, no PTY), so it's safe on a hidden tab
   *  and under the no-resize invariant. */
  captureLayout(): PersistedLayoutNode | null {
    const tree = this.grid.layoutSnapshot();
    if (!tree) return null;
    const map = (n: GridLayoutNode): PersistedLayoutNode | null => {
      if (n.kind === "leaf") {
        const pane = n.pane.capture();
        return pane ? { kind: "leaf", weight: n.weight, pane } : null;
      }
      const children = n.children
        .map(map)
        .filter((c): c is PersistedLayoutNode => c !== null);
      if (children.length === 0) return null;
      if (children.length === 1) return children[0]; // a split that lost siblings collapses
      return { kind: "split", dir: n.dir, weight: n.weight, children };
    };
    return map(tree);
  }

  /** Capture the tab's minimized (docked) panes, which live outside the layout
   *  tree and so are missed by captureLayout — otherwise a docked agent session
   *  would be silently dropped on restore (#194 P4). */
  captureDocked(): PersistedPane[] {
    return this.grid
      .dockedPanes()
      .map((p) => p.capture())
      .filter((p): p is PersistedPane => p !== null);
  }

  /** Classify every pane in the tab (visible AND docked) for the per-tab agent
   *  counter / orchestration markers (#194 P4, tabcounts.ts). */
  paneInfos(): TabPaneInfo[] {
    return this.grid.allPanes().map((p) => p.tabPaneInfo());
  }

  /** Any unsaved editor edits in this tab — visible panes AND docked ones (#217).
   *  Closing the tab disposes them all, so a minimized editor's dirty buffer counts
   *  every bit as much as a visible one's. */
  hasUnsavedWork(): boolean {
    return this.grid.allPanes().some((p) => p.hasUnsavedWork());
  }

  /** Every pane's editor-buffer report, labelled with this tab's name (#219). The
   *  app-quit guard concatenates these across ALL tabs — including hidden ones, whose
   *  dirty buffers are exactly the ones a human forgets they left open — and the pure
   *  `dirtyBuffers` filter decides which ones are unsaved. Docked panes included: a
   *  minimized editor dies with the app just the same. */
  bufferReports(): PaneBufferReport[] {
    return this.grid
      .allPanes()
      .map((p) => p.bufferReport(this.name))
      .filter((r): r is PaneBufferReport => r !== null);
  }

  dispose(): void {
    this.disposed = true;
    // Kill every pane (visible or docked) so the tab's PTYs don't leak.
    for (const pane of this.grid.allPanes()) this.grid.closePane(pane, true);
    this.el.remove();
  }
}
