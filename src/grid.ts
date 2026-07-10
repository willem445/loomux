// Split-tree layout: panes live at the leaves, splits are flex rows or
// columns with draggable dividers. Splitting in the same direction as the
// parent inserts a sibling (so repeated splits form an even matrix rather
// than a lopsided staircase); splitting across creates a nested split.
//
// On top of splitting, panes can be dragged by their header to reorder
// (swap two slots) or re-dock to another pane's edge, maximized to cover the
// grid, and minimized out of the tree into a restorable dock. The drag
// decision logic (which zone the pointer is over → what happens) lives in the
// pure, unit-tested `layout.ts`; this file owns the DOM/tree mutation.

import { Pane, type PaneEvents, type PaneOptions } from "./pane";
import { dropZoneFor, indicatorFor, zoneToPlacement, type DropZone } from "./layout";
import { dockChipAttention } from "./attention";
import { planGroupMinimize } from "./group";
import { shouldFocusNewPane, shouldRestoreFocus, shouldPreserveMaximize } from "./panefocus";

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

/** A pure, DOM-free description of the split layout, so a tab preview can
 *  composite every pane arranged like the real layout without touching the live
 *  (hidden, zero-width) elements (#63). `weight` is the flex-grow the
 *  node occupies in its parent split. */
export type GridLayoutNode =
  | { kind: "leaf"; weight: number; pane: Pane }
  | { kind: "split"; dir: Dir; weight: number; children: GridLayoutNode[] };

const MIN_PANE_PX = 80;
/** Pixels the pointer must travel from the header press before a click turns
 *  into a drag — keeps taps (focus, dblclick-rename) from starting a drag. */
const DRAG_THRESHOLD_PX = 6;

const nodeEl = (n: TreeNode): HTMLElement => (n.kind === "leaf" ? n.pane.el : n.el);

/** A snapshot of the keyboard focus, taken before a grid relayout so it can be
 *  handed back afterward (#117). `sel` is the text caret/selection for
 *  input/textarea controls (the steering strip is a textarea) — refocusing
 *  alone can drop the caret to the end and lose the human's insertion point. */
interface FocusSnapshot {
  el: HTMLElement;
  sel: { start: number; end: number; dir: "forward" | "backward" | "none" } | null;
}

/** Capture the currently-focused element and its caret. Returns null when
 *  nothing meaningful holds focus (no active element, or it fell to <body>) —
 *  there's then nothing to restore. */
function captureFocus(): FocusSnapshot | null {
  const el = document.activeElement;
  if (!(el instanceof HTMLElement) || el === document.body) return null;
  let sel: FocusSnapshot["sel"] = null;
  if (el instanceof HTMLTextAreaElement || el instanceof HTMLInputElement) {
    // selectionStart is null for input types that don't expose a caret (number,
    // email, …); only snapshot a real one.
    if (el.selectionStart !== null && el.selectionEnd !== null) {
      sel = {
        start: el.selectionStart,
        end: el.selectionEnd,
        dir: el.selectionDirection ?? "none",
      };
    }
  }
  return { el, sel };
}

/** Restore a focus snapshot after a relayout, if the decision table says to and
 *  the element is still in the document. Caret/selection is re-applied for text
 *  controls so typing resumes exactly where it left off. */
function restoreFocus(prior: FocusSnapshot | null, takeFocus: boolean): void {
  const connected = !!prior && prior.el.isConnected;
  if (!shouldRestoreFocus(takeFocus, prior !== null, connected)) return;
  const { el, sel } = prior!;
  el.focus({ preventScroll: true });
  if (sel && (el instanceof HTMLTextAreaElement || el instanceof HTMLInputElement)) {
    // Guard: setSelectionRange throws on inputs that don't support selection.
    try {
      el.setSelectionRange(sel.start, sel.end, sel.dir);
    } catch {
      // Focus alone is enough for controls without a selection model.
    }
  }
}

