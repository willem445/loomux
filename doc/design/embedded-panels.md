# Embedded panels: any floating view beside the terminal, not over it (#361)

The ask (#361): dock UI items — the task board, the group lifecycle panel,
git, GitHub issues, etc. — *beside or below* the agent CLI, resizing the
terminal so both the full CLI and the panel are fully visible at once. Every
one of these views is today a **floating overlay**: it covers part of the
terminal rather than sharing space with it, precisely because CLAUDE.md's
hard constraint 1 says *never resize the PTY for a UI feature*. This note
works out the boundary that constraint actually draws, lands the task board
on the legitimate side of it, and then generalizes the same mechanism to
four more views: git, GitHub issues, the audit log, and the group lifecycle
panel. The file-editor overlay is deliberately excluded — see *What's
excluded* below.

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
embedding a view is the human saying "give this its own space beside the
terminal," exactly the sentence a split already answers. So:

- **Embed / un-embed and the swap-to-a-different-view toggle are each ONE
  discrete resize event**, fired from an explicit click — never from a
  resize, a refresh, a repaint, or any other passive trigger.
- **A divider drag between the terminal and the panel resizes the PTY the
  SAME way a grid split's own divider already does** — see *Divider
  mechanics* below for exactly what that means, because "one resize on
  release" turns out not to be it.
- **The embedded panel's own internal changes never reach the PTY.** Once
  open, refreshing a list, expanding a row, typing in a filter — all of it
  repaints inside `.pane-embed-panel` only. Verified per view below, not
  assumed.
- **The floating overlay is untouched and stays available, for every view.**
  Embedding is an alternative presentation the human opts into, not a
  replacement; every overlay's no-resize mechanics (`overlaysize.ts`,
  `Pane.overlayClamp`, `updateTermShift`) are byte-for-byte what they were
  before this note.

If a future embedded view ever needed continuous resizing driven by
something OTHER than a direct drag or an explicit toggle, that would cross
back onto the wrong side of the line above and this design would need
revisiting — it has not come up for any of the five.

## Divider mechanics: matched to what splits actually do, not to a guess

The original brief for this work assumed grid splits resize the PTY **once,
on drag release**. That turned out to be inaccurate, and it's worth
recording precisely because the correction is what makes "an embedded panel
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

The embedded panel's divider (`embedsplit.ts` + `Pane.makeEmbedDivider`) is
built to hit that exact same code path, not a bespoke one: dragging it sets
`termEl.style.flex` inline, same as a grid divider does to a pane's element,
so the *same* `ResizeObserver` → debounced `applyFit()` → same-size-skip
chain fires on the *same* schedule. There is no second resize discipline to
audit — there is one, and every divider drives it identically, regardless of
which view currently occupies the panel. What IS fired once per drag
(mirroring `grid.ts`'s own `up()` handler) is **persistence**: the settled
fraction is written to `Pane.embedFrac` and reported via `onRecordChanged`
only on `mouseup`, never per `mousemove`.

`embedsplit.ts` is pure and DOM-free (`test/embedsplit.test.ts`), and
mirrors `grid.ts`'s inline divider math on purpose: before/after sizes and
flex-grow weights, a delta clamped so neither side crosses its floor, then
redistributed proportionally so the pair's total flex-grow is preserved.
Reusing that shape is the point, not an incidental convenience. It is also
genuinely view-agnostic: it never mentions the task board (or any other
view) by name — every call site passes in whichever view's own floor
applies (see *Per-view floors*, below).

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
the OTHER thing). So this is **embed** instead: `.pane-embed-host`,
`.pane-embed-panel`, `.pane-embed-divider`, `Pane.toggleEmbedSlot()`, the
persisted `embed` field, and a header button labeled "Embed beside the
terminal" / "Un-embed — back to a floating overlay."

