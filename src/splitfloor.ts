// Pure, DOM-free weight math for pane splitting (#885). No DOM imports, so
// it's unit-testable under `node --test` (mirrors layout.ts / embedsplit.ts /
// overlaysize.ts); `grid.ts` owns the tree/DOM mutation and asks this module
// only "what weights should the row come out with".
//
// Deliberately no cross-module import (every other pure, node:test-covered
// module in this codebase — layout.ts, overlaysize.ts, embedsplit.ts,
// spawnexpiry.ts, taskboard.ts — is self-contained): tsc's build forbids the
// explicit `.ts` extension an intra-src import would need for `node --test` to
// resolve it directly (TS5097), so a bare specifier here would work for one
// runner and not the other.
//
// WHY THERE ARE TWO POLICIES. A split's cost has to be paid out of somebody's
// pixels, and who pays depends on who asked:
//
//   - A HUMAN splitting the pane they are looking at means "give this new
//     thing space out of THIS pane" — every other pane on screen should sit
//     as still as the layout can hold it. That is `halve`, and it is what
//     tmux/wezterm do.
//   - A PROGRAMMATIC fan-out (the multi-agent welcome form placing five
//     agents at once) means "lay these out as an even matrix". Halving
//     repeatedly there would produce a 1/2, 1/4, 1/8, 1/16 sliver staircase.
//     That is `share`, which is what every split did before #885.
//
// Two intents, two policies, one mechanism — see
// `doc/design/pane-splitting-and-floors.md`.
//
// ONLY the SAME-DIRECTION case needs a policy at all. A cross-direction split
// (split-down on a pane inside a row) replaces the target's slot with a nested
// two-way split whose children are 1 and 1 — the target's own slot keeps its
// weight, so the outer row never moves and both policies already agree. That
// path stays in `grid.ts` untouched; there is nothing for this module to
// decide about it.

/** Who pays for the new pane's space in a SAME-DIRECTION split.
 *
 *  - `halve` — the target pane alone: it and the newcomer each take half of
 *    the target's weight, so the row's TOTAL is unchanged and every other
 *    sibling keeps its exact `grow / total` RATIO. Note what that does and
 *    does not promise: a sibling's SHARE of the row is untouched, but the row
 *    itself has one more divider in it, so its free space shrinks by that
 *    divider's ~4px and each sibling loses its ratio's slice of them (~1px in
 *    a typical row — see `planRowSplit`). Every human split gesture uses this.
 *  - `share` — the whole row: the newcomer takes an even 1/N slice on top of
 *    the existing weights, so the total grows and every sibling shrinks
 *    proportionally. Pre-#885 behaviour, kept for programmatic batch
 *    placement. */
export type SplitPolicy = "halve" | "share";

/** What a same-direction insert should leave the parent split's children at. */
export interface RowSplitPlan {
  /** The row's flex-grows AFTER the insert, in child order — one entry per
   *  child, the newcomer included. */
  weights: number[];
  /** Which entry of `weights` is the newly inserted pane, i.e. the index the
   *  caller must splice its node in at. Handed back (rather than recomputed
   *  from `before` at the call site) so the weight array and the child array
   *  can never disagree about where the new pane went. */
  insertedIndex: number;
}

/** Read a live `style.flexGrow` (or a persisted weight) as a usable grow.
 *
 *  Falls back to 1 for anything unusable — an element that never had `flex`
 *  set reads as `""`, which is exactly the `style.flex ||= "1 1 0"` default
 *  the pre-#885 insert path applied, so an unweighted child keeps behaving as
 *  it did. Zero and negatives are repaired too: CSS silently DROPS a negative
 *  flex-grow rather than rejecting it (the same trap `embedDragGrow` clamps
 *  its output for), and a zero-grow child in a `flex-basis: 0` row is an
 *  invisible pane. */
export function parseGrow(raw: string | null | undefined): number {
  const n = raw === null || raw === undefined ? NaN : parseFloat(raw);
  return Number.isFinite(n) && n > 0 ? n : 1;
}

/** Plan a SAME-DIRECTION insert: a new pane joins `weights` as a flat sibling
 *  of the child at `targetIndex`, landing after it (or `before` it).
 *
 *  Flat, not nested, on purpose: the N-way row is why a divider drag only ever
 *  negotiates with its two immediate neighbours (grid.ts `makeDivider`,
 *  embedsplit.ts's module comment). `halve` changes who pays for the split,
 *  never that structure.
 *
 *  Inputs are repaired rather than trusted (see `parseGrow`) — this is the
 *  last place a junk weight can be caught before it reaches a style
 *  attribute. */
export function planRowSplit(
  weights: readonly number[],
  targetIndex: number,
  policy: SplitPolicy,
  before = false
): RowSplitPlan {
  const row = weights.map((w) => (Number.isFinite(w) && w > 0 ? w : 1));
  // An empty row has no target to split off; hand back one usable weight so a
  // caller in an impossible state still writes a valid layout.
  if (row.length === 0) return { weights: [1], insertedIndex: 0 };

  const idx = Math.max(0, Math.min(row.length - 1, Math.trunc(targetIndex) || 0));
  const insertedIndex = before ? idx : idx + 1;

  let newcomer: number;
  if (policy === "halve") {
    // The target pays alone: half stays, half goes to the newcomer. The sum is
    // preserved exactly (halving a float is exact), so no sibling's grow/total
    // ratio moves at all.
    //
    // The ratio is the whole guarantee — it does NOT follow that a sibling's
    // pixels are untouched, because the DENOMINATOR of `ratio × freeSpace`
    // also moves: a same-direction insert adds a divider, and a divider is a
    // real flex sibling (`.split.row > .divider`: 6px wide with -1px side
    // margins, so a 4px outer footprint) taken off the top before free space
    // is distributed. Each sibling therefore loses `ratio × 4px` — about 1px
    // in a four-pane row, about 3.6px for a sibling holding 90% of it. That
    // is usually zero terminal columns, but `shouldResizePty` (panefit.ts)
    // compares a `cols x rows` string, so a sibling whose width sat within
    // 1px of a cell boundary (~8.4px at the default font) does issue one real
    // PTY resize. Fewer resizes than `share`, which moves every sibling by
    // tens of px — not zero. Do not restore the stronger claim.
    const half = row[idx] / 2;
    row[idx] = half;
    newcomer = half;
  } else {
    // Pre-#885: an even share of the row as it stood, added on top. Existing
    // weights are left alone; it is the grown TOTAL that re-shares the row.
    newcomer = 1 / row.length;
  }

  row.splice(insertedIndex, 0, newcomer);
  return { weights: row, insertedIndex };
}
