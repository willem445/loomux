// Unit tests for the pure even-split weight math (#936). Run with `npm test`
// (Node's built-in runner strips the TypeScript types natively).
//
// The assertions are phrased as the two INTENTS — "a split you never dragged
// is even after any insert" and "closing a pane hands its space out equally,
// keeping the total" — rather than as an expected-weights table, which would
// pass just as happily with a subtly wrong policy substituted. Where a literal
// weight is asserted it is because that number IS the intent (a mean, a total).
import { test } from "node:test";
import assert from "node:assert/strict";
import { planEvenInsert, planRemoval, readGrow } from "../src/paneequalize.ts";

/** Each entry's share of the split — the only thing a pane's size depends on. */
const shares = (weights: readonly number[]): number[] => {
  const total = weights.reduce((a, b) => a + b, 0);
  return weights.map((w) => w / total);
};

const total = (weights: readonly number[]): number => weights.reduce((a, b) => a + b, 0);

/** Assert two splits size their panes identically — the only comparison that
 *  means anything across a rescale, since a pane's size is its share. */
const assertSameShares = (got: readonly number[], want: readonly number[], what: string): void => {
  assert.equal(got.length, want.length, `${what}: pane count changed`);
  const [g, w] = [shares(got), shares(want)];
  for (let i = 0; i < g.length; i++) {
    assert.ok(
      Math.abs(g[i] - w[i]) < 1e-9,
      `${what}: pane ${i} moved from ${w[i]} to ${g[i]} (${JSON.stringify(got)} vs ${JSON.stringify(want)})`
    );
  }
};

/** Assert every pane in the split holds the same share, within float noise. */
const assertEven = (weights: readonly number[], what: string): void => {
  const expected = 1 / weights.length;
  for (const s of shares(weights)) {
    assert.ok(
      Math.abs(s - expected) < 1e-9,
      `${what}: expected every pane at ${expected}, got shares ${JSON.stringify(shares(weights))}`
    );
  }
};

// ---------- insert ----------

test("a row nobody has dragged is still even after a split — at every count", () => {
  // The regression #936 reports: the 3rd, 4th and 5th splits used to arrive at
  // 1/2, 1/3, 1/4 against a growing total (20%, 11.8%, 8.1% of the row) while
  // their siblings kept 40%+. Five splits, five even panes, is the whole ask.
  let weights = [1];
  for (let panes = 2; panes <= 6; panes++) {
    weights = planEvenInsert(weights, weights.length - 1).weights;
    assert.equal(weights.length, panes);
    assertEven(weights, `${panes} panes`);
  }
});

test("the newcomer is never the runt of the row", () => {
  // The failure a human sees is comparative: the pane you just made is
  // dramatically smaller than the one you made it from. Whatever the policy
  // does elsewhere, a fresh pane may not land below its siblings' average.
  const row = [1, 1, 1, 1];
  const { weights, insertedIndex } = planEvenInsert(row, 3);
  const s = shares(weights);
  const others = s.filter((_, i) => i !== insertedIndex);
  const mean = others.reduce((a, b) => a + b, 0) / others.length;
  assert.ok(
    s[insertedIndex] >= mean - 1e-9,
    `newcomer at ${s[insertedIndex]} is below the sibling mean ${mean}`
  );
});

test("a split the human dragged keeps that drag: the others' ratios to each other are untouched", () => {
  // 25/75 is a deliberate gesture. Adding a pane must re-share the row (it has
  // to come from somewhere) but must not silently re-level the two panes that
  // were dragged apart.
  const row = [0.5, 1.5];
  const { weights, insertedIndex } = planEvenInsert(row, 1);
  const kept = weights.filter((_, i) => i !== insertedIndex);
  assert.equal(kept.length, 2);
  assert.ok(
    Math.abs(kept[1] / kept[0] - 1.5 / 0.5) < 1e-9,
    `the dragged 1:3 ratio became ${kept[1] / kept[0]}:1`
  );
});

test("the newcomer takes the mean, so it is an equal share exactly when the row was equal", () => {
  assert.equal(planEvenInsert([1, 1, 1], 0).weights[1], 1);
  // Uneven row: the mean of 1 and 3 is 2 — bigger than the small pane, smaller
  // than the big one, which is what "a fair slice of this row" means.
  assert.equal(planEvenInsert([1, 3], 0).weights[1], 2);
});

test("insert lands the newcomer beside its target, on the side asked for", () => {
  assert.equal(planEvenInsert([1, 2, 3], 0).insertedIndex, 1);
  assert.equal(planEvenInsert([1, 2, 3], 0, true).insertedIndex, 0);
  assert.equal(planEvenInsert([1, 2, 3], 2).insertedIndex, 3);
});

