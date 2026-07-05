# Design: reliable copy & paste

Status: implemented (issue #65).

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
