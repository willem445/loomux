// Unit tests for the split placement-policy weight math (#885 slice A). Run
// with `npm test`.
//
// These assert INTENT, not arithmetic: the whole point of the `halve` policy
// is a property about the panes it does NOT touch ("splitting the pane I am
// in must not move anybody else"), and the point of keeping `share` is that
// programmatic fan-out still lays out as an even matrix. Every test below is
// phrased as one of those two claims — an expected-weights table would pass
// just as happily with the two policies swapped.
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseGrow, planRowSplit } from "../src/splitfloor.ts";

/** Sum of a row's flex-grows — what decides each sibling's SHARE of the row,
 *  since every child is `flex: <grow> 1 0` (basis 0). A sibling's on-screen
 *  size is `grow / total × freeSpace`, so "weight unchanged AND total
 *  unchanged" is exactly "this sibling's share of the row did not move".
 *
 *  Deliberately not "did not move in pixels": `freeSpace` is the other factor,
 *  and a same-direction insert also adds a divider, which is a real flex
 *  sibling costing the row ~4px (see `planRowSplit`'s halve branch). This
 *  module owns the ratio; the pixels are the ratio times a denominator this
 *  module cannot see. */
const total = (w: readonly number[]): number => w.reduce((a, b) => a + b, 0);

const closeTo = (actual: number, expected: number, msg: string): void => {
  assert.ok(
    Math.abs(actual - expected) < 1e-9,
    `${msg}: expected ~${expected}, got ${actual}`
  );
};

// A 3-wide row as the app actually produces one: split a root pane (nested
// 2-way, both children grow 1), then split the right-hand one under `halve`
// (1 → 0.5 + 0.5). Weights are deliberately NOT all equal, so a policy that
// "helpfully" re-evens the row is caught rather than accidentally matching.
const ROW = [1, 0.5, 0.5] as const;

test("halve: siblings' weights are unchanged by a split", () => {
  for (const targetIndex of [0, 1, 2]) {
    const { weights, insertedIndex } = planRowSplit(ROW, targetIndex, "halve");
    // Everything that is neither the target nor the newcomer must come out
    // byte-identical to what went in.
    const survivors = weights.filter((_, i) => i !== insertedIndex && i !== targetIndexAfter(targetIndex, insertedIndex));
    const expected = ROW.filter((_, i) => i !== targetIndex);
    assert.deepEqual(survivors, expected, `splitting index ${targetIndex} disturbed a sibling`);
  }
});

/** Where the target's own weight ended up after the newcomer was spliced in. */
function targetIndexAfter(targetIndex: number, insertedIndex: number): number {
  return insertedIndex <= targetIndex ? targetIndex + 1 : targetIndex;
}

test("halve: the row's total weight is unchanged, so every sibling keeps its exact share of the row", () => {
  for (const targetIndex of [0, 1, 2]) {
    const { weights } = planRowSplit(ROW, targetIndex, "halve");
    closeTo(total(weights), total(ROW), `splitting index ${targetIndex} changed the row's total`);
  }
});

test("halve: the split is paid for by the target pane alone", () => {
  const { weights, insertedIndex } = planRowSplit(ROW, 0, "halve");
  const target = weights[targetIndexAfter(0, insertedIndex)];
  const newcomer = weights[insertedIndex];
  closeTo(target, newcomer, "the two halves are not the same size");
  closeTo(target + newcomer, ROW[0], "the pair does not add back up to what the target had");
});

test("share: the newcomer takes an even 1/N slice and no existing weight is rewritten", () => {
  const { weights, insertedIndex } = planRowSplit(ROW, 0, "share");
  // 1/N of a 3-child row — the even-matrix policy the multi-agent fan-out
  // wants, unchanged from pre-#885 behaviour.
  closeTo(weights[insertedIndex], 1 / 3, "newcomer did not open at 1/N");
  assert.deepEqual(
    weights.filter((_, i) => i !== insertedIndex),
    [...ROW],
    "share rewrote an existing pane's weight"
  );
});

