# Terminal cursor desync on resize (#430)

## Symptom

In a plain PowerShell pane, while output is streaming, typed input renders
inside earlier, already-populated output instead of at the live prompt. The
shell processes the input fine -- the typed echo is just painted at a stale
buffer position. Full symptom writeup and repro leads: #430's investigation
comment.

## Root cause

loomux sideloads **conpty 1.22** (`src-tauri/resources/conhost/`,
`build.rs`'s `copy_sideloaded_conhost`) via `portable-pty`, which always
creates the pseudoconsole with `PSEUDOCONSOLE_RESIZE_QUIRK`
(`portable-pty-0.9.0/src/win/psuedocon.rs`). That flag means conpty **never
repaints the buffer on resize** and trusts the terminal emulator to reflow
itself (introduced in microsoft/terminal#4741, whose own PR description names
the tradeoff: a terminal not prepared for it gets a cursor "in the wrong
position").

xterm.js's own reflow (`Buffer.ts`'s `_reflowLarger`/`_reflowSmaller`) has
**always** refused to touch the row the cursor sits on -- intentionally, on
the assumption a normal shell repaints its own prompt line on `SIGWINCH`.
Under conpty's resize-quirk mode, nothing repaints it: PSReadLine asks conpty
where the cursor is (`GetConsoleCursorInfo`) and repaints its input line at
those coordinates, which conpty computes assuming the buffer already
reflowed. Since xterm's copy of that line never moved, the repaint lands
inside stale, already-rendered output.

This is the signature of xterm.js#5319 / microsoft/terminal#18725, both
citing conpty 1.22+ plus xterm.js as the exact combination loomux runs.

## The upstream picture is worse than #430's investigation comment says

#430's investigation comment (plan-22) cited xterm.js#5321 ("Reflow on
resize using similar logic to conpty") as an upstream fix already released in
`@xterm/xterm` 6.0.0. That does not hold up:

- xterm.js#5321 landed, then was **reverted wholesale** by xterm.js#5358
  ("Revert conpty-specific reflow handling") after it bricked terminal
  buffers in VS Code (microsoft/vscode#251800). The revert explicitly
  reopened xterm.js#5319 -- loomux's exact signature issue.
- xterm.js#5319 is closed again, but the maintainer's own closing comment is
  "Let's track this in microsoft/vscode#224488 for now" -- closed for issue
  hygiene, not because the bug is fixed.
- microsoft/vscode#224488 (VS Code's own effort to sideload a newer
  conpty.dll -- **the same build loomux ships**, 1.22.250204002) is itself
  closed, with the sideload currently **disabled in VS Code Insiders**.
  User reports against that exact build (Aug/Sep 2025) describe duplicated
  and misplaced text on resize -- our bug class, still open in the project
  that tried the same fix first.
- Empirically: replaying loomux's exact `windowsPty` config through
  `@xterm/headless` 5.5.0 and a stock 6.0.0 produces **byte-identical**
  buffer state for the same wrapped-output + resize + continued-write
  sequence. The version bump alone changes nothing for this bug.

**There is no confirmed complete upstream fix as of `@xterm/xterm` 6.0.0.**

## What the upgrade actually buys

`@xterm/xterm` 6.0.0 does ship one adjacent, opt-in lever:
**`reflowCursorLine`** (xterm.js#5234, added for a different issue, #5213).
It defaults to `false` ("shells usually handle this themselves" -- exactly
the assumption conpty's resize-quirk breaks). Setting it `true` makes xterm
reflow the cursor's own line like every other line, instead of leaving it in
place. No **stable** `@xterm/xterm` 5.x release shipped this option (5.5.0's
typings have zero mentions of it; the only stable 5.x releases are 5.4.0 and
5.5.0) -- it first shipped in a `5.6.0-beta.*` prerelease (xterm.js#5295 was
filed against `5.6.0-beta.96`), which isn't a production dependency choice,
so 6.0.0 is the first **stable** release that has it and the v6 bump is
taken **for `reflowCursorLine`**, not because v6 itself is "the fix" for
#430.

`pane.ts` sets `reflowCursorLine: true` alongside `windowsPty`, gated to the
same branch (`backend.conpty_build > 0`): normal shells against a
non-quirked host already repaint their own prompt line, and forcing a reflow
there risks fighting that repaint instead of doing nothing.

**This is a measurable mitigation, not a confirmed complete fix.** Its own
PR author flagged an open question in the same breath as introducing it:
"It looks like this results in the cursor being in a different place than
where it started ... maybe this behavior is expected?"
(xtermjs/xterm.js#5234).

### Precise characterization: row fixed, column not, in both directions

`test/xterm-reflow.test.ts` measures the actual cursor position after a
resize (not just where a subsequent write happens to land, which can hide
or exaggerate the real defect depending on incidental row-width alignment).
The result, confirmed on both a **widening** and a **narrowing** resize:

- **Row: corrected by `reflowCursorLine`, in both directions.** Without the
  option the row stays stale; with it, the row matches what a fully-correct
  reflow would produce.
- **Column: never corrected, in either direction.** This is not a bug
  pending an upstream fix -- xterm.js#5522 (merged, milestone 7.0.0, not
  yet released) documents it as intentional, closing xterm.js#5295
  ("Cursor is incorrectly positioned on reflow of cursor line"): *"this
  will not move the cursor position, only the line contents."* Installed
  6.0.0 predates that note (it lands in 7.0.0's typings only), but the
  behavior it documents is already what 6.0.0 does.
- **A real, partial upside on narrowing:** without `reflowCursorLine`, a
  narrowing resize doesn't reflow the cursor's line at all, and content
  beyond the old (wider) row's boundary becomes unreachable -- measured 72
  of 120 characters survive. With the option, all 120 survive, even though
  the column is still wrong. Not full correctness, but real content
  preservation loomux gets from this bump that it didn't have before.

So: don't read "the row is fixed" as "the cursor ends up right" -- only the
row does. Whether a subsequent write visibly splits across a row boundary
or merely lands at the wrong column within the right row is incidental (how
far the always-wrong column sits from the new row's width), not a
widen-vs-narrow asymmetry in the underlying defect. Watch for wrong-column
placement by eye in both directions; narrowing additionally used to lose
content outright, which this mitigation fixes.

## What loomux can still do (follow-up, not this PR)

Tracked in #432 (filed alongside this PR). Section 2 of that issue -- the
three loomux-side hardening items below -- shipped in the #432 PR itself
(this section records what landed); section 1 (the cursor-column residual)
remains nothing-to-do, upstream having declined to fix it.

- The cursor-column residual above -- nothing to *do* yet (upstream has
  declined to fix it as of milestone 7.0.0), just watch for a future change
  of position.
- The investigation's own loomux-side aggravators, now the more valuable
  lever precisely because there is no complete upstream fix to lean on --
  **all three shipped** (`pane.ts`'s `doFit`/`doResize`/`beginResizeHold`/
  `endResizeHold`, `panefit.ts`'s `shouldResizePty`, `grid.ts`'s
  `makeDivider`):
  - **Resize-storm coalescing.** A divider drag used to issue a
    `ResizePseudoConsole` roughly every 16ms for the whole drag (`pane.ts`'s
    fit debounce, `grid.ts`'s `makeDivider`). `Pane.beginResizeHold` /
    `endResizeHold` now bracket every drag that can change a pane's
    `termEl` size -- `grid.ts`'s split divider (held across every pane in
    the grid, since a nested split's drag can resize leaves the divider
    doesn't directly touch) and `pane.ts`'s own embed-slot divider (held on
    just that pane). While held, `doFit`/`doResize` still fit xterm's own
    buffer on every debounced tick, so the terminal renders at the right
    size throughout the drag, but withhold the PTY resize; the last
    matching `endResizeHold` forces an immediate fit, so the settled size
    reaches the PTY right at drag-end (mouseup, or the window
    blur/Escape paths `dragsession.ts` already treats as an end) instead of
    once per animation frame for the whole drag. The overlay height divider
    (`makeOverlayDivider`) was never part of this storm -- it floats over
    the terminal and never touches `termEl` (CLAUDE.md constraint 1) -- so
    it doesn't hold.
  - **Serializing resize behind the write queue.** `fit.fit()` used to run
    synchronously inside the debounce callback while `term.write()` is
    parsed asynchronously (xterm's own `WriteBuffer`), so bytes the PTY
    already sent under the old geometry could still be unparsed and get
    interpreted under the new one. `doFit` now defers the geometry change
    (`doResize`, which calls `fit.fit()` and decides on the PTY resize)
    behind `term.write("", () => this.doResize())` -- the callback only
    runs once every write already queued ahead of it has been parsed.
    Geometry-independent UI (the overlay clamp, `growCompose`) stays
    outside that queue so it still reacts immediately to a layout change.
  - **Not latching `sentSize` before the resize IPC resolves.** A single
    failed `ResizePseudoConsole` (still swallowed, but no longer via a bare
    `.catch(() => {})`) used to still mark its size as sent, leaving
    xterm's idea of the PTY's geometry wrong until some unrelated later
    size change happened to paper over it. `sentSize` now only latches in
    the resolved-success branch; on failure it's left as it was, so the
    next fit tick sees the same mismatch again and retries. Because the
    latch moved behind the IPC round-trip, a second fit tick can now land
    while the first call is still in flight -- `resizePending` (paired with
    `shouldResizePty`'s new `pending` check) dedups an identical repeat of
    an outstanding call without blocking a genuinely different size that
    arrives before the first one resolves.

  The coalescing/dedup decision itself (`shouldResizePty`'s `held` and
  `pending` handling) is covered by `test/panefit.test.ts`, mirroring how
  the pre-existing hidden/same-size/no-pty skips are tested there. The
  divider-drag DOM wiring (hold begin/end pairing, the write-queue
  deferral) is hand-verified only, per this repo's convention of not
  simulating a DOM in tests -- see the #432 PR for what to check by hand
  (a real divider drag under load).

## The storm the brackets could not see (#1149)

`beginResizeHold`/`endResizeHold` above coalesce a *drag*, because a drag has
a start and an end to bracket. Nothing bracketed the other shape, and the
debounce those brackets sit on could not coalesce it either:

- **`applyFit` debounced on 16 ms**, and a `ResizeObserver` callback is
  delivered once per frame in the rendering steps -- 16.7 ms apart at 60 Hz.
  A `setTimeout` armed for 16 ms resolves from the task queue *before* the
  next frame's delivery, so the window never closed over two consecutive
  frames. A debounce narrower than the interval between the events it
  debounces is not a debounce; it is a one-frame delay.
- **`#sessions` animates its width over 240 ms** (`styles.css`) as an *in-flow
  flex item*, so opening or closing the session browser walks `#grid-area`
  through ~15 intermediate widths. Each one was a fit: an xterm buffer reflow
  over the whole scrollback, plus a `ResizePseudoConsole` -- so ~15 ConPTY
  resizes per pane per toggle, every one of them a fresh roll of the
  cursor-desync dice this note is about, and on a six-pane tab ~90 for one
  click. That is what #1149 reported as lag.

The fix is in the debounce itself rather than in another pair of brackets:
`src/resizeburst.ts`'s `planFit` waits `FIT_WINDOW_MS` (60) for the geometry
to *stop moving* and fits once at the settled size, with a ceiling of
`FIT_MAX_WAIT_MS` (400) so a gesture that never settles -- a window-edge drag
-- still reflows periodically instead of freezing at a stale size. The two
constants are coupled on purpose: the ceiling must clear the longest animated
transition plus one window, because a ceiling that fired mid-transition would
put a ConPTY resize at an *intermediate* geometry, which is the repaint the
coalescer exists to remove. `test/resizeburst.test.ts` pins that inequality,
so a future transition longer than 240 ms fails there with the reason rather
than quietly costing an extra resize.

Being in the debounce is the point. A bracket covers the gestures somebody
remembered to bracket; this covers every consumer of the path -- the
transition, equalize, autosize, a native split, the window's own resize, and
the side-dock autosize (#1150), which was written afterwards and inherited it
for its own toggle without adding a line to the resize path. Not for every
gesture it has: the dock also re-widths itself when the ROOM around it changes,
and a room that is itself animating re-targets the dock's 240 ms ease, so the
composite burst can outlast the ceiling -- one panel's transition driving
another's. That case is open as #1203, and the isolation this section's pin
assumes (each panel's transition checked against the ceiling on its own) is
exactly what makes it invisible to `test/resizeburst.test.ts`.

### Where it schedules MORE, and why that is the trade

It would be convenient to say the coalescer only ever *removes* resizes. It
does not, and the exception is worth stating precisely because it is sharp.
Two conditions, and it needs both:

- the burst **outlasts** `FIT_MAX_WAIT_MS`, so a ceiling fit fires inside it;
- the `ResizeObserver` gap is **under 16 ms** -- a display above 62.5 Hz --
  where the old window was *wider* than the frame interval and so collapsed a
  burst of any length into a single trailing fit.

Cross both and this schedules more: a 2 s window-edge drag is 1 fit today on a
144 Hz display and 5 here. Everywhere else it schedules fewer or the same, and
for every burst *shorter* than the ceiling -- which is every animated
transition in the app, and all of #1149 -- exactly one against one-per-frame.

What is being given up is not a property anyone chose. On a high-refresh
display today, a gesture with no settled geometry leaves the terminal **frozen
at its pre-gesture size for the entire gesture**, because the old debounce
never fired at all; at 60 Hz the identical code reflowed ~60 times a second.
Those are two faces of the same accident -- where the frame gap happened to
fall relative to a 16 ms window -- and the ceiling replaces both with one
chosen cadence. `test/resizeburst.test.ts` pins the boundary in both
directions (62.5 Hz vs 63 Hz; a ceiling-length burst vs a longer one) so it
stays a measured trade rather than a sentence, and the added fits are capped
at one per ceiling.

**The alternative that would make the universal claim true, considered and not
taken.** Let a ceiling fit reflow xterm but withhold its `resizePty` until the
burst settles -- the `held` state #432 already built for divider drags. That
caps ConPTY resizes at one per burst on every display. It is not free: it puts
xterm and the child at *different* geometries for up to `FIT_MAX_WAIT_MS` at a
time, which is exactly the divergence this whole note is about under conpty's
resize quirk, and it would introduce it on the window-drag path, where no
display rate has it today. Trading a resize-count regression above 62.5 Hz for
a correctness-class regression at every rate is a product call rather than a
refactor, so it is recorded here rather than taken quietly.

What it does not change: the intermediate widths still happen. The panel
still animates, the terminals are simply not re-fitted at each step, so they
reflow once when it settles. The alternative -- making `#sessions` an overlay
that occludes rather than displaces -- would remove the resize entirely, but
it is a different product decision (it changes what opening the browser
*does* to the grid) and it would fix only `#sessions`.

The app has now gone the OTHER way on that same decision, which is worth
recording here because this note is where the cost lives. `.sidedock` was
built as exactly that overlay, and #1150 moved it into the flex row at the
human's direction: opening the dock autosizes the open panes rather than
covering them, at one coalesced resize per pane per toggle. It is a second
consumer of this policy that pays what `#sessions` pays, and it pays it
because the coalescer made the price a discrete event instead of a per-frame
storm. See doc/design/side-dock.md for the argument.

## Changelog watch (tracked in #432)

- **microsoft/vscode#224488** (or its successor) -- if VS Code's conpty.dll
  sideload effort lands a real fix and re-enables it, the same fix likely
  applies here and the mitigation above may become unnecessary or
  replaceable.
- **xterm.js#5295 / #5522** -- the cursor-column gap. Upstream has declined
  to fix it as of milestone 7.0.0 (documented as intentional, not tracked
  as a bug); re-check if that ever changes.
