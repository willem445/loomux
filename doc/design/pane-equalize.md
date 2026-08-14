# Even splits, and reclaiming a closed pane's space (#936)

A pane's size is `flex-grow / (sum of the split's grows)` of that split's free
space. Every pane-sizing decision in `grid.ts` is therefore a decision about
that fraction, and both of the ones a human triggers most often — split, close —
used to get the denominator wrong in the same direction: the layout drifted
further from even every time it changed.

## What the arithmetic did

**Insert.** A same-direction split gave the newcomer `1 / childCountBefore` and
left every incumbent's weight alone. Starting from one pane and splitting right
four times:

| panes | weights | shares |
| --- | --- | --- |
| 2 | 1, 1 | 50%, 50% |
| 3 | 1, 1, 0.5 | 40%, 40%, 20% |
| 4 | 1, 1, 0.5, 0.33 | 35%, 35%, 18%, 12% |
| 5 | 1, 1, 0.5, 0.33, 0.25 | 32%, 32%, 16%, 11%, 8% |

The fifth pane is a quarter the width of the first. `grid.ts`'s own header
comment and `docs/core-concepts.md` both promised an *even matrix* "rather than
a lopsided staircase"; the arithmetic built the staircase.

**Close.** `removeFromTree` spliced the leaf out and left the survivors' weights
untouched, so flex re-shared the freed space **proportionally** — in proportion
to how big each survivor already was. Close the 32% pane in the five-pane row
above and the other 32% pane takes nearly half of what was freed while the 8%
pane takes an eighth of it. A sliver stays a sliver.

What that is *not*: flex-grow with `flex-basis: 0` always fills its container,
so no weight combination can leave literally unpainted background behind a
closed pane. The "dead, unusable space" of #936 is the reclaimed space landing
on whichever pane was already biggest — often a large, near-empty welcome form —
while the panes you actually work in stay too narrow to use.

## The rule

`src/paneequalize.ts` is pure and DOM-free; `grid.ts` keeps the tree and the DOM
and asks it only what the split's grows should come out as.

- **A newcomer joins at the mean** of the weights already in the split. On a
  split nobody has dragged that is exactly an equal share, so N panes sit at
  1/N — at every N, not just the first.
- **A departing pane's weight is handed to the survivors in equal absolute
  parts.** The split's total is preserved exactly, and an even split stays even.

Together they give the invariant the issue asks for: *a layout nobody has
dragged is even after any sequence of splits and closes.*

### Why equal absolute parts, and not a full re-equalize

A human who dragged a divider to 25/75 asked for 25/75, and a close elsewhere in
the row is no reason to throw that away. Handing the freed weight out in equal
absolute parts still moves a skewed split **toward** even — the sliver gains the
same absolute weight as the giant, which is far more of its own size — so the
reclaim is real without overwriting an explicit gesture. Re-equalizing on every
close was rejected for that reason; it is also the more surprising behaviour,
since the pane you closed is not the pane whose size you would expect to change.

### Why preserving the split's total matters

Nothing outside the split can see its total (a nested split's own weight in its
parent is a separate number). The reason to preserve it is *inside*: the next
insert takes the mean, so a total that drifts on every close makes every later
newcomer drift with it. Preserving it is what makes the invariant hold across a
whole session rather than a single operation.

## Scope, and what this deliberately does not touch

- **Cross-direction splits are unchanged.** Splitting *down* on a pane in a row
  replaces that pane's slot with a nested two-way split at 1 and 1 — already
  even, and the outer row never moves.
- **The multi-agent welcome fan-out still nests.** It alternates direction per
  pane (`main.ts`), so every fan-out placement takes the cross-direction path
  and each agent halves the previous one: 1/2, 1/4, 1/8, 1/16. That is a second,
  independent source of "each successive pane is tinier", it is not what a human
  splitting by hand hits, and fixing it means teaching the tree to insert a row
  *beside a split* rather than beside a leaf — a structural change with its own
  design. Named here so a reader does not mistake this note for having covered
  it.
- **Floors are not here.** "Refuse a split that would put a pane below a usable
  size" is #885 slice B, whose constants are deliberately left for the human
  demo to tune. The relationship is worth stating, though: N equal panes at 1/N
  is the arrangement that keeps the *smallest* pane as large as it can be for a
  given pane count, so an even policy is what makes any floor bite as late as
  possible.
- **`halve` (#885 slice A, PR #900) is untouched by this.** That PR gives human
  split gestures a policy where the target pane alone pays, and it is parked at
  a human demo. This note's rule is the arithmetic of the *even* policy — the
  one the fan-out and the dock restore path use, and the one main has for every
  gesture today. If the demo keeps `halve` for human gestures, #936's "splits
  should fill equally" is a question about `halve` that the human has to settle;
  the two policies are a call-site choice, not a conflict in this math.
