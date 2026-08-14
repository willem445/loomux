# Autosize: even out every pane, on demand (#936)

A layout drifts. Splitting pays for a new pane out of the pane you split (#885
slice A), which is what makes a split feel local — every other pane sits still —
but repeat it into the newest pane and the sizes walk: 1/2, 1/4, 1/8, 1/16. Drag
a divider or two and it walks further. Getting back to "just make them all the
same" meant dragging every divider by hand, and there was no gesture for it.

Autosize is that gesture: one button (`▦` in the top bar) and one chord
(`Ctrl+Shift+A`) that give every pane in the tab an equal share of the space.

## On demand, and never automatic

This is the part to not "improve" later. Splits keep their local, halve-the-
target behaviour; nothing re-levels a layout behind the human's back. A layout
that re-evened itself on every open and close would move panes the human
deliberately sized, and would make a split's cost unpredictable — the thing
slice A's policy exists to make predictable. One explicit gesture, one explicit
result.

It follows that Autosize is also *idempotent*: pressing it twice is pressing it
once. It is a button people press twice.

## The rule: weight each node by the panes under it

The obvious implementation — give every child of every split the same weight —
does not produce equal panes. A pane's size is the **product** of its share at
each level down to it, so evening out one level at a time cannot even out the
leaves. In a row of `[A, column[B, C]]`, equal weights give A half the tab and B
and C a quarter each.

`src/paneautosize.ts` weights each node by the number of panes underneath it. A
child holding *k* of its split's *n* panes takes *k/n* of that split, so by
induction every pane lands on 1/total. The same row comes out as thirds: A takes
a third of the width, the column takes two thirds, and B and C take half of that
each.

The module is pure and DOM-free — `grid.ts` walks its live tree into a shape of
empty objects and hands that over, so the module never sees a pane, an element,
or a measurement. `test/paneautosize.test.ts` asserts the property the feature
promises (every leaf's computed share is 1/N) rather than the weights it
returns, because a weights table would pass just as happily for the wrong policy
above.

## What "equal" is true of, precisely

Equal **area**, in the space flex has to distribute. Not equal width, not equal
height, and not equal to the pixel:

- **Dividers are `flex: none`**, so their pixels come off a split's free space
  before shares are computed. Branches at different depths carry different
  numbers of dividers, so two panes' areas differ by those few pixels. Fixing
  that would mean deriving layout from measured geometry, which is the coupling
  the no-resize constraint distrusts.
- **`.pane` carries a 60px min-width/min-height floor.** A tab holding more
  panes than fit at that floor cannot have equal panes at all; flex clamps the
  offenders and re-shares the surplus. Autosize asks for the best arrangement
  available rather than fighting it. (Refusing to create a pane that would
  breach a usable floor is #885 slice B, a separate concern.)

Equal *area* also still allows different shapes — a tall narrow pane and a short
wide one can hold the same area. That is inherent to a split tree: the tree says
which panes stack and which sit side by side, and Autosize deliberately does not
restructure it. It re-weights; it never re-arranges.

## Fullscreen, and the persisted layout

Autosize exits fullscreen first. It is a request to *see* the layout evened out,
and re-weighting a tree hidden behind a maximized pane would look like the
button did nothing.

It then fires the grid's `onChange`, exactly as a finished divider drag does, so
the new weights are persisted (#194 P4) and a restored session comes back evened
out rather than reverting to the layout you pressed the button to get rid of.

## Constraint 1

A discrete, human-initiated layout operation — the same class as a split or a
divider drag, and the sanctioned side of the line `doc/design/embedded-panels.md`
draws in *The PTY-resize boundary, argued*: the constraint targets continuous,
chrome-driven resizing, not a layout gesture the human aimed at the layout. Each
pane whose size actually changes issues one resize, and `shouldResizePty`
(`panefit.ts`) drops the ones whose `cols x rows` did not move. No new resize
trigger class, nothing periodic, nothing that fires without a click or a chord.

Docked panes are untouched — they are outside the tree and hold no space to even
out.

## The chord

`Ctrl+Shift+A`, which `shortcuts.ts` had parked since the agents mode was
removed (#194). It sits with the other layout gestures (`Ctrl+Shift+E`/`O`
split, `Ctrl+Shift+M` maximize) rather than in the `Alt+<key>` space, which is
overlays and focus.

Per the agent-CLI reference discipline, stated as the three distinct things it
is: Claude Code's interactive-mode reference **documents** `Ctrl+A` ("Move
cursor to start of current line") and `Ctrl+_`/`Ctrl+Shift+-` (undo), and
**does not document** `Ctrl+Shift+A` — the unshifted `Ctrl+A` an agent or shell
actually uses is untouched, and `test/shortcuts.test.ts` pins that it stays
untouched. Copilot CLI's reference pages are **silent** on `Ctrl+Shift+A` (its
CLI reference index carries no key table), so that CLI is unverified rather
than confirmed free.
