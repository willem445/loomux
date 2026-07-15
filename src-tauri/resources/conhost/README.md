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

Both files must sit together; the upstream MIT `LICENSE` in this directory
covers them — see Provenance below for where it came from.

If the files are missing, everything still works — loomux just falls back to
the inbox conhost with its resize repaints.

## Provenance

The `conpty.dll` and `OpenConsole.exe` currently in this directory (win10
x64) are the exact bytes from wezterm commit
[`4accc376f341`](https://github.com/wezterm/wezterm/commit/4accc376f3411f2cbf4f92ca46f79f7bc47688a1)
("update bundled conpty build", 2025-02-08), which took them from the
`Microsoft.Windows.Console.ConPTY` NuGet package built by the
microsoft/terminal project's release pipeline, version `1.22.250204002`. That
version string is embedded in both binaries' PE `VersionInfo`
(`ProductName: Windows Terminal`, `ProductVersion: 1.22.250204002`) and can be
checked with `(Get-Item conpty.dll).VersionInfo` in PowerShell — verify it
still matches this note if the files are ever replaced.

`LICENSE` in this directory is microsoft/terminal's upstream MIT license text
(https://github.com/microsoft/terminal/blob/main/LICENSE), which the
`resources/conhost/*` bundle glob in `tauri.conf.json` picks up and ships
alongside the binaries in every installer automatically — no separate wiring
needed. See the root `THIRD_PARTY_NOTICES.md` for the project-wide summary.
