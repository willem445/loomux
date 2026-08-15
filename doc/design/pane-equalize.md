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

### Why the weights are re-based anyway (#954)

The shares are stable across a whole session; the *numbers carrying them* are
not. A close preserves the total across one fewer pane, so it multiplies the
row by `(n+1)/n`, and an insert arrives at the mean, which leaves the mean
exactly where it found it. Every open/close cycle therefore scales the whole row
up — 4/3 of itself on a three-pane row — with the layout staying perfectly
correct the entire time. Around 120 cycles of that and the `flex` attributes,
and the saved layout, are in exponential notation; far enough and the total is
`Infinity`, which is the one junk value the per-weight repair cannot catch,
since every individual weight is still a finite positive number and only their
*sum* is nonsense. The newcomer's mean is then `Infinity`, and so is every pane
in the split.

**What the exponential form does and does not cost, since it is the number
people notice first.** The threshold is exact and it is JavaScript's, not
CSS's: `String(n)` switches to exponential at 1e21, and 1.5^120 is
`1.3519202917880824e+21` — so a two-pane row reaches it at almost exactly the
120 cycles above, and `style.flex` is then written as
`"1.3519202917880824e+21 1 0"`. It is tempting to call that a parse failure and
make it the case for the band. **It is not one.** CSS `<number>` has permitted
scientific notation since css-values-3 ("optionally, it can be concluded by the
letter `e` or `E` followed by an integer indicating the base-ten exponent"), it
has been available across browsers since 2015, and `JSON.parse` round-trips the
same literal, so neither the style attribute nor `tabs.json` is damaged by the
notation itself. What the exponential form actually costs is that the numbers
stop being readable, diffable or hand-editable — in DevTools, in a saved layout,
in a bug report — while the *real* cliff, `Infinity`, keeps approaching behind
it. The band is worth having for the cliff; the exponential notation is the
early warning that the row is heading for it, and is worth writing down mainly
so nobody re-derives the dropped-declaration story and fixes this for a reason
that isn't true.

So `paneequalize.ts` re-bases a row whose largest weight has left `[1e-3, 1e3]`,
putting that largest pane back at exactly 1 — the value a fresh pane is written
at. Three things make this cheap rather than a second policy:

- **It cannot move a pane** — with one pathological exception, which is why the
  code applies the per-weight repair twice. Both operations commute with a
  uniform rescale (the mean of `k·w` is `k·mean`; an equal absolute share of
  `k·freed` is `k·each`), so magnitude carries no layout information at all; the
  test compares an absurdly-scaled row's results against the same shape at 1x
  and demands identical shares. The exception is a row whose *spread* is already
  wider than a float can hold: `[1e-320, 1e300]` re-bases **through zero** —
  `1e-320 / 1e300` underflows — and the repair then lifts that zero back to 1,
  so a pane holding ~1e-620 of the row comes out holding half of it. That is a
  real share change, and it is the one the heading would otherwise deny. It is
  also unreachable by anything the app itself does: drift is a uniform rescale,
  which never widens spread, and a divider drag is bounded by the pixels on
  screen. Only a hand-edited `tabs.json` gets there — and the alternative,
  writing the zero through, is a pane that cannot be seen or clicked at all.
- **It is measured on the largest weight, not the total,** because a total can
  already be `Infinity` by the time anything looks at it, and catching that is
  half the point.
- **The band is wide, not tight.** Normalizing on every call would be simpler,
  but the rescale — meaningless as it is to the layout — still rewrites numbers
  a human may recognise in DevTools or in a persisted layout. Doing it only once
  they have stopped being readable keeps the common path byte-for-byte what it
  was, which is also why every assertion written for #936 still holds unchanged.
  `1e3` also leaves the whole approach to `Infinity` on the far side of the
  band: a re-based row is bounded by ~1.5e3 after any single operation, which is
  305 decimal orders of headroom, so the overflow case is not being defended
  against by a narrow margin.

It is self-healing in the direction that matters: a layout saved by a build
without this comes back at whatever magnitude it drifted to and is re-based by
the first split or close in it.

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
