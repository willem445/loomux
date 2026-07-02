// Split-tree layout: panes live at the leaves, splits are flex rows or
// columns with draggable dividers. Splitting in the same direction as the
// parent inserts a sibling (so repeated splits form an even matrix rather
// than a lopsided staircase); splitting across creates a nested split.

import { Pane, type PaneEvents, type PaneOptions } from "./pane";

type Dir = "row" | "column";

interface LeafNode {
  kind: "leaf";
  pane: Pane;
  parent: SplitNode | null;
}

interface SplitNode {
  kind: "split";
  dir: Dir;
  el: HTMLElement;
  children: TreeNode[];
  parent: SplitNode | null;
}

type TreeNode = LeafNode | SplitNode;

const MIN_PANE_PX = 80;

const nodeEl = (n: TreeNode): HTMLElement => (n.kind === "leaf" ? n.pane.el : n.el);

export class Grid {
  private root: TreeNode | null = null;
  private active: Pane | null = null;
  private leaves = new Map<Pane, LeafNode>();

  constructor(
    private rootEl: HTMLElement,
    private onEmpty: () => void
  ) {}

  get activePane(): Pane | null {
    return this.active;
  }

  get paneCount(): number {
    return this.leaves.size;
  }

  panes(): Pane[] {
    return [...this.leaves.keys()];
  }

  findByPtyId(ptyId: number): Pane | undefined {
    return this.panes().find((p) => p.ptyId === ptyId);
  }

  /** Create a pane and place it: first pane fills the grid, later panes
   *  split relative to `relativeTo` (default: the active pane). */
  async openPane(
    opts: PaneOptions,
    events: PaneEvents,
    dir: Dir = "row",
    relativeTo?: Pane
  ): Promise<Pane> {
    const pane = new Pane(events);
    const leaf: LeafNode = { kind: "leaf", pane, parent: null };
    this.leaves.set(pane, leaf);

    const target = relativeTo ?? this.active;
    if (!this.root || !target) {
      this.root = leaf;
      pane.el.style.flex = "1 1 0";
      this.rootEl.appendChild(pane.el);
    } else {
      this.insertBeside(this.leaves.get(target)!, leaf, dir);
    }

    this.setActive(pane);
    await pane.start(opts);
    return pane;
  }

  private insertBeside(at: LeafNode, leaf: LeafNode, dir: Dir): void {
    const parent = at.parent;
    if (parent && parent.dir === dir) {
      // Same-direction split: add a sibling right after the target,
      // giving it an equal share of the row/column.
      const idx = parent.children.indexOf(at);
      parent.children.splice(idx + 1, 0, leaf);
      leaf.parent = parent;
      const share = 1 / (parent.children.length - 1);
      for (const c of parent.children) nodeEl(c).style.flex ||= "1 1 0";
      nodeEl(leaf).style.flex = `${share} 1 0`;
      this.renderSplit(parent);
    } else {
      // Cross-direction: replace the leaf with a new 2-way split.
      const split: SplitNode = {
        kind: "split",
        dir,
        el: document.createElement("div"),
        children: [at, leaf],
        parent,
      };
      split.el.className = `split ${dir}`;
      split.el.style.flex = at.pane.el.style.flex || "1 1 0";
      if (parent) {
        parent.children[parent.children.indexOf(at)] = split;
        this.renderSplit(parent);
      } else {
        this.root = split;
        this.rootEl.replaceChildren(split.el);
      }
      at.parent = split;
      leaf.parent = split;
      at.pane.el.style.flex = "1 1 0";
      leaf.pane.el.style.flex = "1 1 0";
      this.renderSplit(split);
    }
  }

