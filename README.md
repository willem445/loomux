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
npx loomux-desktop            # download + launch in one shot
npm install -g loomux-desktop # then run `loomux` anytime
```

The `loomux-desktop` npm package is a tiny, dependency-free launcher: it
fetches the matching release asset for your platform (Windows installer, macOS
`.dmg`, or Linux `AppImage`), installs/caches it, and launches it. Pass
`--reinstall` to force a fresh download. (The package is named `loomux-desktop`
because the bare `loomux` name on npm belongs to an unrelated tmux tool; the
command it installs is still `loomux`.)

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
| Open in editor | `Alt+E` (or the `</>` button in a pane header) |
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

### Open in editor

Loomux is a terminal, not an editor — so when you need to open files in a real
editor, the `</>` button in a pane header (or `Alt+E`) launches your editor on
that pane's current folder. The first time, you're asked for the editor
command; it's remembered after that.

- Set it to `code` (VS Code), `zed`, `subl`, or any command on your `PATH`, or
  to a full path to the editor executable.
- The workspace folder is passed as the editor's sole argument, spawned
  detached — the editor keeps running independently of loomux.
- Right-click the `</>` button any time to change the editor command.

If nothing is configured, or the editor can't be found/launched, loomux shows a
short toast explaining what went wrong.

### Git view

`Alt+G` (or the ⑂ icon in a pane header) overlays a git panel on the pane,
scoped to the repository the shell is currently in — a commit graph, a diff
preview, and the working-tree changes with staging and commit. It never
resizes the terminal underneath. Press `Esc` (or ✕) to return.

Toolbar (top-right of the graph):

| Button | Does |
| --- | --- |
| ↓ | **Pull** the current branch — fast-forward only, so it never creates a surprise merge; a diverged branch reports the conflict instead. |
| ↑ | **Push** the current branch. If it has no upstream yet, you're offered to publish it to the remote and set tracking. |
| ↻ | **Fetch** from all remotes (with prune) and refresh the view. |

Click the **branch name** in the header to switch branches — the menu lists
every local branch plus remote-tracking branches (checking a remote one out
creates a local tracking branch).

**Right-click a commit** for its actions: checkout (detached), create a branch
or tag here, cherry-pick / revert / merge / rebase onto the current branch, or
copy the commit hash or subject. **Right-click a branch/tag chip** to check it
out directly (double-click still works too).

History-changing operations (cherry-pick, revert, merge, rebase) ask for
confirmation first. If any of them hit a conflict, loomux aborts the operation
and leaves your working tree exactly as it was, reporting the conflict — it
never leaves you in a half-finished, conflicted state to untangle. Resolve
those in a terminal.

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
same board through its tools. Issue and PR chips are **clickable** — they open
in your browser.

**Merge gate:** when an item reaches `pr` or `human-testing` — the point where
only you can decide — the board shows two buttons instead of making you type
into the orchestrator. **✓ Approve** marks the item done and tells the
orchestrator to merge. **✎ Changes** opens a box for your findings, records
them on the board, and sends them to the orchestrator to route back to a
worker. Both land as a message in the orchestrator pane, exactly as if you'd
written it.

**Attention routing:** the human is the scheduler's bottleneck, so loomux
surfaces *which* pane needs you instead of making you scan them. A pane earns a
pulsing **needs-attention** chip when an agent is parked on a prompt only you
can answer (a permission dialog or question — detected from the pane going
output-quiet on a prompt-shaped last screen, the same output heuristics the
stalled-agent watchdog uses, and suppressed while you're typing into it), when a
worker **reports** done or blocked, or when its task reaches a **human merge
gate**. Turning to the pane (or clicking the chip) clears a report badge; live
signals clear themselves when the condition passes. On the task board, items
that only you can advance (`pr`, `human-testing`, `blocked`) are highlighted so
what's waiting on you stands out. Each group also has an optional **desktop
notification** toggle (🔔 in the group lifecycle panel) that raises an OS toast
for those report/blocked/idle-with-prompt events — off by default, durable
per-group. Badges and highlights are header/board overlays; nothing ever
resizes the PTY.

**Audit viewer:** every orchestration pane has an audit toggle (`Alt+A` or
the history icon) opening the group's `audit.jsonl` as a filterable timeline
— every prompt, spawn, task edit, delivery outcome, and state write, one row
each. Filter by actor, action, or agent; free-text search the details;
expand any row to read the full prompt/task text verbatim (the field that
made this log worth grepping). A **follow** button live-tails new lines, and
rotation is transparent (the rotated `audit.1.jsonl` generation is read
alongside the current one). The overlay floats over the terminal like the
git and task-board views — it never resizes the PTY.

**Group lifecycle:** the orchestrator pane has a lifecycle toggle (`Alt+O` or
the group icon) with a one-glance summary — how many agents are live, the role
breakdown, uptime (per agent and for the whole group), each agent's state, and
running session cost with a group total. From here you can **pause** the group
(loomux stops delivering prompts so its agents finish their turn and idle out —
reversible with resume) or **End orchestration**, which kills *every* agent in
the group at once instead of ✕-clicking panes one by one. Ending is destructive,
so it takes a second confirming click; an optional **remove worktrees** checkbox
also deletes each agent's git worktree (uncommitted changes are lost, but the
branches — where the PRs live — are always kept). The teardown is audited, closes
the group's panes for you, and clears any pause so a later relaunch starts clean.

**Per-task sessions:** each worker is scoped to exactly one work item, and
loomux records its session id on the roster and task board. Claude ids are
pre-assigned at spawn; Copilot mints its own id on boot, so loomux watches
`~/.copilot/session-state` and binds the pane's new session a few seconds
after it starts. Either way, follow-ups on a finished task *resume* that
worker's session (same context, same workspace) instead of cold-starting a
new agent or disturbing a busy one — for Claude and Copilot groups alike.

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
- **Pane awareness** ("agent is waiting for input" badges): realized as
  attention routing (see above). The backend's `run_attention` scan reads each
  pane's pty output counter + tail + last-keystroke time (observed in `pty.rs`'s
  reader thread) and emits an `orch-attention` event the frontend badges panes
  from; add a new attention source in `attention_tick`.
