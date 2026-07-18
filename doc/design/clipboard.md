# Design: reliable copy & paste

Status: implemented (issue #65, extended by #370).

## Problem

Two clipboard symptoms in terminal panes:

- **Copy silently fails.** An agent CLI (e.g. claude code) prints "copied!" but
  the system clipboard is unchanged.
- **Paste lags or doesn't land.** Pasting into a pane sometimes drops input or
  leaves the target app in a stuck state.

Both were root-caused before fixing.

## Copy: OSC 52 was never wired

A CLI copies to the *system* clipboard by emitting an OSC 52 escape:

```
ESC ] 52 ; <Pc> ; <Pd> BEL
```

`<Pc>` selects the clipboard (`c` = clipboard, `p` = primary, …) and `<Pd>` is
the base64 of the UTF-8 text. **xterm.js does not implement OSC 52.** It only
exposes `parser.registerOscHandler(52, …)`; with no handler registered the
sequence is parsed and discarded. So the CLI's own "copied!" message is
optimistic — nothing ever reached the OS clipboard.

The pane already registered an OSC 7 handler (cwd reporting) but none for 52.

### Fix

`src/clipboard.ts` parses the payload (`parseOsc52`, pure/DOM-free and unit
tested) and `pane.ts` registers a handler that writes the decoded text via
`writeClipboard` (async Clipboard API, with a hidden-`textarea` +
`execCommand` fallback, mirroring gitview's `copyText`).

Read requests (`ESC]52;c;?BEL`) are **ignored on purpose**: servicing one would
leak the clipboard to any process that asks and require writing a reply back
into the PTY. We only ever *write*.

Two hardening guards (review of #68):

- **Size cap before decode.** `parseOsc52` rejects a payload longer than
  `OSC52_MAX_B64_LEN` (1 MiB of base64) *before* calling `atob`, so a hostile or
  buggy CLI can't make us balloon a giant string on the main thread. An oversize
  payload is distinguished from a benign ignore (`{ok:false, reason:"oversize"}`)
  so the pane can toast it.
- **No silent copy failure.** `writeClipboard` returns whether the write
  actually succeeded; the pane toasts ("Copy failed — click the pane and try
  again") when both the async API *and* the `execCommand` fallback fail. Without
  this a locked-down webview would silently no-op and reintroduce the exact
  "said copied, clipboard empty" symptom with no signal.

Why not the Tauri clipboard plugin? It pulls in `arboard`, which drags a
`getrandom` dependency (banned on this project's Windows 10 baseline — it
imports `ProcessPrng`, failing to load with `0xc0000139`) plus a large
image-clipboard tree. The frontend Clipboard API is dependency-free,
cross-platform (CI runs Linux/macOS/Windows), and matches the existing copy
path. The webview grants clipboard-write to the focused terminal, which is the
state OSC 52 fires in.

## Paste: unordered fire-and-forget IPC writes

Input flowed to the PTY as:

```ts
term.onData((data) => writePty(id, data).catch(() => {}));
```

Every `writePty` is an async Tauri `invoke` that crosses the IPC boundary as an
independent task. Firing them without awaiting lets the backend receive them
**out of order** — each command acquires the per-pty writer lock in
nondeterministic order. A keystroke can overtake a paste, or, worst case, a
bracketed-paste terminator `ESC[201~` can land *before* its body: the target
app stays in paste mode and swallows everything typed next. That is the
"paste lags / doesn't land" report.

### Fix

`src/ptywrite.ts` provides `createOrderedWriter` (pure, unit tested). It
serializes writes into a promise chain so **exactly one `invoke` is in flight
at a time** — the IPC layer therefore receives bytes in xterm's original order
and cannot reorder them. It also:

- buffers input produced before the PTY exists and flushes it in order once
  ready (subsuming the old ad-hoc `inputQueue`);
- splits very large pastes into bounded (16 KiB) chunks via `chunkForPty`, so a
  single multi-megabyte write can't stall ConPTY's small input pipe, and never
  slices a UTF-16 surrogate pair.

`pane.ts` routes all `onData` through the writer and binds it to the PTY id on
spawn.

## Testing

- `test/clipboard.test.ts` — OSC 52 parsing: ASCII + UTF-8 round-trip, empty
  `Pc`, read-request/empty/malformed rejection.
- `test/ptywrite.test.ts` — FIFO ordering when an early send resolves last,
  pre-ready buffering, failure isolation, and `chunkForPty` bounds / surrogate
  safety / exact rejoin.

The interactive halves (does the OS clipboard actually change; does a real
paste into a bracketed-paste CLI land) are covered by the manual repro matrix
in the PR.

## #370: paste was Ctrl+Shift+V-only, and read failures were swallowed

Two follow-on symptoms, both in the terminal's keydown handler (not the OSC 52
path above, which was already sound):

- **Plain `Ctrl+V` did nothing.** The handler matched only
  `ctrlKey && shiftKey && KeyV` (Windows Terminal convention — plain `Ctrl+V`
  is traditionally a shell's readline "quoted insert next char"). Muscle
  memory reaches for plain `Ctrl+V` first; getting silence read as "paste is
  broken here."
- **A blocked clipboard read was silent.**
  `navigator.clipboard.readText().then(...).catch(() => {})` — a rejected read
  (focus loss, permission, non-secure context) looked identical to a keypress
  that did nothing. The copy side already had a toast for this (#65); paste
  didn't.

### Fix

`src/pasteflow.ts` (DOM-free, unit tested) decides the gestures: `isPasteKey`
matches `Ctrl+Shift+V` unconditionally (the original binding, kept) and plain
`Ctrl+V` **only when the `pasteOnPlainCtrlV` setting allows it** (default on
— see "The plain-Ctrl+V tradeoff, and the setting" below). `isCopyKey` is
unchanged — `Ctrl+Shift+C` only, since plain `Ctrl+C` must stay SIGINT.
`isPasteKey` also refuses to match with Alt held (`e.altKey`), guarding
against a keyboard-layout collision: on layouts where AltGr (= Ctrl+Alt) + V
types a character, an unguarded plain-Ctrl+V match would eat `Ctrl+Alt+V` as
a paste instead of letting that character reach the shell.

`src/clipboard.ts` gained `readClipboard()`, the paste-side mirror of
`writeClipboard`: async Clipboard API first, a hidden-`textarea` +
`execCommand("paste")` fallback second, and an explicit `{ok: false}` only
when *both* fail — never swallowed. `pane.ts`'s `pasteFromClipboard()` surfaces
that with the same toast convention `copyToClipboard` already uses. An empty
clipboard (`ok: true, text: ""`) is not a failure — it's a legitimate no-op.

**Right-click paste**, which no other pane kind needed (a native `<textarea>`/
`<input>` gets it from the browser for free) but the terminal — a canvas — did
not: `pasteflow.ts`'s `buildTerminalMenu(hasSelection)` builds a small Copy/
Paste menu, rendered through the existing generic `contextmenu.ts` (the same
renderer panemenu.ts and filemenu.ts use). Copy is shown-but-disabled without
a selection, the same "teach, don't hide" convention filemenu.ts uses for an
inapplicable item.

### The plain-Ctrl+V tradeoff, and the setting

The first version of this fix bound plain `Ctrl+V` to paste **unconditionally**
and named only readline's quoted-insert as the cost — understating it. Review
caught the real casualty: **vim/nvim's `Ctrl+V` enters VISUAL BLOCK mode**, one
of vim's everyday, signature motions, and vim/nvim users are exactly the kind
of power user a terminal multiplexer's audience skews toward. The same holds
for any other TUI or agent CLI running in the pane that binds plain `Ctrl+V`
itself — readline's quoted-insert is the least of what an unconditional
interception costs.

The review also caught an overstated justification: "every terminal emulator
already binds plain `Ctrl+V`" is not true. Windows Terminal does, by default —
but gnome-terminal, iTerm2, kitty, and alacritty all default to
`Ctrl+Shift+V` specifically to leave plain `Ctrl+V` free for whatever is
running in the pane.

This is the exact shape of tradeoff `shortcuts.ts` already made a call on for
`Alt+V` (#155): loomux used to intercept it, discovered it was Claude Code's
own paste-image binding, and **stopped intercepting** rather than keep
stealing a key an agent pane needed. Plain `Ctrl+V` is the same failure mode —
a multiplexer-level binding shadowing a key the program *inside* the pane
wants — so it gets the same resolution in spirit: don't force the choice on
everyone, offer it.

**`src/settings.ts`** adds `pasteOnPlainCtrlV: boolean` (default `true`) as
loomux's first durable app setting, persisted the same way the tab set already
is — a sibling `settings.json` written through two new backend commands
(`load_settings`/`save_settings`, `uistate.rs`) that reuse the *exact same*
atomic-write + corrupt-quarantine primitives `load_ui_tabs`/`save_ui_tabs`
already use, not a new storage mechanism. `main.ts` loads it once at boot;
`pane.ts`'s keydown handler reads it synchronously via `settings.getSettings()`
on every keystroke (a settings object can't be threaded through
`attachCustomKeyEventHandler`'s synchronous callback any other way).

There is **no Settings/Preferences UI anywhere in loomux today** — checked
before choosing this shape, so this isn't a fallback taken to avoid building
one. The setting is config-file-only: on first run (or after a corrupt file
is quarantined) loomux seeds `settings.json` with the defaults, so the file
exists to be found and hand-edited; there is no live reload, so a change takes
effect on the next launch. `Ctrl+Shift+V` pastes unconditionally either way —
turning the setting off only returns plain `Ctrl+V` to the pane, exactly the
pre-#370 behavior for that one key.

### Testing

- `test/pasteflow.test.ts` — key matching (`Ctrl+Shift+V` always pastes;
  plain `Ctrl+V` pastes only when the setting says so; `Ctrl+Alt+V`/AltGr and
  `Ctrl+C` alone are never a paste/copy) and the menu shape (Copy disabled
  without a selection, Paste always live).
- `test/settings.test.ts` — `encodeSettings`/`decodeSettings` round-trip,
  first-run/corrupt-file `null` handling, and per-key fallback so a partial or
  future-versioned hand-edit degrades gracefully instead of losing the file.
- DOM wiring (the keydown handler, the right-click menu, the toast) and the
  settings load/seed-on-first-run are hand-validated — see the PR body for the
  manual steps.