test("the two policies part company exactly where the human feels it: share grows the row's total, halve does not", () => {
  const halved = planRowSplit(ROW, 0, "halve");
  const shared = planRowSplit(ROW, 0, "share");
  // Growing the total is what shrinks EVERY sibling's share (grow/total), and
  // is precisely the "the whole row re-flows when I split one pane" feel #885
  // is about. Under halve the total is fixed, so the target's share is the
  // only one that moves — the siblings' pixels still give up their slice of
  // the new divider's ~4px (see planRowSplit), which is a different and much
  // smaller thing.
  closeTo(total(halved.weights), total(ROW), "halve changed the row's total");
  assert.ok(
    total(shared.weights) > total(ROW) + 1e-9,
    "share should grow the row's total (that is what re-shares the row)"
  );
});

test("a split lands after the target by default and before it when asked (split-left / split-up, drag-to-edge)", () => {
  const after = planRowSplit(ROW, 1, "halve");
  assert.equal(after.insertedIndex, 2, "default insert should land after the target");
  const before = planRowSplit(ROW, 1, "halve", true);
  assert.equal(before.insertedIndex, 1, "`before` insert should land ahead of the target");
  // Same geometry either way — only the order differs.
  closeTo(total(before.weights), total(after.weights), "insert side changed the row's total");
  assert.equal(before.weights.length, ROW.length + 1);
});

test("a degenerate weight never leaks a NaN/negative flex-grow into the layout", () => {
  // A hand-edited tabs.json, an unset flex, or a stale float can put junk in a
  // row; CSS silently DROPS a negative flex-grow rather than rejecting it
  // (same trap embedsplit.ts documents), so a bad input must be repaired here
  // rather than written to a style attribute.
  for (const junk of [NaN, 0, -3, Infinity]) {
    for (const policy of ["halve", "share"] as const) {
      const { weights } = planRowSplit([junk, 1], 0, policy);
      for (const w of weights) {
        assert.ok(Number.isFinite(w) && w > 0, `${policy} produced a ${w} from a ${junk} weight`);
      }
    }
  }
});

test("splitting a lone pane still yields two halves of what it had", () => {
  const { weights, insertedIndex } = planRowSplit([2], 0, "halve");
  assert.equal(weights.length, 2);
  closeTo(weights[insertedIndex], 1, "newcomer should be half of 2");
  closeTo(total(weights), 2, "a lone pane's split should not change the total either");
});

test("an out-of-range target is clamped instead of producing a hole in the row", () => {
  for (const idx of [-1, 5]) {
    const { weights, insertedIndex } = planRowSplit(ROW, idx, "halve");
    assert.equal(weights.length, ROW.length + 1, `target ${idx} lost or duplicated a pane`);
    assert.ok(insertedIndex >= 0 && insertedIndex < weights.length);
    for (const w of weights) assert.ok(Number.isFinite(w) && w > 0);
  }
});

test("an empty row is a no-op that still hands back one usable weight", () => {
  const { weights, insertedIndex } = planRowSplit([], 0, "halve");
  assert.deepEqual(weights, [1]);
  assert.equal(insertedIndex, 0);
});

test("parseGrow reads a live flex-grow and falls back to 1 on anything unusable", () => {
  closeTo(parseGrow("0.5"), 0.5, "a real weight should survive");
  // An element that never had `flex` set reads as "" — the pre-#885 code's own
  // `style.flex ||= '1 1 0'` default, preserved so an unweighted child keeps
  // behaving exactly as it did.
  assert.equal(parseGrow(""), 1);
  assert.equal(parseGrow(null), 1);
  assert.equal(parseGrow(undefined), 1);
  assert.equal(parseGrow("banana"), 1);
  assert.equal(parseGrow("-2"), 1);
  assert.equal(parseGrow("0"), 1);
});