export class Grid {
  private root: TreeNode | null = null;
  private active: Pane | null = null;
  private leaves = new Map<Pane, LeafNode>();
  /** The one fullscreen pane, if any (CSS overlay; still in the tree). */
  private maximized: Pane | null = null;
  /** Panes parked out of the tree, oldest first — rendered as dock chips. */
  private minimizedPanes: Pane[] = [];
  /** Whether this whole grid is hidden (its project tab is inactive, #63). Held
   *  so a pane opened INTO a hidden tab (a background orchestrator spawn) drops
   *  its WebGL context immediately too, not only the panes present at switch
   *  time — otherwise hidden background tabs would silently accumulate GL
   *  contexts (browsers cap them). See setHidden / GL policy in the design doc. */
  private hidden = false;

  constructor(
    private rootEl: HTMLElement,
    private dockEl: HTMLElement,
    private onEmpty: () => void
  ) {
    this.rootEl.addEventListener("pointerdown", (e) => this.onPointerDown(e));
    this.renderDock();
  }

  get activePane(): Pane | null {
    return this.active;
  }

  get paneCount(): number {
    return this.leaves.size;
  }

  panes(): Pane[] {
    return [...this.leaves.keys()];
  }

  /** Every pane the grid owns, visible and minimized — used by group-wide
   *  scans (e.g. attention routing) that must reach docked panes too. */
  allPanes(): Pane[] {
    return [...this.leaves.keys(), ...this.minimizedPanes];
  }

  /** Show/hide the whole grid for a project-tab switch (#63). Records the state
   *  (so later-opened panes inherit it — see openPane) and drops/reloads every
   *  pane's WebGL context accordingly. This is a rendering concern ONLY: the PTY
   *  and in-memory buffer are untouched, so hiding issues no resize and loses no
   *  scrollback. The container's `display:none` is what actually zeroes each
   *  pane's width and thus suppresses PTY resizes (panefit.ts); this just frees
   *  the GPU contexts a hidden tab doesn't need. */
  setHidden(hidden: boolean): void {
    this.hidden = hidden;
    for (const pane of this.allPanes()) pane.setHidden(hidden);
  }

  /** A snapshot of the split tree (dir + flex weights + panes at the leaves),
   *  for compositing a tab preview (#63). Reads the in-memory tree and
   *  the elements' flex-grow — never geometry — so it works while the whole tab
   *  is hidden/zero-width. Minimized (docked) panes are outside the tree and so
   *  aren't included. Null when the grid is empty. */
  layoutSnapshot(): GridLayoutNode | null {
    const walk = (n: TreeNode): GridLayoutNode => {
      const weight = parseFloat(nodeEl(n).style.flexGrow || "1") || 1;
      if (n.kind === "leaf") return { kind: "leaf", weight, pane: n.pane };
      return { kind: "split", dir: n.dir, weight, children: n.children.map(walk) };
    };
    return this.root ? walk(this.root) : null;
  }

  findByPtyId(ptyId: number): Pane | undefined {
    return (
      this.panes().find((p) => p.ptyId === ptyId) ??
      this.minimizedPanes.find((p) => p.ptyId === ptyId)
    );
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
    const takeFocus = this.placeLeaf(pane, !!opts.background, dir, relativeTo);
    await pane.start(opts, takeFocus);
    return pane;
  }

  /** Land a pane in "setup" state (#194): placed in the grid like any pane but
   *  with NO PTY — it shows the welcome/pane-setup form (`formEl`) until the user
   *  submits, at which point the caller converts it via `pane.startFromWelcome`.
   *  No terminal is opened here, so nothing can resize a ConPTY before submit. */
  openWelcomePane(events: PaneEvents, formEl: HTMLElement, dir: Dir = "row", relativeTo?: Pane): Pane {
    const pane = new Pane(events);
    const takeFocus = this.placeLeaf(pane, false, dir, relativeTo);
    pane.startWelcome(formEl);
    if (takeFocus) pane.focusWelcome();
    return pane;
  }

