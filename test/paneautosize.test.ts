// Unit tests for the pure Autosize weight math (#936). Run with `npm test`.
//
// These assert the PROPERTY the feature promises — after autosize, every pane
// holds an equal share of the tab — by computing each leaf's actual share the
// way flex does (the product of its share at every level down to it), not by
// comparing the returned weights to a table. A weights table would pass just as
// happily for "give every child of every split the same weight", which is the
// wrong answer for any nested layout and is exactly the mistake this module
// exists to avoid.
import { test } from "node:test";
import assert from "node:assert/strict";
import { equalizeWeights, paneCount, type EqualizedWeights, type SplitShape } from "../src/paneautosize.ts";

/** Every leaf's share of the whole tab, in leaf order — the product of
 *  `weight / siblingTotal` at each level, which is what flex-grow with
 *  `flex-basis: 0` actually resolves to (dividers aside; see the module
 *  header). */
function leafShares(node: EqualizedWeights, carried = 1): number[] {
  if (!node.children || node.children.length === 0) return [carried];
  const total = node.children.reduce((n, c) => n + c.weight, 0);
  return node.children.flatMap((c) => leafShares(c, (carried * c.weight) / total));
}

const assertAllEqual = (shares: number[], what: string): void => {
  const expected = 1 / shares.length;
  for (const s of shares) {
    assert.ok(
      Math.abs(s - expected) < 1e-9,
      `${what}: every pane should hold ${expected} of the tab, got ${JSON.stringify(shares)}`
    );
  }
};

/** Shorthand tree builders, so a test reads as a layout. */
const leaf = (): SplitShape => ({});
const split = (...children: SplitShape[]): SplitShape => ({ children });

test("a flat row comes out in equal shares", () => {
  assertAllEqual(leafShares(equalizeWeights(split(leaf(), leaf(), leaf()))), "three in a row");
});

test("a nested split comes out equal too — the case 'one weight each' gets wrong", () => {
  // [A, [B, C]]: giving every child of every split the same weight would make A
  // half the tab and B and C a quarter each. Equal panes means A takes a third
  // of the row and the nested column takes two thirds.
  const shares = leafShares(equalizeWeights(split(leaf(), split(leaf(), leaf()))));
  assert.equal(shares.length, 3);
  assertAllEqual(shares, "one pane beside a stacked pair");
});

test("the halving staircase is flattened — the layout #936 complains about", () => {
  // What repeated cross-direction splitting builds: 1/2, 1/4, 1/8, 1/8. One
  // Autosize and all four panes are quarters.
  const staircase = split(leaf(), split(leaf(), split(leaf(), leaf())));
  const before = leafShares({
    weight: 1,
    children: [
      { weight: 1 },
      { weight: 1, children: [{ weight: 1 }, { weight: 1, children: [{ weight: 1 }, { weight: 1 }] }] },
    ],
  });
  assert.deepEqual(before, [0.5, 0.25, 0.125, 0.125]); // the staircase, stated
  assertAllEqual(leafShares(equalizeWeights(staircase)), "after autosize");
});

test("a deep, lopsided tree still comes out equal at every leaf", () => {
  // Nothing about the rule depends on the tree being tidy: six panes at four
  // different depths, in both directions.
  const tree = split(
    split(leaf(), leaf(), leaf()),
    split(leaf(), split(leaf(), leaf()))
  );
  const shares = leafShares(equalizeWeights(tree));
  assert.equal(shares.length, 6);
  assertAllEqual(shares, "six panes, four depths");
});

test("a node's weight is exactly the number of panes under it", () => {
  // The rule itself, stated once: this is what makes the shares come out, and
  // it is the line a future editor is most likely to 'simplify' to 1.
  const tree = split(leaf(), split(leaf(), leaf()));
  const w = equalizeWeights(tree);
  assert.equal(w.weight, 3);
  assert.equal(w.children![0].weight, 1);
  assert.equal(w.children![1].weight, 2);
  assert.equal(w.children![1].children![1].weight, 1);
});

test("a lone pane is a valid, positive weight", () => {
  const w = equalizeWeights(leaf());
  assert.equal(w.weight, 1);
  assert.equal(w.children, undefined);
  assertAllEqual(leafShares(w), "one pane");
});

test("no node can come out at a zero weight, even from a malformed tree", () => {
  // A zero grow in a flex-basis:0 split is an INVISIBLE pane, so a childless
  // split — which grid.ts cannot produce, since it collapses a split that drops
  // to one child — must still not produce one. A malformed tree may lay out
  // oddly; it may not make a pane disappear.
  const weird = split(leaf(), { children: [] });
  const w = equalizeWeights(weird);
  const walk = (n: EqualizedWeights): void => {
    assert.ok(n.weight > 0 && Number.isFinite(n.weight), `weight ${n.weight} would hide a node`);
    n.children?.forEach(walk);
  };
  walk(w);
});

test("paneCount counts leaves, not nodes", () => {
  assert.equal(paneCount(leaf()), 1);
  assert.equal(paneCount(split(leaf(), leaf())), 2);
  assert.equal(paneCount(split(leaf(), split(leaf(), split(leaf(), leaf())))), 4);
  assert.equal(paneCount({ children: [] }), 1); // degenerate: never zero
});

/** A node as the real callers carry it: the shape, plus the weight this module
 *  must ignore. Both of `grid.ts`'s trees have one — the live tree reads its
 *  panes' `flexGrow`, and a `PersistedLayoutNode` stores it — so "the weights
 *  are not an input" is a claim about actual inputs, not a hypothetical. */
interface WeightedShape extends SplitShape {
  weight: number;
  children?: WeightedShape[];
}

test("the weights already on the tree are not an input — the shape is the whole input", () => {
  // This replaces an idempotence test that could not fail: it called
  // equalizeWeights twice on one input and compared, which is a tautology for
  // any deterministic pure function of the shape, whatever the function does.
  //
  // The property a human actually depends on is structural. Autosize reads the
  // tree's SHAPE and never its current weights, so it lands on the same answer
  // from any layout with that shape — dragged to a sliver, evened out a moment
  // ago, or restored from a session file. That is also what makes it
  // indifferent to whichever split policy ships underneath it.
  const shape = split(leaf(), split(leaf(), leaf()));
  const expected = equalizeWeights(shape);

  // Dragged hard: a near-invisible pane beside a giant, at both levels.
  const dragged: WeightedShape = {
    weight: 97,
    children: [
      { weight: 0.01 },
      { weight: 1e6, children: [{ weight: 3 }, { weight: 400 }] },
    ],
  };
  assert.deepEqual(
    equalizeWeights(dragged),
    expected,
    "a dragged layout must land on the weights its bare shape would get"
  );
  assertAllEqual(leafShares(equalizeWeights(dragged)), "a dragged layout, evened");

  // The second press, as it really happens: the tree grid.ts walks now carries
  // the FIRST press's weights, so the round trip is the honest idempotence test.
  assert.deepEqual(equalizeWeights(expected), expected, "pressing it twice must not drift the layout");
});