// ---------- removal ----------

test("closing a pane hands its space out equally, so an even row stays even", () => {
  // The headline of #936: close one of five equal panes and the other four
  // must be four equal panes, not four panes plus a re-skew.
  let weights = [1, 1, 1, 1, 1];
  weights = planRemoval(weights, 2);
  assert.equal(weights.length, 4);
  assertEven(weights, "after closing the middle pane");
  weights = planRemoval(weights, 0);
  assertEven(weights, "after closing the first pane too");
});

test("closing a pane leaves no dead weight: the split's total is exactly what it was", () => {
  // "Weights re-sum to full" — the freed weight goes to the survivors, never
  // out of the row. A total that drifts on every close makes the next insert's
  // mean drift with it, which is how a layout ends up permanently skewed.
  const row = [1, 0.5, 2, 0.25];
  const before = total(row);
  for (let i = 0; i < row.length; i++) {
    const after = planRemoval(row, i);
    assert.ok(
      Math.abs(total(after) - before) < 1e-9,
      `removing index ${i} changed the total from ${before} to ${total(after)}`
    );
  }
});

test("the freed space goes to the panes that need it, not to the pane that already has it", () => {
  // The old behaviour: splice the pane out and let flex re-share
  // PROPORTIONALLY, so the giant absorbs nearly all of it and the sliver stays
  // a sliver. Equal absolute shares mean the sliver's share grows by more of
  // its own size than the giant's does — the row moves toward even, never away.
  const row = [4, 1, 0.5]; // a giant and two slivers, plus the pane being closed
  const closed = 3;
  const beforeShares = shares(row.concat(closed));
  const after = planRemoval(row.concat(closed), 3);
  const afterShares = shares(after);
  const growth = (i: number) => afterShares[i] / beforeShares[i];
  assert.ok(
    growth(2) > growth(1) && growth(1) > growth(0),
    `smaller panes must gain the most, relatively: gains were ${[0, 1, 2].map(growth).join(", ")}`
  );
  // And the skew genuinely narrows rather than merely not widening.
  const spreadBefore = Math.max(...beforeShares.slice(0, 3)) / Math.min(...beforeShares.slice(0, 3));
  const spreadAfter = Math.max(...afterShares) / Math.min(...afterShares);
  assert.ok(spreadAfter < spreadBefore, `spread went from ${spreadBefore} to ${spreadAfter}`);
});

test("a removal that empties or all-but-empties the split says so cleanly", () => {
  assert.deepEqual(planRemoval([1], 0), []);
  assert.deepEqual(planRemoval([], 0), []);
  // Two-pane split: the survivor holds the whole total. grid.ts collapses a
  // one-child split into its parent's slot right after and overwrites this,
  // but the number handed back is still a valid, usable weight.
  assert.deepEqual(planRemoval([1, 3], 0), [4]);
});

test("an out-of-range removal invents no weight", () => {
  assert.deepEqual(planRemoval([1, 2], 5), [1, 2]);
  assert.deepEqual(planRemoval([1, 2], -1), [1, 2]);
});

// ---------- degenerate input ----------

test("a junk weight can never reach a style attribute, and never poisons the mean", () => {
  // CSS silently DROPS a negative flex-grow and a zero-grow child in a
  // flex-basis:0 row is an invisible pane, so both are repaired to 1 — as are
  // NaN and Infinity, which would otherwise spread through the mean to every
  // pane in the split.
  for (const junk of [NaN, 0, -2, Infinity, -Infinity]) {
    const inserted = planEvenInsert([1, junk, 1], 0).weights;
    for (const w of inserted) {
      assert.ok(Number.isFinite(w) && w > 0, `insert leaked ${w} from ${junk}`);
    }
    assertEven(inserted, `junk ${junk} repaired to 1`);

    const removed = planRemoval([1, junk, 1], 0);
    for (const w of removed) {
      assert.ok(Number.isFinite(w) && w > 0, `removal leaked ${w} from ${junk}`);
    }
  }
});

test("readGrow falls back to 1 for anything a style attribute can hand it", () => {
  assert.equal(readGrow("2.5"), 2.5);
  assert.equal(readGrow(""), 1); // an element that never had flex set
  assert.equal(readGrow(null), 1);
  assert.equal(readGrow(undefined), 1);
  assert.equal(readGrow("0"), 1); // would be an invisible pane
  assert.equal(readGrow("-1"), 1); // CSS drops it; we must not write it
  assert.equal(readGrow("banana"), 1);
});

