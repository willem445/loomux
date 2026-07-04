# Loomux

A dead simple terminal multiplexer for AI agent management without all the bloat.

*Loom* + *mux*: a loom is the frame that holds every thread in place while
the fabric is woven — here, the frame holding a matrix of terminal panes,
each one carrying an agent (or just a shell).

[![CI](https://github.com/willem445/loomux/actions/workflows/ci.yml/badge.svg)](https://github.com/willem445/loomux/actions/workflows/ci.yml)

Windows Terminal–class smoothness with the multiplexing features it lacks:
instant matrix splits, nameable panes, and a native session browser that
restores Claude Code and GitHub Copilot CLI sessions straight into a pane.

![sample](sample.jpg)

## Install

**npm (any platform)** — if you already have Node 18+:

```sh
npx loomux              # download + launch in one shot
npm install -g loomux   # then run `loomux` anytime
```

The `loomux` npm package is a tiny, dependency-free launcher: it fetches the
matching release asset for your platform (Windows installer, macOS `.dmg`, or
Linux `AppImage`), installs/caches it, and launches it. Pass `--reinstall` to
force a fresh download.

**Windows**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/willem445/loomux/main/install.ps1 | iex"
```

**macOS / Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/willem445/loomux/main/install.sh | sh
```

Or grab an installer from the [latest release](https://github.com/willem445/loomux/releases/latest).
Builds are unsigned for now — on macOS, if the app is reported as damaged,
run `xattr -cr /Applications/Loomux.app` (the install script does this for you).

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

## Agent orchestration

Loomux natively supports an **orchestrator / worker** pattern: a long-lived
planning agent that manages a small fleet of worker agents, each in its own
visible pane, with a reviewer agent per PR — and you only gatekeep the final
review and merge.

**Launch:** turn on *✦ agents* mode, open a new pane, and pick
**Orchestrator + workers** in the launcher. Choose the agent CLI (Claude
Code or Copilot CLI — the model dropdowns are populated by querying the
selected CLI's own help, so new models like `fable` appear automatically,
with a custom-entry escape hatch), the repository, how many idle workers
to start with, and the guardrails: max live agents, per-role models, and
permissions. Permissions are either *Auto* (Claude Code's native auto
permission mode plus pre-approved `git`/`gh` and loomux agent tools —
recommended) or *Accept edits only*; loomux never uses
`--dangerously-skip-permissions`. The launcher warns inline when the
selected agent CLI isn't installed, and an agent pane that dies with an
error stays open so you can read what happened. The launcher's
**Multiple panes** mode also spawns N independent agent panes at once (a
worktree name fans out to `name-1 … name-N`).

**How it works:** loomux hosts a local MCP server; every agent pane in a
group connects with its own identity token (`--strict-mcp-config`, so
workers see nothing else). The orchestrator plans work as GitHub issues
(labeled `agent-managed`), decides worktree-vs-branch per task by
mergeability, and delegates via tools that *type prompts into the worker's
CLI* — you see every instruction verbatim in the pane, can steer any agent
by typing yourself, and everything lands in an audit log. Workers follow the
standard flow (branch → implement → tests that test intent → docs → PR) and
report back; reviewers post `gh pr review`s. **No agent ever merges** — you
do, after your own review.

Panes are badged by role and group number (`ORCH 1` / `W 1` vs `ORCH 2` /
`W 2`) with a per-group accent color, so parallel orchestrations — even on
the same repository — pair up at a glance. Unrelated panes are fully
isolated from a group's tools.

**Task board:** the orchestrator pane has a board toggle (`Alt+T` or the
list icon) showing the group's work queue — status per item (`queued`,
`in-progress`, `review`, `pr`, `human-testing`, `done`, `blocked`), issue/PR
links, notes, and priority order. You can add, edit, annotate, reorder, and
delete tasks; the orchestrator is notified of your edits and maintains the
same board through its tools.

**Custom agent profiles:** loomux reuses the workspace's *standard* agent
and tool definitions — no loomux-specific config. Personas come from
`.github/agents/<name>.agent.md` (the same files Copilot CLI uses:
frontmatter `name`/`description`, instructions body; optional loomux
extensions `model`, `kind: reviewer`, `allow` for extra pre-approved
commands). MCP tool servers come from the repo's standard `.mcp.json` and
are available to **every** agent in the group: Copilot loads the file
natively, and for Claude loomux merges it into each agent's config (Claude
runs with `--strict-mcp-config` for group isolation, which would otherwise
skip it; the loomux identity entry can't be shadowed). The orchestrator
sees available profiles in its kickoff and spawns them with
`spawn_agent(profile: "embedded-dev", ...)` — on Claude the persona is
injected as the agent's system prompt, on Copilot via its native
`--agent <name>`. Profiles are re-read on every spawn, so edits apply to
the next agent immediately.

**Per-task sessions:** each worker is scoped to exactly one work item, and
loomux pre-assigns Claude session ids at spawn, recording them on the
roster and task board. Follow-ups on a finished task *resume* that worker's
session (same context, same workspace) instead of cold-starting a new agent
or disturbing a busy one.

**Guardrails** are enforced by loomux, not the model: a hard cap on live
agents (≤12), models pinned per role at launch, and the permission mode
fixed at group creation (native auto mode or acceptEdits — never bypass).

**Restart after loomux closes:** orchestration sessions are marked in the
session browser (`ORCH` / `W` / `REV` chips). Clicking a dead group's
orchestrator session restores the *whole* orchestration — same group id,
state, task board, and audit history, with fresh MCP identity wired into
the resumed conversation. Worker/reviewer sessions rejoin their group when
it's running. A plain `claude --resume` would come back powerless (no MCP
tools, no task board); this path never does.

**Persistence:** each group keeps durable state under
`<data dir>/loomux/orchestration/<group>/` — `state.json` (the
orchestrator's queue/plan memory, written via a tool after every change),
`audit.jsonl` (every tool call, prompt, spawn, and exit, one JSON line
each), `agents.json` (the roster: which sessions belonged to which role),
and the rendered role instructions. The group id is derived from the
repo path, so relaunching an orchestrator on the same repo resumes its
state; GitHub issues remain the source of truth for the work queue.

Requirements: `claude` CLI on PATH; `gh` CLI authenticated for the
issue/PR/review workflow.

## Architecture

```
src-tauri/src/
  pty.rs            PTY lifecycle (spawn/write/resize/kill) + output streaming
  sessions.rs       agent session discovery (one scan_* fn per agent source)
  orchestration/    agent groups: registry, guardrails, MCP server, audit
  lib.rs            Tauri wiring
src/
  pty.ts            typed bridge to the backend (invoke + event bus)
  pane.ts           one terminal pane: xterm instance + header UI
  grid.ts           split-tree layout, dividers, focus navigation
  sessions.ts       session browser sidebar
  launcher.ts       new-agent-pane dialog (single / multi / orchestrator)
  orchestration.ts  frontend half of agent groups (panes, badges, focus)
  shortcuts.ts      app-level keybindings (single source of truth)
  main.ts           composition root
```

The seams for future AI features (ccmux-style agent status, notifications,
orchestration) are deliberate:

- **New agent sources**: add a `scan_*` function in `sessions.rs`.
- **New backend capabilities**: add a `#[tauri::command]` and a typed
  wrapper in `pty.ts` — the frontend never touches IPC directly elsewhere.
- **Pane awareness** (e.g. "agent is waiting for input" badges): the raw
  output stream already flows through `Pane`; hook it there or observe it
  backend-side in `pty.rs`'s reader thread.
