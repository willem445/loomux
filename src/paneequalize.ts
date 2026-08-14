// Pure, DOM-free weight math for keeping a split row/column EVEN as panes are
// added to and taken out of it (#936). `grid.ts` owns the tree and the DOM; it
// asks this module only "what flex-grows should this split's children come out
// with", which is what makes the policy testable under `node --test`.
//
// Deliberately no cross-module import, matching every other pure module here
// (layout.ts, overlaysize.ts, embedsplit.ts, spawnexpiry.ts): tsc forbids the
// explicit `.ts` extension a bare intra-src import would need for the node
// runner to resolve it (TS5097), so a shared helper would work for one runner
// and not the other.
//
// WHAT WAS WRONG, AND WHY IT LOOKED LIKE DEAD SPACE (#936)
//
// A pane's size is `grow / total` of its split's free space. Both halves of
// the old math got that denominator wrong:
//
//   - INSERT gave the newcomer `1 / childCountBeforeInsert` and left every
//     incumbent's weight alone. In a row of two default panes (1, 1) the third
//     pane arrives at 0.5 against a total of 2.5 — 20% of the row while its
//     siblings hold 40% each. The fourth arrives at 1/3 (11.8%), the fifth at
//     0.25 (8.1%). The row's own header comment and docs/core-concepts.md both
//     promised an "even matrix"; what the arithmetic actually built was the
//     lopsided staircase they promised to avoid. Splitting five times left a
//     pane roughly a tenth the width of its oldest sibling.
//   - REMOVE spliced the closed pane out and left the survivors untouched, so
//     flex handed the freed space out PROPORTIONALLY — in proportion to how
//     big each survivor already was. The pane that was already large soaked up
//     nearly all of it and a sliver stayed a sliver. Closing the big pane in a
//     skewed row does not restore a usable layout; it re-skews it.
//
// Note what the second one is NOT: flex-grow with `flex-basis: 0` always fills
// its container, so no combination of weights can leave literally unpainted
// background behind a closed pane. The "dead, unusable space" of #936 is the
// reclaimed space landing on one pane (often a big, near-empty welcome form)
// while the panes you still work in stay slivers.
//
// THE RULE. Both operations keep an untouched layout exactly even:
//
//   - a newcomer joins at the MEAN of the weights already in the split, so a
//     row that was even is even again afterwards (N panes at 1/N), and
//   - a departing pane's weight is handed to the survivors in EQUAL ABSOLUTE
//     shares, so the split's total is unchanged and an even row stays even.
//
// Equal absolute shares rather than a full re-equalize on purpose: a human who
// dragged a divider to 25/75 asked for that, and a close somewhere else in the
// row is no reason to throw it away. Handing the freed weight out evenly still
// moves a skewed row TOWARD even (the sliver gains the same absolute weight as
// the giant, so it gains far more of its own size), which is the reclaim #936
// asks for, without overwriting an explicit human gesture.

/** The result of an insert: the split's flex-grows in child order, plus where
 *  the newcomer landed. Handed back together (rather than recomputed at the
 *  call site) so the weight array and the child array can never disagree about
 *  which entry is the new pane. */
export interface EvenInsertPlan {
  weights: number[];
  insertedIndex: number;
}

/** Read a live `style.flexGrow` (or a persisted weight) as a usable grow.
 *
 *  Anything unusable falls back to 1 — the same default the `style.flex ||=
 *  "1 1 0"` line in the old insert path applied, so a child that never had a
 *  flex set keeps behaving as it did. Zero and negative are repaired too, not
 *  just NaN: CSS silently DROPS a negative flex-grow rather than rejecting the
 *  declaration, and a zero-grow child in a `flex-basis: 0` split is an
 *  invisible pane — a weight that would make a pane vanish must never reach a
 *  style attribute. */
export function readGrow(raw: string | null | undefined): number {
  const n = raw === null || raw === undefined ? NaN : parseFloat(raw);
  return Number.isFinite(n) && n > 0 ? n : 1;
}

/** Repair a whole row the same way, so one junk entry can't poison the mean. */
function sane(weights: readonly number[]): number[] {
  return weights.map((w) => (Number.isFinite(w) && w > 0 ? w : 1));
}

/** Plan a SAME-DIRECTION insert: a new pane joins `weights` as a flat sibling
 *  of the child at `targetIndex`, landing after it (or `before` it).
 *
 *  The newcomer takes the MEAN of the weights already there. On a row nobody
 *  has dragged that is exactly an even share — three panes at 1, then four at
 *  1, then five — which is the "even matrix" this layout has always claimed
 *  and is also the arrangement that keeps the SMALLEST pane as large as it can
 *  be for a given pane count (N equal panes at 1/N is the max-min split of a
 *  row). Floors and refusing a split that would breach them are a separate
 *  concern (#885 slice B); this policy is what makes them bite as late as
 *  possible.
 *
 *  Flat, not nested, on purpose: the N-way row is why a divider drag only ever
 *  negotiates with its two immediate neighbours (grid.ts `makeDivider`).
 *
 *  Inputs are repaired rather than trusted — this is the last place a junk
 *  weight can be caught before it reaches a style attribute. */
export function planEvenInsert(
  weights: readonly number[],
  targetIndex: number,
  before = false
): EvenInsertPlan {
  const row = sane(weights);
  // An empty split has no target to insert beside and no mean to take; hand
  // back one usable weight so a caller in an impossible state still writes a
  // valid layout rather than `flex: NaN 1 0`.
  if (row.length === 0) return { weights: [1], insertedIndex: 0 };

  const idx = Math.max(0, Math.min(row.length - 1, Math.trunc(targetIndex) || 0));
  const insertedIndex = before ? idx : idx + 1;
  const mean = row.reduce((a, b) => a + b, 0) / row.length;

  row.splice(insertedIndex, 0, mean);
  return { weights: row, insertedIndex };
}

/** Plan a removal: the pane at `removedIndex` leaves the split, and its weight
 *  is shared out in equal absolute parts among the survivors.
 *
 *  The split's TOTAL weight is preserved exactly, which is the property that
 *  matters to everything outside this split: a nested split's own weight in
 *  its parent is a separate number, but a row whose total drifts on every
 *  close makes the next insert's mean drift with it. Preserving it means the
 *  arithmetic stays stable across any number of open/close cycles.
 *
 *  Returns the survivors' weights in child order. A removal that empties the
 *  split returns an empty array, and one that leaves a single child returns
 *  the whole total on that child — `grid.ts` collapses a one-child split into
 *  its parent's slot right after, overwriting that weight, so it is written
 *  for the caller that does not. */
export function planRemoval(weights: readonly number[], removedIndex: number): number[] {
  const row = sane(weights);
  if (row.length === 0) return [];
  const idx = Math.trunc(removedIndex);
  // Out of range: nothing left the split, so nothing is redistributed. Better
  // than redistributing a weight that no pane gave up, which would inflate the
  // total on every stray call.
  if (idx < 0 || idx >= row.length) return row;

  const freed = row[idx];
  row.splice(idx, 1);
  if (row.length === 0) return [];
  const each = freed / row.length;
  return row.map((w) => w + each);
}