  /** Insert a freshly-constructed pane's leaf into the tree and settle focus.
   *  Shared by `openPane` (which then spawns a PTY) and `openWelcomePane` (which
   *  renders a setup form instead). Returns whether the new pane took focus.
   *
   *  `background` is an orchestrator-driven spawn that must not steal focus/active
   *  from where the human is typing (#117) nor collapse a fullscreen view (#155). */
  private placeLeaf(pane: Pane, background: boolean, dir: Dir, relativeTo?: Pane): boolean {
    // Snapshot the human's focus FIRST — before any relayout below. Both
    // exitMaximize and insertBeside → renderSplit do replaceChildren(), which
    // detaches the focused pane's subtree (the steering strip or a terminal) and
    // re-appends it, implicitly blurring it to <body> so keystrokes go nowhere
    // (#117; same DOM-detach class as #113). We hand it back after, unless the
    // new pane is meant to take focus (see restoreFocus/takeFocus).
    const prior = captureFocus();
    // A background (orchestrator-driven) spawn must not collapse the human's
    // fullscreen view (#155): keep the pane maximized and grow the split tree
    // underneath it. A human-initiated open still exits fullscreen so the new
    // pane is shown in its landed layout. `keepMaximized` is the pane to re-lift
    // after the tree mutation, or null when we let maximize exit as before.
    const keepMaximized = shouldPreserveMaximize(!background, this.maximized !== null)
      ? this.maximized
      : null;
    if (this.maximized && !keepMaximized) this.exitMaximize(); // a layout change exits fullscreen
    const leaf: LeafNode = { kind: "leaf", pane, parent: null };
    this.leaves.set(pane, leaf);

    const target = relativeTo ?? this.active;
    const wasEmpty = !this.root || !target;

    // A background spawn opens the pane without stealing focus/active from where
    // the human is typing (#117) — but an empty grid still focuses, or the app
    // would be left with no active terminal.
    const takeFocus = shouldFocusNewPane(!background, wasEmpty);

    if (wasEmpty) {
      this.root = leaf;
      pane.el.style.flex = "1 1 0";
      this.rootEl.appendChild(pane.el);
    } else {
      this.insertBeside(this.leaves.get(target)!, leaf, dir);
    }

    // Preserving fullscreen (#155): insertBeside re-seated the maximized pane's
    // element into the now-hidden split, so lift it back to the top layer. We
    // never removed `.has-maximized`/`.maximized`, so no pane repainted; the new
    // pane sits in the hidden subtree (zero width → no fit → no PTY resize) and
    // becomes visible when the human unmaximizes. Do this BEFORE restoreFocus so
    // the re-lift's detach/reattach blur is the human's last focus event undone.
    if (keepMaximized) this.rootEl.appendChild(keepMaximized.el);

    if (takeFocus) this.setActive(pane);
    // Hand focus back synchronously, in the same tick as the relayout — JS is
    // single-threaded, so no keystroke can interleave between the blur and this
    // restore; typing continues uninterrupted, mid-word. Do this before the
    // caller awaits pane.start so the async PTY spawn never runs with focus
    // parked on <body>.
    else restoreFocus(prior, takeFocus);
    // A pane opened into a hidden tab (a background orchestrator spawn) must not
    // hold a WebGL context the tab isn't showing — drop it now, matching the
    // rest of the hidden tab (#63 GL policy). Reloaded when the tab is shown.
    if (this.hidden) pane.setHidden(true);
    return takeFocus;
  }

