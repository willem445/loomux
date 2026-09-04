# The pane header, and what it does when it runs out of room

The header is a single flex row: the CLI mark, the role badge, the pane name,
the status chips, the folder/branch box (which is also the row's spacer), and
then the control cluster. At a comfortable width every control is a 23px button
in that row. At a narrow one they run together and become hard to hit — the
complaint #2191 was filed for.

The answer is an **overflow fold**: below a threshold the header keeps a
priority set inline and collapses everything else into one `⋯` button whose
hover/focus menu carries the rest. Below *that* it keeps going — #2335 reported
a minimum-width header showing neither the window controls nor the `⋯`, so the
fold is a four-rung ladder ending with the `⋯` button alone.

`src/paneheader.ts` decides *whether*; `src/pane.ts` moves the elements;
`.pane-overflow-menu` in `src/styles.css` is the strip. This note carries the
three decisions that are not obvious from any of them.

## The priority order, and the ladder it produces

There is one priority order, and every fold decision is a step down it. Last
thing dropped first:

1. **The `⋯` button.** Once it is in the row it is never taken out again: it is
   the only route to everything that folded, so dropping it is what left #2335's
   minimum-width header with no reachable control at all.
2. **Minimize and maximize** — the `priority: true` set in `Pane`'s
   `headerControls` registry.
3. **The pane name's readable floor** (`TITLE_MIN_W`, 56px, ~8 characters at the
   header's 11.5px type). The name is what makes a wall of agents legible and it
   is also the pane's drag handle — `Grid.onPointerDown` refuses to start a
   reorder from a button, so the name is what a human grabs to move a pane.
   It still gives way before a control does, because truncating a name degrades
   gracefully (`setName` puts the **full** name in the element's `title`, and the
   menu's name row carries it once the row does not) where folding a control
   costs a whole extra gesture. Spend the cheap thing first.
4. **Everything else**, in header order: the six orchestration toggles (tasks,
   needs-you, audit, timeline, group, fold-group), open-in-editor, the three
   overlay toggles (issues, git, file editor), per-session notes (#2116), both
   splits, and **close**.

`planHeaderFit` turns that order into a **ladder** of four progressively
narrower rows, each reached only when the one above it stops fitting:

| rung | the row holds | the name |
| --- | --- | --- |
| `full` | every control | on its floor |
| `folded` | `⋯` + minimize + maximize | on its floor |
| `squeezed` | `⋯` + minimize + maximize | floor released — ellipsises freely |
| `minimal` | `⋯` alone | out of the row; the menu's first row carries it |

A control that only some panes have — notes is the first — is registered like
any other and left `hidden` where it does not apply. `syncHeaderOverflow` reads
a zero natural width as "a control this pane does not have", so it never
reaches the policy, and the pane that reveals one schedules a pass rather than
waiting for a resize to notice.

**Close folding is deliberate and it is the one judgment call here.** The ask
named exactly three things as always-visible and put everything else in the
menu; ✕ is not among the three. It is also the control with the most other ways
to reach it (the dock chip's own ✕, the pane menu, the app's shortcuts), and the
one whose accidental activation costs the most — a ✕ crowded against a maximize
button on a 300px pane is the failure mode, not the fix. If that reads wrong in
use, moving a control between the two sets is one `priority` flag in
`Pane`'s `headerControls` registry and nothing else: the policy takes the set as
data, and the menu, the measurement and the tests all follow it.

## The chrome is priced twice, and that is deliberate

`fixedWidth` — the CLI mark, the role badge, the chips and the folder/branch
items, each at its natural, unshrunk width — is what the chrome **wants**. That
is the right number for deciding when to FOLD, because folding is how the row
buys the chrome the width it wants. It is the wrong number for the two rungs
below the fold, which are what happens *after* the chrome has already given way.

The first cut priced all four rungs off `fixedWidth`, and `queue-badge.spec.ts`
found it on CI: with a stalled queue badge lit, the chip WANTED ~180px, the
folder path wanted more, and the ladder descended to `minimal` on a header with
hundreds of pixels to spare — the pane name disappearing because a label was
long. The spec's drag-handle floor read `0px` against `55.4px` without the badge.

So `squeezed` and `minimal` read `chromeFloorWidth` instead: the width the
chrome **cannot** give up.

**That width is measured, and the first attempt to reason it away was wrong.**
The cut that introduced `chromeFloorWidth` defaulted it to 0 and had `pane.ts`
pass nothing, on the argument that the shrink rule below makes every non-control
header child able to give up everything. It does not: `flex-shrink` scales a
child's *content*, and cannot touch its border, its padding or the row's own
6px `gap`, so each lit chip keeps ~20px at any width. Measured in Chromium at
the grid's own `MIN_PANE_PX` of 80, that left the `⋯` reachable with **one**
chip lit and clipped with **two** — and a stranded pane wears two by
construction, since `.pane-attn-dismiss` is revealed alongside `.pane-attn`
whenever the reason is dismissible. #2335, unfixed, for the panes most likely to
be narrow.

`measureHeaderFixed` therefore returns two numbers rather than one — `want` and
`floor` — and `irreducibleWidth` reads each child's border and padding off the
box it actually has. The 0 default survives only as the answer for a caller that
does not measure, and it errs toward folding **late**.

## The narrowest rung, and the promise only flexbox can make (#2335)

At `minimal` the pane name leaves the row outright rather than being clipped
further. A name cut to one glyph carries no information and still spends the
~20px the `⋯` needs, so it moves into the menu instead, as its first row: the
label *is* the current name and pressing it starts the same rename that F2 and a
double-click on the header name start. `overflowMenuIds` derives that row from
the plan (`title === "hidden"`) rather than taking it as a separate input, so
"the row has no name" and "the menu offers one" are one decision asked once.

The policy can only decide what the row **contains**. What #2335 actually
reported is a row that contained the right things and still showed none of them:
`.pane-btn` and every status chip but two are `flex: none`, so a row that does not fit
does not shrink — it *overflows*, and `.pane`'s own `overflow: hidden` clips
whatever hangs off the right end. Once the window-control cluster has folded,
the thing hanging off the right end is the `⋯` itself.

So the fitting half of the promise is made in CSS, and deliberately **not** under
`.pane.header-minimal`: every header child that is not a control becomes
`flex-shrink: 1; min-width: 0; overflow: hidden` at *every* width, so flexbox
takes the deficit out of the CLI mark, the role badge and the chips before it
takes anything out of a control. Scoping it to the narrowest rung would leave the
rung above it — where the row is `⋯` + minimize + maximize and the chrome is
still `flex: none` — overflowing exactly as before.

Two things are exempt, **by property rather than by name**. Controls (`.pane-btn`)
are what the row exists to keep. And a chip carrying `chip-yields` declares a
much heavier shrink weight of its own — `.pane-queue` and `.pane-mail` are both
`flex: 0 100 auto` (#814/#894), so that they give up their room before the pane
name, which is also the drag handle, gives up any. The rule's specificity beats
theirs, so a blanket `flex-shrink: 1` flattens that 100 to a 1 and the chip takes
the title's room instead — the exact failure `queue-badge.spec.ts:270` is written
for. The first cut of this rule named `.pane-queue` in a `:not()` and missed
`.pane-mail`, which has no equivalent spec; marking the chips is what stops the
third one going the same way.

`.pane.header-minimal` then takes **everything but the `⋯` out of the row** — the
name, the CLI mark, the role badge and every chip. Shrinking them is not enough,
for the reason in the section above: their borders and padding survive any
`flex-shrink`, and two lit chips' worth is more than an 80px pane has to spare.

It is `position: absolute` + `visibility: hidden`, never `display: none`, and
that is the same argument the overflow menu's own closed state makes one section
up: a `display: none` child measures **zero**, so `measureHeaderFixed` would read
a narrower header the moment this rung was entered, the ladder would climb back
out, the chips would return, and it would fold again — a flap driven by the fix
for the clip. Out of flow but still laid out, every width the policy reads is the
same on both sides of the boundary.

Two things stay: the `⋯`, and the rename input if F2 is open (it is the one
header child that is a control without being a `.pane-btn`, and the gesture has
to put an editable field somewhere).

**What that costs, stated rather than glossed.** Three of the chips are
`<button>`s with click handlers — `.pane-attn` (focus and acknowledge),
`.pane-attn-dismiss` (clear a latched `stranded`) and `.pane-channel` — and none
of them is a registered `headerControl`, so they are not in the menu either.
At this rung they are unclickable until the pane is widened. Their *signals*
survive on the header itself (`.pane.needs-attention .pane-header` and
`.pane.connected .pane-header` both tint it), and the dock chip carries the same
state, so nothing goes unnoticed — but the gestures are gone, and the honest fix
if that matters is to register them like every other control rather than to leave
them in a row that cannot hold them. They were clipped and unclickable at this
width before #2335 too; this makes that deliberate instead of accidental.

The header's height is `--pane-header-h` at every rung either way, so
`.pane-term`'s box never moves and no PTY is resized — constraint 1 holds at the
narrowest rung for exactly the reason it holds at the others.

**The residual, stated rather than claimed away.** `measureHeaderFixed` counts
one gap per item and gives the pane name no gap of its own, so the ladder's
demand runs up to one `HEADER_GAP_W` (6px) below what the row really needs. In
that band the *rightmost* control — maximize, since `⋯` sits at the head of the
cluster in DOM order — can lose a pixel or two off its right edge before the
ladder drops a rung. The `⋯` cannot be the one clipped: it is leftmost of the
three, so anything reaching it has already taken the other two, and by then
`minimal` has fired and taken every other item out of the row. And none of the
CSS half is covered by a test — this repo validates DOM wiring by hand — so the
figures above come from a Chromium probe against this stylesheet, not from a
guard that would go red if someone narrowed the rule.

## Why the menu is an overlay

CLAUDE.md hard constraint 1: never resize the PTY for a UI feature. The obvious
alternative — let the header **wrap** to a second row — grows `.pane-header`'s
box, which shrinks `.pane-term`, which resizes ConPTY, which repaints and
pollutes the user's scrollback. Every pane crossing the threshold would do it,
and a divider drag would do it repeatedly.

So the strip is `position: absolute` inside `.pane` (the header's nearest
positioned ancestor), out of flow entirely. Folding changes *which items sit in
the header row* and nothing else; the header's height stays `--pane-header-h` at
every width, and nothing on the path calls `fit()` or `resizePty`.

It is a child of `.pane-header` rather than of `.pane` for one reason that is not
about pixels: DOM order. Placed immediately after the `⋯` button, Tab from that
button lands *inside* the menu instead of skipping past it to minimize.

`.pane` is `overflow: hidden`, so the strip is clipped to the pane — which makes
"inside the window" (what the ask asks for) implied by, and strictly stronger
than, what `menuLeftFor` enforces. The strip wraps (`flex-wrap`) and caps its own
`max-width`/`max-height` against the pane, so a very narrow pane gets a
multi-row strip rather than a clipped one.

`menuLeftFor`'s answer is written to the element as a **`transform`**, never as
`left`, and that is a layout fact rather than a taste. An absolutely-positioned
box with `width: auto` is shrink-to-fit against the space between its `left` and
its containing block's right edge, so writing the computed offset back as `left`
would change the very width the computation read — a strip anchored near the
right edge would measure narrow, wrap into rows it had room not to need, and
then be placed from a width that no longer applied. `left: 0` is fixed in the
stylesheet, which fixes the width at "as much of the pane as the strip wants";
the transform then slides the laid-out box and costs layout nothing.

## Why closed is `visibility: hidden`, never `display: none`

This is the part that looks like a transition convenience and is not.

`planHeaderFit` prices each rung of the ladder from the width that rung's row
*would* need. For those decisions to be stable, none of those widths may depend
on the rung we are currently on. Three things make them independent:

- the pane name contributes a constant floor, never its rendered width, which
  flexbox shrinks;
- `fixedWidth` covers only items that stay in the row either way, and the meta
  box is measured by its **items** rather than by its own (stretched) box;
- a folded control is still **laid out**, so it measures the same width in the
  menu as it did in the row.

A `display: none` menu breaks the third. The policy would read a narrower control
set the moment it folded, conclude there is room, unfold, re-measure wide, and
fold again — a flap driven by the fix for the flap, at the browser's
`ResizeObserver` cadence. `visibility: hidden` keeps the box and drops the paint;
it removes the strip from the tab order and the accessibility tree exactly as
`display: none` would.

A control that measures **zero** is therefore never "a zero-width control": it is
a control this pane kind does not have (`hidden`, or `display: none` from
`.pane.is-content .pane-btn.pty-only`), and it is dropped from the policy's input
entirely rather than folded.

## Hysteresis, and why it is the second line of defence

Drop a rung the moment its row stops fitting; climb back up only when there is
`HYSTERESIS_W` (24px) of **spare** room. That asymmetry is applied between every
adjacent pair of rungs, not just at #2191's one threshold, and every rung's
demand is state-independent (above), so the dead band between two rungs is the
whole of the hysteresis: a width oscillating inside it cannot change the rung,
whichever side it came from.

That is deliberately belt-and-braces. The invariant above is what makes the
decision stable; the dead band is what keeps a slowly-dragged divider from
toggling the icon set once per frame as it crosses a single threshold.

## All-or-nothing, not progressive

Each rung folds its whole set together. Progressive folding — shedding one icon
at a time as the header narrows — packs more into the row, at the cost of a menu
whose contents change every 25px. A human who has learned where the git icon sits in
the menu should find it in the same place at every narrow width. The ask asked
for the same thing ("everything else aggregates into a single overflow icon"),
and the predictable version is also the one with a handful of thresholds to make
non-flapping rather than one per icon.

## Refusals

**A rung that buys nothing is not on the ladder.** Each candidate is kept only
if it is *strictly narrower* than the last rung kept above it, which is one rule
covering three cases that used to be argued separately:

- The `⋯` button is wider than one `.pane-btn` (25px against 23px), so a header
  with a single foldable control — a welcome pane, whose only control is its ✕ —
  would spend room to hide its one control. Both folding rungs are dropped for
  that pane, so its ✕ stays in the row and clickable at *every* width. The 25px
  is load-bearing, not cosmetic; `OVERFLOW_BTN_W` in `pane.ts` says so.
- A header whose controls are all priority has nothing to fold, so `folded` is
  dropped — but `minimal` is not, because folding the priority set is exactly
  what buys the room down there. That is the case #2335 was filed for.
- `squeezed` is dropped when the name floor is already zero.

An explicit "nothing to fold" early return was written for the first of those,
found to redden no test under mutation, and removed; this guard already decides
it. It runs *before* the width check, so a header with nothing worth folding can
never be carried into a folded state that would render an overflow button over
an empty menu.

**A header with no measurable width** is the other refusal. A pane in an
inactive project tab is `display: none`, so everything measures 0. Guessing
"fold" there would fold every pane in a hidden tab; the current rung is carried
instead, and the next real measurement decides. A rung that is not on this
pane's ladder is not carried — it drops to the widest, which is the
empty-menu case above.

## What detects the change

A `ResizeObserver` on **the header** and on **the meta box** — never on the
terminal, and it shares nothing with `resizeburst.ts`, which coalesces a
different thing (the PTY fit). Two targets because two things change the row:

- the pane resizing, which moves the header's own box;
- a chip appearing, or a folder/branch label changing, which leaves the header's
  box untouched but takes room out of the same row. The meta box is the row's
  flex spacer, so every such change shows up as its width moving.

The pass is debounced (`HEADER_SYNC_MS`, 60ms) so the measuring reads stay off
every drag frame, and it moves nothing when the decision has not changed.
