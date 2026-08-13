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
  thing space out of THIS pane"*. Everything else on screen should sit
  perfectly still. That is **`halve`**: the target and the newcomer each take
  half of the target's weight, so the row's total is unchanged and every
  other sibling keeps its exact pixel share.
- A **programmatic fan-out** — the multi-agent welcome form placing five
  agents in one pass — means *"lay these out as an even matrix"*. That is
  **`share`**, the pre-#885 policy, kept unchanged.

`share` is not legacy debt to be migrated away; it is the right answer for
batch placement. Halving repeatedly through a five-agent fan-out deals out a
1/2, 1/4, 1/8, 1/16 sliver staircase — exactly the lopsided layout the
even-matrix policy was written to avoid. Keeping both is the design.

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

- `halve` resizes exactly **one** existing pane per split. The `1/N`
  re-share resized **every** sibling in the row. Strictly fewer
  `ResizePseudoConsole` calls per split than before.
- Drag paths keep the existing commit-on-release coalescing
  (`beginResizeHold` → one resize per pane per drag, #432). No live-drag PTY
  resizing existed and none is added.
- No new resize *trigger* is introduced anywhere: the policy is evaluated
  inside structural operations that were already resizing.

The untouched-sibling property is machine-checked rather than asserted:
`e2e/tests/pane-split.spec.ts` splits the leftmost pane of a 3-wide row and
requires the other two panes' bounding boxes to be unchanged. "Didn't move"
is "was never resized", because `applyFit` skips a same-size fit
(`panefit.ts`) — so that spec is the constraint-1 claim, tested. Its
tolerance is one divider width (6px, `styles.css`): a divider is a real flex
sibling, so a fourth pane in the row also inserts a third divider and takes
those pixels out of the row's free space. That is the only legitimate
movement, it is bounded by a single divider no matter how wide the row, and
it is an order of magnitude below what a `1/N` re-share moves.

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

## Floors and deliberate overflow

Slice B. The shape agreed in the plan (#885, plan-389 §4), recorded here so
slice A's reader knows where this is going and does not "fix" a gap that is
deliberately still open:

- Named per-axis floor constants live in `splitfloor.ts`, single-sourced and
  tuned with the human at the demo. Deliberately font-independent pixel
  approximations rather than live cell metrics — state-dependent geometry
  feeding layout decisions is the coupling constraint 1 distrusts.
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
