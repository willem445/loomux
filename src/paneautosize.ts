// Pure, DOM-free weight math for the Autosize action (#936): make every pane
// in a tab the same size, on demand. `grid.ts` owns the tree and the DOM and
// hands this module only the SHAPE of the split tree; what comes back is a
// weight for every node, which `applyLayoutWeights` writes onto the elements.
//
// Deliberately no cross-module import, matching every other pure module here
// (layout.ts, overlaysize.ts, embedsplit.ts): tsc forbids the explicit `.ts`
// extension a bare intra-src import would need for the node runner to resolve
// it (TS5097), so a shared helper would work for one runner and not the other.
//
// WHY WEIGHT BY PANE COUNT, NOT "1 EACH"
//
// The obvious reading of "make them all equal" — give every child of every
// split the same weight — does not do it. Take a row of [A, column[B, C]]:
// equal weights make A half the tab and B and C a quarter each. A pane's size
// is the PRODUCT of its share at every level down to it, so evening out one
// level at a time cannot even out the leaves.
//
// Weighting each node by the number of panes underneath it does, exactly: a
// child holding k of its split's n panes takes k/n of that split, so by
// induction every pane ends up with 1/total of the tab. In the example above A
// gets 1/3 of the width and the column gets 2/3, then B and C get half of that
// each — three equal thirds.
//
// WHAT "EQUAL" IS TRUE OF, PRECISELY. Equal *area*, in the space flex has to
// distribute — not equal width, not equal height, and not equal to the pixel:
//
//   - dividers are `flex: none` (grid.ts `makeDivider`), so their pixels come
//     off a split's free space BEFORE the shares are computed. Branches at
//     different depths carry different numbers of dividers, so two panes'
//     areas differ by those few pixels. Nothing here can fix that without
//     making layout depend on measured geometry, which is the coupling the
//     no-resize constraint distrusts;
//   - `.pane` has a 60px min-width/min-height floor in CSS. A tab holding more
//     panes than fit at that floor cannot have equal panes at all, and flex
//     clamps the offenders and re-shares the surplus among the rest. Autosize
//     does not fight that; it just asks for the best arrangement available.
//
// So the honest claim is: every pane gets an equal share of what there is to
// distribute. Do not restore a stronger one.

/** The shape of a split tree, with everything this module does not need
 *  stripped: a leaf is a node with no `children`. `grid.ts` walks its live
 *  tree into this; `PersistedLayoutNode` is structurally one of these too. */
export interface SplitShape {
  children?: readonly SplitShape[];
}

/** A weight for every node of the same tree, in the same order. Structurally a
 *  `WeightNode` (grid.ts), so `applyLayoutWeights` takes it directly. */
export interface EqualizedWeights {
  weight: number;
  children?: EqualizedWeights[];
}

/** How many panes (leaves) sit under a node, itself included when it is one.
 *
 *  A split with no children is not a state `grid.ts` can produce — it collapses
 *  a split the moment it drops to one child — but it is counted as one pane
 *  rather than zero anyway, because the alternative is a zero weight, and a
 *  zero-grow node in a `flex-basis: 0` split is an invisible one. A malformed
 *  tree should lay out oddly, never vanish. */
export function paneCount(node: SplitShape): number {
  if (!node.children || node.children.length === 0) return 1;
  return node.children.reduce((n, child) => n + paneCount(child), 0);
}

/** Weights that give every pane in the tree an equal share of the tab.
 *
 *  Each node's weight is the number of panes underneath it, which is the whole
 *  rule — see the module header for why "1 each" is not the same thing and for
 *  what "equal" is and is not true of.
 *
 *  The root's own weight is returned for completeness and is inert: the root
 *  element is the only child of the grid container, so any positive grow makes
 *  it fill. */
export function equalizeWeights(node: SplitShape): EqualizedWeights {
  const weight = paneCount(node);
  if (!node.children || node.children.length === 0) return { weight };
  return { weight, children: node.children.map(equalizeWeights) };
}
