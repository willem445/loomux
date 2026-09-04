# The pane header, and what it does when it runs out of room

The header is a single flex row: the CLI mark, the role badge, the pane name,
the status chips, the folder/branch box (which is also the row's spacer), and
then the control cluster. At a comfortable width every control is a 23px button
in that row. At a narrow one they run together and become hard to hit — the
complaint #2191 was filed for.

The answer is an **overflow fold**: below a threshold the header keeps a
priority set inline and collapses everything else into one `⋯` button whose
hover/focus menu carries the rest.

`src/paneheader.ts` decides *whether*; `src/pane.ts` moves the elements;
`.pane-overflow-menu` in `src/styles.css` is the strip. This note carries the
three decisions that are not obvious from any of them.

## The priority order

Inline at every width:

1. **The pane name.** It is what makes a wall of agents legible, and it is also
   the pane's drag handle — `Grid.onPointerDown` refuses to start a reorder from
   a button, so the name is what a human grabs to move a pane. It ellipsises
   rather than folding, and never below `TITLE_MIN_W` (56px, ~8 characters at the
   header's 11.5px type). Because the rendered text is then cut, `setName` puts
   the **full** name in the element's `title`.
2. **Minimize.**
3. **Maximize.**

Everything else folds, in header order: the six orchestration toggles (tasks,
needs-you, audit, timeline, group, fold-group), open-in-editor, the three overlay
toggles (issues, git, file editor), both splits, and **close**.

**Close folding is deliberate and it is the one judgment call here.** The ask
named exactly three things as always-visible and put everything else in the
menu; ✕ is not among the three. It is also the control with the most other ways
to reach it (the dock chip's own ✕, the pane menu, the app's shortcuts), and the
one whose accidental activation costs the most — a ✕ crowded against a maximize
button on a 300px pane is the failure mode, not the fix. If that reads wrong in
use, moving a control between the two sets is one `priority` flag in
`Pane`'s `headerControls` registry and nothing else: the policy takes the set as
data, and the menu, the measurement and the tests all follow it.

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

`planHeaderFit` decides from `fullWidth` — the width the row *would* need with
every control inline. For that decision to be stable, `fullWidth` must not depend
on the current fold state. Three things make it independent:

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

Fold when the full set stops fitting; unfold only when there is `HYSTERESIS_W`
(24px) of **spare** room. Both thresholds read the same state-independent
`fullWidth`, so the dead band between them is the whole of the hysteresis: a
width oscillating inside it cannot change the state, whichever side it came from.

That is deliberately belt-and-braces. The invariant above is what makes the
decision stable; the dead band is what keeps a slowly-dragged divider from
toggling the icon set once per frame as it crosses a single threshold.

## All-or-nothing, not progressive

The non-priority set folds together. Progressive folding — shedding one icon at a
time as the header narrows — packs more into the row, at the cost of a menu whose
contents change every 25px. A human who has learned where the git icon sits in
the menu should find it in the same place at every narrow width. The ask asked
for the same thing ("everything else aggregates into a single overflow icon"),
and the predictable version is also the one with a single threshold to make
non-flapping.

## Refusals

Two widths never fold, whatever the header measures:

- **Folding would not save room.** The `⋯` button is wider than one `.pane-btn`
  (25px against 23px), so a header with a single foldable control — a welcome
  pane, whose only control is its ✕ — is left alone rather than having its one
  control hidden behind a wider button. This one guard also covers the degenerate
  case where every control is priority; an explicit "nothing to fold" early
  return was written, found to redden no test under mutation, and removed.
- **The header has no measurable width.** A pane in an inactive project tab is
  `display: none`, so everything measures 0. Guessing "fold" there would fold
  every pane in a hidden tab; the previous state is carried instead, and the next
  real measurement decides.

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
