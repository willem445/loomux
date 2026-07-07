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
  }

  focus(): void {
    this.grid.activePane?.focus();
  }

  dispose(): void {
    this.disposed = true;
    // Kill every pane (visible or docked) so the tab's PTYs don't leak.
    for (const pane of this.grid.allPanes()) this.grid.closePane(pane, true);
    this.el.remove();
  }
}