**Generalizing to `GitView` surfaced a second collision, in the opposite
direction.** `GitView` already had a constructor option named `embedded`
(#217: is this view hosted as a whole content PANE — no terminal at all —
rather than an overlay?). That is a *different concept* from this feature
(a view sharing space with a terminal that's still right there), and the
two are easy to conflate on the same class: `new GitView({ embedded: false,
onToggleEmbed: … })` in the constructor, followed later by
`gitView.setEmbedded(true)` as the human toggles the NEW feature, would read
as the two being connected when they aren't — the ctor flag never changes
after construction, and it stays `false` in both the overlay AND the new
embedded-panel mode (neither has a terminal to give up; only the true
content-pane hosting sets it). So every embeddable view's runtime toggle
method is named **`setPanelActive(active: boolean)`**, not `setEmbedded` —
applied uniformly to all five views (including `TasksView`, which has no
collision of its own, for one consistent interface across the set) so a
reader never has to remember which view's method means what.

## What's embeddable, and what isn't

Five views are embeddable: the **task board**, **git**, **GitHub issues**,
the **audit log**, and the **group lifecycle panel** ("lifecycle status" in
the issue — the panel behind `GroupView`'s overlay toggle). All five are
wired through one generic engine in `pane.ts` (`EmbedKind`, `EmbedEntry`,
`embedRegistry`, `openView`/`closeView`/`toggleView`/`toggleEmbedSlot`) —
see *The generic engine*, below.

**The file-editor overlay (`Alt+F`) is deliberately NOT embeddable.** It
already has a strictly better path to the same outcome: the editor
**content pane** (#217, `FileEditView`'s `embedded` mode — the very flag
this note's naming section discusses). A content pane gives the editor the
whole of its own pane — full width, a real tab-bar entry, state that
survives a session restore the normal way — where a same-pane sub-panel
would give it a narrower strip that competes with the terminal for room and
disappears the moment the pane closes. "Open in editor pane" (the file
browser's row action) is the answer to "I want to keep this editor open
beside my agent"; embedding the *overlay* version would be a strictly worse
version of a feature that already exists. The overlay stays exactly what it
was: a quick look, `Esc` to dismiss.

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
  floorPx(): number;                   // live floor, for the overlay clamp AND the embed panel
}
```

`openView`/`closeView`/`toggleView` treat every kind uniformly through this
table — there is no per-view branching left in the open/close/toggle path
itself, only in each view's own `ensureXView()` (which still differs, on
purpose: `GitView` and `FileEditView` gate on `refuseOverlay` — a content
pane has no terminal to share space with at all — while the orchestration
family gates on `orchGroup`/the header button's own `hidden`).

**Only one view may be embedded at a time.** There is a single
`Pane.embedHostEl` / `embedPanelEl` / `embedDividerEl` triple, shared by
whichever kind currently occupies it (`Pane.embedKind`) — not one host per
view. A second view's embed toggle **swaps** the slot: `toggleEmbedSlot`
closes the previous occupant outright (never demotes it back to a floating
overlay) and opens the new one embedded. A silent reopen elsewhere would be
a more surprising UX than "the slot now shows what you asked for, and the
previous occupant is closed — the same one click that opened it reopens
it." This also means embedding never needs to reserve room for more than
one panel's worth of chrome, and the divider math never has to reason about
N panels sharing a terminal.

**Per-view floors.** `EmbedEntry.floorPx()` feeds BOTH the overlay height
clamp (`Pane.overlayClamp`) and the embed-panel divider floor
(`embedDragGrow`'s `minPanelPx`) — one function, two hosts, exactly the
generalization the group panel's own pre-existing overlay floor
(`groupFloor()`, its measured fixed chrome) already modeled. Four of the
five views share the generic default (`EMBED_MIN_PANEL_PX`); the group
panel is the one with genuinely variable chrome (the suspended-budget banner
can appear/disappear), so its entry's `floorPx` calls `Pane.groupFloor()`
live, same as it always did for the overlay.

**Reclamping when a floor grows after the panel is already open.**
`GroupView.onResize` (fired when its own render adds/removes the suspended
banner) used to call `reclampGroupOverlay()` — an overlay-only, px-height
operation. It's now `Pane.reclampViewFloor("group")`, which checks whichever
host is CURRENTLY active for the group panel and reclamps that one:
overlay → the same px-height clamp as before; embedded → a divider nudge via
`embedDragGrow(…, deltaPx: 0, …, newFloor)`. Passing a zero delta still
produces a corrective nudge, because a panel already smaller than the new
floor makes `sizePanel - minPanelPx` negative in that function's own clamp
— see `embedsplit.ts`. No other view's floor changes after it opens today,
so no other view wires `onResize` at all; the hook exists on the interface
generically (any future view with variable chrome can use it) but the
callback itself is optional and only `GroupView` supplies one.

**Error recovery, generalized.** `toggleGitView` originally wrapped its
whole "open" branch in a `try`/`catch` — a `refresh()` failure mid-open
would otherwise leave the pane half-toggled (overlay showing, view not
actually ready). `openView` now wraps EVERY kind's `show()` the same way:
never leave the pane half-toggled, retract whichever host was opening, let
the error surface (global handler shows a banner). This was a no-op change
in practice for the four views that never threw on `show()` — trading a
narrower, easy-to-forget-to-copy guard for a systemic one.

## Coexistence (#361 NB-4), generalized

The original review round (rev-28) caught an asymmetry for the task board
alone: opening it embedded unconditionally closed the other floating
overlays, but the OTHER overlays only closed the board when it was in
overlay mode — an embedded board was correctly left alone by them, but not
by itself. The fix generalizes cleanly: `Pane.closeOtherOverlays(except)`
loops every `EmbedKind` and closes ONLY the ones currently showing AS AN
OVERLAY (`entry.overlayEl` not hidden, and not the pane's current
`embedKind`) — an embedded view is structurally invisible to this loop, for
every kind, not just the one that happened to be reviewed first. The
file-editor overlay (never embeddable) still participates as a plain
floating panel: it closes the other five when IT opens, and is closed by
any of the five opening as overlays, the same as before #361.

## Reuse, not a fork

Every embeddable view is the same class, the same instance, in both modes.
`Pane.openView`/`closeView` move `entry.viewEl` between the overlay host and
the pane's single embed panel with a plain `appendChild`/`insertBefore` —
which detaches an element from wherever it currently lives — so there is
exactly one instance of each view per pane regardless of how many times the
human switches modes or swaps the embed slot between views, and each view's
internal state survives the move untouched. Verified per view, not assumed:

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
  and it turns out to cover the embed-panel hosting for free, since neither
  one is the floating overlay's absolute-position-plus-fixed-height model.
  `close()` (`hide()`) explicitly dismisses any open context menu
  (`closeMenu()`) — preserved by `EmbedEntry.hide`, called on every close in
  either mode, not just the overlay's. No async work in `show()`/`hide()`
  crosses a mode switch, so #401's async command changes have nothing to
  race here.
- **`IssuesView`** — no internal ResizeObserver at all (a plain list; its
  CSS is `flex: 1`, filling whichever host it's in). `hide()` closes any
  open create-issue form or detail pane first, preserved the same way.
- **`AuditView`** — a live-follow poll timer (`followTimer`), gated by an
  explicit toggle button, not by open/close — unaffected by which host it's
  in, and already stopped by `dispose()` regardless.
- **`GroupView`** — see *Per-view floors* above for its one real piece of
  mode-aware logic (the floor). Its own poll timer (`pollTimer`, started in
  `show()`) has a pre-existing quirk worth naming without conflating it with
  this PR: `show()` is called on every open in EITHER mode (same call
  frequency as before #361) but nothing clears `pollTimer` on close, only on
  `dispose()` — a latent issue that predates this PR and isn't worsened by
  it, so it's noted here rather than fixed here.

The overlay host (`.git-overlay`, one per view — unchanged) and the pane's
single embed host (`.pane-embed-host` / `.pane-embed-panel`) are both
created lazily — the overlay the first time that view opens in ANY mode,
the shared embed host the first time ANY view is embedded — and left in the
DOM afterward, hidden via the app-wide `[hidden] { display: none
!important; }` rule rather than created/destroyed. Same reuse idiom every
overlay in `pane.ts` already used for itself.

## Persisted shape

`PersistedPane.embed: { view: PersistedEmbedView; share: number } | null`
(`tabstore.ts`) — `null` means every view opens as its floating overlay (the
pre-#361 default); a `{view, share}` record names which view is embedded and
its share of the split, in the same units a split node's own `weight`
already persists (a flex-grow ratio, not a pixel size). Additive: an old
tabs.json simply never carries the key, and `decodePane` treats a missing or
malformed value as `null` — no schema bump, the same pattern `role` and the
files/git root used when they were added.

**`PersistedEmbedView` is `"tasks" | "audit" | "group"` — a strict subset of
`pane.ts`'s own `EmbedKind`.** Only the orchestration-family views are
representable in the persisted shape at all; `git`/`issues` are never
written here. See *Why only three views survive a restart*, below, for why —
`decodePane` rejects an `embed.view` naming anything else (falls back to
`null`, pinned by `test/tabstore.test.ts`), so a hand-edited or
future-loomux-written file naming an unsupported view degrades safely
instead of producing a `Pane.restoreEmbed` call for a kind this build
doesn't know how to restore.

**Migrated from the unreleased `taskEmbed: number | null` shape.** This
field didn't ship in a release — it was introduced and renamed within the
same PR (#404 rev-28 approved the task-board-only shape; this generalization
replaced it before merge). `decodePane` still reads the legacy key leniently
(`decodeEmbed(r.embed, r.taskEmbed)`, synthesizing `{view: "tasks", share:
taskEmbed}` when only the old key is present, new key winning if both are)
— the cost of tolerating one more shape is a few lines; the cost of not is a
silently dropped preference on the next boot after a stray hand-edited or
pre-rebase tabs.json. `test/tabstore.test.ts` pins both the migration and
the new-wins-over-stale-legacy case.

**Why only three views survive a whole-group resume.** Orchestration panes
are never auto-resumed on app restart (`panerestore.ts`'s `dormant-group` —
the human clicks Resume, deliberately, to avoid a credit/process-storm on
every boot). That dormancy is what gives `tasks`/`audit`/`group` a natural
restore hook: the embed preference rides the SAME path `role` and
`sessionId` already ride — captured into the dormant placeholder's record
(`main.ts`'s `case "dormant-group"`), read back off it in
`resumeDormantGroup`, and matched — by session id, the same key
`planGroupResume` itself matches on — to the pane that comes back once
`resumeOrchSession` actually resolves (`Pane.restoreEmbed`). `git`/`issues`
are embeddable on EVERY pane kind (including a plain terminal), but a plain
terminal restore has no dormant-placeholder indirection at all — it
re-spawns directly, immediately, with nothing "captured, then reapplied
later" to hook a preference onto. Threading that through would mean adding
an embed field to every other `RestoreAction` variant
(`spawn-terminal`/`fresh-agent`/`dormant-agent`/…) and a matching apply-call
at each of `main.ts`'s several live-pane-creation sites — real additional
plumbing, not a two-line extension of what orch panes already have. So
`Pane.capture()` only ever writes `embed` when the currently-embedded kind
is orchestration-family AND the pane itself is `kind === "orch"`
(`isRestorableEmbedKind`); embedding git or issues on any pane is fully
functional for the pane's live lifetime, including moving between tabs, but
does not survive a full app quit + relaunch. This is a real, deliberate
scope boundary, not an oversight — if it needs to move, the extension point
is exactly the "captured, then reapplied once the real pane exists" pattern
`tasks`/`audit`/`group` already use, just without the dormancy step to hang
it on.

**This is the one place today a captured per-pane UI preference is threaded
through a whole-group resume** — every OTHER overlay's open/closed state has
never needed to be, because none of them was ever meant to be a station kept
open across a restart. `embedBySession` in `main.ts` is deliberately built
as a plain `sessionId → {view, share}` map rather than folded into the
resume plan itself (`planGroupResume`'s `GroupMember` intentionally stays
`{sessionId, role}` — a scheduling/matching plan, not a preference bag).

**Known gap: a respawn-fresh fallback loses the preference.** The match
above is keyed on the CAPTURED session id — the one `resumeOrchSession` was
asked to `--resume`. If that resume attempt fails at runtime (a deleted
transcript, any other resume-time CLI failure) and `shouldRespawnFresh`
(`panerestore.ts`) fires its one-shot fresh-in-place respawn, the pane ends
up carrying a NEW session id that was never in `embedBySession`. The lookup
then misses, `Pane.restoreEmbed` is never called for that member, and its
embedded view simply opens as the floating overlay next time — a silent
fallback to the pre-#361 default, not a crash or a stuck state. Accepted:
the member's *conversation* is already gone in this scenario (that is what
triggered the respawn), so losing a UI layout preference alongside it is the
smaller loss on the same bad path, and re-embedding after the fact is a
single click.

## Manual validation (the human)

The production app can't be launched from this session (#394) — these are
the steps for the human to run.

**Per-view basics (repeat for task board, git, issues, audit, and the group
lifecycle panel):**

1. Open the view as the overlay (unchanged). Click the new embed button in
   its header (⬒) — it should move to sit BELOW the terminal, both fully
   visible, with a thin draggable divider between them, and the button
   should read as pressed (⬓, accent-colored).
2. Type in the terminal — the CLI should still be fully usable, at its
   (now shorter) size, with no repaint storm in scrollback from the embed
   itself.
3. Drag the divider — the terminal and the panel should resize smoothly,
   respecting a minimum height on each side (dragging hard in either
   direction should stop short of collapsing the other pane's chrome). For
   the group lifecycle panel specifically: trigger the suspended-budget
   banner (or any state that grows its fixed chrome) while embedded — the
   divider should nudge up on its own rather than letting the footer clip.
4. Click ⬓ again — the view should return to the floating overlay.
5. Re-embed, close it (its own hotkey or ✕) — it should close taking the
   panel with it; the terminal regains full height. Reopening the SAME view
   should come back embedded (the mode preference persisted for the
   session).
6. Open a SECOND embeddable view and click ITS embed button while the first
   is still embedded — the first should close outright (not silently
   reappear as an overlay) and the second should take the slot. This is the
   swap semantics from *The generic engine*, above — it should never look
   like two panels are fighting for the same space.

**Coexistence and content panes:**

7. With one view embedded, open a DIFFERENT view as a floating overlay — the
   two should coexist without either closing the other (#361 NB-4,
   generalized to every view). Confirm the reverse too: with a floating
   overlay open, embed a different view — the floating one should stay put.
8. On a content pane (a `files`/`editor`/`git`/`workflow` pane, #214/#217):
   confirm neither the embed button nor the overlay is offered for
   git/issues/file-editor there (`refuseOverlay`, unchanged) — there is no
   terminal on a content pane to share space with.

**Restart survival (orchestration-family views only):**

9. Embed the task board (or audit, or the group panel), quit and relaunch
   loomux with that group's tab still around — it should restore dormant
   (unchanged). Click **Resume**: the embedded view should come back
   already embedded, at roughly the size it was left at. Do this on a group
   with at least one worker/reviewer pane open alongside the orchestrator —
   the dormant-shadow exclusion `findResumedPaneIndex` guards (a stale
   placeholder carrying the same captured session id as the pane actually
   being resumed) is only unit-tested against synthetic candidates; this is
   the one path that exercises it against the real grid/DOM, and with more
   than one member in flight it's the scenario most likely to surface an
   ordering assumption the synthetic test can't see.
10. Embed git or issues on a plain terminal or agent pane, then quit and
    relaunch — confirm it comes back as the floating overlay (the
    documented, deliberate scope boundary above), not embedded and not
    missing entirely.
