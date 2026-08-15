// Split-tree layout: panes live at the leaves, splits are flex rows or
// columns with draggable dividers. Splitting in the same direction as the
// parent inserts a flat sibling; splitting across creates a nested split.
//
// A same-direction insert has TWO placement policies (#885), picked per call
// site and computed in the pure, DOM-free `splitfloor.ts`:
//
//   - `halve` — the target pane pays for the newcomer out of its OWN weight,
//     so the row's total is unchanged and every other pane keeps its exact
//     grow/total RATIO. That is a ratio guarantee, not a pixel one: the new
//     divider is itself a flex sibling and takes ~4px off the row's free
//     space, so each sibling loses its ratio's share of those 4px (~1px in a
//     typical row) and drops a terminal column on the occasions when that
//     lands across a cell boundary. So: ONE guaranteed PTY resize (the
//     target) plus a rare sub-cell nudge per sibling — against `share`, which
//     moves and resizes every sibling in the row, every time. Every HUMAN
//     split gesture uses this: the pane you are standing in is the one that
//     shrinks, which is what tmux/wezterm do and what a split "feels" like.
//   - `share` — the newcomer joins at the MEAN of the weights already in the
//     row, so a row that was even is even again (N panes at 1/N) and repeated
//     placements form a real even matrix rather than a lopsided staircase.
//     Kept for PROGRAMMATIC batch placement — the multi-agent welcome fan-out,
//     a pane rejoining from the dock, a restore replay — where halving
//     repeatedly would deal out a 1/2, 1/4, 1/8, 1/16 sliver staircase.
//
// Cross-direction splits need no policy: nesting a two-way split inside the
// target's slot already halves the target alone. See
// doc/design/pane-splitting-and-floors.md.
//
// WHERE THE THREE PANE SLICES MEET. `halve` is #885's `splitfloor.planRowSplit`.
// `share` is #936's `paneequalize.planEvenInsert` — NOT splitfloor's own
// `share` branch, which is the pre-#885 "1/N on top of the existing total" and
// is exactly the staircase #936 reported. Removal is #936's too: a closing
// pane's weight goes to its survivors in equal absolute parts (`planRemoval`)
// rather than being dropped for flex to re-share proportionally. Autosize
// (`paneautosize.ts`) is the third, on-demand operation: it levels the WHOLE
// tab across nesting when the human asks, and never by itself.
//
// That routing is also what keeps the #954 magnitude clamp in the path. It
// lives at `paneequalize`'s entry points, so every operation that can inflate
// a row's weights — the close, which preserves the total across one fewer
// pane — re-bases an out-of-band row on the way in. `halve` reaches no clamp
// and needs none: it preserves the row's total EXACTLY, which is both why it
// cannot inflate anything and the guarantee a re-base would rewrite.
// `test/splitfloor.test.ts` pins the routing; the design note argues it.
//
// On top of splitting, panes can be dragged by their header to reorder
// (swap two slots) or re-dock to another pane's edge, maximized to cover the
// grid, and minimized out of the tree into a restorable dock. The drag
// decision logic (which zone the pointer is over → what happens) lives in the
// pure, unit-tested `layout.ts`; this file owns the DOM/tree mutation.

import { Pane, type PaneEvents, type PaneOptions, type ContentPaneOptions } from "./pane";
import type { PersistedPane } from "./tabstore";
import { dropZoneFor, indicatorFor, zoneToPlacement, type DropZone } from "./layout";
import { dockChipAttention } from "./attention";
import { dockChipQueue, queuePresentation } from "./queuebadge";
import { planGroupMinimize } from "./group";
import { shouldFocusNewPane, shouldRestoreFocus, shouldPreserveMaximize } from "./panefocus";
import { startDragSession } from "./dragsession";
import { planEvenInsert, planRemoval, readGrow } from "./paneequalize";
import { equalizeWeights, type SplitShape } from "./paneautosize";
// `parseGrow` is deliberately NOT imported: it is byte-for-byte `readGrow`
// above, and one reader for the row keeps the two policies' weight repair
// identical rather than merely equivalent-looking.
import { planRowSplit, type SplitPolicy } from "./splitfloor";

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

/** The minimal shape `applyLayoutWeights` walks in parallel with the live tree:
 *  a per-node flex-grow, and children for splits. A `PersistedLayoutNode`
 *  (tabstore.ts) is structurally one of these, so a rebuild passes it directly. */
