# Loomux

A multiplexer for AI agent management with best in class orchestration!

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

![sample](sample.jpg)

### Meets you where you are

Every rung is a complete tool on its own — climb when you're ready:

1. **Terminal multiplexer** — Windows-first GUI terminal: instant matrix
   splits, project tabs, session restore. No agents required.
2. **Agent multiplexer** — panes that know they carry an agent: alert chips
   when a CLI needs you, resume Claude Code / Copilot sessions into a pane.
3. **Agent orchestration, native** — a planning agent delegates GitHub issues
   to worker and reviewer panes. Every prompt visible, every action audited,
   guardrails host-enforced, no agent ever merges.
4. **Custom agent workflows** — commit `.loomux/workflow.yml` and your repo
   declares its own roster and merge gate: five focused reviewers, five
   prompts, five models.

Plus a git view, file editor, file explorer, and voice prompts — one
keystroke away on any rung, never disturbing the shell underneath.

### Why loomux over…

- **tmux / zellij / [herdr](https://github.com/ogulcancelik/herdr)** — they
  stop at rungs 1–2. herdr multiplexes your agents; loomux manages your
  agents' work.
- **Prompt-layer orchestrators
  ([superpowers](https://github.com/obra/superpowers),
  [gstack](https://github.com/garrytan/gstack),
  [oh-my-claudecode](https://github.com/yeachan-heo/oh-my-claudecode),
  [gsd-pi](https://github.com/open-gsd/gsd-pi))** — pipelines and review
  gates written as prompts *inside* one agent CLI, which an agent can talk
  its way past. Loomux enforces from outside the process: a merge gate that
  mechanically refuses, hard token-budget stops, consent for repo-authored
  config, whole-group restart resume. Complementary, not competing — install
  them inside a worker's pane.
- **IDE-shaped agent platforms** — loomux is still a terminal: lightweight,
  native, opens your IDE instead of embedding one.
- **Unattended agent fleets** — loomux picks trust over throughput: watch and
  steer any agent mid-task, and the human keeps the merge button.

### Pane kinds

Every pane starts on the welcome screen and declares what it becomes — there is
no global mode:

| Kind | What it is |
| --- | --- |
| **Agent** | A coding-agent CLI (Claude, Copilot, or a custom command), optionally fanned out to *N* panes each in its own git worktree. |
| **Orchestrator + workers** | An orchestrator plus idle workers in their own project tab, with guardrails. |
| **Terminal** | A plain shell: PowerShell, Command Prompt, or Git Bash. |
| **File explorer** | A native-style **file manager** rooted at a folder you pick — no terminal underneath, no process, ever. |
| **File editor** | The file tree + code editor above, as a **pane** rather than an overlay, rooted at a folder you pick. |
| **Git** | The git view — graph, status, diffs, staging, worktree switching — as a **pane**, over a repo you pick. |

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
- [Autonomous & supervised modes](https://willem445.github.io/loomux/autonomous-mode) — idle-tick autonomy, token budget, auto-merge/release, dangerous mode
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
  fileedit.rs       file-editor overlay (#174): lazy tree, read/write (atomic + hash conflict), streaming gitignore-aware search/replace (#207) + path-only name enumeration (#214); server-side path safety
  filemgr.rs        file-MANAGER pane (#214): list, new file/folder, rename, delete-to-Recycle-Bin, open-with-default-app, open-with chooser, reveal-in-OS-file-manager; reuses fileedit's path choke point. Shell APIs come from the `windows` dep we already have (ShellExecuteW + SHFileOperationW)
  filehash.rs       file hashing (#214): SHA-256/512, SHA-1, CRC-32/16/8 — streamed off-thread on a worker (never the main thread), cancellable via the #207 registry
  lib.rs            Tauri wiring
src/
  pty.ts            typed bridge to the backend (invoke + event bus)
  pane.ts           one pane: xterm instance + header UI -- or, for a CONTENT pane, a PTY-less surface: file manager (#214), file editor or git view (#217)
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
  launcher.ts       in-pane welcome / pane-setup form (Agent / Orchestrator / Terminal / File-explorer / File-editor / Git kind picker)
  panesetup.ts      pure kind-selection + validation core for the welcome screen (DOM-free, unit-tested)
  orchestration.ts  frontend half of agent groups (panes, badges, focus)
  shortcuts.ts      app-level keybindings (single source of truth)
  fileapi.ts        typed bridge to fileedit.rs (per-feature wrapper, like git.ts)
  fileedit.ts       the file editor (#174): tree + code editor + "Go to file" name search + content search/replace. Two hosts: the Alt+F overlay, and an editor PANE (#217, `embedded`) (DOM wiring)
  fileexplorer.ts   the file MANAGER a files pane hosts (#214): browse, open-with-default-app, new file/folder, rename, delete, context menu, SHA-256 column, Go to file (DOM wiring)
  fileexplorermodel.ts pure file-manager core: listing order, rooted navigation, breadcrumb, formatting, inline-edit validation, op-target binding (DOM-free, unit-tested)
  filemenu.ts       pure context-menu model: what appears, what it acts on (target bound at menu-open) (DOM-free, unit-tested)
  contextmenu.ts    generic context-menu renderer: placement, submenus, Esc/click-away (DOM wiring)
  filehashmodel.ts  pure hashing policy: auto-hash threshold, digest cache keying (path+size+mtime), formatting (DOM-free, unit-tested)
  filemgr.ts        typed bridge to filemgr.rs + filehash.rs (per-feature wrapper, like fileapi.ts)
  filematch.ts      pure file-NAME matching + ranking for "Go to file" (#214, DOM-free, unit-tested)
  modal.ts          the shared confirm/choice dialog (used by the editor and the file manager)
  filetreemodel.ts  pure lazy-tree model: sort/merge/flatten (DOM-free, unit-tested)
  fileicons.ts      pure filename -> inline-SVG icon mapping (DOM-free, unit-tested)
  searchresults.ts  pure search grouping + tree-hit + replace-selection model (DOM-free, unit-tested)
  searchsession.ts  pure streaming-search state machine: batch/cancel + result cap + enumeration-source pick (#207, DOM-free, unit-tested)
  dirtystate.ts     pure conflict/close-guard decisions -- shared by the editor's Esc/close and the editor PANE's close (#217) (DOM-free, unit-tested)
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
