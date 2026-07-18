# Embedded panels: the task board beside the terminal, not over it (#361)

The ask (#361): dock UI items like the task board or the group lifecycle panel
*beside or below* the agent CLI, resizing the terminal so both the full CLI
and the panel are fully visible at once. Today every one of these views —
git, GitHub issues, the task board, the audit log, the group lifecycle panel
— is a **floating overlay**: it covers part of the terminal rather than
sharing space with it, precisely because CLAUDE.md's hard constraint 1 says
*never resize the PTY for a UI feature*. This note works out the boundary
that constraint actually draws, and lands the task board (the concrete ask)
on the legitimate side of it. The group lifecycle panel is not implemented
here — see *Scope* below.

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
docking the task board is the human saying "give this its own space beside
the terminal," exactly the sentence a split already answers. So:

- **Dock / un-embed and the mode toggle are ONE discrete resize event each**,
  fired from an explicit click — never from a resize, a refresh, a repaint,
  or any other passive trigger.
- **A divider drag between the terminal and the panel resizes the PTY the
  SAME way a grid split's own divider already does** — see *Divider
  mechanics* below for exactly what that means, because "one resize on
  release" turns out not to be it.
- **The embedded panel's own internal changes never reach the PTY.** Once
  open, refreshing the task list, expanding a row's notes, or typing in the
  add-task field only repaints inside `.pane-embed-panel` — nothing about
  those touches `termEl`'s box.
- **The floating overlay is untouched and stays available.** Embedding is an
  alternative presentation the human opts into, not a replacement; every
  overlay's no-resize mechanics (`overlaysize.ts`, `Pane.overlayClamp`,
  `updateTermShift`) are byte-for-byte what they were before this note.

If a future embedded view ever needed continuous resizing driven by
something OTHER than a direct drag or an explicit toggle, that would cross
back onto the wrong side of the line above and this design would need
revisiting — it has not come up for the task board.

## Divider mechanics: matched to what splits actually do, not to a guess

