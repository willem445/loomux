# Loomux

A dead simple terminal multiplexer for AI agent management without all the bloat.

[![CI](https://github.com/willem445/loomux/actions/workflows/ci.yml/badge.svg)](https://github.com/willem445/loomux/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-github%20pages-blue)](https://willem445.github.io/loomux/)

*Loom* + *mux*: a loom is the frame that holds every thread in place while the
fabric is woven — here, the frame holding a matrix of terminal panes, each one
carrying an agent (or just a shell).

Windows Terminal–class smoothness with the multiplexing features it lacks:
instant matrix splits, nameable panes, a native session browser that restores
Claude Code and GitHub Copilot CLI sessions straight into a pane, and a built-in
**orchestrator/worker** workflow for running a fleet of AI agents you gatekeep
only at review and merge.

![sample](sample.jpg)

## Install

**npm (any platform)** — if you already have Node 18+:

```sh
npx loomux-desktop            # download + launch in one shot
npm install -g loomux-desktop # then run `loomux` anytime
```

**Windows**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/willem445/loomux/main/install.ps1 | iex"
```

**macOS / Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/willem445/loomux/main/install.sh | sh
```

Or grab an installer from the [latest release](https://github.com/willem445/loomux/releases/latest).
Builds are unsigned for now — on macOS, if the app is reported as damaged, run
`xattr -cr /Applications/Loomux.app` (the install script does this for you).

## 📖 Documentation

**User docs live at → <https://willem445.github.io/loomux/>**

- [Getting started](https://willem445.github.io/loomux/getting-started) — install, first launch, first agent pane
- [Core concepts](https://willem445.github.io/loomux/core-concepts) — panes, the split grid, and the shortcut table
- [Orchestration guide](https://willem445.github.io/loomux/orchestration) — agent groups, the task board, the label workflow
- Feature pages — [git view](https://willem445.github.io/loomux/features/git-view), [GitHub issues](https://willem445.github.io/loomux/features/github-issues), [voice prompts](https://willem445.github.io/loomux/features/voice-prompts), [steering](https://willem445.github.io/loomux/features/steering)
- [Troubleshooting](https://willem445.github.io/loomux/troubleshooting) — whisper DLLs, `gh` auth, mic permission, disk

The site is built from Markdown under [`docs/`](docs/) and published on each
release by [`.github/workflows/docs.yml`](.github/workflows/docs.yml).

## Stack

- **Backend:** Rust + [Tauri 2](https://tauri.app) + [`portable-pty`](https://crates.io/crates/portable-pty)
  (WezTerm's PTY layer — real ConPTY on Windows, forkpty on macOS/Linux, so
  escape sequences, colors, and wide characters render exactly as a native
  terminal; no tmux-style re-emulation quirks)
- **Frontend:** [xterm.js](https://xtermjs.org) (the emulator VS Code uses) with
  the WebGL renderer + Unicode 11 addon, vanilla TypeScript, Vite. No UI
  framework.

The Windows installer ships one prebuilt, MIT-licensed runtime — a **modern
ConPTY host** (`conpty.dll` + `OpenConsole.exe`, committed in
`src-tauri/resources/conhost/`) for clean terminal resize. See
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). Voice input's whisper.cpp
runtime is **not** shipped — it's an opt-in download (see the
[voice prompts docs](https://willem445.github.io/loomux/features/voice-prompts)).

## Develop

```sh
npm install        # once
npm run tauri dev  # develop (hot-reloads the UI)
npm run tauri build  # produce a distributable app / installer
npm test           # unit tests (Node's built-in runner; no extra deps)
```

Backend checks (what CI gates on) run from `src-tauri/`: `cargo check --locked`
and `cargo test --locked`.

## Contributing

- **[`CLAUDE.md`](CLAUDE.md)** — the hard constraints (never resize the PTY, no
  getrandom crates on the Windows baseline, no live agent testing, …) and the
  code conventions. Read it before changing code.
- **[`doc/design/`](doc/design/)** — per-feature design notes explaining *why*
  each subsystem is built the way it is.
- **Architecture map** — the source tree and its seams:

```
src-tauri/src/
  pty.rs            PTY lifecycle (spawn/write/resize/kill) + output streaming
  sessions.rs       agent session discovery (one scan_* fn per agent source)
  orchestration/    agent groups: registry, guardrails, MCP server, audit
  obs.rs            crash observability: panic hook, breadcrumb log, unclean-exit notice
  voice.rs          voice prompts (#58): mic capture (cpal) -> local whisper.cpp subprocess
  uistate.rs        durable UI state (project tabs #63): atomic tabs.json store
  lib.rs            Tauri wiring
src/
  pty.ts            typed bridge to the backend (invoke + event bus)
  pane.ts           one terminal pane: xterm instance + header UI
  grid.ts           split-tree layout, dividers, focus, drag/maximize/minimize
  layout.ts         pure drag-reorder geometry (unit-tested, DOM-free)
  tabs.ts           project tabs (#63): TabManager -- tab list, active tab, routing (DOM-free)
  workspace.ts      one tab = a Grid + its own dock; hide/show, GL policy, preview composite
  tabbar.ts         the tab strip: switch/close/new, rename, color, alert/status chips, preview
  tabroute.ts       pure tab routing + preview scale/sanitizer (unit-tested, DOM-free)
  tabstore.ts       pure encode/decode + schema validation of the persisted tab set
  panefit.ts        pure "hidden => no PTY resize" decision (the no-resize invariant)
  sessions.ts       session browser sidebar
  launcher.ts       new-agent-pane dialog (single / multi / orchestrator)
  orchestration.ts  frontend half of agent groups (panes, badges, focus)
  shortcuts.ts      app-level keybindings (single source of truth)
  voice.ts          pure voice logic: target decision + push-to-talk state machine
  voicecontrol.ts   global single-capture controller; routes transcripts to focus
  main.ts           composition root (owns the TabManager + OrchWiring router)
```

Extension seams: new agent sources add a `scan_*` in `sessions.rs`; new backend
capabilities add a `#[tauri::command]` plus a typed wrapper in `pty.ts` (the
frontend never touches Tauri IPC directly elsewhere).

Requirements for the agent workflow: `claude` CLI on `PATH`; `gh` CLI
authenticated for the issue/PR/review flow.
