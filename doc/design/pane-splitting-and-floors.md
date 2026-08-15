# Pane splitting: who pays for the new pane (#885)

The ask (#885): make splitting feel **native** — "split the pane I am in",
the way tmux, wezterm and zellij do it — instead of a global re-flow of the
whole layout; and, separately, stop unbounded subdivision from producing
panes too small to read.

This note covers the first half, which is what ships in slice A: the
**placement policy**. The floor constants, the capacity predicates and the
overflow-to-dock behaviour are slice B and land in this note's *Floors*
section when they do.

## What was actually missing

Not "split relative to the active pane" — `grid.ts` has always placed a new
pane relative to the active one (`placeLeaf` defaults `relativeTo` to
`this.active`). Every gesture already did the right thing about *where*.

The gap was **geometry**. `insertBeside` has two cases:

- **Cross-direction** (split-down on a pane inside a row): the target leaf is
  replaced by a nested two-way split. The target's slot keeps its weight and
  its two children get one each — genuine in-place halving, already native,
  nothing to fix.
- **Same-direction** (split-right on a pane already inside a row): a flat
  sibling joins the parent row at `1/N`, *added on top of* the existing
  weights. Because a flex child's size is `grow / total`, growing the total
  shrinks **every** sibling in the row.

So splitting one pane re-flowed the entire row. That is the "global layout
flow" feel #885 is reacting to, and it is one line of arithmetic deep.

## Two intents, two policies, one mechanism

The cost of a split has to come out of somebody's pixels. Who should pay
depends entirely on **who asked**, and loomux has two askers:

- A **human** splitting the pane they are looking at means *"give this new
  thing space out of THIS pane"*. Everything else on screen should sit as
  still as the layout can hold it. That is **`halve`**: the target and the
  newcomer each take half of the target's weight, so the row's total is
  unchanged and every other sibling keeps its exact **share** of the row.
  (Its share, not its pixels — see *What "the siblings don't move" actually
  means* below, which is the sharpest thing in this note.)
- A **programmatic fan-out** — the multi-agent welcome form placing five
  agents in one pass — means *"lay these out as an even matrix"*. That is
  **`share`**: the newcomer joins at the **mean** of the weights already in the
  row, so a row that was even is even again at every pane count.

`share` is not legacy debt to be migrated away; it is the right answer for
batch placement. Halving repeatedly through a five-agent fan-out deals out a
1/2, 1/4, 1/8, 1/16 sliver staircase — exactly the lopsided layout the
even-matrix policy was written to avoid. Keeping both is the design.

### Which module serves each policy (#885 + #936 + #954)

The two policies are served by **two different modules**, and that split is
load-bearing rather than incidental:

| policy | planner | why |
| --- | --- | --- |
| `halve` | `splitfloor.planRowSplit` | this slice's own arithmetic, unchanged |
| `share` | `paneequalize.planEvenInsert` | #936's even-matrix fix |

`splitfloor` has a `share` branch of its own, and **`grid.ts` deliberately does
not route to it.** That branch is the pre-#885 rule — *an even `1/N` slice on
top of the existing weights* — which is precisely the staircase #936 reported
and fixed: it gives the third pane in a row 20% against siblings holding 40%
each. `planEvenInsert` gives the newcomer the row's mean instead, which is the
even matrix both modules' comments have always promised.

On a row whose weights have drifted, the difference stops being cosmetic. Feed
`[2e9, 6e9, 4e9]` to each and the routed arm re-bases the row into
`[0.33, 1, 0.67, 0.67]`; splitfloor's own arm returns
`[2e9, 6e9, 0.33, 4e9]` — a newcomer holding roughly one part in ten billion,
which is a pane the human cannot see. `test/splitfloor.test.ts` pins both
halves of that: the counterfactual, and (as a source scan, in the shape
`test/transport.test.ts` established) that `planRowSplit` is only ever called
with the `halve` literal.

The branch is kept rather than deleted because it is the #885 module's own API
and its removal is a separate, reviewable change — but nothing calls it, and
**deleting it is the obvious follow-up** now that the routing is settled. It is
named here so the next reader does not "restore" it as a simplification.

### Where the clamp sits, and why `halve` is not under it

#954 added a magnitude clamp: `paneequalize` re-bases a row whose largest
weight has left `[1e-3, 1e3]`, preserving every share exactly. It lives at that
module's **entry points**, so the routing above composes with it unchanged —
the `share` insert and every close (`planRemoval`) pass through it, and an
out-of-band row comes back in band on the next one of either.

**`halve` reaches no clamp, and that is deliberate.** The argument is that it
cannot cause the problem the clamp exists for. #954's drift comes from the one
operation that raises a row's magnitude: a close preserves the row's total
across *one fewer pane*, so the mean climbs by `(n+1)/n` every open/close
cycle. `halve` preserves the row's total **exactly** — that is the guarantee
that makes a human split local — so it is magnitude-neutral by construction,
and re-basing there would rewrite the very numbers the guarantee is about while
buying nothing.

Two consequences worth stating rather than discovering later:

- A row that arrives out of band (a layout persisted by a pre-#954 build) and
  is only ever *halve*-split stays out of band until a close or a programmatic
  placement touches it. Nothing is visibly wrong — shares are exact at any
  magnitude — and the first close re-bases it.
- `halve` drives the *smallest* weight down geometrically (a half, a quarter,
  an eighth), and the clamp keys on a row's **largest** weight, so it will not
  fire on that. What bounds it is not arithmetic but the pane floor below:
  long before a weight is small enough to matter, the pane is too small to
  split. That is a floors question, not a clamp question.

The arithmetic lives in the pure, DOM-free `src/splitfloor.ts`
(`planRowSplit`), unit-tested under `node --test` like `layout.ts`,
`panefit.ts` and `embedsplit.ts`; `grid.ts` keeps the tree and DOM mutation
and asks the module only *"what weights should this row come out with"*.
`insertBeside`'s policy parameter is **required**, not defaulted, so every
call site has to state which of the two intents it is — the four human
gestures (`Ctrl+Shift+E`/`Ctrl+Shift+O`, the two top-bar buttons, a pane
header's ◫/⬓, and drag-to-edge) say `halve`; the fan-out, dock restores and
restore replay say `share`.

### What stays flat

`halve` changes the weights, never the structure: a same-direction split
still inserts a **flat** sibling into the N-way row. That flatness is why a
divider drag only ever negotiates with its two immediate neighbours
(`grid.ts`'s `makeDivider`, and the same convention `embedsplit.ts`'s module
comment documents). Always nesting on a same-direction split would halve just
as well, but tree depth and divider count would grow without bound and every
divider would start trading space across subtree boundaries.

### Rejected

- **Always nest on same-direction splits** — see above.
- **Switch the fan-out to `halve` too** — the sliver staircase.
- **Make `halve` the only policy** — same thing; restore replay would not
  care (`applyLayoutWeights` overwrites every weight after the tree is
  rebuilt), but the fan-out would.
- **Give the new pane an instant shell instead of the welcome form** — in
  loomux a new pane's question is "which agent/surface", not "another
  shell"; an instant-shell split would be a second spawn path for a minority
  case. Revisit at the demo if it feels un-native.

## Hard constraint 1, argued

`doc/design/embedded-panels.md`'s *The PTY-resize boundary, argued* already
draws the line this work stands on: constraint 1 targets **continuous,
chrome-driven resizing** — a badge appearing, an overlay opening, a tab
becoming active — sized from triggers the human never aimed at the terminal.
A **split is a discrete, user-initiated layout operation**; its one resize is
the operation's own honest cost, which is why `grid.ts` has always
legitimately resized PTYs on split.

This work does not merely stay on the legitimate side of that line — it
**reduces** the cost:

- `halve` guarantees **one** resize per split — the pane being split — and
  leaves every sibling's share of the row untouched. `1/N` re-shared the row,
  so it resized **every** sibling in it, every time. The next section is
  precise about the residue `halve` does not eliminate; even counting that
  residue at its worst, this is strictly and dramatically fewer
  `ResizePseudoConsole` calls per split.
- Drag paths keep the existing commit-on-release coalescing
  (`beginResizeHold` → one resize per pane per drag, #432). No live-drag PTY
  resizing existed and none is added.
- No new resize *trigger* is introduced anywhere: the policy is evaluated
  inside structural operations that were already resizing.

### What "the siblings don't move" actually means

`halve` preserves each sibling's `grow / total` **ratio**. It does not
preserve each sibling's **pixels**, and the difference is not pedantry — it
is the difference between a claim that is true and one that is not:

- A pane's size is `grow / total × freeSpace`. `halve` holds the first factor
  fixed. It cannot hold the second: a same-direction insert also adds a
  **divider**, and a divider is a real flex sibling (`.split.row > .divider`
  is `width: 6px` with `margin: 8px -1px` — a **4px** outer footprint) taken
  off the top before free space is distributed.
- So every sibling loses `ratio × 4px`. About **1px** each in a four-pane
  row; about 3.6px for a sibling holding 90% of a row.
- `shouldResizePty` (`panefit.ts`) skips only an identical `cols x rows`, and
  a cell is ~8.4px wide at the default font — so a sibling whose width sat
  within that 1px of a cell boundary **does** issue one real PTY resize. On
  the order of one split in eight, per sibling, in a typical row; more for a
  sibling holding a large share of a wide one.

The honest claim, which is still the whole win: **one guaranteed resize (the
pane being split), plus an occasional sub-cell nudge that costs a sibling one
column** — against `share`, which moved every sibling by tens of pixels and
resized all of them, every split. Do not let this get re-compressed into
"never resizes its PTY" in a future edit; it is wrong, and the honest version
loses nothing.

`e2e/tests/pane-split.spec.ts` machine-checks exactly the true property: it
splits the leftmost pane of a 3-wide row and asserts each untouched pane's
**share of the row** is unchanged (measured with no magic numbers — the panes'
widths sum to the row's distributable space by construction), plus a plain
pixel bound of one divider's footprint. It is deliberately **not** labelled a
proof that no sibling's PTY resized: a `cols` flip needs ~1px, so no
bounding-box spec can exclude one. What it does exclude, by an order of
magnitude, is a re-share of the row.

## What is NOT changed

- **`PersistedLayoutNode` / `tabs.json`** — untouched. Weights were already
  arbitrary floats, and restore replays the tree and then overwrites every
  weight via `applyLayoutWeights`, so the policy is not even observable
  during a restore.
- **Keyboard bindings and buttons** — the same four gestures, in the same
  places. `insertBeside` already supports `before`, so split-left/split-up
  variants are a cheap follow-on if the demo asks for them.
- **Cross-direction splits** — already halving; not touched.
- **Dependencies** — none added, of any kind.
- **The session browser's "resume this session" pane** (`main.ts`'s
  `grid.openPane` for a hand-resumed session) keeps `share`. It is a human
  click, but what it performs is a **restore** — bringing a recorded session
  back into the layout — not "give this new thing space out of the pane I am
  in", which is the sentence `halve` answers. Same reading as a dock restore
  and a replayed layout. Flagged here because it is the one call site where a
  reader will reasonably ask.

## Floors and deliberate overflow

Slice B. The shape agreed in the plan (#885, plan-389 §4), recorded here so
slice A's reader knows where this is going and does not "fix" a gap that is
deliberately still open:

- Named per-axis floor constants live in `splitfloor.ts`, single-sourced and
  tuned with the human at the demo. Deliberately font-independent pixel
  approximations rather than live cell metrics — state-dependent geometry
  feeding layout decisions is the coupling constraint 1 distrusts.
- **A floor below 60px would be inert**, and it also bounds slice A's
  untouched-sibling property: `.pane` carries `min-width: 60px` /
  `min-height: 60px` (`styles.css`). Flex clamps an item at its minimum and
  redistributes the surplus to the *other* items, so once repeated halving
  drives the target's computed width under 60px (roughly the 5th consecutive
  halve of one pane in a 1600px row) the siblings **grow** and resize. Below
  that point the ratio guarantee above stops holding, which is one more
  reason the floors exist — and a reason to pick them well above 60.
- A capacity predicate gates split gestures, drag-to-edge drops and dock
  restores. Floors gate **new growth** only.
- A window shrink does **not** trigger relayout or eviction; panes degrade
  proportionally below floor, matching the accepted degradation
  `embedsplit.ts` and `overlaysize.ts` already document.
- A session restore replays a layout the human explicitly had, floors or no
  floors — restoring what existed is never blocked by a rule about growth.
- When a gesture would break a floor, the new pane opens **in the dock**
  (zero PTY resizes — `openPaneMinimized` never fits) rather than being
  refused outright: the dock preserves the human's intent, and attention
  routing (#6) means a docked agent's ask is never lost.

**Carried in from another review, for slice B to settle.** rev-645 (on the S3
workflow-pane slice) deferred a geometry finding to "#885's floors decision",
which is this note's slice B: a pane's **fixed chrome is far larger than the
60px floor** — an inspector-bearing pane budgets on the order of 210px + 340px
of non-negotiable furniture against a `MIN_PANE_PX` of 80 — so a pane can pass
the floor and still have no usable content area at all. That is a real gap in
"what is a usable pane", and it is the same question slice B has to answer when
it picks its constants: a floor expressed as one number for every pane kind
cannot be right when the kinds have order-of-magnitude different chrome.

Recorded here rather than fixed: this slice sets **who pays** for a split, not
**when a split is refused**, and no constant in it is the one that finding is
about. Slice B owns it, with the human at the demo — which is also the only
place the numbers can honestly be tuned.