// ---------- magnitude (#954) ----------

/** A magnitude a human can still read out of a style attribute, and that a
 *  float can keep multiplying without drama. Deliberately far looser than
 *  anything the module aims for: the claim under test is "bounded", not
 *  "bounded at exactly this number", so a policy that lands anywhere sane
 *  passes and only a policy that grows without limit fails. */
const LEGIBLE = 1e6;

/** Assert a split's weights are numbers a human and a float can both live
 *  with — the property that survives any number of open/close cycles. */
const assertBounded = (weights: readonly number[], what: string): void => {
  for (const w of weights) {
    assert.ok(
      w <= LEGIBLE && w >= 1 / LEGIBLE,
      `${what}: weight ${w} is outside any legible range (${JSON.stringify(weights)})`
    );
  }
};

test("150 open/close cycles leave the weights bounded, not in exponential notation", () => {
  // #954. Every close preserves the split's total while dropping a pane, so the
  // MEAN of the row rises by (n+1)/n per cycle — 4/3 on a three-pane row — and
  // an insert (which arrives at that mean) does nothing to bring it back down.
  // The ratios stay right the whole way, which is why this is invisible until
  // the weights themselves reach exponential notation in a style attribute
  // (~1e18 by 150 cycles) and stop having float headroom above them.
  let weights = [1, 1, 1];
  for (let cycle = 0; cycle < 150; cycle++) {
    weights = planEvenInsert(weights, 1).weights; // open a pane beside the middle one
    weights = planRemoval(weights, 2); // close that same pane again
    assert.equal(weights.length, 3, `cycle ${cycle} changed the pane count`);
    assertEven(weights, `cycle ${cycle}`); // the layout must stay right, too
  }
  assertBounded(weights, "after 150 open/close cycles");
});

test("a split that arrives at an absurd scale is re-based, and no pane moves doing it", () => {
  // Both directions of absurd: a layout persisted by a build without the clamp
  // comes back at whatever magnitude it drifted to, and a hand-edited or
  // long-shrunk one comes back near the denormals where ratios stop being
  // representable. Either way the split's arithmetic is scale-free — the same
  // shape at any magnitude sizes its panes identically — so re-basing one into
  // range must be invisible: same shares, same layout, legible numbers.
  const shape = [2, 6, 4];
  for (const scale of [1e9, 1e-9]) {
    const absurd = shape.map((w) => w * scale);

    const inserted = planEvenInsert(absurd, 1).weights;
    assertBounded(inserted, `insert into a row at ${scale}x`);
    assertSameShares(inserted, planEvenInsert(shape, 1).weights, `insert at ${scale}x`);

    const removed = planRemoval(absurd, 0);
    assertBounded(removed, `removal from a row at ${scale}x`);
    assertSameShares(removed, planRemoval(shape, 0), `removal at ${scale}x`);
  }
});

test("a weight big enough to overflow the split's total still never reaches a style attribute", () => {
  // Two panes at 1e308 sum to Infinity, so the mean the newcomer arrives at is
  // Infinity and the freed weight a close shares out is Infinity/n — junk that
  // the per-weight repair upstream cannot see, because every INPUT weight is a
  // perfectly finite positive number. Re-basing before the arithmetic is what
  // keeps the sum finite.
  const overflowing = [1e308, 1e308, 1e308];
  for (const [what, weights] of [
    ["insert", planEvenInsert(overflowing, 0).weights],
    ["removal", planRemoval(overflowing, 0)],
  ] as const) {
    for (const w of weights) {
      assert.ok(Number.isFinite(w) && w > 0, `${what} leaked ${w} from an overflowing row`);
    }
    assertEven(weights, `${what} on an overflowing row`);
  }
});

// ---------- the invariant that ties the two halves together ----------

test("any sequence of splits and closes leaves an undragged layout even", () => {
  // The property #936 actually asks for, exercised as a session rather than as
  // a single call: open five panes, close two, open two more. Every step of
  // that must leave equal panes — it is the arithmetic drifting across a
  // sequence that produced the reported layout, not any one operation.
  let weights = [1];
  const opens = [0, 1, 2, 3];
  for (const at of opens) weights = planEvenInsert(weights, at).weights;
  assertEven(weights, "five panes open");

  weights = planRemoval(weights, 4);
  weights = planRemoval(weights, 0);
  assertEven(weights, "after closing two");
  assert.equal(weights.length, 3);

  weights = planEvenInsert(weights, 1).weights;
  weights = planEvenInsert(weights, 0).weights;
  assertEven(weights, "after opening two more");
  assert.equal(weights.length, 5);
});
