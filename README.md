# Loomux

A dead simple terminal multiplexer for AI agent management without all the bloat.

[![CI](https://github.com/willem445/loomux/actions/workflows/ci.yml/badge.svg)](https://github.com/willem445/loomux/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-github%20pages-blue)](https://willem445.github.io/loomux/)

*Loom* + *mux*: a loom is the frame that holds every thread in place while the
fabric is woven — here, the frame holding a matrix of terminal panes, each one
carrying an agent (or just a shell — PowerShell, Command Prompt, or Git Bash,
picked per pane in the welcome screen).

Windows Terminal–class smoothness with the multiplexing features it lacks:
instant matrix splits, nameable panes, a native session browser that restores
Claude Code and GitHub Copilot CLI sessions straight into a pane, and a built-in
**orchestrator/worker** workflow for running a fleet of AI agents you gatekeep
only at review and merge.

Every pane also carries an in-app **file editor** (`Alt+F`): a lazy file tree
with extension icons, a CodeMirror code editor with per-language highlighting,
and project-wide search-and-replace — floating over the terminal so the shell
below is never disturbed. Available everywhere, plain terminals included. See
the [design note](doc/design/fileedit.md).

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
  pty.rs            PTY lifecycle (spawn/write/resize/kill) + output streaming; per-kind Terminal shells (PowerShell/cmd/Git Bash, #194) + Git Bash discovery
  sessions.rs       agent session discovery (one scan_* fn per agent source)
  orchestration/    agent groups: registry, guardrails, MCP server, audit
  obs.rs            crash observability: panic hook, breadcrumb log, unclean-exit notice
  voice.rs          voice prompts (#58): mic capture (cpal) -> local whisper.cpp subprocess
  uistate.rs        durable UI state (project tabs #63): atomic tabs.json store
  fileedit.rs       file-editor overlay (#174): lazy tree, read/write (atomic + hash conflict), search/replace; server-side path safety
  lib.rs            Tauri wiring
src/
  pty.ts            typed bridge to the backend (invoke + event bus)
  pane.ts           one terminal pane: xterm instance + header UI
  grid.ts           split-tree layout, dividers, focus, drag/maximize/minimize
  layout.ts         pure drag-reorder geometry (unit-tested, DOM-free)
  tabs.ts           project tabs (#63): TabManager -- tab list, active tab, routing (DOM-free)
  workspace.ts      one tab = a Grid + its own dock; hide/show, GL policy, preview composite
  tabbar.ts         the tab strip: switch/close/new, rename, color, alert chips, deterministic agent counter + orchestration markers (#194), preview
  tabroute.ts       pure tab routing + preview scale/sanitizer (unit-tested, DOM-free)
  tabstore.ts       pure encode/decode + schema validation of the persisted tab set (tabs + per-tab pane layout + restore pref, #194)
  restoredecision.ts pure restore-vs-fresh-vs-ask decision for the boot splash (DOM-free, unit-tested, #194)
  panerestore.ts    pure per-pane restore policy + layout-tree -> ordered rebuild plan + agent resume-command builder (DOM-free, unit-tested, #194)
  restoresplash.ts  cold-boot "restore last session?" overlay (thin DOM over restoredecision.ts, #194)
  tabcounts.ts      pure per-tab live-agent counter + live/dormant orchestration markers (DOM-free, unit-tested, #194)
  groupresume.ts    pure whole-group resume plan: orchestrator first, delegates rejoin-or-skip (DOM-free, unit-tested, #194)
  panefit.ts        pure "hidden => no PTY resize" decision (the no-resize invariant)
  sessions.ts       session browser sidebar
  launcher.ts       in-pane welcome / pane-setup form (Agent / Orchestrator / Terminal kind picker)
  panesetup.ts      pure kind-selection + validation core for the welcome screen (DOM-free, unit-tested)
  orchestration.ts  frontend half of agent groups (panes, badges, focus)
  shortcuts.ts      app-level keybindings (single source of truth)
  fileapi.ts        typed bridge to fileedit.rs (per-feature wrapper, like git.ts)
  fileedit.ts       file-editor overlay (#174): tree + code editor + search/replace (DOM wiring)
  filetreemodel.ts  pure lazy-tree model: sort/merge/flatten (DOM-free, unit-tested)
  fileicons.ts      pure filename -> inline-SVG icon mapping (DOM-free, unit-tested)
  searchresults.ts  pure search grouping + tree-hit + replace-selection model (DOM-free, unit-tested)
  dirtystate.ts     pure conflict/close-guard decisions (DOM-free, unit-tested)
  eol.ts            pure line-ending detect/normalize/re-apply for EOL-safe dirty tracking (unit-tested)
  findwidget.ts     pure in-file-find logic: regex build + "n of m" match count (DOM-free, unit-tested)
  editorwidget.ts   swappable editor widget: lazy CodeMirror 6 (One Dark) + custom find panel + textarea fallback
  voice.ts          pure voice logic: target decision + push-to-talk state machine
  voicecontrol.ts   global single-capture controller; routes transcripts to focus
  main.ts           composition root (owns the TabManager + OrchWiring router)
```

Extension seams: new agent sources add a `scan_*` in `sessions.rs`; new backend
capabilities add a `#[tauri::command]` plus a typed wrapper in `pty.ts` — or, for
a self-contained feature, a dedicated wrapper module (`git.ts`, `gh.ts`,
`fileapi.ts`). Either way the frontend never touches Tauri IPC directly.

Requirements for the agent workflow: `claude` CLI on `PATH`; `gh` CLI
authenticated for the issue/PR/review flow.
