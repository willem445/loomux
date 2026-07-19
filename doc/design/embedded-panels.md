# Embedded panels: up to three floating views docked beside the terminal (#361)

The ask (#361): dock UI items — the task board, the group lifecycle panel,
git, GitHub issues, etc. — *beside or below* the agent CLI, resizing the
terminal so both the full CLI and the panel are fully visible at once, and
(the demoed follow-up) dock *several at once* — one to the left, one to the
right, one on the bottom — so everything needed is visible while working.
Every one of these views is today a **floating overlay**: it covers part of
the terminal rather than sharing space with it, precisely because
CLAUDE.md's hard constraint 1 says *never resize the PTY for a UI feature*.
This note works out the boundary that constraint actually draws, lands the
task board on the legitimate side of it, generalizes the mechanism to four
more views (git, GitHub issues, the audit log, the group lifecycle panel —
the file-editor overlay is deliberately excluded, see *What's embeddable*),
and then generalizes AGAIN from one shared slot to three independent ones —
left, right, bottom.

## The PTY-resize boundary, argued

Constraint 1 exists because a ConPTY resize on the Windows 10 inbox conhost
repaints the whole screen, and a full-screen TUI then duplicates that repaint
into scrollback — a cost with no matching benefit when the *trigger* is
incidental chrome (a badge appearing, an overlay opening, a tab becoming
active). That is what the constraint targets: **continuous, chrome-driven
resizing**, sized from things the human didn't directly ask to resize the
terminal for.

A **split** has never been read that way. Dragging a pane's edge to create a
second pane resizes every terminal in the affected subtree — `grid.ts` has
always done this — and nobody has proposed floating panes over each other
instead. The reason is the trigger: a split is a **discrete, user-initiated
layout operation**. The human picked "give this new thing its own space,"
and if that costs one resize (or a throttled run of them while they drag the
divider), that is the operation's own honest cost, not chrome tax.

An embedded panel is a split in this sense, not an overlay in that one:
docking a view to an edge is the human saying "give this its own space
beside the terminal," exactly the sentence a split already answers — and
docking a SECOND or THIRD view is just that sentence said again, once per
edge. So:

- **Docking to an edge, un-docking, and moving a view to a different edge are
  each ONE discrete resize event**, fired from an explicit menu pick — never
  from a resize, a refresh, a repaint, or any other passive trigger.
- **A divider drag between the terminal and a docked panel resizes the PTY
  the SAME way a grid split's own divider already does**, for all THREE
  possible dividers alike — see *Divider mechanics* below for exactly what
  that means, because "one resize on release" turns out not to be it.
- **A docked panel's own internal changes never reach the PTY — with one
  narrow, bounded, and named exception.** For four of the five views (task
  board, git, issues, audit), refreshing a list, expanding a row, typing in
  a filter — none of it changes the panel's MINIMUM size, so none of it
  touches `termEl`. Verified per view below, not assumed. **The group panel
  is the one view whose own fixed chrome can genuinely grow after it opens**
  (the suspended-budget banner appearing) — `reclampViewFloor`, generalized
  from the pre-#361 `reclampGroupOverlay` that has always done the
  equivalent for the overlay. When the group panel is DOCKED and its floor
  grows, `reclampViewFloor` DOES write to a divider's `flex` to keep the
  growing chrome from clipping — a real, content-triggered PTY resize, not a
  click. This is an honest exception, not swept under "internal changes
  never reach the PTY" — it survives scrutiny on three grounds, not by
  definition: it is **rare** (one specific state transition, not every
  render or refresh), it is **bounded** (only ever grows the panel to the
  minimum ITS OWN fixed controls need, never runaway or unbounded), and it
  drives the exact same `ResizeObserver` → debounced `applyFit()` →
  same-size-skip path every other resize in this design already does — a
  new TRIGGER for an existing, already-safe mechanism, not a second one. No
  other view has an equivalent path today; if one needed it, the same
  three-part bar (rare, bounded, same mechanism) is what it would have to
  clear.
- **The floating overlay is untouched and stays available, for every view,
  independent of what else is docked.** Docking is an alternative
  presentation the human opts into, not a replacement; every overlay's
  no-resize mechanics (`overlaysize.ts`, `Pane.overlayClamp`,
  `updateTermShift`) are byte-for-byte what they were before this note. (The
  overlay's OWN pre-#361 `reclampGroupOverlay` equivalent never touched the
  PTY at all — only the overlay's own CSS height — precisely because the
  overlay never shares real layout space with the terminal in the first
  place. The docked case above only exists BECAUSE docking is a real split
  with genuinely shared space; it has no overlay-mode counterpart.)

If a future docked view ever needed CONTINUOUS resizing driven by something
other than a direct drag or an explicit menu pick, that would cross back
onto the wrong side of the line above and this design would need
revisiting. The group panel's floor-grow above does NOT cross that line —
it is occasional and self-terminating, never continuous — but it is the one
case today where "driven by something other than a direct drag or an
explicit pick" is literally true, so it is named here rather than left for
a reader to discover by tracing `onResize` themselves.

## Layout: three independent slots, not a shared one

**Up to three views may be docked simultaneously — left, right, bottom.**
No `"top"`: the pane header already owns that edge, and nothing has asked
for it. Each edge is its own independent slot (`Pane.embedSlots: Record<
EmbedSide, EmbedSlotState>`), holding at most one view, with its own divider
and its own persisted share.

**Corner layout: bottom spans the full width, rather than sitting only
beside the terminal.** The alternative — bottom nested between left and
right, i.e. three columns with the middle one further split into term-over-
bottom — was rejected as the more complex of the two credible shapes for no
real gain: a human docking three panels at once almost always wants a wide
strip along the bottom (a log, a board) and narrower strips down the sides
(a status glance), not a bottom panel pinched between two side panels. The
DOM reflects the choice directly:

```
embedHostEl (column)
  embedRowEl (row)              — the width axis: left | center
    left divider + slot           (hidden unless occupied)
    embedCenterEl (row)          — term | right
      termEl
      right divider + slot        (hidden unless occupied)
  bottom divider + slot          (hidden unless occupied — spans embedRowEl's FULL width)
```

**Nested, not a flat 5-child row — this is what keeps every divider's own
drag math a plain two-element pair.** The naive alternative is one flex row
with five children (left-slot, left-divider, term, right-divider,
right-slot); grid.ts's own N-way splits use exactly that shape, and it works
there because each divider only ever trades space with its two IMMEDIATE
neighbors, leaving the others alone. Reusing it here directly runs into a
real problem: dragging the LEFT divider would trade space between the left
slot and `termEl` specifically — but if RIGHT is also occupied, `termEl`
alone is not "the rest of the row," and the divider math would need to
reason about a THIRD element (the right slot) it isn't touching at all. The
fix is the nesting above: `embedCenterEl` wraps `termEl` and the optional
right slot into ONE real element, so the left divider's far side is that
single element, not "term plus whatever else happens to be on the right."
Every divider — left, right, bottom — ends up with a plain, real,
single-element pair (`Pane.dividerPair`), and `embedDragGrow`
(embedsplit.ts) never has to know about more than two elements at a time.
This is the same trick grid.ts's own split TREE already uses for nested
splits (a 2-child split containing another 2-child split) — applied here
because two of the three dividers (left, right) share the same row.

**Clamp precedence: terminal floor > panel floors > shares, enforced per
divider.** Every divider's pair always has the TERMINAL's own floor
(`EMBED_MIN_TERM_PX`) on one side — directly for right (`termEl` is its
literal "before" element) and bottom (the row's own minimum height IS the
terminal's, since left/right share the row's height), and indirectly for
left via `embedCenterFloor` (below). A panel's own floor is the OTHER side,
and shares (the human's chosen split point) only ever move flex-grow WITHIN
whatever room those two floors leave. This is a **per-divider guarantee, not
a global solver**: if the pane itself is smaller than the sum of every
currently-active floor, some region necessarily ends up smaller than its
stated floor from pure arithmetic — the same accepted degradation
`overlayClamp` (`overlaysize.ts`) already documents for the single-overlay
case ("min wins when the pane is too short to honor the reserve"). A drag
can never make that WORSE; it just can't retroactively fix an
already-too-small pane.

**`embedCenterFloor` is the one place precedence has to compose across more
than one divider.** The left divider's far side, `embedCenterEl`, is a
composite — `termEl` plus, if occupied, the right divider and right slot —
so ITS floor has to reserve room for whatever is nested inside it, not just
the terminal:

```ts
export function embedCenterFloor(rightPanelFloorPx: number | null): number {
  return rightPanelFloorPx === null
    ? EMBED_MIN_TERM_PX
    : EMBED_MIN_TERM_PX + rightPanelFloorPx + EMBED_DIVIDER_PX;
}
```

`rightPanelFloorPx` is `null` when right is unoccupied (collapsing to plain
`EMBED_MIN_TERM_PX` — exactly what the floor would be with nothing nested at
all), or the right slot's own floor when it IS occupied. `Pane.dividerFloors`
evaluates this LIVE on every left-divider drag and on every reclamp, so
docking or un-docking RIGHT immediately changes what the LEFT divider is
willing to do — and `Pane.embedViewAtSide`/`unembedView` explicitly
reclamp LEFT (`reclampSlotDivider("left")`) whenever right's occupancy
changes, so a left panel that was fine before right got docked doesn't sit
below the new composed floor until the human happens to touch its divider.
`test/embedsplit.test.ts` pins the composition directly, including the case
where right's floor alone would exceed what a naive (uncomposed) clamp would
have allowed.

**Left/right's panel-side floor is a fixed constant, not each view's own
`floorPx()`.** `EmbedEntry.floorPx()` measures how much VERTICAL chrome a
view needs (header, list, footer stacked) — meaningful for the bottom slot's
HEIGHT floor and the overlay's height clamp, both unchanged from the
pre-multi-slot design. It does not transfer to "how narrow can this same
vertical stack go," so left/right deliberately do NOT route through it at
all: every view's width floor when docked to an edge is the same
`EMBED_MIN_PANEL_PX` (180px), full stop, never overridden — including the
group panel, whose only source of floor VARIATION (the suspended-budget
banner) is a vertical concern that doesn't change how narrow its own layout
can go. This is a deliberate v1 simplification, not an oversight: correctly
measuring a view's *minimum width* would need each view to report a second,
width-specific floor, and nothing today needs that precision.

## Divider mechanics: matched to what splits actually do, not to a guess

The original brief for this work assumed grid splits resize the PTY **once,
on drag release**. That turned out to be inaccurate, and it's worth
recording precisely because the correction is what makes "a docked panel
resizes the terminal the way a split does" a checkable claim rather than a
slogan:

A grid split's divider (`grid.ts`, `makeDivider`) updates `flex` inline on
every `mousemove` while dragging. Each of those layout changes fires the
terminal's `ResizeObserver` (`pane.ts`, wired in the constructor), which
calls `applyFit()` — and `applyFit()` is **debounced 16ms, and skips a
same-size call** (`shouldResizePty`, `panefit.ts`, pinned by
`test/panefit.test.ts`). So a real split's divider drag resizes the PTY
**continuously, but frame-throttled and de-duplicated** — not zero times
during the drag and one at the end.

Every one of the pane's own three dividers (`Pane.wireEmbedDivider`) is
built to hit that exact same code path, not a bespoke one: dragging any of
them sets `termEl`'s (or, for left, `embedCenterEl`'s) `flex` inline, same
as a grid divider does to a pane's element, so the *same* `ResizeObserver` →
debounced `applyFit()` → same-size-skip chain fires on the *same* schedule,
regardless of which edge is being dragged or how many other edges are
currently occupied. There is no second resize discipline to audit — there
is one. What IS fired once per drag (mirroring `grid.ts`'s own `up()`
handler) is **persistence**: the settled fraction is written to that slot's
own `frac` and reported via `onRecordChanged` only on `mouseup`, never per
`mousemove`.

`embedsplit.ts` is pure and DOM-free (`test/embedsplit.test.ts`), and
mirrors `grid.ts`'s inline divider math on purpose: before/after sizes and
flex-grow weights, a delta clamped so neither side crosses its floor, then
redistributed proportionally so the pair's total flex-grow is preserved.
Reusing that shape is the point, not an incidental convenience. `frac` is
always "the PANEL's own share of its pair," regardless of whether the panel
happens to be the "before" or "after" element (`Pane.applySlotGrow`/the
`fracFromGrow(counterpart, panel)` extraction in `wireEmbedDivider`'s `up`
handler) — left's panel is physically BEFORE its divider, right's and
bottom's are AFTER, and neither `pane.ts` nor a human reading a persisted
`share` needs to remember which.

**Why the floors are duplicated, not imported.** The panel-side and
terminal-side minimums (`EMBED_MIN_PANEL_PX` / `EMBED_MIN_TERM_PX`, 180 /
100) are deliberately the same numbers as the floating overlay's own
`OVERLAY_MIN_H` / `TERM_RESERVE_H` (`overlaysize.ts`) — same reasoning (a
visible terminal strip; a panel's own header/list/footer chrome doesn't
clip) — but `embedsplit.ts` does **not** import them. Every pure,
`node:test`-covered module in this codebase (`layout.ts`, `overlaysize.ts`,
`spawnexpiry.ts`, `taskboard.ts`, …) is self-contained, and there's a real
mechanical reason none of them cross-import: `tsc`'s build rejects an
explicit `.ts` import extension (`TS5097`), but `node --test` — which loads
these files directly, with no bundler — cannot resolve a bare extensionless
specifier at all. A module that imports another pure module has no single
spelling that satisfies both runners. Duplicating two numbers, with a
comment naming what they mirror, was cheaper than teaching either runner
about the other.

## Naming: two collisions, not one

**"dock" was already taken.** The issue (and the human) call this "docking."
The codebase already has a feature called **the dock**:
`grid.minimize()`/`grid.restore()` park a whole pane out of the split tree
into a strip of `.dock-chip` restore buttons — a taskbar, not a split.
Calling the new feature "dock" too would collide with that vocabulary in the
UI copy (a "Dock" button living a few pixels from "Minimize to the dock")
and in the code (`dockSyncListener`, `renderDock`, `.dock-chip` already mean
the OTHER thing). So this is **embed** internally — `.pane-embed-host`,
`.pane-embed-panel`, `.pane-embed-divider`, `Pane.embedViewAtSide()`, the
persisted `embeds` field — while the human-facing copy says "docked" /
"docking" freely (the side-picker menu literally reads "Embed left" /
"Embed right" / "Embed bottom" / "Un-embed," but the surrounding prose in
this note and the README says "dock" where that's the more natural word —
there is no ambiguity in ENGLISH prose the way there is in a button two
pixels from "Minimize to the dock").

**Generalizing to `GitView` surfaced a second collision, in the opposite
direction.** `GitView` already had a constructor option named `embedded`
(#217: is this view hosted as a whole content PANE — no terminal at all —
rather than an overlay?). That is a *different concept* from this feature
(a view sharing space with a terminal that's still right there), and the
two are easy to conflate on the same class. So every embeddable view's
runtime toggle method is named **`setPanelActive(active: boolean)`**, not
`setEmbedded` — applied uniformly to all five views (including `TasksView`,
which has no collision of its own, for one consistent interface across the
set) so a reader never has to remember which view's method means what.

## What's embeddable, and what isn't

Five views are embeddable: the **task board**, **git**, **GitHub issues**,
the **audit log**, and the **group lifecycle panel** ("lifecycle status" in
the issue — the panel behind `GroupView`'s overlay toggle). All five are
wired through one generic engine in `pane.ts` (`EmbedKind`, `EmbedEntry`,
`embedRegistry`, `openView`/`closeView`/`toggleView`/`embedViewAtSide`/
`unembedView`) — see *The generic engine*, below. Any THREE of the five may
be docked at once, one per edge; the other two (or however many aren't
docked) stay available as floating overlays.

**The file-editor overlay (`Alt+F`) is deliberately NOT embeddable.** It
already has a strictly better path to the same outcome: the editor
**content pane** (#217, `FileEditView`'s `embedded` mode — the very flag
this note's naming section discusses). A content pane gives the editor the
whole of its own pane — full width, a real tab-bar entry, state that
survives a session restore the normal way — where a same-pane sub-panel
would give it a narrower strip that competes with the terminal (and now
potentially two OTHER docked panels) for room, and disappears the moment
the pane closes. "Open in editor pane" (the file browser's row action) is
the answer to "I want to keep this editor open beside my agent"; embedding
the *overlay* version would be a strictly worse version of a feature that
already exists. The overlay stays exactly what it was: a quick look, `Esc`
to dismiss.

## The generic engine

Every embeddable view registers itself into `Pane.embedRegistry` (a
`Map<EmbedKind, EmbedEntry>`) the first time its own `ensureXView()` lazily
constructs it:

```ts
interface EmbedEntry {
  overlayEl: HTMLElement;              // the view's own floating-overlay host (unchanged)
  viewEl: HTMLElement;                 // the view's own root element
  show(): void;
  hide?(): void;                       // extra per-view cleanup beyond hiding the host
  setPanelActive(active: boolean): void;
  floorPx(): number;                   // live floor, for the overlay clamp AND the bottom slot
}
```

`openView`/`closeView`/`toggleView` treat every KIND uniformly through this
table — there is no per-view branching left in the open/close/toggle path
itself, only in each view's own `ensureXView()` (which still differs, on
purpose: `GitView` and `FileEditView` gate on `refuseOverlay` — a content
pane has no terminal to share space with at all — while the orchestration
family gates on `orchGroup`/the header button's own `hidden`).

**A SEPARATE `Pane.embedSlots: Record<EmbedSide, EmbedSlotState>` holds
which kind (if any) occupies each of the three edges**, plus that edge's own
persisted share and its permanent (created-once, `hidden`-toggled) panel and
divider elements. `Pane.sideOf(kind)` — a plain linear scan over three
entries, not a second map kept in sync with the first — answers "is `kind`
currently docked, and where"; nothing else needs a reverse index for a set
this small.

**Docking to an OCCUPIED edge SWAPS that one slot's occupant; the other two
are always untouched.** `Pane.embedViewAtSide(kind, side)` — the side-picker
menu's action — closes whoever was on `side` outright (never demotes them
back to a floating overlay: a silent reopen elsewhere would be a more
surprising UX than "the slot now shows what you asked for, and the previous
occupant is closed — the same one click that opened it reopens it"), and if
`kind` was ALREADY docked to a DIFFERENT edge, it leaves that edge first (a
view can only occupy one slot at a time, but which one is now a free
choice, not fixed to "bottom" the way the single-slot design was).
`Pane.unembedView(kind)` is the separate, explicit "back to the floating
overlay" action, also touching only the one slot `kind` was in.

**The side-picker menu, not a plain toggle.** Each view's embed button
(unchanged position/icon, `⬒`/`⬓`) now opens a small menu — reusing
`contextmenu.ts`'s existing `showContextMenu`, not a bespoke dropdown —
listing Left / Right / Bottom (the currently-docked one, if any, checked)
and, when docked anywhere, an "Un-embed" item. `Pane.showEmbedMenu(kind,
anchor)` builds and owns this entirely; the views themselves don't know
`EmbedSide` exists at all — they only know clicking their button asks the
pane "where should I go?" (`onEmbedMenu: (anchor: HTMLElement) => void`,
replacing the single-slot design's plain `onToggleEmbed: () => void`) — the
same division of responsibility the rest of this engine already keeps
(views are dumb UI; the pane owns embed state).

**Side memory: real for a currently-docked view, not built as a separate
preference for an un-docked one.** While a view stays docked, "which side"
IS its own memory — the persisted `embeds` array (below) already carries
`{view, side, share}` for exactly the views that are docked when a snapshot
is captured, so quitting and relaunching (for the orchestration-family
views that survive a restart) brings each one back on the SAME edge it was
on. What this does NOT do: remember which side a view was on AFTER it's
been un-docked back to a floating overlay, so a later re-dock defaults to
last time's edge. Building that would mean a second, independent
preference — persisted per view, updated on every dock/un-dock regardless
of outcome, with its own decode path — for a marginal convenience (saving
one menu click on a re-dock) that nothing in the ask specifically requires
beyond the word "remember." Scoped out deliberately rather than half-built:
the side-picker menu shows exactly the state that's real (checked = where
you are now, if anywhere), never a stale hint for where you used to be.

**Per-edge floors.** `EmbedEntry.floorPx()` feeds the overlay height clamp
(`Pane.overlayClamp`) AND the bottom slot's own height floor — unchanged
from the pre-multi-slot design. Left/right instead use the fixed
`EMBED_MIN_PANEL_PX` constant for every view, per *Layout*'s own
explanation above of why a view's vertical-chrome floor doesn't transfer to
a width concern.

**Floor-GROW protection (`reclampViewFloor`, below) is bottom-only, by the
same logic — a deliberate scope boundary, not a gap (#361 rev-58 NB3).** A
view whose fixed chrome grows after it opens (today, only the group
panel's suspended-budget banner) only ever gets a divider nudged to make
room when it's hosted as the floating overlay or docked BOTTOM — both route
through the view's own live `floorPx()`. Docked LEFT or RIGHT, the floor is
the fixed width constant above, which has no HEIGHT component to grow at
all — there is nothing for a height-wise content growth to widen. So a
short pane with a lot of fixed-row chrome stacked in the group panel
(header, summary, max-agents, workflow row, autonomy row, and then a
banner) can still, in principle, overflow the box vertically while docked
to a side. The fix is `overflow-y: auto` on `.group-view` when its ancestor
is `.pane-embed-panel.side-left`/`.side-right` (styles.css) — scroll rather
than clip. This is intentionally the SMALLER of two possible fixes: the
alternative, growing a SECOND floor dimension (how much HEIGHT a left/right
panel needs) and threading it through `dividerFloors`'s width-axis math,
would need every left/right divider's clamp to reason about two axes for a
case that, in practice, only the group panel's fixed chrome can even
trigger. Scroll-not-clip is the honest, bounded answer until something
actually needs the second axis.

**Reclamping when a floor changes after a panel is already docked.**
`Pane.reclampViewFloor(kind)` looks up which side `kind` occupies (if any)
and delegates to `reclampSlotDivider(side)`, which re-applies that side's
CURRENT `dividerFloors` to its CURRENT sizes (a zero-delta "drag" — passing
zero still produces a real corrective nudge, because a size already below
the new floor makes the relevant `sizeX - minX` term negative in
`embedDragGrow`'s own clamp; see embedsplit.ts). Two triggers use this
today: `GroupView.onResize` (its floor growing from content, unchanged
concept from the single-slot design), and `embedViewAtSide`/`unembedView`
explicitly reclamping LEFT whenever RIGHT's occupancy changes (per *Layout*
above — left's composed far-side floor depends on it). **`reclampSlotDivider`
itself cascades LEFT's own reclamp into a follow-up reclamp of RIGHT (#361
rev-58 NB2):** left's counterpart element is the composite `embedCenterEl`,
which nests right's own divider pair (`termEl` | right's panel) — resizing
`embedCenterEl` changes the box that pair's flex-grow ratio divides, and
that ratio has no floor awareness of its own. Without the cascade, growing
left (e.g. its own view's floor demanding more room) could silently shrink
`embedCenterEl` enough to push an occupied right slot below ITS floor with
nothing correcting it, since nothing had directly touched right's divider.
The cascade only ever runs right-ward from left (never the reverse: dragging
right's own divider only trades space *inside* `embedCenterEl`, never
resizing it, so it can't affect left) and is a no-op whenever right isn't
occupied or wasn't actually pushed under its floor. No other view's
floor changes after it opens, so no other view wires `onResize`; the hook
stays optional on the interface for any future view that needs it.

**The single-occupant invariant, enforced per SLOT, not just intended
(#361 rev-38 blocker, generalized).** The original single-slot bug: swapping
the shared panel's occupant A→B left BOTH views' elements parented in it and
visible — `closeView` hid the panel but never relocated A's element, and
`openView` used `appendChild` (adds) rather than replacing. Fixed two ways,
deliberately redundant, and BOTH now operate per-slot rather than on one
shared panel:

1. `closeView`'s docked branch returns the evicted/closed view to its OWN
   overlay host (`entry.overlayEl.insertBefore(entry.viewEl, …)`) — parked
   and hidden, exactly where a never-docked view already lives between
   opens — for WHICHEVER slot it was in.
2. `openView`'s docked branch uses `slot.panelEl.replaceChildren(entry.viewEl)`,
   not `appendChild` — EACH slot's own panel can hold at most one child BY
   CONSTRUCTION, regardless of whether step 1 (or any future code path)
   forgot to clean up first.

Manual validation (below) exercises rapid A→B→A swaps ON THE SAME EDGE, and
separately docking three DIFFERENT views to three DIFFERENT edges at once —
the latter is the multi-slot generalization's own new way this invariant
could have broken (one slot's cleanup accidentally touching another's), and
it's structurally impossible here: each slot's panel/divider pair is a
wholly separate DOM subtree, so `replaceChildren` on the left slot can never
affect the right or bottom slot's contents.

**A restored (or merely stale) share is floor-clamped on open.**
`openView`'s docked branch calls `reclampViewFloor(kind)` immediately after
applying the initial `growFromFrac`/`applySlotGrow` split, on every docked
open — restore included, since `restoreEmbeds` opens through this same
path. Cheap and idempotent (a no-op when the current share already clears
the floor).

**Error recovery, generalized.** `openView` wraps every kind's `show()` in
the never-leave-the-pane-half-toggled recovery `toggleGitView` originally
had only for itself: retract whichever host was opening (the correct SLOT
for a docked view, or the overlay), let the error surface (global handler
shows a banner).

## Coexistence (#361 NB-4), generalized to N slots

`Pane.closeOtherOverlays(except)` loops every `EmbedKind` and closes ONLY
the ones currently showing AS AN OVERLAY (`entry.overlayEl` not hidden, and
`sideOf(kind) === null`) — a docked view, on ANY of the three edges, is
structurally invisible to this loop. So a human can have a view docked left,
another docked bottom, and still pop open a THIRD view as a floating
overlay (say, a quick look at issues) without any of it closing anything
else — the only thing a floating overlay's OWN open still closes is OTHER
floating overlays, never a docked one, on any edge. The file-editor overlay
(never embeddable) still participates as a plain floating panel: it closes
every OTHER floating overlay when IT opens, and is closed by any of the
five opening as an overlay, same as before #361.

## Reuse, not a fork

Every embeddable view is the same class, the same instance, in EVERY mode
(overlay, or docked to any of the three edges). `Pane.openView`/`closeView`
move `entry.viewEl` between the overlay host and whichever slot's panel with
a plain `appendChild`/`insertBefore`/`replaceChildren` — which detaches an
element from wherever it currently lives — so there is exactly one instance
of each view per pane regardless of how many times the human moves it
between edges or swaps a slot's occupant, and each view's internal state
survives the move untouched. Verified per view, not assumed:

- **`TasksView`** — an `orch-tasks-changed` subscription, expanded/selected
  row sets, an in-flight edit. Unaffected by reparenting (listeners live on
  elements inside `tasksView.el`, which moves as a subtree).
- **`GitView`** — the one with the most internal state (repo root, worktree
  selection, commit log, diff selection) and its own nested resizable
  sub-panes (graph | diff over the changes strip). It was ALREADY
  container-agnostic before this PR: its inner layout has always re-clamped
  to `this.el`'s own live size via its own `ResizeObserver`
  (`this.resizeObs.observe(this.el)`), which is exactly what content-panes.md
  calls "the second sizing model" — built for the #217 content-pane hosting,
  and it turns out to cover the docked-panel hosting for free (LEFT, RIGHT,
  or BOTTOM — the view never needs to know which), since none of them is the
  floating overlay's absolute-position-plus-fixed-height model. `hide()`
  explicitly dismisses any open context menu (`closeMenu()`) — preserved by
  `EmbedEntry.hide`, called on every close regardless of mode or edge.
- **`IssuesView`** — no internal ResizeObserver at all (a plain list; its
  CSS is `flex: 1`, filling whichever host it's in). `hide()` closes any
  open create-issue form or detail pane first, preserved the same way.
- **`AuditView`** — a live-follow poll timer (`followTimer`), gated by an
  explicit toggle button, not by open/close — unaffected by which host or
  edge it's in, and already stopped by `dispose()` regardless.
- **`GroupView`** — see *Layout* above for its one real piece of mode-aware
  logic (the floor). Its own poll timer (`pollTimer`, started in `show()`)
  had a pre-existing quirk: `show()` fires on every open in ANY mode, but
  nothing cleared `pollTimer` on close — only `dispose()` did. Rarely hit
  before #361 (closing/reopening the overlay repeatedly was the only
  trigger); the single-slot generalization already made swapping a
  one-click, repeatable action, which is what made it worth fixing rather
  than continuing to note-and-defer: `GroupView.hide()` clears the timer,
  wired into the registry's `hide` callback so `closeView` stops it on
  every close, from every edge, in either mode. `show()` also defensively
  clears any stray timer before arming a new one.

The overlay host (`.git-overlay`, one per view — unchanged) and each edge's
slot (`.pane-embed-panel.side-*` / `.pane-embed-divider.side-*`) are all
created lazily and left in the DOM afterward, hidden via the app-wide
`[hidden] { display: none !important; }` rule rather than
created/destroyed — the same reuse idiom every overlay in `pane.ts` already
used for itself, now applied to three slots instead of one.

## Persisted shape

`PersistedPane.embeds: PersistedEmbed[]` (`tabstore.ts`) — an array of up to
three `{ view: PersistedEmbedView; side: "left" | "right" | "bottom"; share:
number }` records, one per currently-docked edge. Empty = nothing docked,
every view opens as its floating overlay (the pre-#361 default). `share` is
in the same units a split node's own `weight` already persists (a flex-grow
ratio, not a pixel size). Additive: an old tabs.json simply never carries
the key, and `decodePane` treats a missing or malformed value as `[]` — no
schema bump, the same pattern `role` and the files/git root used when they
were added. Malformed individual entries are dropped, not the whole array;
two entries claiming the SAME side are also de-duplicated (first wins) —
`test/tabstore.test.ts` pins both.

**`PersistedEmbedView` is `"tasks" | "audit" | "group"` — a strict subset of
`pane.ts`'s own `EmbedKind`.** Only the orchestration-family views are
representable in the persisted shape at all; `git`/`issues` are never
written here. See *Why only three views survive a restart*, below.

**Migrated from BOTH earlier shapes, neither of which ever shipped in a
release.** This PR generalized twice within the same review cycle — first
task-board-only (`taskEmbed: number`), then any-of-five-but-one-slot
(`embed: {view, share}`), then this multi-slot shape — and `decodePane`
stays lenient across all three, newest-present-shape-wins:

```
embeds: [{view, side, share}, …]     ← current
embed:  {view, share}                ← pre-multi-slot; migrates to side: "bottom"
taskEmbed: number                    ← pre-generalization; migrates to {view: "tasks", side: "bottom"}
```

The cost of tolerating two extra shapes is a few lines; the cost of not is a
silently dropped preference on the next boot after a stray hand-edited or
pre-rebase tabs.json. `test/tabstore.test.ts` pins every migration path and
the precedence between them.

**Why only three views survive a whole-group resume.** Orchestration panes
are never auto-resumed on app restart (`panerestore.ts`'s `dormant-group` —
the human clicks Resume, deliberately, to avoid a credit/process-storm on
every boot). That dormancy is what gives `tasks`/`audit`/`group` a natural
restore hook: the docked-view preferences ride the SAME path `role` and
`sessionId` already ride — captured into the dormant placeholder's record
(`main.ts`'s `case "dormant-group"`), read back off it in
`resumeDormantGroup`, and matched — by session id, the same key
`planGroupResume` itself matches on — to the pane that comes back once
`resumeOrchSession` actually resolves (`Pane.restoreEmbeds`, plural — it
iterates every entry and re-docks each to its own recorded edge).
`git`/`issues` are embeddable on EVERY pane kind (including a plain
terminal), but a plain terminal restore has no dormant-placeholder
indirection at all — it re-spawns directly, immediately, with nothing
"captured, then reapplied later" to hook a preference onto. Threading that
through would mean adding an embeds field to every other `RestoreAction`
variant (`spawn-terminal`/`fresh-agent`/`dormant-agent`/…) and a matching
apply-call at each of `main.ts`'s several live-pane-creation sites — real
additional plumbing, not a small extension of what orch panes already have.
So `Pane.capture()` only ever writes an `embeds` entry for a currently-
docked kind that is BOTH orchestration-family AND on a pane where `kind ===
"orch"` (`isRestorableEmbedKind`); docking git or issues to any pane is
fully functional for the pane's live lifetime, including moving between
tabs, but does not survive a full app quit + relaunch. This is a real,
deliberate scope boundary, not an oversight — if it needs to move, the
extension point is exactly the "captured, then reapplied once the real pane
exists" pattern `tasks`/`audit`/`group` already use, just without the
dormancy step to hang it on.

**This is the one place today a captured per-pane UI preference is threaded
through a whole-group resume** — every OTHER overlay's open/closed state has
never needed to be, because none of them was ever meant to be a station kept
open across a restart. `embedsBySession` in `main.ts` is deliberately built
as a plain `sessionId → PersistedEmbed[]` map rather than folded into the
resume plan itself (`planGroupResume`'s `GroupMember` intentionally stays
`{sessionId, role}` — a scheduling/matching plan, not a preference bag).

**Known gap: a respawn-fresh fallback loses the preference.** The match
above is keyed on the CAPTURED session id — the one `resumeOrchSession` was
asked to `--resume`. If that resume attempt fails at runtime (a deleted
transcript, any other resume-time CLI failure) and `shouldRespawnFresh`
(`panerestore.ts`) fires its one-shot fresh-in-place respawn, the pane ends
up carrying a NEW session id that was never in `embedsBySession`. The
lookup then misses, `Pane.restoreEmbeds` is never called for that member,
and every view it had docked simply opens as the floating overlay next
time — a silent fallback to the pre-#361 default, not a crash or a stuck
state. Accepted: the member's *conversation* is already gone in this
scenario (that is what triggered the respawn), so losing a UI layout
preference alongside it is the smaller loss on the same bad path, and
re-docking after the fact is one click per view.

## Manual validation (the human)

The production app can't be launched from this session (#394) — these are
the steps for the human to run.

**Per-view basics (repeat for task board, git, issues, audit, and the group
lifecycle panel):**

1. Open the view as the overlay (unchanged). Click its embed button (⬒) —
   a small menu should open: Left / Right / Bottom, no checkmarks yet (not
   docked). Pick **Bottom** — the view should move to sit BELOW the
   terminal, both fully visible, with a thin draggable divider between
   them, and the button should read as pressed (⬓, accent-colored).
2. Type in the terminal — the CLI should still be fully usable, at its
   (now shorter) size, with no repaint storm in scrollback from the dock
   itself.
3. Drag the divider — the terminal and the panel should resize smoothly,
   respecting a minimum size on each side (dragging hard in either
   direction should stop short of collapsing the other one's chrome). For
   the group lifecycle panel specifically: trigger the suspended-budget
   banner (or any state that grows its fixed chrome) while docked bottom —
   the divider should nudge to make room on its own rather than letting the
   footer clip. Now dock the group panel LEFT or RIGHT instead, shrink the
   pane's overall height so the panel is short, and trigger the same banner
   — floor-grow protection doesn't apply on a side dock (#361 rev-58 NB3,
   deliberate scope boundary — see the design note), so the panel should
   SCROLL to keep the footer reachable rather than clip it under
   `overflow: hidden`.
4. Reopen the embed menu — **Bottom** should now show a checkmark. Pick
   **Left** — the SAME view should move from bottom to the left edge in one
   step (not close-then-reopen-visibly): **the bottom slot's panel AND its
   divider must both fully disappear — not just go empty** (#361 rev-58's
   blocking finding: the origin slot's `kind` was nulled out before the
   close, so `closeView` couldn't find it and left an empty panel+divider
   sitting there). Open devtools and confirm `.pane-embed-panel.side-bottom`
   and `.pane-embed-divider.side-bottom` are both `[hidden]`, and a left
   slot has appeared, sized reasonably. Confirm dragging the LEFT divider
   now resizes width, not height. Repeat the same move in the OTHER
   direction (left → right, right → bottom, etc.) — the bug was specific to
   the move-source code path, not to any one pair of edges.
5. Click **Un-embed** — the view should return to the floating overlay.
6. Re-dock it, close it (its own hotkey or ✕) — it should close taking the
   slot with it; the terminal regains full size on that edge. Reopening the
   SAME view should come back docked, on the SAME edge it was last on
   (this session's memory — see *Side memory*, above; it does NOT survive
   an un-embed back to overlay first).

**Multiple simultaneous slots — the core of this generalization:**

7. Dock the task board LEFT, git BOTTOM, and issues RIGHT, all at once —
   all three should be visible together with the terminal in the middle,
   each with its own working divider, and none of the three should have
   affected the others' sizes when it was docked. Drag each of the three
   dividers in turn and confirm each one only ever trades space with its
   own two neighbors (dragging the left divider must not visibly change the
   bottom panel's height, etc.).
8. With left AND right both occupied, drag the LEFT divider hard toward the
   terminal — it should stop leaving enough room for BOTH the terminal AND
   the (still fully visible, unclipped) right panel, not just the terminal
   alone. This is the composed-floor case (`embedCenterFloor`) — open
   devtools and confirm `embedCenterEl`'s measured width never drops below
   roughly `EMBED_MIN_TERM_PX + 180 + 6`.
8b. Same setup (left AND right both occupied) — this time drag the LEFT
   divider hard AWAY from the terminal (growing the left panel, shrinking
   `embedCenterEl`, which contains both the terminal AND the right panel).
   The right panel must stay at or above its own floor — it should NOT be
   silently squeezed just because nothing touched its own divider directly
   (#361 rev-58 NB2: `embedCenterEl`'s own resize doesn't respect right's
   floor on its own; `reclampSlotDivider` has to cascade from left into a
   follow-up reclamp of right). Open devtools and confirm the right panel's
   measured width never drops below ~180px through the whole drag.
9. With a third view already docked to a FREE edge, embed a FOURTH view
   onto an OCCUPIED edge (say, bottom, currently holding git) — git should
   close outright (back to its own floating overlay's `hidden` state, not
   silently reappearing as an overlay) and the new view should take the
   bottom slot; the OTHER two edges must be completely unaffected. Then dock
   the ORIGINAL view (git) back onto that same bottom edge, then swap again,
   rapidly, a few times (A→B→A→B) — at every step exactly one view's
   content should be visible in that one slot, and opening devtools on
   `.pane-embed-panel.side-bottom` at any point should show it holding
   exactly one child. This is the scenario that caught the single-slot
   design's original dual-visible bug, now re-checked per edge.

**Coexistence and content panes:**

10. With views docked on all three edges, open a FOURTH, non-embeddable
    surface as a floating overlay (or a fifth embeddable view you haven't
    docked yet) — it should open as an overlay over the terminal without
    closing anything docked. Confirm the reverse too: with a floating
    overlay open, dock a different view to a free edge — the floating one
    should stay put.
11. On a content pane (a `files`/`editor`/`git`/`workflow` pane, #214/#217):
    confirm neither the embed button nor the overlay is offered for
    git/issues/file-editor there (`refuseOverlay`, unchanged) — there is no
    terminal on a content pane to share space with.

**Restart survival (orchestration-family views only):**

12. Dock the task board left and the group panel bottom, quit and relaunch
    loomux with that group's tab still around — it should restore dormant
    (unchanged). Click **Resume**: BOTH should come back docked, on their
    original edges, at roughly the sizes they were left at. Do this on a
    group with at least one worker/reviewer pane open alongside the
    orchestrator — the dormant-shadow exclusion `findResumedPaneIndex`
    guards (a stale placeholder carrying the same captured session id as
    the pane actually being resumed) is only unit-tested against synthetic
    candidates; this is the one path that exercises it against the real
    grid/DOM, and with more than one member in flight it's the scenario
    most likely to surface an ordering assumption the synthetic test can't
    see. Also try it with the group panel specifically resized very small
    before quitting (near its floor) — on Resume it should come back at
    least as tall as the group panel's CURRENT measured chrome, not
    clipped, even if that floor grew between sessions.
13. Embed git or issues on a plain terminal or agent pane, then quit and
    relaunch — confirm it comes back as the floating overlay (the
    documented, deliberate scope boundary above), not docked and not
    missing entirely.
