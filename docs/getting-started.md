---
title: Getting started
layout: default
nav_order: 2
---

# Getting started
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

## Install

Loomux ships as a native desktop app for Windows, macOS, and Linux. Pick
whichever install path suits you — they all land on the same app.

### npm (any platform)

If you already have **Node 18+**, the quickest path is the tiny launcher
package:

```sh
npx loomux-desktop            # download + launch in one shot
npm install -g loomux-desktop # then run `loomux` anytime
```

`loomux-desktop` is a small, dependency-free launcher: it fetches the matching
release asset for your platform (Windows installer, macOS `.dmg`, or Linux
`AppImage`), installs/caches it, and launches it. Pass `--reinstall` to force a
fresh download.

> The package is named `loomux-desktop` because the bare `loomux` name on npm
> belongs to an unrelated tmux tool — but the command it installs is still
> `loomux`.

### Windows (one-liner)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/willem445/loomux/main/install.ps1 | iex"
```

### macOS / Linux (one-liner)

```sh
curl -fsSL https://raw.githubusercontent.com/willem445/loomux/main/install.sh | sh
```

### Release assets (manual)

Prefer to grab an installer yourself? Every build is published to
[the latest GitHub release](https://github.com/willem445/loomux/releases/latest):

| Platform | Asset |
| --- | --- |
| Windows | `*-setup.exe` (installer) or `*.msi` |
| macOS (Apple Silicon) | `*_aarch64.dmg` |
| macOS (Intel) | `*_x64.dmg` |
| Linux | `*.AppImage` (portable), `*.deb`, or `*.rpm` |

Builds are **unsigned** for now. On macOS, if the app is reported as damaged,
clear the quarantine attribute:

```sh
xattr -cr /Applications/Loomux.app
```

(The install script does this for you.)

## First launch

Open loomux and you get a single terminal pane running your default shell — it
behaves like any native terminal, because under the hood it *is* one (real
ConPTY on Windows, forkpty on macOS/Linux, via WezTerm's PTY layer). Colors,
escape sequences, and wide characters render exactly as they would natively.

From here you can:

- **Split** the pane into a matrix — `Ctrl+Shift+E` (right) or `Ctrl+Shift+O`
  (down). See [Core concepts](core-concepts.html) for the whole grid model.
- **Name** a pane with `F2` so you can tell your agents apart.
- **Restore a past agent session** with the session browser (`Ctrl+Shift+P`) —
  it scans your machine for resumable Claude Code and Copilot CLI sessions and
  drops the one you pick back into a pane, in its original folder. See the
  [session browser](features/session-browser.html).

## Your first agent pane

Loomux is built to run AI coding agents, but it doesn't bundle them — it drives
the CLIs you already have installed. The two first-class ones are:

- **[Claude Code](https://claude.com/claude-code)** — the `claude` CLI.
- **[GitHub Copilot CLI](https://github.com/github/copilot-cli)** — the
  `copilot` CLI.

Make sure at least one is installed and on your `PATH`. Then, to open an agent
in a pane:

1. Open a new pane (`Ctrl+Shift+E`/`O` to split, `Ctrl+Shift+T` for a new tab).
   Every pane starts on the **welcome / pane-setup screen**.
2. Choose the **Agent** kind, pick the agent CLI and model, leave **Panes** at 1,
   and click **Create**.

The **Autopilot — pre-approve all tools** checkbox (on by default) launches the
agent with tools pre-approved so it stops prompting you to approve each edit or
command — Claude Code's native Auto mode plus pre-approved `git`/`gh`, or
Copilot's `--allow-all-tools --allow-all-paths`. Uncheck it to launch in the
CLI's normal interactive mode. Loomux never uses
`--dangerously-skip-permissions`. Your last choice is remembered for next time.

Want more than one agent? Set **Panes** above 1 on the Agent kind to spawn *N*
independent agent panes at once. And when you're ready to hand a whole queue of
work to a fleet that manages itself, that's the
[orchestration guide](orchestration.html).

## What you need installed

| For | Requirement |
| --- | --- |
| Running an agent pane | `claude` and/or `copilot` on your `PATH` |
| The issues/PR view and the orchestration PR workflow | `gh` CLI, authenticated (`gh auth login`) |
| Voice prompts (Windows, opt-in) | a whisper.cpp runtime + a model — see [Voice prompts](features/voice-prompts.html) |

If a required CLI is missing, loomux tells you inline rather than failing
silently — the launcher warns when a selected role's CLI isn't installed, and
the issues panel says so if `gh` isn't set up.