  private insertBeside(at: LeafNode, leaf: LeafNode, dir: Dir, before = false): void {
    const parent = at.parent;
    if (parent && parent.dir === dir) {
      // Same-direction split: add a sibling next to the target, giving it an
      // equal share of the row/column.
      const idx = parent.children.indexOf(at);
      parent.children.splice(before ? idx : idx + 1, 0, leaf);
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
        children: before ? [leaf, at] : [at, leaf],
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
    // Minimized panes live outside the tree — just drop the dock chip.
    const minIdx = this.minimizedPanes.indexOf(pane);
    if (minIdx >= 0) {
      this.minimizedPanes.splice(minIdx, 1);
      pane.setDockSyncListener(null);
      pane.dispose(killBackend);
      this.renderDock();
      return;
    }

    const leaf = this.leaves.get(pane);
    if (!leaf) return;
    if (this.maximized) this.exitMaximize(); // re-seat any lifted pane first
    this.leaves.delete(pane);
    this.removeFromTree(leaf);
    pane.dispose(killBackend);

    if (this.active === pane) {
      this.active = null;
      const next = this.panes()[0];
      if (next) this.setActive(next);
    }
    if (!this.root) {
      // Prefer bringing a parked pane back over spawning a fresh shell, so
      // minimized work isn't stranded behind a brand-new pane.
      const parked = this.minimizedPanes[this.minimizedPanes.length - 1];
      if (parked) this.restore(parked);
      else this.onEmpty();
    }
  }

  /** Detach a leaf's element and unlink it from the tree, collapsing a
   *  now-single-child split. Does NOT dispose the pane — used by close (which
   *  disposes after), minimize, and move. */
  private removeFromTree(leaf: LeafNode): void {
    const parent = leaf.parent;
    leaf.pane.el.remove();
    if (!parent) {
      this.root = null;
    } else {
      parent.children.splice(parent.children.indexOf(leaf), 1);
      if (parent.children.length === 1) this.collapse(parent);
      else this.renderSplit(parent);
    }
    leaf.parent = null;
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

  // ---------- maximize ----------

  /** Toggle a pane to/from fullscreen.
   *
   *  The pane's element is lifted to a top layer directly under the grid and
   *  the rest of the tree is hidden with `display:none`. That's deliberate: a
   *  hidden pane's terminal reports a zero width, which its own `applyFit`
   *  skips — so the *other* panes never resize their PTYs (no scrollback
   *  pollution), and on restore they return to an identical size that the
   *  same-size guard also skips. Only the maximized pane genuinely changes
   *  size, so it alone issues one debounced fit. The split tree model is left
   *  intact; restoring just re-seats the element in its slot. */
  toggleMaximize(pane: Pane): void {
    if (!this.leaves.has(pane)) return; // parked/unknown panes can't maximize
    if (this.maximized === pane) {
      this.exitMaximize();
    } else {
      if (this.maximized) this.exitMaximize();
      this.rootEl.classList.add("has-maximized");
      this.rootEl.appendChild(pane.el); // lift out of the tree into the top layer
      pane.setMaximized(true);
      this.maximized = pane;
      this.setActive(pane);
      pane.focus();
    }
  }

  /** Drop the current fullscreen pane back into its slot. A no-op if nothing
   *  is maximized. Structural mutations call this first so they never have to
   *  reason about the lifted element. */
  private exitMaximize(): void {
    const pane = this.maximized;
    if (!pane) return;
    this.maximized = null;
    pane.setMaximized(false);
    this.rootEl.classList.remove("has-maximized");
    const leaf = this.leaves.get(pane);
    if (!leaf) return; // pane was closed while maximized — nothing to re-seat
    if (leaf.parent) this.renderSplit(leaf.parent);
    else this.rootEl.replaceChildren(pane.el);
    pane.focus();
  }

  // ---------- minimize / dock ----------

  /** Park a pane in the dock: pull it out of the tree (its PTY keeps running)
   *  and render a restore chip. Refuses to minimize the last visible pane so
   *  the grid is never left empty. */
  minimize(pane: Pane): void {
    const leaf = this.leaves.get(pane);
    if (!leaf) return;
    if (this.leaves.size <= 1) return;
    if (this.maximized) this.exitMaximize();
    this.leaves.delete(pane);
    this.removeFromTree(leaf);
    this.minimizedPanes.push(pane);
    // While docked the pane's header is out of the DOM, so mirror any change
    // the chip shows — attention (#6) or a rename (#95r) — onto its dock chip.
    pane.setDockSyncListener(() => this.renderDock());
    if (this.active === pane) {
      this.active = null;
      const next = this.panes()[0];
      if (next) this.setActive(next);
    }
    this.renderDock();
  }

  /** Bring a parked pane back into the grid, beside the active pane (or as the
   *  root if the grid is empty). Reuses the live Pane — its terminal buffer and
   *  PTY are intact; re-attaching triggers a single genuine fit. */
  restore(pane: Pane): void {
    const idx = this.minimizedPanes.indexOf(pane);
    if (idx < 0) return;
    if (this.maximized) this.exitMaximize();
    this.minimizedPanes.splice(idx, 1);
    pane.setDockSyncListener(null);
    // Restoring a docked pane is "turning to it" — clear a latched attention
    // report the same way clicking a pane does; live reasons re-badge its
    // header on the next scan.
    pane.acknowledgeAttention();
    const leaf: LeafNode = { kind: "leaf", pane, parent: null };
    this.leaves.set(pane, leaf);
    pane.el.style.flex = "1 1 0";

    const target = this.active;
    const targetLeaf = target ? this.leaves.get(target) : undefined;
    if (!this.root || !targetLeaf) {
      this.root = leaf;
      this.rootEl.replaceChildren(pane.el);
    } else {
      this.insertBeside(targetLeaf, leaf, "row");
    }
    this.setActive(pane);
    pane.focus();
    this.renderDock();
  }

  // ---------- batch minimize / restore (group fold, #46) ----------

  /** Minimize several panes as one batch: all the tree surgery happens in this
   *  synchronous pass, so the survivors' ResizeObservers coalesce into a single
   *  debounced fit each (one relayout, not one per pane) — a 6-pane fold never
   *  triggers a ConPTY resize storm. Skips unknown/already-docked panes and,
   *  like `minimize`, never empties the grid. Dock + active pane are refreshed
   *  once at the end. */
  minimizeMany(panes: Pane[]): void {
    if (this.maximized) this.exitMaximize();
    let changed = false;
    for (const pane of panes) {
      const leaf = this.leaves.get(pane);
      if (!leaf) continue; // not visible (already docked) or unknown
      if (this.leaves.size <= 1) break; // keep at least one pane in the grid
      this.leaves.delete(pane);
      this.removeFromTree(leaf);
      this.minimizedPanes.push(pane);
      // Docked panes mirror attention onto their dock chip (see `minimize`).
      pane.setDockSyncListener(() => this.renderDock());
      if (this.active === pane) this.active = null;
      changed = true;
    }
    if (!changed) return;
    if (this.active === null) {
      const next = this.panes()[0];
      if (next) this.setActive(next);
    }
    this.renderDock();
  }

  /** Restore several docked panes as one batch — the mirror of `minimizeMany`.
   *  Each lands beside the previously restored one (or the active pane) so the
   *  group comes back as a coherent cluster; the single synchronous pass keeps
   *  the fits coalesced. Focus + dock settle once at the end. */
  restoreMany(panes: Pane[]): void {
    if (this.maximized) this.exitMaximize();
    let last: Pane | null = null;
    for (const pane of panes) {
      const idx = this.minimizedPanes.indexOf(pane);
      if (idx < 0) continue; // not docked / unknown
      this.minimizedPanes.splice(idx, 1);
      pane.setDockSyncListener(null);
      // Restoring is "turning to" the pane — clear a latched attention report,
      // same as `restore`.
      pane.acknowledgeAttention();
      const leaf: LeafNode = { kind: "leaf", pane, parent: null };
      this.leaves.set(pane, leaf);
      pane.el.style.flex = "1 1 0";

      const targetLeaf = this.active ? this.leaves.get(this.active) : undefined;
      if (!this.root || !targetLeaf) {
        this.root = leaf;
        this.rootEl.replaceChildren(pane.el);
      } else {
        this.insertBeside(targetLeaf, leaf, "row");
      }
      // Seat the next restore beside this one, not back at the orchestrator.
      this.setActive(pane);
      last = pane;
    }
    if (last) last.focus();
    this.renderDock();
  }

  /** The group-fold toggle (#46): fold a whole orchestration group's
   *  worker/reviewer panes into the dock, or restore them if already folded.
   *  The orchestrator's own pane is never touched. The visible/docked decision
   *  and target selection live in the pure `planGroupMinimize`. */
  toggleGroupMinimize(groupId: string): void {
    const states = this.allPanes().map((pane) => ({
      pane,
      orchGroupId: pane.orchGroupId,
      orchRole: pane.orchRole,
      minimized: this.minimizedPanes.includes(pane),
    }));
    const plan = planGroupMinimize(states, groupId);
    if (!plan) return;
    const targets = plan.targets.map((t) => t.pane);
    if (plan.action === "minimize") this.minimizeMany(targets);
    else this.restoreMany(targets);
  }

  private renderDock(): void {
    this.dockEl.replaceChildren();
    if (this.minimizedPanes.length === 0) {
      this.dockEl.hidden = true;
      return;
    }
    this.dockEl.hidden = false;
    const label = document.createElement("span");
    label.className = "dock-label";
    label.textContent = "Minimized";
    this.dockEl.appendChild(label);

    for (const pane of this.minimizedPanes) {
      const chip = document.createElement("button");
      chip.className = "dock-chip";
      const accent = pane.accentColor;
      if (accent) chip.style.setProperty("--dock-accent", accent);
      else chip.classList.add("plain");
      chip.addEventListener("click", () => this.restore(pane));

      // Surface attention routing (#6) on the chip: a docked worker that needs
      // the human pulses (red when urgent), so minimizing never hides the ask.
      const attn = dockChipAttention(pane.name, pane.attention);
      chip.classList.toggle("needs-attention", attn.needsAttention);
      chip.classList.toggle("urgent", attn.urgent);
      chip.title = attn.title;

      const name = document.createElement("span");
      name.className = "dock-chip-name";
      name.textContent = pane.name;

      const close = document.createElement("span");
      close.className = "dock-chip-close";
      close.textContent = "✕";
      close.title = `Close ${pane.name}`;
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        this.closePane(pane);
      });

      chip.append(name, close);
      this.dockEl.appendChild(chip);
    }
  }

