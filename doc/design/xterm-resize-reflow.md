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
place. `@xterm/xterm` 5.x never shipped this option -- there is no smaller
upgrade that gets it; 6.0.0 is the first release that has it, so the v6 bump
is taken **for `reflowCursorLine`**, not because v6 itself is "the fix" for
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

### Known limitation: narrowing resizes

`test/xterm-reflow.test.ts` pins both the mitigation (a **widening** resize
puts the echo back on the geometrically correct row) and a known gap: the
same config replayed with a **narrowing** resize still produces a garbled
layout (the echo splits across two rows around a stray run of blank
padding, worse than merely landing on the wrong row). Watch for this by eye
particularly when *shrinking* a pane mid-stream; growing a pane is the
better-covered direction.

## What loomux can still do (follow-up, not this PR)

The investigation's own loomux-side aggravators are now the more valuable
lever, precisely because there is no complete upstream fix to lean on:

- **Resize-storm coalescing** -- a divider drag issues a `ResizePseudoConsole`
  roughly every 16ms for the whole drag (`pane.ts`'s fit debounce,
  `grid.ts`'s `makeDivider`); coalescing to one resize on drag-end shrinks
  the window in which xterm's and conpty's row models can diverge.
- **Serializing resize behind the write queue** -- `fit.fit()` applies
  synchronously while `term.write()` is parsed asynchronously; deferring the
  resize until pending writes are parsed prevents bytes received under the
  old geometry from being parsed under the new one.
- **Not latching `sentSize` before the resize IPC resolves** (`pane.ts`) --
  a single failed `ResizePseudoConsole` currently leaves xterm out of sync
  with conpty until the next size change.

These are scoped to a separate PR (loomux issue tracker: the hardening
follow-up referenced from #430).

## Changelog watch

Re-check microsoft/vscode#224488 (or its successor) periodically: if VS
Code's conpty.dll sideload effort lands a real fix and re-enables it, the
same fix likely applies here and the mitigation above may become
unnecessary or replaceable.
