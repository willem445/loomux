# Weft

A sleek terminal multiplexer for AI agent management.

In weaving, the *weft* is the thread drawn through the fixed warp to make
the fabric — here, terminal panes woven into a matrix, each one carrying an
agent (or just a shell).

Windows Terminal–class smoothness with the multiplexing features it lacks:
instant matrix splits, nameable panes, and a native session browser that
restores Claude Code and GitHub Copilot CLI sessions straight into a pane.

## Stack

- **Backend:** Rust + [Tauri 2](https://tauri.app) + [`portable-pty`](https://crates.io/crates/portable-pty)
  (WezTerm's PTY layer — real ConPTY on Windows, forkpty on macOS/Linux, so
  escape sequences, colors, and wide characters render exactly as a native
  terminal; no tmux-style re-emulation quirks)
- **Frontend:** [xterm.js](https://xtermjs.org) (the emulator VS Code uses)
  with the WebGL renderer + Unicode 11 addon, vanilla TypeScript, Vite.
  No UI framework.

## Run

```sh
npm install        # once
npm run tauri dev  # develop (hot-reloads the UI)
npm run tauri build  # produce a distributable app / installer
```

## Use

| Action | Shortcut |
| --- | --- |
| Split right | `Ctrl+Shift+E` (or ◫ in a pane header) |
| Split down | `Ctrl+Shift+O` (or ⬓) |
| Close pane | `Ctrl+Shift+W` (or ✕) |
| Rename pane | `F2`, or double-click its title |
| Move focus | `Alt+←/→/↑/↓` (or click) |
| Resize panes | drag the divider between them |
| Session browser | `Ctrl+Shift+P` (or the *sessions* button) |
| Copy / paste | `Ctrl+Shift+C` / `Ctrl+Shift+V` (`Ctrl+V` also works) |

Splitting in the same direction adds a sibling column/row — repeated splits
form an even matrix instead of a lopsided staircase.

### Session browser

Scans the local machine for resumable agent sessions:

- **Claude Code** — `~/.claude/projects/*/​*.jsonl` (titled by the first
  real prompt, resumed with `claude --resume <id>`)
- **Copilot CLI** — `~/.copilot/session-state/*/workspace.yaml`
  (resumed with `copilot --resume <id>`)

Clicking a session opens a new pane in the session's original working
directory and resumes it there. The pane is auto-named from the session.

## Architecture

```
src-tauri/src/
  pty.rs        PTY lifecycle (spawn/write/resize/kill) + output streaming
  sessions.rs   agent session discovery (one scan_* fn per agent source)
  lib.rs        Tauri wiring
src/
  pty.ts        typed bridge to the backend (invoke + event bus)
  pane.ts       one terminal pane: xterm instance + header UI
  grid.ts       split-tree layout, dividers, focus navigation
  sessions.ts   session browser sidebar
  shortcuts.ts  app-level keybindings (single source of truth)
  main.ts       composition root
```

The seams for future AI features (ccmux-style agent status, notifications,
orchestration) are deliberate:

- **New agent sources**: add a `scan_*` function in `sessions.rs`.
- **New backend capabilities**: add a `#[tauri::command]` and a typed
  wrapper in `pty.ts` — the frontend never touches IPC directly elsewhere.
- **Pane awareness** (e.g. "agent is waiting for input" badges): the raw
  output stream already flows through `Pane`; hook it there or observe it
  backend-side in `pty.rs`'s reader thread.