  // ---------- drag to reorder / re-dock ----------

  private onPointerDown(e: PointerEvent): void {
    if (e.button !== 0 || this.maximized) return;
    const el = e.target as HTMLElement;
    const header = el.closest(".pane-header");
    if (!header) return;
    // Header controls (buttons, rename input, folder/branch chips) keep their
    // own behavior — never start a drag from them.
    if (el.closest("button, input, .pane-meta-item")) return;
    if (this.leaves.size < 2) return; // nothing to reorder into
    const pane = this.paneForEl(header);
    if (pane) this.beginDrag(pane, e);
  }

  private paneForEl(el: Element): Pane | null {
    for (const pane of this.leaves.keys()) {
      if (pane.el.contains(el)) return pane;
    }
    return null;
  }

  /** Which target pane (and zone within it) a viewport point lands on, ignoring
   *  the pane being dragged. */
  private hitTest(x: number, y: number, source: Pane): { pane: Pane; zone: DropZone } | null {
    for (const pane of this.leaves.keys()) {
      if (pane === source) continue;
      const r = pane.el.getBoundingClientRect();
      if (x < r.left || x > r.right || y < r.top || y > r.bottom) continue;
      return { pane, zone: dropZoneFor(r.width, r.height, x - r.left, y - r.top) };
    }
    return null;
  }