export interface WeightNode {
  weight: number;
  children?: WeightNode[];
}

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
    private onEmpty: () => void,
    /** Fired whenever the pane set / layout changes (open, close) so the host can
     *  re-render the tab strip's live agent counter and re-persist the layout
     *  (#194 P4). Defaults to a no-op for callers that don't care. */
    private onChange: () => void = () => {}
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

  /** Every pane the grid owns, docked ones included — the population
   *  `allPanes()` would return, WITHOUT building the array. Used by the
   *  attention gate's topology token (#743 S5), which runs on a 3 s tick and
   *  must not allocate to answer "did the pane set move?". */
  get allPaneCount(): number {
    return this.leaves.size + this.minimizedPanes.length;
  }

  panes(): Pane[] {
    return [...this.leaves.keys()];
  }

  /** Every pane the grid owns, visible and minimized — used by group-wide
   *  scans (e.g. attention routing) that must reach docked panes too. */
  allPanes(): Pane[] {
    return [...this.leaves.keys(), ...this.minimizedPanes];
  }

  /** Just the docked (minimized) panes — the ones OUTSIDE the split tree, so
   *  layoutSnapshot misses them. Captured separately for restore (#194 P4). */
  dockedPanes(): Pane[] {
    return [...this.minimizedPanes];
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
   *  split relative to `relativeTo` (default: the active pane).
   *
   *  `policy` defaults to `share` — this opener's callers are programmatic
   *  (the multi-agent fan-out, restore replay, orchestrator spawns), which is
   *  exactly what the even-matrix policy is for. A human gesture routes
   *  through `openWelcomePane` and passes `halve`. */
  async openPane(
    opts: PaneOptions,
    events: PaneEvents,
    dir: Dir = "row",
    relativeTo?: Pane,
    policy: SplitPolicy = "share"
  ): Promise<Pane> {
    const pane = new Pane(events);
    const takeFocus = this.placeLeaf(pane, !!opts.background, dir, relativeTo, policy);
    await pane.start(opts, takeFocus);
    // Re-notify now that the PTY exists: placeLeaf fired onChange BEFORE start, so
    // the pane was still ptyId-less (live:false) and the agent counter undercounted
    // it. A second notify after start settles the count (#194 P4 HIGH-1).
    this.onChange();
    return pane;
  }

  /** Open a pane that starts life ALREADY docked (#387: spawn-minimized flash).
   *  The old path — `openPane` (which lands the pane in a real, full-size tree
   *  slot) followed by a `minimize()` call once the PTY finished spawning — held
   *  that visible slot for the whole async spawn round-trip, so the pane painted
   *  at least one full-size frame before folding into the dock. This one never
   *  creates a leaf for the pane at all: it goes straight into `minimizedPanes`
   *  and the dock chip is the only thing that ever renders. `pane.el` stays
   *  detached (the same end state `minimize()` leaves a docked pane in), so
   *  `fit()` inside `pane.start()` finds a zero-size container and leaves xterm at
   *  its construction default (80x24) rather than sizing — and resizing the PTY —
   *  to a layout slot the human never sees. `restore()` still does the one real
   *  fit, when the pane actually becomes visible.
   *
   *  Mirrors `minimize()`'s own "never leave the grid empty" rule: with no
   *  visible pane yet there is nothing to dock behind, so this falls back to a
   *  normal (visible) open — the same no-op-and-show-it-anyway `minimize()`
   *  itself would produce in that edge case. */
  async openPaneMinimized(
    opts: PaneOptions,
    events: PaneEvents,
    dir: Dir = "row",
    relativeTo?: Pane
  ): Promise<Pane> {
    if (!this.root) return this.openPane(opts, events, dir, relativeTo);
    const pane = new Pane(events);
    this.minimizedPanes.push(pane);
    pane.setDockSyncListener(() => this.renderDock());
    this.renderDock();
    await pane.start(opts, false); // never takes focus — it's never shown
    this.onChange();
    return pane;
  }

  /** Land a pane in "setup" state (#194): placed in the grid like any pane but
   *  with NO PTY — it shows the welcome/pane-setup form (`formEl`) until the user
   *  submits, at which point the caller converts it via `pane.startFromWelcome`.
   *  No terminal is opened here, so nothing can resize a ConPTY before submit.
   *
   *  This is the opener every human split gesture lands in (Ctrl+Shift+E/O,
   *  the top-bar buttons, a pane header's ◫/⬓), so those pass `halve` (#885).
   *  It also serves restore fail-softs (a pane whose folder is gone comes back
   *  as a welcome form), which keep the `share` default: a replayed layout is
   *  programmatic placement, not a gesture. */
  openWelcomePane(
    events: PaneEvents,
    formEl: HTMLElement,
    dir: Dir = "row",
    relativeTo?: Pane,
    policy: SplitPolicy = "share"
  ): Pane {
    const pane = new Pane(events);
    const takeFocus = this.placeLeaf(pane, false, dir, relativeTo, policy);
    pane.startWelcome(formEl);
    if (takeFocus) pane.focusWelcome();
    return pane;
  }

  /** Land a CONTENT pane (#214 files, #217 editor / git): placed in the grid like any
   *  other pane — it splits, moves, docks, maximizes and counts identically — but with
   *  NO PTY. Its content is the file manager / file editor / git view rooted at
   *  `opts.root`, permanently. No terminal opens, so nothing here can resize a ConPTY.
   *  Synchronous (there is no process to await), unlike `openPane`. */
  openContentPane(
    events: PaneEvents,
    opts: ContentPaneOptions,
    dir: Dir = "row",
    relativeTo?: Pane
  ): Pane {
    const pane = new Pane(events);
    // `share`: content panes are opened programmatically beside their source
    // (the file browser's "open in editor pane", restore replay) — not one of
    // the four human split gestures #885 moved to `halve`.
    const takeFocus = this.placeLeaf(pane, !!opts.background, dir, relativeTo, "share");
    pane.startContent({ ...opts, background: !takeFocus });
    // Re-notify now that the pane KNOWS what kind it is. placeLeaf fired onChange while
    // it was still a bare Pane, and `capture()` would have serialized it as a rootless
    // terminal leaf — same reason openPane re-notifies once its PTY exists.
    this.onChange();
    return pane;
  }

  /** Land a pane in DORMANT restore state (#194 P4): placed like any pane but
   *  with NO PTY — it shows `contentEl` (a Start/Resume affordance the caller
   *  wires) and stands in for a persisted leaf we deliberately did NOT auto-spawn
   *  (a no-session agent, or an orchestration pane whose group stays dormant). No
   *  terminal opens, so nothing resizes a ConPTY; `record` is retained so the
   *  pane re-captures identically if the session is closed without resuming.
   *  Opened `background` so a restore rebuild never fights the human for focus. */
  openDormantPane(
    events: PaneEvents,
    record: PersistedPane,
    contentEl: HTMLElement,
    dir: Dir = "row",
    relativeTo?: Pane
  ): Pane {
    const pane = new Pane(events);
    // `share`: this opener exists only for restore replay — programmatic
    // placement, and `applyLayoutWeights` overwrites every weight afterwards
    // anyway, so the policy is not even observable here.
    this.placeLeaf(pane, true, dir, relativeTo, "share");
    pane.startDormant(record, contentEl);
    return pane;
  }

  /** Overwrite every node's flex-grow to match a persisted weight tree of the
   *  SAME structure (session restore #194). `openPane`/`openDormantPane` reset
   *  flex to equal shares as they split, so a rebuild replays the whole tree then
   *  calls this once to put the saved divider positions back (the 25/75 drag that
   *  would otherwise snap to 50/50). `weights` must mirror the tree the replay
   *  just built — panerestore's plan guarantees that — and a shape mismatch stops
   *  at the divergence rather than throwing. */
  applyLayoutWeights(weights: WeightNode): void {
    if (!this.root) return;
    const walk = (n: TreeNode, w: WeightNode): void => {
      nodeEl(n).style.flex = `${w.weight} 1 0`;
      if (n.kind === "split" && w.children) {
        n.children.forEach((c, i) => {
          const cw = w.children![i];
          if (cw) walk(c, cw);
        });
      }
    };
    walk(this.root, weights);
  }

  /** Autosize (#936): give every pane in this tab an equal share of the space,
   *  on demand. The human's answer to a layout that has drifted — a nested
   *  staircase from the agent fan-out, a row where one pane ended up holding
   *  most of the width — without having to drag each divider back by hand.
   *
   *  What it adds over the even-share split policy (#945, `paneequalize.ts`):
   *  that policy keeps a row even WITHIN one split — a newcomer at the mean, a
   *  closing pane's weight shared out in equal absolute parts — and each of its
   *  operations touches only the row it happened in. Equality across NESTING is
   *  a different property: a pane beside a stacked pair is a half and two
   *  quarters however even each split is on its own, because a pane's size is
   *  the product of its share at every level. That product is what this levels,
   *  and it is why the gesture is not made redundant by #945.
   *
   *  On demand is the whole design: nothing levels the tab by itself, and a
   *  divider the human dragged stays where they put it until they ask. It is
   *  also indifferent to which policy a split gesture ends up with (#900's
   *  halve or #945's even share) — it reads the tree's SHAPE, never its
   *  weights.
   *
   *  The weights come from the pure `equalizeWeights` — each node weighted by
   *  the number of panes under it, which is what makes the LEAVES equal rather
   *  than each split's children (see paneautosize.ts). Applied through the same
   *  `applyLayoutWeights` a session restore uses, so there is one place that
   *  writes a whole tree's weights.
   *
   *  Fullscreen exits first: autosize is a request to see the layout, and
   *  silently re-weighting a tree the human cannot see (with one pane covering
   *  it) would look like the button did nothing.
   *
   *  Constraint 1: a discrete, human-initiated layout operation, the same class
   *  as a split or a divider drag — one resize for each pane whose size actually
   *  changes, and `shouldResizePty` (panefit.ts) drops the ones whose `cols x
   *  rows` did not move. Nothing continuous, nothing chrome-driven. Docked panes
   *  are untouched: they are outside the tree and hold no space to even out. */
  autosize(): void {
    if (!this.root) return;
    if (this.maximized) this.exitMaximize();
    // Hand the module the SHAPE and nothing else — no panes, no elements. It is
    // a parallel tree of empty objects, a few dozen at the very most, and it is
    // what keeps `paneautosize.ts` honestly DOM-free rather than DOM-free by
    // convention. (Passing the live nodes would also trip TS's weak-type check:
    // `SplitShape`'s only field is optional, so a LeafNode shares no property
    // with it.)
    const shape = (n: TreeNode): SplitShape =>
      n.kind === "leaf" ? {} : { children: n.children.map(shape) };
    this.applyLayoutWeights(equalizeWeights(shape(this.root)));
    // The new weights are what a restore must reproduce (#194 P4) — persist
    // them, exactly as a finished divider drag does.
    this.onChange();
  }

  /** Insert a freshly-constructed pane's leaf into the tree and settle focus.
   *  Shared by `openPane` (which then spawns a PTY) and `openWelcomePane` (which
   *  renders a setup form instead). Returns whether the new pane took focus.
   *
   *  `background` is an orchestrator-driven spawn that must not steal focus/active
   *  from where the human is typing (#117) nor collapse a fullscreen view (#155).
   *  `policy` is the caller's split intent (#885) — see `insertBeside`. */
  private placeLeaf(
    pane: Pane,
    background: boolean,
    dir: Dir,
    relativeTo: Pane | undefined,
    policy: SplitPolicy
  ): boolean {
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
      this.insertBeside(this.leaves.get(target)!, leaf, dir, policy);
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
    this.onChange();
    return takeFocus;
  }

  /** Insert `leaf` beside `at`. `policy` decides who pays for the new pane's
   *  space on a SAME-DIRECTION insert (see the module comment); it is required
   *  rather than defaulted so every call site states which intent it is —
   *  a human gesture (`halve`) or programmatic placement (`share`). */
  private insertBeside(
    at: LeafNode,
    leaf: LeafNode,
    dir: Dir,
    policy: SplitPolicy,
    before = false
  ): void {
    const parent = at.parent;
    if (parent && parent.dir === dir) {
      // Same-direction split: add a flat sibling next to the target. The FLAT
      // N-way row is deliberate under both policies — it is why a divider drag
      // only ever negotiates with its two immediate neighbours — so the policy
      // changes the weights, never the structure.
      //
      // THE ONE PLACE #885 AND #936 MEET. `halve` is #885's policy, unchanged:
      // the target pays for the newcomer out of its own weight, so a human
      // split is local. `share` — programmatic batch placement — is served by
      // #936's `planEvenInsert` rather than `splitfloor`'s own `share` branch,
      // because the two slices disagree about exactly this arithmetic and #936
      // is the fix: splitfloor's `share` is the pre-#885 "newcomer at 1/N on
      // top of the existing total", which is the lopsided 1, .5, .33, .25
      // staircase #936 reported. `planEvenInsert` gives the newcomer the MEAN
      // of the row, which is the even matrix both slices' comments promise.
      //
      // It also puts every magnitude-raising path through `paneequalize`'s
      // entry clamp (#954): a close preserves the row's total across one fewer
      // pane, so it is the operation that inflates weights, and `planRemoval`
      // re-bases an out-of-band row on the way in. `halve` needs no clamp and
      // deliberately does not get one — it preserves the row's total EXACTLY,
      // which is the guarantee that makes a human split local, and a re-base
      // would rewrite the very numbers that guarantee is about. See
      // doc/design/pane-splitting-and-floors.md, "Where the clamp sits".
      //
      // Same plan shape (`weights` + `insertedIndex`) either way, so the
      // write-back below is common to both.
      const idx = parent.children.indexOf(at);
      const grows = parent.children.map((c) => readGrow(nodeEl(c).style.flexGrow));
      const plan =
        policy === "halve"
          ? planRowSplit(grows, idx, "halve", before)
          : planEvenInsert(grows, idx, before);
      parent.children.splice(plan.insertedIndex, 0, leaf);
      leaf.parent = parent;
      // Write every child's weight, not just the newcomer's: under `halve` the
      // untouched siblings get written back the value they already had (so
      // nothing moves), and under `share` it is what makes `paneequalize` the
      // single source of the row's arithmetic (including a re-base, which
      // rewrites all of them at once while moving no pane).
      parent.children.forEach((c, i) => {
        nodeEl(c).style.flex = `${plan.weights[i]} 1 0`;
      });
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
      this.onChange();
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
    this.onChange();
  }

  /** Detach a leaf's element and unlink it from the tree, collapsing a
   *  now-single-child split. Does NOT dispose the pane — used by close (which
   *  disposes after), minimize, and move.
   *
   *  The leaving pane's weight is handed to its surviving siblings in equal
   *  parts (`planRemoval`) instead of being dropped on the floor. Dropping it
   *  is not free even though flex always fills the split: flex re-shares the
   *  freed space PROPORTIONALLY, so the pane that was already biggest takes
   *  nearly all of it and a sliver stays a sliver — the "close a pane, get
   *  dead unusable space back" of #936. */
  private removeFromTree(leaf: LeafNode): void {
    const parent = leaf.parent;
    leaf.pane.el.remove();
    if (!parent) {
      this.root = null;
    } else {
      const idx = parent.children.indexOf(leaf);
      const weights = planRemoval(
        parent.children.map((c) => readGrow(nodeEl(c).style.flexGrow)),
        idx
      );
      parent.children.splice(idx, 1);
      if (parent.children.length === 1) {
        // A one-child split is about to be replaced by that child, which takes
        // the SPLIT's own weight in the grandparent's slot — so there is no
        // sibling to redistribute to and nothing `weights` could usefully say.
        this.collapse(parent);
      } else {
        parent.children.forEach((c, i) => {
          nodeEl(c).style.flex = `${weights[i]} 1 0`;
        });
        this.renderSplit(parent);
      }
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
      // Coalesce every pane's PTY resize to one call at drag-end instead of
      // one per animation frame for the whole drag (#432 item 1) — a nested
      // split's leaves can resize even though this divider only directly
      // touches its own before/after children, so hold the whole grid
      // rather than trying to walk the subtree. Captured once (not
      // re-read in `end`) so begin/end stay balanced 1:1 per pane even in
      // the vanishingly unlikely case the leaf set changes mid-drag.
      const dragPanes = this.panes();
      dragPanes.forEach((p) => p.beginResizeHold());

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
      const end = () => {
        div.classList.remove("dragging");
        dragPanes.forEach((p) => p.endResizeHold());
        // A finished divider drag changed the flex weights that layoutSnapshot
        // captures — persist them so a restore reproduces THIS split, not the
        // pre-drag one (#194 P4). Terminal (one per drag), not per-mousemove, so
        // no write storm; persistTabs dedups an unchanged snapshot. Fires from
        // mouseup OR a drag that ends without one (window blur, Escape) —
        // `startDragSession` (#361 review: this divider's own copy of the same
        // gap left a stranded `dragging` class when a drag lost mouseup, a
        // cosmetic issue here but a real stuck state on the embed/overlay
        // dividers that share this pattern — fixed once, for all of them).
        this.onChange();
      };
      startDragSession({ onMove: move, onEnd: end });
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
    // Docking changed the tree AND which panes are captured (docked panes are
    // captured separately, #194 P4 MED-6) — persist + re-count.
    this.onChange();
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
      // `share`: a dock restore is a pane REJOINING the grid, not the human
      // splitting the pane they are in — the row makes room for it the way it
      // always has. (#885 slice B revisits what happens when there is no room.)
      this.insertBeside(targetLeaf, leaf, "row", "share");
    }
    this.setActive(pane);
    pane.focus();
    this.renderDock();
    this.onChange();
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
    // A batch fold moved panes between the tree and the dock — capture the new
    // shape so a fold-then-quit restores them docked, not re-expanded (#194 P4).
    this.onChange();
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
        // `share`, as in `restore` — and doubly so here: a batch group-unfold
        // laying panes out as an even matrix is exactly what that policy is
        // for, where halving each restore in turn would deal out slivers.
        this.insertBeside(targetLeaf, leaf, "row", "share");
      }
      // Seat the next restore beside this one, not back at the orchestrator.
      this.setActive(pane);
      last = pane;
    }
    if (last) last.focus();
    this.renderDock();
    this.onChange();
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

      // Cross-workspace channel membership (#271): a docked pane's header chip
      // is out of the DOM, so mirror it here too — else minimizing a connected
      // pane looks like it silently disconnected. Attention (above) wins the
      // tooltip when both are present; it's the more urgent ask.
      const channel = pane.channelBadge;
      if (channel) {
        chip.classList.add("connected");
        chip.style.setProperty("--connect-color", channel.color);
        if (!attn.needsAttention) {
          chip.title = `${channel.label} — connected to ${channel.peers.join(", ") || "…"}. Click to restore.`;
        }
      }

      // Delivery-queue depth (#814): a docked pane's header chip is out of the
      // DOM, and delegate agent panes open minimized by default — so without
      // this the panes whose queues actually back up are the ones with no
      // visible reading at all. Its own marker rather than a class on the chip:
      // the count is the fact worth showing, and "3 waiting" versus "1 waiting"
      // is not something styling can say.
      const reading = pane.queueDepth;
      const queue = dockChipQueue(reading);
      if (queue && reading) {
        const marker = document.createElement("span");
        marker.className = "dock-chip-queue";
        marker.textContent = queue.marker;
        if (queue.stalled) marker.dataset.stalled = "true";
        chip.appendChild(marker);
        // Attention still owns the tooltip when it is present (it is the more
        // urgent ask, the rule the channel mirror above already follows); a
        // stalled queue outranks a plain channel membership, though, because it
        // is a thing going wrong rather than a state of affairs.
        if (!attn.needsAttention && (queue.stalled || !channel)) {
          chip.title = `${pane.name}: ${queuePresentation(reading).title} · click to restore.`;
        }
      }

      const name = document.createElement("span");
      name.className = "dock-chip-name";
      name.textContent = pane.name;

      const close = document.createElement("span");
      close.className = "dock-chip-close";
      close.textContent = "✕";
      close.title = `Close ${pane.name}`;
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        // Through the pane's close REQUEST, not straight to closePane: this is a
        // human-initiated single-pane close, identical in meaning to the header ✕,
        // so it must hit the same unsaved-edits guard. Closing it directly here
        // silently discarded a docked file explorer's dirty buffer (rev-100).
        pane.requestClose();
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
        // A committed drag-reorder changed the split-tree order/shape that a
        // restore must reproduce — persist it (#194 P4 HIGH-2).
        this.onChange();
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
   *  restructure, so affected panes may resize once.
   *
   *  `halve` (#885): drag-to-edge is one of the four human split gestures —
   *  "put this pane HERE, in that pane's space" — so the DROP TARGET is what
   *  pays for the arriving pane.
   *
   *  That is the only half of this operation the policy governs, and the other
   *  half moves panes regardless of it: the pane also LEAVES a slot, and since
   *  #936 `removeFromTree` hands its weight to the surviving siblings in equal
   *  absolute parts, so the old row's TOTAL is preserved and every survivor
   *  grows by the same absolute amount. When the drag stays inside one row,
   *  that is the same row — e.g. [A=1, B=1, C=1] with C dropped on A's right
   *  edge becomes [A=1.5, B=1.5] on the way out and ends [A=.75, C=.75,
   *  B=1.5], so B goes from a third of the row to a half. That is correct (a
   *  departing pane has to give its space back) and is not what this policy
   *  governs; it just means "the rest of the layout doesn't move" is a claim
   *  about a SPLIT and not about a MOVE. */
  moveToEdge(source: Pane, target: Pane, dir: Dir, before: boolean): void {
    if (source === target || this.maximized) return;
    const leaf = this.leaves.get(source);
    const targetLeaf = this.leaves.get(target);
    if (!leaf || !targetLeaf) return;
    this.removeFromTree(leaf);
    source.el.style.flex = "1 1 0";
    this.insertBeside(targetLeaf, leaf, dir, "halve", before);
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
