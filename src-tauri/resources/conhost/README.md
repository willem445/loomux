# Sideloaded ConPTY host

Drop `conpty.dll` and `OpenConsole.exe` (x64) into this directory to replace
the inbox Windows conhost for loomux terminals.

## Why

The inbox ConPTY on Windows 10 (frozen on the 19041 console codebase)
repaints the entire screen whenever a PTY is resized. Every pane split,
divider drag, or window resize therefore pushes a duplicate frame of any
running TUI (Claude Code, etc.) into scrollback. A modern conhost — built
from [microsoft/terminal](https://github.com/microsoft/terminal), MIT
licensed — honors the `PSEUDOCONSOLE_RESIZE_QUIRK` flag that portable-pty
already passes, and emits nothing on resize; xterm.js reflows the buffer
itself.

## How it's wired

- portable-pty prefers a `conpty.dll` found next to the executable over
  kernel32's ConPTY; that DLL launches the adjacent `OpenConsole.exe` as the
  console host.
- `build.rs` copies both files from here to the cargo target dir so `tauri
  dev` and local builds pick them up.
- `tauri.conf.json` bundles them into the installed app directory.
- The frontend queries `pty_backend_info` and sets xterm's `windowsPty`
  option to match (modern build → xterm keeps its own reflow; inbox →
  heuristics for the old repaint behavior).

## Where to get the binaries

Either build OpenConsole from microsoft/terminal, or take the pair that
WezTerm vendors (also built from microsoft/terminal):

    https://github.com/wezterm/wezterm/tree/main/assets/windows/conhost

Both files must sit together; include the upstream MIT `LICENSE` alongside
them when distributing.

If the files are missing, everything still works — loomux just falls back to
the inbox conhost with its resize repaints.
