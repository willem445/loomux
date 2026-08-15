# Autosize: even out every pane, on demand (#936)

A layout drifts, and it still drifts after #945 made splits and closes even —
which is the thing to be precise about, because the shape of the drift is what
this gesture is answering.

What #945 gives, exactly: a same-direction split lands the newcomer at the
**mean** of the weights already in that row, and a close hands the departing
pane's weight to its survivors in **equal absolute parts**. Both are confined to
the one row or column that changed; no other split in the tab moves. So a row
nobody has dragged is even at every pane count, and a row somebody has dragged
keeps the ratio they dragged (an insert leaves the incumbents' weights untouched
and takes the newcomer's share out of all of them uniformly; a close narrows the
skew rather than discarding it).

What that leaves — and what Autosize is for:

- **Nesting.** Even is a property *within* a split. A cross-direction split
  replaces one leaf's slot with a nested two-way split, and the outer row is
  then even between the panes beside it and that **pair** — a half and two
  quarters, not three thirds. The agent fan-out alternates direction per pane
  and so nests every time: 1/2, 1/4, 1/8, 1/16. Nothing in the even-share
  arithmetic can fix this, because it is not a fact about any one split; it is
  the product of shares down a path.
- **Deliberate drags.** A divider the human moved stays moved, by design. When
  they want it gone, they want it gone in one gesture, not by dragging every
  divider back by hand.

Autosize is that gesture: one button (`▦` in the top bar) and one chord
(`Ctrl+Shift+A`) that give every pane in the tab an equal share of the space,
across every level of nesting.

## On demand, and never automatic

This is the part to not "improve" later. A split or a close re-shares **the row
it happened in** and nothing else; no operation levels the whole tab, and none
of them crosses a nesting boundary. A layout that re-evened itself across every
level on every open and close would move panes the human deliberately sized, at
moments they did not choose — the drift this gesture removes would become a
thing that happens *to* them. One explicit gesture, one explicit result.

Which policy a human's split gesture ends up with is also not this note's
business. Autosize reads the tree's **shape** and never its weights, so it lands
on the same answer whether a split halved the target (#900 slice A) or handed
the newcomer an even share (#945) — the two are a call-site choice in `grid.ts`,
and this gesture is compatible with either by construction rather than by
agreement.

It follows that Autosize is *idempotent*, and in the way that actually matters:
not "the same pure call twice" — true of any deterministic function, and
unfalsifiable — but that the weights already on the tree are **not an input**.
Press it on a layout dragged anywhere at all and it lands on the same weights it
would give the bare shape. It is a button people press twice.

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
than confirmed free — a reference that lists no bindings is not evidence of no
conflict.

That distinction has a consequence worth stating rather than leaving implied:
the chord is withheld from every terminal pane (`isAppShortcut`), so a CLI that
*does* bind it loses it with no escape hatch. Settling it needs a human with
Copilot running, not another doc read, so it is a demo-checklist item and open
at the time of writing.
