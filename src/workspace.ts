// A Workspace: one project tab's Grid + its own minimize dock, wrapped in a
// container that lives in the workspace stack (#63, Option A). The tab model and
// switching logic are in the DOM-free tabs.ts; this is the Grid/DOM half.
//
// Switching tabs shows/hides these containers with `display:none` — inheriting
// the maximize no-resize guarantee: a hidden container's panes report zero width
// and so issue no PTY resize (panefit.ts `shouldResizePty`; CLAUDE.md
// constraint 1). Panes are NEVER detached on hide — detaching would lose
// scrollback beyond the backend's ring — only hidden.

import { Grid } from "./grid";
import type { Pane } from "./pane";
import type { ManagedWorkspace } from "./tabs";

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
   *  app-wide "never leave the grid empty" rule. */
  constructor(
    readonly id: string,
    onEmpty: (ws: Workspace) => void
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

    this.grid = new Grid(gridRoot, dock, () => {
      if (!this.disposed) onEmpty(this);
    });
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
    // Drop each pane's WebGL context while hidden (freeing it for the active
    // tab, cutting idle VRAM), reload it on show. Rendering-only — the PTY and
    // buffer are untouched, so no resize / no scrollback loss (#63 phase 4).
    for (const pane of this.grid.allPanes()) pane.setHidden(!visible);
  }

  focus(): void {
    this.grid.activePane?.focus();
  }

  /** The pane a hover thumbnail shows: prefer one that needs the human (that's
   *  what you'd want to peek at), else the active pane, else the first. */
  private previewPane(): Pane | null {
    const panes = this.grid.allPanes();
    return (
      panes.find((p) => p.attention !== null) ?? this.grid.activePane ?? panes[0] ?? null
    );
  }

  /** Serialize the preview pane's full current viewport (see ManagedWorkspace). */
  livePreview(): string {
    return this.previewPane()?.serializeViewport() ?? "";
  }

  dispose(): void {
    this.disposed = true;
    // Kill every pane (visible or docked) so the tab's PTYs don't leak.
    for (const pane of this.grid.allPanes()) this.grid.closePane(pane, true);
    this.el.remove();
  }
}