  /** Remove a pane from the tree. `killBackend=false` when the process
   *  already exited on its own. */
  closePane(pane: Pane, killBackend = true): void {
    const leaf = this.leaves.get(pane);
    if (!leaf) return;
    this.leaves.delete(pane);

    const parent = leaf.parent;
    pane.dispose(killBackend);

    if (!parent) {
      this.root = null;
    } else {
      parent.children.splice(parent.children.indexOf(leaf), 1);
      if (parent.children.length === 1) {
        this.collapse(parent);
      } else {
        this.renderSplit(parent);
      }
    }

    if (this.active === pane) {
      this.active = null;
      const next = this.panes()[0];
      if (next) this.setActive(next);
    }
    if (!this.root) this.onEmpty();
  }

  /** Replace a single-child split with its child. */
  private collapse(split: SplitNode): void {
    const child = split.children[0];
    child.parent = split.parent;
    nodeEl(child).style.flex = split.el.style.flex || "1 1 0";
    if (split.parent) {
      split.parent.children[split.parent.children.indexOf(split)] = child;
      this.renderSplit(split.parent);
    } else {
      this.root = child;
      this.rootEl.replaceChildren(nodeEl(child));
    }
    split.el.remove();
  }

  /** (Re)attach a split's children and dividers to its element. */
  private renderSplit(split: SplitNode): void {
    split.el.replaceChildren();
    split.children.forEach((child, i) => {
      if (i > 0) split.el.appendChild(this.makeDivider(split, i));
      split.el.appendChild(nodeEl(child));
    });
  }

  private makeDivider(split: SplitNode, index: number): HTMLElement {
    const div = document.createElement("div");
    div.className = "divider";
    div.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const horizontal = split.dir === "row";
      const before = nodeEl(split.children[index - 1]);
      const after = nodeEl(split.children[index]);
      const startPos = horizontal ? e.clientX : e.clientY;
      const sizeB = horizontal ? before.offsetWidth : before.offsetHeight;
      const sizeA = horizontal ? after.offsetWidth : after.offsetHeight;
      const growB = parseFloat(before.style.flexGrow || "1");
      const growA = parseFloat(after.style.flexGrow || "1");
      const total = sizeB + sizeA;
      const growTotal = growB + growA;
      div.classList.add("dragging");

      const move = (ev: MouseEvent) => {
        const raw = (horizontal ? ev.clientX : ev.clientY) - startPos;
        const delta = Math.max(
          MIN_PANE_PX - sizeB,
          Math.min(sizeA - MIN_PANE_PX, raw)
        );
        const newB = ((sizeB + delta) / total) * growTotal;
        before.style.flex = `${newB} 1 0`;
        after.style.flex = `${growTotal - newB} 1 0`;
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

  setActive(pane: Pane): void {
    if (this.active === pane) return;
    this.active?.setActive(false);
    this.active = pane;
    pane.setActive(true);
  }

  /** Move focus to the geometrically nearest pane in a direction. */
  moveFocus(direction: "left" | "right" | "up" | "down"): void {
    if (!this.active) return;
    const from = this.active.el.getBoundingClientRect();
    const cx = from.left + from.width / 2;
    const cy = from.top + from.height / 2;

    let best: Pane | null = null;
    let bestDist = Infinity;
    for (const pane of this.leaves.keys()) {
      if (pane === this.active) continue;
      const r = pane.el.getBoundingClientRect();
      const px = r.left + r.width / 2;
      const py = r.top + r.height / 2;
      const ok =
        (direction === "left" && px < cx - 1) ||
        (direction === "right" && px > cx + 1) ||
        (direction === "up" && py < cy - 1) ||
        (direction === "down" && py > cy + 1);
      if (!ok) continue;
      const primary =
        direction === "left" || direction === "right"
          ? Math.abs(px - cx)
          : Math.abs(py - cy);
      const secondary =
        direction === "left" || direction === "right"
          ? Math.abs(py - cy)
          : Math.abs(px - cx);
      const dist = primary + secondary * 2;
      if (dist < bestDist) {
        bestDist = dist;
        best = pane;
      }
    }
    if (best) {
      this.setActive(best);
      best.focus();
    }
  }
}
