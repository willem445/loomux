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
  narrow, bounded, and named exception.** For seven of the eight views (task
  board, git, issues, audit, the file editor, the progress timeline, the
  NEEDS-YOU panel),
  refreshing a list, expanding a row, typing in a filter — none of it
  changes the panel's MINIMUM size, so none of it touches `termEl`.
  Verified per view below, not assumed. (The timeline is the one whose
  content is geometry-dependent, and it still holds: it re-lays its chart
  out INSIDE whatever box it is given, and its floor is the generic
  `EMBED_MIN_PANEL_PX` — the chart shrinks, the panel's minimum does not
  move, so the terminal never hears about it. #608.)
  **The group panel
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
`setEmbedded` — applied uniformly to all eight views (including `TasksView`
and `FileEditView`, neither of which has a collision of its own, for one
consistent interface across the set) so a reader never has to remember
which view's method means what. `FileEditView` in particular already had
an UNRELATED `embedded` ctor option of its own (the #217 content-pane
flag) before it became embeddable via this engine — the exact same naming
trap `GitView` hit, avoided the same way.

## What's embeddable, and what isn't

**Eight** views are embeddable: the **task board**, **git**, **GitHub
issues**, the **audit log**, the **group lifecycle panel** ("lifecycle
status" in the issue — the panel behind `GroupView`'s overlay toggle), the
**file editor** overlay (`Alt+F`), the **progress timeline** (`Alt+W`,
#608), and the **NEEDS-YOU panel** (`Alt+Q`, #1091). All eight are wired
through one generic engine in `pane.ts`
(`EmbedKind`, `EmbedEntry`, `embedRegistry`,
`openView`/`closeView`/`toggleView`/`embedViewAtSide`/`unembedView`) — see
*The generic engine*, below. Any THREE of the eight may be docked at once,
one per edge; the rest (however many aren't docked) stay available as
floating overlays.

**The progress timeline (#608) is the cheap case this note predicted.** It
needed no engine change at all: one `EmbedKind`, one `ensureTimelineView`
registering an `EmbedEntry`, and the generic open/close/toggle/dock paths
carried it. It is gated exactly like the audit log (every orchestration
pane, not orchestrator-only — the data is the group's and the view is
read-only), and it is restorable on the same terms as `audit`: the DOCK
preference survives a restart, its window preset and category chips do not,
the same way a restored audit log does not restore its filters. The one
thing worth noting for a future view: it is the first embeddable view whose
CONTENT is geometry-dependent, so it observes **its own container's** width
(a `ResizeObserver` on the chart div) and re-lays out from that. It never
reads the terminal's box, which is what keeps the PTY-resize boundary above
untouched — see `doc/design/progress-timeline.md`.

**Git was part of the generic engine from the same round that generalized
past the task board** — it needed nothing new for this round beyond
verifying it: `toggleGitView` already routed through `Pane.toggleView`
(so the docked-toggle no-op applies to it by construction, same as every
other kind), and its own `setPanelActive` already disabled/retitled its
internal ✕ while docked. The one thing this round added for git is
restorability (below).

**The file editor was excluded through three prior rounds, on a real
argument that this round's user-directed scope increase overrides rather
than invalidates.** The original case: the editor **content pane** (#217,
`FileEditView`'s `embedded` mode — the very flag this note's naming section
discusses) is a STRICTLY BETTER answer to "I want to keep an editor open
long-term beside my agent" — full pane width, a real tab-bar entry, state
that survives a session restore the normal way. None of that changed, and
the content pane remains the right tool for that job; "Open in editor
pane" (the file browser's row action) still creates a wholly separate
`FileEditView` instance (`Pane.editorPaneView` — never the same object as
the dockable overlay's `Pane.fileEditView`, so there is no risk of the two
colliding or one spawning a duplicate of the other). What the exclusion
argument didn't account for is a DIFFERENT use case every other docked
view already serves: a quick, SAME-PANE edit while continuing to watch the
terminal, without giving up a whole separate pane's width for it — exactly
what "dock it beside the agent" already means for git/tasks/audit/issues.
The user asked for it directly, having used the other five; that is a
refinement on work already in flight, not drift, and folds into this PR
per this repo's own policy on user-directed scope increases.

**The file editor's unsaved-buffer lifecycle (#219) is preserved, not
bypassed, by embedding it** — see *#219 interaction*, below, for the one
place embedding actually touches that lifecycle (the toggle-close path)
and the reasoning for why every OTHER #219 guarantee (pane-kill, tab-close,
app-quit) needed no changes at all.

## #219 interaction: what embedding touches, and what it doesn't

The file editor is the one embeddable view that can hold real, irreplaceable
human work (an unsaved buffer) — every #219 guarantee about that had to be
re-checked against "now it can also be docked," not assumed safe.

**Unaffected, by construction: pane-kill, tab-close, and app-quit.** All
three go through `Pane.unsavedHolder()` — `this.editorPaneView ??
this.workflowPaneView ?? this.fileEditView` — which returns the SAME
`FileEditView` INSTANCE regardless of whether it's currently an overlay,
docked to an edge, or hidden. `canDiscard()` (called from there) only ever
asks `isDirtyNow()` — never the display mode — so `confirmClose()`
(pane-kill/tab-close) and the app-quit guard's `dirtyBuffers()` enumeration
are exactly as correct for a DOCKED editor as they always were for the
overlay one, with zero logic changes needed. This was verified by reading
`unsavedHolder()`'s own call sites, not assumed from the shared-instance
argument alone.

**Cosmetic-but-real: the quit confirm's WHERE label (rev-27 finding).**
`bufferReport()`'s dirty DETERMINATION never needed the display mode (the
paragraph above) — but the LABEL it attaches (`DirtyHost`) does, since the
whole point of the label is telling the human WHERE to go look. Before this
fix, a docked editor's buffer reported as `"overlay"` — accurate enough to
not be WRONG (it is still the same Alt+F editor, not a content pane), but
missing the one fact a docked editor's own quirk makes worth stating: it
LOOKS like a permanent fixture of the pane rather than something you
opened and could still be holding edits in, exactly the opposite instinct
an overlay (something you visibly dismissed) gives you. `DirtyHost` grew a
third value, `"docked"`, and `bufferReport()` sets it via `this.sideOf(
"editor") !== null` (reachable only in the branch that already ruled out
the content-pane case, so it's unambiguous which holder is being asked
about); `dirtyBufferLines` labels it `"(docked editor)"`, alongside the
existing `"(Alt+F editor)"` for a plain overlay. `test/dirtystate.test.ts`
pins the new label.

**The one place embedding DOES touch #219: the toggle-close path
(Escape/✕/header-button/keybinding) — because it's the one #219 path that
was never actually about destroying the buffer.** Before #361, `Alt+F`
toggling the overlay CLOSED (hid) it without ever asking about unsaved
edits — correctly, because hiding doesn't touch the buffer, only
`dispose()` (pane teardown, guarded by `confirmClose()` above) does. The
view's OWN internal ✕/Escape (`requestClose`), though, DOES ask first via
`confirmDiscard()` — a UX courtesy layer above the toggle, not a
data-safety requirement, since a "yes" there ACTUALLY reverts the buffer
(`revertBuffer()`) even though the resulting hide is just as non-destructive
as Alt+F's. Once docked views' toggles became no-ops (the OTHER #361
user-demo finding, above), this courtesy layer became a hazard: `Escape`
on a DOCKED, dirty editor would ask "discard unsaved changes?", a "yes"
would ACTUALLY DISCARD the buffer, and then the underlying toggle would
no-op anyway (the panel stays open, undocked only via the side menu) — a
real edits loss for a click that didn't even close anything.

**The fix: `shouldConfirmDiscardBeforeClose(docked, dirty)`
(`dirtystate.ts`, pure) — docked wins outright, skipping the confirm
dialog entirely rather than letting it fire ahead of a no-op.**
`FileEditView.requestClose` calls it before ever showing
`confirmDiscard()`; when docked, it goes straight to `this.host.onClose()`
(`Pane.toggleFileEditView` → `Pane.toggleView("editor")`), which is where
the SAME toast/disabled-button affordance every other docked view's
toggle already shows takes over. Undocked, the decision falls back to the
plain #219 rule (`closeDecision`), unchanged. `test/dirtystate.test.ts`
pins both branches.

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

## Overlay toggle vs. dock: disabled, not fixed (#361 user-demo finding)

A live demo surfaced a real bug: with a view docked, clicking its OWN
overlay toggle — the pane header's button, the matching keybinding
(Alt+G/T/A/O/I), or the view's own internal ✕/Escape (`onClose`, wired
straight back to the same `toggleXView`) — closed/reparented the view but
left the SLOT's own panel+divider sitting there visible and empty: a black,
dead rectangle where the view used to be. Root cause: `toggleView`'s
close/open decision and the SLOT's own open/closed state are two
independently-driven pieces of visibility for the SAME view — the toggle
flips the view's own `hidden` flag (`GitView`/`IssuesView`'s own duplicate
of it; see *Reuse, not a fork* below) and, separately, `closeView` flips the
slot's `panelEl.hidden` — and while every path THIS review traced kept the
two in lockstep, that's a standing tax on every future change to either
side, with a large surface of entry points (six views × header button ×
keybinding × internal close × Escape × `pane-meta`'s branch-name click) any
one of which reintroduces the same bug if it ever forgets to.

The fix removes the tax instead of continuing to pay it: **while a view is
docked, its overlay toggle is disabled outright — a no-op, not a fixed
close/reopen.** `embedToggleAction(docked, visible)` (`embedtoggle.ts`, pure
and DOM-free) is the single decision every toggle path now goes through —
`docked` wins unconditionally, so a docked view's toggle can never again
race or drift out of sync with its slot, because it no longer DOES
anything to either piece of state. The only way to make a docked view stop
sharing space with the terminal is the explicit **Un-embed** action in its
side menu, which already goes through `unembedView` — a single, correct,
already-tested code path — and the plain toggle works normally again the
moment it does.

**The single choke point is what makes the fix actually cover every entry
point.** `Pane.toggleView(kind)` is the one function EVERY toggle path
already funneled through before this fix (each view's public
`toggleXView()` method — itself called by the header button, `main.ts`'s
keybinding dispatch, and the view's own `onClose`/Escape handler) — so
putting `embedToggleAction`'s guard there, once, closes off the whole class
at the root rather than requiring the same check copy-pasted into five
views' worth of buttons and handlers, each one a chance to miss.

**Visible affordance, not just a silent no-op.** A no-op keybinding or
metadata click (the branch-name label isn't a real `<button>`, so it can't
be `disabled`) shows a toast (`showToast`, the same mechanism
`refuseOverlay` already uses for "isn't available in a ___ pane") — "The
git view is docked — un-embed it (its side menu) to use this toggle." Every
REAL button gets a stronger, persistent affordance: the pane header's own
toggle button (`Pane.syncEmbedToggleButton`) and each view's own internal ✕
(extended into every view's existing `setPanelActive`, the same hook that
already updates the embed button's icon on every dock/undock) are both
`disabled` and retitled while docked, restored the moment it un-embeds.
`.pane-btn:disabled` (styles.css) dims further than the plain hover-reveal
opacity and drops the pointer cursor, so a docked toggle reads as
unavailable rather than merely inactive.

## Coexistence (#361 NB-4), generalized to N slots

`Pane.closeOtherOverlays(except)` loops every `EmbedKind` and closes ONLY
the ones currently showing AS AN OVERLAY (`entry.overlayEl` not hidden, and
`sideOf(kind) === null`) — a docked view, on ANY of the three edges, is
structurally invisible to this loop. So a human can have a view docked left,
another docked bottom, and still pop open a THIRD view as a floating
overlay (say, a quick look at issues) without any of it closing anything
else — the only thing a floating overlay's OWN open still closes is OTHER
floating overlays, never a docked one, on any edge. The file-editor overlay
participates on exactly the same terms as every other kind: it closes every
OTHER floating overlay when IT opens, and is closed by any of the other
seven opening as an overlay, same as before #361. (It was described here as
"never embeddable" when this section was written, which stopped being true
once the #361 scope increase made it an `EmbedKind` like the rest — the
behaviour described is unchanged, only the aside was stale.)

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
- **`FileEditView`** (#361 scope increase) — the most STATEFUL view here:
  an open file's buffer, dirty tracking, tree/search state, an in-flight
  content-search enumeration. None of it is display-mode-aware; `show()`/
  `hide()` never touch the buffer at all (only `dispose()`, pane teardown,
  does — see *#219 interaction*, above). `hide()` already cleared its
  search timer pre-#361 (the overlay-vs-#217-content-pane duality this view
  already had); nothing new needed there for docking. The one addition is
  `setPanelActive`, matching every other view's shape exactly — disables +
  retitles its own ✕ while docked, same as the other seven.

The overlay host (`.git-overlay`, one per view — unchanged) and each edge's
slot (`.pane-embed-panel.side-*` / `.pane-embed-divider.side-*`) are all
created lazily and left in the DOM afterward, hidden via the app-wide
`[hidden] { display: none !important; }` rule rather than
created/destroyed — the same reuse idiom every overlay in `pane.ts` already
used for itself, now applied to three slots instead of one.

## Performance: bounding a docked panel's own render cost (#361 user-demo)

Docking makes a panel's OWN scrollable list a real flex sibling that
resizes continuously while a divider is dragged — that's the whole point
(*The PTY-resize boundary*, above). It also surfaced a cost overlay mode
mostly hid: a floating overlay's HEIGHT rarely got dragged past a screenful,
but a docked panel is now resized as casually as a grid split, and a demo
with a long-running group's audit log — 2735 entries, one live DOM row
each — was measurably laggy to drag once docked. The lag isn't from any
`render()` call during the drag (nothing calls it — no view has a
`ResizeObserver` reacting to its own container size, `GitView` included);
it's the browser's OWN layout engine, forced to re-lay-out thousands of DOM
nodes on every single `mousemove` frame just because their SHARED
ANCESTOR's cross-axis size changed, even though nothing about any
individual row's own content did. Two independent fixes, addressing two
different halves of that cost:

**1. Bound the row count, don't just tolerate it — `auditwindow.ts`.** The
audit log is the one view in this set with a genuinely unbounded growth
path: `audit.jsonl` is append-only for the life of a group, unlike the task
board (curated — done items get cleared) or the issues list (bounded by
whatever the repo actually has open). Rather than ever holding the full log
in the DOM, `AuditView` renders only the newest `AUDIT_WINDOW_SIZE` (300)
MATCHING (post-filter) entries by default; scrolling near the top of the
list backfills another `AUDIT_WINDOW_STEP` (300) further back
(`maybeBackfill`), preserving the human's scroll position across the
rebuild (`scrollTop` is restored by the height DELTA the newly-prepended
rows added, not reset to 0). The pure decision, `nextWindowStart`, is
DOM-free and covers the three cases that matter: a filter change
invalidates the old window index outright (reset to the newest slice); new
entries arriving while the human is AT THE TAIL (`nearBottom`, following)
slide the window forward to stay capped, so a long follow session never
re-grows past the cap; new entries arriving while the human has scrolled up
to read history leave the window exactly where it is — a follow poll must
never yank someone's place in the backlog out from under them just because
new entries landed at a tail they aren't looking at. `test/auditwindow.test.ts`
pins all three, plus the backfill step and its floor at 0.

**2. Suspend layout of a docked panel's content for the DURATION of a
drag, not just bound its steady-state size — the `.resizing` class.** Even
at 300 rows, dragging fires `mousemove` at display refresh rate, and every
tick still forces a reflow that has nothing to do with what the human is
actually watching (the DIVIDER's position, not the list's internal
layout). `Pane.wireEmbedDivider`'s mousedown adds `resizing` to the SLOT's
own `panelEl` (never to `beforeEl`/`counterpartEl` — the terminal side of
any divider is never touched by this); `makeOverlayDivider` does the same
to the overlay host for the identical cost during a plain (non-docked)
overlay height-drag. `styles.css` scopes `content-visibility: hidden` to
each heavy view's own list class under `.resizing`
(`.audit-list`/`.tasks-list`/`.issues-list`/`.group-list`) — the browser
skips layout AND paint of the rows entirely while the class is present, and
lays them out ONCE, normally, the instant it's removed on release. This is
safe specifically because every one of those list elements sizes itself via
an explicit `flex: 1` (`flex-basis: 0`, never `auto`) from its OWN flex
parent — `content-visibility: hidden` implies `contain: size`, which only
changes an element's size if that size was ever DERIVED from its content in
the first place; none of these are, so the panel's outer box keeps
resizing exactly as smoothly as before, only its rows stop costing
anything until the drag ends.

**`.resizing` must come off on every way a drag can end, not just
`mouseup` — `dragsession.ts` (post-merge review finding).** The first cut
of fix 2 removed `.resizing` only from a `mouseup` handler, mirroring
grid.ts's own pre-existing `dragging`-class pattern exactly — but the
consequence of stranding it is not the same. A drag that ends WITHOUT a
`mouseup` (Alt-Tab away mid-drag fires window `blur`; the mouse button is
still physically down, but the browser never delivers a matching up event
to a window that no longer has focus) used to leave grid.ts's `dragging`
class stuck — a cosmetic leftover highlight, low stakes. The identical gap
on `.resizing` strands `content-visibility: hidden` on a docked view's own
list — a REAL stuck state (the list stays invisible) until some later,
unrelated resize happens to touch that side again. `startDragSession`
(`src/dragsession.ts`) is the fix, shared rather than forked: one helper
wires `mousemove` + THREE end signals (`mouseup`, window `blur`, `Escape`)
and guarantees its `onEnd` callback fires exactly once regardless of which
one actually ends the drag, tearing down whatever `mousedown`-time state
the caller applied. Every divider in the codebase (grid.ts's pane splits,
both embed-slot and overlay dividers here) now goes through it — including
grid.ts's own, so the precedent gets the same fix rather than the new code
diverging from it. Ending early is treated exactly like a normal release
(whatever size the drag reached stands); this fixes the STRANDED-STATE bug
only, not a "cancel and restore the pre-drag size" feature nothing asked
for. `test/dragsession.test.ts` pins the exactly-once/all-listeners-removed
guarantee against a plain fake event target (mirrors `domutil.ts`'s
narrow-interface testability pattern — `startDragSession` takes an
injectable target, defaulting to `window`, precisely so this is
unit-testable without a real DOM); the Alt-Tab-away scenario itself is
manual-validation step 21, since nothing short of a real window losing
focus can confirm the browser-level behavior the pure test assumes.

**Task board, issues, and the file editor's tree: the same discipline where
it's free, not the same windowing.** `.resizing`'s `content-visibility`
rule (fix 2) costs nothing extra to extend to `.tasks-list`/`.issues-list`/
`.fileedit-tree` (added alongside the #361 scope increase that made the
editor dockable — its tree can genuinely hold as many rows as a large
repo has files) and is applied to all three. Row-windowing (fix 1) is NOT
extended to any of them, on purpose: the task board is actively curated by
the orchestrator/human (done items get cleared, #120) rather than growing
forever, the issues list is bounded by whatever GitHub actually returns
for the repo, and the file tree only ever renders the CURRENTLY EXPANDED
folders (the tree itself is not a flat unbounded list the way the audit
log is) — none of the three has audit's specific "genuinely unbounded,
append-only, thousands over a long session" shape, and windowing any of
them would trade away seeing the full board/list/tree for a problem that
(today) doesn't reproduce there. If a repo's issue count, a board's task
count, or a single folder's file count ever grows large enough to
reproduce the same symptom, `auditwindow.ts`'s pattern is the one to reach
for — it isn't audit-specific in its logic, only in its current wiring.

## Right-dock expand-left lag (#361 user-demo finding) — `embedTermWrapEl`

A live demo found the three dividers were NOT actually symmetric: dragging
the RIGHT divider LEFT (growing the right-docked panel, shrinking the
terminal) lagged; the identical gesture on the LEFT or BOTTOM divider did
not. Two of the reviewer's three suspected mechanisms were ruled out by
reading the code, not by guessing: `.resizing` (the drag-perf fix, above)
is applied to `slot.panelEl` from the SAME unconditional line in
`wireEmbedDivider` regardless of `side` — no per-side branch skips it. And
the `content-visibility` CSS selectors (`.pane-embed-panel.resizing
.audit-list`, etc.) match on the PANEL'S class, not the SIDE — a docked
view's list sits in the identical wrapper shape whichever edge it's on.
Both were confirmed false, not assumed false.

**The one CONFIRMED structural asymmetry:** before this fix, `dividerPair`
and `counterpartEl` used `termEl` ITSELF as the right divider's `beforeEl`
— there was nothing else for it to be, since `embedCenterEl`'s row was
exactly `[termEl, right's divider, right's panel]`. That made right the
ONLY one of the three dividers whose drag handler wrote `.style.flex`
directly onto `termEl` — the exact node `resizeObs` observes — on every
single `mousemove`. Left's and bottom's far side has always been a WRAPPER
(`embedCenterEl` / `embedRowEl` respectively) that resizes `termEl` only as
an INDIRECT, computed consequence of the wrapper's OWN flex-grow changing;
`termEl`'s own inline style is never touched by their drags at all. `grep`
confirms this directly: before this fix, `termEl.style.flex` was written in
exactly one place in the entire codebase — the right divider's `move`
handler.

**Honesty about what this note can and can't claim.** This environment
cannot launch `npm run tauri dev` (#394 — a live GUI the human must drive)
and had no working interactive-browser tool available this session, so
there is no measured before/after frame-timing number to report here — the
diagnosis above is a code-level one, not a profiled one. What IS true
without needing a profiler: the asymmetry existed, and it existed on
EXACTLY the divider that lagged. The fix removes it outright rather than
leaving it as an unverified correlation: `embedTermWrapEl`, a thin,
permanent wrapper created once in `ensureEmbedHost` around `termEl` inside
`embedCenterEl` (`embedCenterEl` > `embedTermWrapEl` > `termEl`, instead of
`embedCenterEl` > `termEl` directly). The right divider's `beforeEl`
(`dividerPair`) and `counterpartEl` both now point at `embedTermWrapEl`,
never `termEl` — matching left's and bottom's shape EXACTLY: all three
dividers now resize `termEl` only indirectly, through a wrapper's own
flex-grow, and `termEl`'s own inline style is never written by ANY divider
drag. `termEl` fills the wrapper via its own pre-existing `flex: 1` rule
(`.pane-term`), unaffected either way — the wrapper adds one CSS rule
(`.pane-embed-term-wrap`, mirroring `.pane-embed-center`'s own `flex: 1 1 0;
min-height: 0; min-width: 0;` shape) and no behavior change when nothing is
docked (a single flex child fills its parent identically whether or not
there's an extra pass-through wrapper in between).

If the human's own re-validation (this round explicitly includes "right-
dock expand-left smooth" as a check) still finds it laggy after this fix,
that would mean the asymmetry above, though real, wasn't the (or the only)
cause — worth a follow-up with an actual profiler trace at that point,
which this session's tooling couldn't produce.

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

**`PersistedEmbedView` is every `EmbedKind` except `"issues"`** — today
`"tasks" | "decisions" | "audit" | "group" | "git" | "editor" | "timeline"`. `git` and `editor` joined this
round (#361 scope increase) once it became clear the thing that actually
gated restorability was never "is this an orchestration-family view" — it
was "is this docked on an ORCHESTRATOR pane," which `git`/`editor` satisfy
exactly as well as `tasks`/`audit`/`group` do. `issues` remains the one
excluded kind, for a reason that has nothing to do with what kind of view
it is — see *Why these kinds survive a whole-group resume*, below.

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

**Why these kinds survive a whole-group resume, and `issues` doesn't.**
Orchestration panes are never auto-resumed on app restart (`panerestore.ts`'s
`dormant-group` — the human clicks Resume, deliberately, to avoid a
credit/process-storm on every boot). That dormancy is what gives a docked
view a natural restore hook, and it has NOTHING to do with which view it
is: the docked-view preferences ride the SAME path `role` and `sessionId`
already ride — captured into the dormant placeholder's record (`main.ts`'s
`case "dormant-group"`), read back off it in `resumeDormantGroup`, and
matched — by session id, the same key `planGroupResume` itself matches on
— to the pane that comes back once `resumeOrchSession` actually resolves
(`Pane.restoreEmbeds`, plural — it iterates every entry and re-docks each
to its own recorded edge). `Pane.capture()` writes an `embeds` entry for a
currently-docked kind that is BOTH in `RESTORABLE_EMBED_KINDS` AND on a
pane where `kind === "orch"` — the FIRST half is what `tasks`/`audit`/
`group`/`git`/`editor` all satisfy and `issues` doesn't; the SECOND half
(an orch pane specifically) is what every one of them needs regardless.

`git`, `editor`, and `issues` are ALL embeddable on EVERY pane kind
(including a plain terminal), not just orchestrator panes — but only an
ORCHESTRATOR pane has the dormant-placeholder indirection above at all; a
plain terminal/agent pane restore has none of it — it re-spawns directly,
immediately, with nothing "captured, then reapplied later" to hook a
preference onto. Threading that through would mean adding an embeds field
to every other `RestoreAction` variant (`spawn-terminal`/`fresh-agent`/
`dormant-agent`/…) and a matching apply-call at each of `main.ts`'s several
live-pane-creation sites — real additional plumbing, a materially bigger
lift than widening `RESTORABLE_EMBED_KINDS` was. So docking `git`/`editor`/
`issues` on a PLAIN pane is fully functional for the pane's live lifetime
(including moving between tabs) but does not survive a full app quit +
relaunch, regardless of which of the three it is — that limitation is
about the PANE, not the view. Docking any of the three on an ORCHESTRATOR
pane specifically DOES now survive, except `issues` — which was left out
of `RESTORABLE_EMBED_KINDS` simply because this round's ask was `git` and
`editor`, not because there's a technical reason it couldn't join them:
the exact same one-line widening that added `git`/`editor` would add it
too, whenever that's actually wanted.

A restored `git`/`editor` never restores its OWN content (which commit was
selected; which file was open) — only the DOCK preference (side + share),
identical to how a restored `group`/`tasks`/`audit` never restores its own
scroll position or filter either. For the editor specifically: this is the
SAME rule #217's own `openPathRel` doc comment already states for the
content-pane case ("the buffer is deliberately never persisted... the
file is re-read from disk on restore") — nothing new needed here, the
dockable overlay's restore simply never carries a file path at all, so a
restored docked editor comes back the same way it does after ANY session
restart today: root-less, showing "Pick a folder to browse," exactly
where a freshly-docked one starts.

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

**Restart survival (`tasks`/`decisions`/`audit`/`group`/`git`/`editor`/
`timeline`, on an ORCHESTRATOR pane specifically — not `issues`, and not any
of the seven on a plain terminal/agent pane):**

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
13. Dock git RIGHT and the file editor BOTTOM on an orchestrator pane (#361
    scope increase), quit and relaunch, Resume — both should come back
    docked on their original edges, same as step 12. The editor should come
    back with NO file open (root-less, "Pick a folder to browse") — it
    never persists which file was open, matching the #217 content-pane
    editor's own documented behavior; only the dock side/share round-trips.
14. Embed git, the file editor, or issues on a PLAIN terminal or agent pane
    (not an orchestrator pane), then quit and relaunch — confirm all three
    come back as the floating overlay (the documented, deliberate scope
    boundary above: no dormant-placeholder hook exists for a non-orch pane
    at all), not docked and not missing entirely.
15. Embed issues on an ORCHESTRATOR pane specifically, quit and relaunch —
    confirm it ALSO comes back as the floating overlay, not docked. This is
    the one case in this list that's expected to NOT survive even on an orch
    pane — `issues` was deliberately left out of `RESTORABLE_EMBED_KINDS`
    this round (see the design note's persistence section for why that's a
    scoping choice, not a technical limitation).

**Overlay toggle vs. dock (#361 user-demo finding):**

16. Dock a view (any of the eight) to any edge and leave it open. Click the
    PANE HEADER's own toggle button for that view (not its embed button —
    the plain header icon, e.g. the git/task-board/audit/group/issues/
    file-editor button) — nothing should happen: no black/empty area where
    the panel was, the button should already read as visibly
    dimmer/disabled, and hovering it should show a tooltip like "…is docked
    — un-embed it (its side menu) to use this". This is the exact bug:
    before the fix, this click closed/reparented the view but left the
    slot's own panel+divider sitting there empty.
17. Same setup — press the matching KEYBINDING (Alt+G/T/A/O/I/F) instead of
    clicking the button. A toast should appear ("… is docked — un-embed it
    …") and nothing else should change. Repeat via the view's own internal
    ✕ button (should be visibly disabled with a matching tooltip) and, for
    git, the pane header's branch-name label (not a real button, so no
    disabled state — but clicking it should also just toast, not close
    anything). For the file editor specifically: make the buffer DIRTY
    first, THEN press Escape or click the (disabled) ✕ — the discard-confirm
    dialog must NOT appear at all; it should behave exactly like every
    other docked view's toggle (toast/no-op), never popping "discard unsaved
    changes?" for a click that wouldn't actually close anything (#361
    scope-increase finding — `shouldConfirmDiscardBeforeClose`).
18. Open the embed menu and click **Un-embed** — the view returns to the
    floating overlay, and the SAME header button / keybinding / internal ✕
    that just no-op'd should now close it normally; re-toggling should
    reopen it normally. Confirm this for at least two different views, not
    just one — the fix is centralized, but worth confirming it isn't
    accidentally view-specific. For the file editor: un-embed it WITH a
    dirty buffer, then close it via Escape/✕ — the discard-confirm dialog
    SHOULD appear now (undocked behaves exactly as it always has), and
    declining should leave it open with the edits intact.

**Audit log performance while docked (#361 user-demo finding):**

19. On a group with a large audit log (a long-running session, or grep
    `audit.jsonl`'s line count directly to confirm it's in the hundreds or
    thousands), dock the audit log to any edge. Drag its divider hard, back
    and forth, repeatedly — it should feel as smooth as dragging any other
    docked view's divider, with no visible stutter tied to the log's size.
    Compare against dragging the SAME divider before this fix (or against
    another, smaller view's divider) if you want a direct before/after
    feel.
20. While docked and open, scroll the audit list to the very top — a small
    italic line ("N earlier entries — scroll up to load more") should
    appear, and scrolling further should load more of the backlog, keeping
    your place (the view should NOT jump back to the top or bottom when
    older entries are prepended).
21. Turn on live-follow (▶ follow) while scrolled up reading old entries —
    new entries arriving must NOT yank your scroll position back to the
    tail. Scroll back down to the bottom yourself — follow should resume
    normal live-tailing (auto-scroll to newest) from there, same as before
    this change.
22. Drag the divider while the audit log is actively following (new entries
    arriving during the drag) — should stay smooth; no correctness issue
    expected (follow's own poll is on a 1.5s timer, independent of the
    drag), but worth a look since it's the one path that combines both
    fixes.
23. With the audit log (or any docked view) open, start dragging its
    divider, then Alt-Tab away to a different application WITHOUT releasing
    the mouse button (or otherwise blur the loomux window mid-drag) — then
    Alt-Tab back. The list must be fully visible and interactive, not a
    blank/empty panel (the `.resizing` class — `content-visibility: hidden`
    — must have been removed on the window `blur`, not left stranded
    waiting for a `mouseup` the window never receives with focus
    elsewhere). Repeat for a plain grid split's divider — dragging it and
    Alt-Tabbing away should leave the `dragging` highlight class cleared
    too (cosmetic there, but confirms the shared fix covers grid.ts as
    well). `test/dragsession.test.ts` pins the underlying exactly-once/
    listeners-removed guarantee against a fake event target; this step is
    the one real-DOM confirmation nothing in that pure suite can give.

**Right-dock expand-left smoothness (#361 user-demo finding — no automated
test possible; this environment has no live-app measurement tool):**

24. Dock any view RIGHT. Drag its divider LEFT (growing the panel,
    shrinking the terminal), then RIGHT (shrinking the panel back) — both
    directions should feel exactly as smooth as dragging a LEFT-docked or
    BOTTOM-docked view's divider in either direction. This is the specific
    gesture reported laggy before `embedTermWrapEl`. If it's still
    noticeably worse than left/bottom, the confirmed structural asymmetry
    this fix removed wasn't the only (or the real) cause — flag for a
    follow-up with an actual profiler trace, which this session's tooling
    couldn't produce (see the design note's "Right-dock expand-left lag"
    section for exactly what was and wasn't verifiable here).

**Git view and file editor as newly-dockable views (#361 scope increase):**

25. Dock git to any edge — should behave identically to every other view in
    every respect already covered above (side-picker menu, single-occupant
    swap, divider math, restart survival on an orch pane). Nothing new to
    verify here beyond confirming it: git was already wired into this
    engine from an earlier round.
26. Dock the file editor to any edge. Open a file, edit it (don't save) so
    the dirty dot appears — the pane should resize and the divider should
    drag exactly like any other docked view's, with no PTY-resize
    irregularity tied to having an unsaved buffer open.
27. With the SAME dirty, docked editor: kill the PANE itself (its ✕, or a
    dock-chip ✕ if minimized, or Ctrl+Shift+W) — the #219 discard-confirm
    dialog MUST appear (this is the real data-loss risk the toggle-no-op
    change above deliberately does NOT touch — see the design note's "#219
    interaction" section). Decline it — the pane must stay open, edits
    intact, still docked. Confirm it — the pane (and the docked editor with
    it) closes.
28. Same setup once more: quit the WHOLE APP with a dirty, docked editor
    open somewhere — the app-quit guard's consolidated dirty-buffer prompt
    must list it (labelled the same way an Alt+F overlay's buffer always
    has been — "pane" vs "overlay" labelling is unchanged by docking).
    Cancelling the quit must leave the buffer exactly as it was.
29. Open a SECOND file while the editor is docked (via the tree, or "Go to
    file") — should behave identically to overlay mode: if the current
    buffer is dirty, the SAME #219 discard-confirm gates the switch (this
    path was never routed through the toggle at all, so it was never at
    risk from this round's toggle-no-op change — confirming that is the
    point of this step).