  private beginDrag(source: Pane, down: PointerEvent): void {
    const startX = down.clientX;
    const startY = down.clientY;
    let started = false;
    let hover: { pane: Pane; zone: DropZone } | null = null;
    let indicator: HTMLElement | null = null;
    let ghost: HTMLElement | null = null;

    const start = () => {
      started = true;
      this.setActive(source);
      source.el.classList.add("drag-source");
      document.body.classList.add("dragging-pane");
      indicator = document.createElement("div");
      indicator.className = "drop-indicator";
      indicator.hidden = true;
      document.body.appendChild(indicator);
      ghost = document.createElement("div");
      ghost.className = "drag-ghost";
      ghost.textContent = source.name;
      document.body.appendChild(ghost);
    };

    const move = (ev: PointerEvent) => {
      if (!started) {
        if (Math.hypot(ev.clientX - startX, ev.clientY - startY) < DRAG_THRESHOLD_PX) return;
        start();
      }
      if (ghost) {
        ghost.style.left = `${ev.clientX}px`;
        ghost.style.top = `${ev.clientY}px`;
      }
      hover = this.hitTest(ev.clientX, ev.clientY, source);
      if (indicator) {
        if (hover) {
          this.positionIndicator(indicator, hover.pane, hover.zone);
          indicator.hidden = false;
        } else {
          indicator.hidden = true;
        }
      }
    };

    const finish = (commit: boolean) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("keydown", onKey, true);
      source.el.classList.remove("drag-source");
      document.body.classList.remove("dragging-pane");
      indicator?.remove();
      ghost?.remove();
      if (commit && started && hover && hover.pane !== source) {
        if (hover.zone === "center") {
          this.swap(source, hover.pane);
        } else {
          const placement = zoneToPlacement(hover.zone);
          if (placement) this.moveToEdge(source, hover.pane, placement.dir, placement.before);
        }
      }
      if (started) source.focus();
    };
    const up = () => finish(true);
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        hover = null;
        finish(false);
      }
    };

    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("keydown", onKey, true);
  }

  /** Size and place the snap indicator (a fixed-position overlay) over the
   *  region the drop would occupy. */
  private positionIndicator(indicator: HTMLElement, pane: Pane, zone: DropZone): void {
    const r = pane.el.getBoundingClientRect();
    const frac = indicatorFor(zone);
    indicator.style.left = `${r.left + frac.left * r.width}px`;
    indicator.style.top = `${r.top + frac.top * r.height}px`;
    indicator.style.width = `${frac.width * r.width}px`;
    indicator.style.height = `${frac.height * r.height}px`;
    indicator.dataset.zone = zone;
  }

  /** Swap two panes between their slots, preserving each slot's flex so equal
   *  slots keep identical pixel sizes — and therefore never resize a PTY
   *  (applyFit skips same-size fits). The split structure is untouched. */
  swap(a: Pane, b: Pane): void {
    if (a === b || this.maximized) return;
    const la = this.leaves.get(a);
    const lb = this.leaves.get(b);
    if (!la || !lb) return;

    const fa = a.el.style.flex;
    const fb = b.el.style.flex;
    a.el.style.flex = fb;
    b.el.style.flex = fa;

    const marker = document.createComment("swap");
    a.el.replaceWith(marker);
    b.el.replaceWith(a.el);
    marker.replaceWith(b.el);

    la.pane = b;
    lb.pane = a;
    this.leaves.set(a, lb);
    this.leaves.set(b, la);
  }

  /** Move a pane out of its current slot and re-dock it to an edge of the
   *  target, forming (or joining) a split in that direction. This is a genuine
   *  restructure, so affected panes may resize once. */
  moveToEdge(source: Pane, target: Pane, dir: Dir, before: boolean): void {
    if (source === target || this.maximized) return;
    const leaf = this.leaves.get(source);
    const targetLeaf = this.leaves.get(target);
    if (!leaf || !targetLeaf) return;
    this.removeFromTree(leaf);
    source.el.style.flex = "1 1 0";
    this.insertBeside(targetLeaf, leaf, dir, before);
    this.setActive(source);
    source.focus();
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