The task brief that shaped this work assumed grid splits resize the PTY
**once, on drag release**. That turns out to be inaccurate, and worth
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
audit — there is one, and both dividers drive it identically. What IS fired
once per drag (mirroring `grid.ts`'s own `up()` handler) is **persistence**:
the settled fraction is written to `tasksEmbedFrac` and reported via
`onRecordChanged` only on `mouseup`, never per `mousemove` — a write storm
into tabs.json would be its own problem, independent of the PTY question.

`embedsplit.ts` is pure and DOM-free (`test/embedsplit.test.ts`), and
mirrors `grid.ts`'s inline divider math on purpose: before/after sizes and
flex-grow weights, a delta clamped so neither side crosses its floor, then
redistributed proportionally so the pair's total flex-grow is preserved.
Reusing that shape is the point, not an incidental convenience.

**Why the floors are duplicated, not imported.** The panel-side and
terminal-side minimums (`EMBED_MIN_PANEL_PX` / `EMBED_MIN_TERM_PX`, 180 /
100) are deliberately the same numbers as the floating overlay's own
`OVERLAY_MIN_H` / `TERM_RESERVE_H` (`overlaysize.ts`) — same reasoning (a
visible terminal strip; the panel's own header/list/footer chrome doesn't
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

## Naming: "dock" was already taken

The issue (and the human) call this "docking." The codebase already has a
feature called **the dock**: `grid.minimize()`/`grid.restore()` park a whole
pane out of the split tree into a strip of `.dock-chip` restore buttons — a
taskbar, not a split. Calling the new feature "dock" too would collide with
that vocabulary in the UI copy (a "Dock task board" button living a few
pixels from "Minimize to the dock") and in the code (`dockSyncListener`,
`renderDock`, `.dock-chip` already mean the OTHER thing).

So internally and in the button copy this is **embed**: `.pane-embed-host`,
`.pane-embed-panel`, `.pane-embed-divider`, `Pane.toggleTasksEmbedMode()`,
the persisted `taskEmbed` field, and a header button labeled "Embed beside
the terminal" / "Un-embed — back to a floating overlay." This satisfies the
issue by behavior (the task board shares space with the terminal instead of
covering it) without overloading a word the product already uses for
something else.

## What got embedded: the board only

Two views were named in the issue: the **task board** (the concrete ask,
implemented here) and the **group lifecycle panel** (`GroupView` — the
"lifecycle status" the issue means; it is the panel behind the group-view
overlay toggle on orchestrator panes). Wiring a second view through the same
host mechanism is mechanically similar but not free: `GroupView`'s overlay
carries its own floor calculation (`groupFloor()`, a measured-chrome minimum
so its footer controls can't clip) and its own reclamp-on-resize path
(`reclampGroupOverlay`), neither of which the task board's overlay needed.
Doing that correctly, with its own tests, is real additional scope — so this
PR stops at the board and leaves the extension point named rather than
half-wiring a second view.

**The extension point, concretely:** `Pane.ensureEmbedHost()`,
`placeTasksViewInCurrentHost()`, `openTasksView()`/`closeTasksView()`, and
`embedsplit.ts` are not task-board-specific in their geometry — they operate
on "the terminal" and "whatever view element is currently the embedded
panel's child." A second embedded view needs its own host div (a pane can
only embed one panel at a time in this PR — there is no stacking or
multi-panel layout), its own persisted field alongside `taskEmbed`, and its
own floor if its chrome isn't a fixed height. `TasksView` itself needed no
change to become embeddable beyond the `onToggleEmbed` callback and
`setEmbedded()` — it was never coupled to overlay positioning (unlike, say,
`FileEditView`'s pre-#217 overlay/pane split), so there was nothing to
extract.

## Reuse, not a fork

`TasksView` is the same class, the same instance, in both modes.
`Pane.placeTasksViewInCurrentHost()` moves `tasksView.el` between the
overlay host and the embed panel with a plain `appendChild` — which detaches
it from wherever it currently lives — so there is exactly one `TasksView`
per pane regardless of how many times the human switches modes, and its
internal state (the `orch-tasks-changed` subscription, the expanded/selected
row sets, an in-flight edit) survives the move untouched. The overlay host
(`.git-overlay`) and the embed host (`.pane-embed-host` /
`.pane-embed-panel`) are both created once, lazily, the first time the board
opens in ANY mode, and left in the DOM afterward — hidden via the app-wide
`[hidden] { display: none !important; }` rule rather than
created/destroyed — the same reuse idiom every other overlay in `pane.ts`
already uses for itself.

## Persisted shape

`PersistedPane.taskEmbed: number | null` (`tabstore.ts`) — `null` means the
board opens as the floating overlay (the pre-#361 default, and the only
option for every pane kind besides `orch`); a number is the embedded panel's
share of the split, in the same units a split node's own `weight` already
persists (a flex-grow ratio, not a pixel size). Additive: an old tabs.json
simply never carries the key, and `decodePane` treats a missing or malformed
value as `null` — no schema bump, the same pattern `role` and the files/git
root used when they were added.

**It survives a whole-group resume, not just an ordinary pane recreation.**
Orchestration panes are never auto-resumed on app restart
(`panerestore.ts`'s `dormant-group` — the human clicks Resume, deliberately,
to avoid a credit/process-storm on every boot). That means `taskEmbed` has
to ride the SAME path `role` and `sessionId` already ride to survive a real
app restart: captured into the dormant placeholder's record
(`main.ts`'s `case "dormant-group"`), read back off it in
`resumeDormantGroup`, and matched — by session id, the same key
`planGroupResume` itself matches on — to the pane that comes back once
`resumeOrchSession` actually resolves (`Pane.restoreTaskEmbed`). This is the
one place today a captured per-pane UI preference is threaded all the way
through a whole-group resume; every other overlay's open/closed state has
never needed to be, because none of them was ever meant to be a station kept
open across a restart. If a future preference needs the same treatment, this
is the path to copy — `embedBySession` in `main.ts` is deliberately built as
a plain `sessionId → value` map rather than folded into the resume plan
itself (`planGroupResume`'s `GroupMember` intentionally stays
`{sessionId, role}` — a scheduling/matching plan, not a preference bag).

## Manual validation (the human)

The production app can't be launched from this session (#394) — these are
the steps for the human to run:

1. Open an orchestrator pane, `Alt+T` to open the task board as the
   overlay (unchanged). Click the new embed button in its header (⬒) — the
   board should move to sit BELOW the terminal, both fully visible, with a
   thin draggable divider between them, and the button should read as
   pressed (⬓, accent-colored).
2. Type in the terminal — the CLI should still be fully usable, at its
   (now shorter) size, with no repaint storm in scrollback from the
   embed itself.
3. Drag the divider — the terminal and the panel should resize smoothly,
   respecting a minimum height on each side (dragging hard in either
   direction should stop short of collapsing the other pane's chrome).
4. Click ⬓ again — the board should return to the floating overlay,
   covering the terminal's top portion as it always has.
5. Re-embed, close the board (`Alt+T` or its ✕) — it should close taking
   the panel with it; the terminal regains full height. `Alt+T` again
   should reopen it EMBEDDED (the mode preference persisted across
   close/reopen within the session).
6. Quit and relaunch loomux with that group's tab still around: it should
   restore dormant (unchanged). Click **Resume**: the task board should
   come back already embedded, at roughly the size it was left at.
7. Confirm the floating overlay still works normally for git / issues /
   audit / group on the same pane, including while the task board is
   embedded — the two should coexist without fighting for space or
   closing each other.
