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

Orrerix ships as a native desktop app for Windows, macOS, and Linux. Pick
whichever install path suits you — they all land on the same app.

> **Coming from Loomux?** The npm package (`loomux-desktop`), the command
> (`loomux`) and the installed app have all been renamed, with no compatibility
> shim — see [Upgrading from Loomux](#upgrading-from-loomux) below.

### npm (any platform)

If you already have **Node 18+**, the quickest path is the tiny launcher
package:

```sh
npx orrerix            # download + launch in one shot
npm install -g orrerix # then run `orrerix` anytime
```

`orrerix` is a small, dependency-free launcher: it fetches the matching
release asset for your platform (Windows installer, macOS `.dmg`, or Linux
`AppImage`), installs/caches it, and launches it.

```sh
orrerix            # launch the installed app (installs it first if missing)
orrerix update     # install/refresh the app from the newest release on your channel
orrerix version    # print the launcher's version
orrerix help       # full usage
```

Plain `orrerix` **never** updates an existing install. Installing over a running
Orrerix closes it — and everything running inside it — so when you update is your
call, not the launcher's.

`orrerix update` picks the newest release **on the channel you are already on**
and never installs an older build over a newer one:

| You have installed | `orrerix update` gives you |
| --- | --- |
| a stable release (`1.0.0`) | the newest **stable** release |
| a beta/RC (`1.1.0-beta11`) | the newest release of either kind |

If the launcher cannot read the version of your installed Orrerix, `orrerix
update` stops and says so rather than guessing — it has no way to tell an update
from a downgrade, so it does neither. Installing your preferred build once from
the releases page clears it.

To move from stable onto the beta train (or back), install that build yourself
from [the releases page](https://github.com/willem445/orrerix/releases) — the
launcher will not switch channels for you. On Linux the app is a cached
AppImage, so `orrerix update` refreshes the cache; quit the running AppImage
first, since it cannot be overwritten while it is running.

`orrerix --reinstall` still works as a deprecated alias for `orrerix update`, but
its meaning changed in v1.1: it used to install the version matching the
launcher itself, and now it installs the newest release on your channel.

### Upgrading from Loomux

The launcher used to be published as `loomux-desktop` and installed a command
called `loomux`. Both were renamed with the app, and there is no compatibility
shim. `loomux-desktop` has also been removed from npm entirely, so
`npx loomux-desktop` and `npm install -g loomux-desktop` now fail rather than
installing anything.

If you installed it globally before that, the `loomux` command is still on your
machine — removing a package from the registry does not remove it from your disk:

```sh
npm uninstall -g loomux-desktop
npm install -g orrerix
```

The new launcher still recognises an app installed under the old name, so
`orrerix update` sees the version you already have and will not downgrade it,
and a Linux AppImage the old launcher cached is launched rather than
re-downloaded.

What it does **not** do is replace a Loomux install in place — see
[Loomux and Orrerix install side by side](#loomux-and-orrerix-install-side-by-side).

### Windows (one-liner)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/willem445/orrerix/main/install.ps1 | iex"
```

### macOS / Linux (one-liner)

```sh
curl -fsSL https://raw.githubusercontent.com/willem445/orrerix/main/install.sh | sh
```

### Release assets (manual)

Prefer to grab an installer yourself? Every build is published to
[the GitHub releases page](https://github.com/willem445/orrerix/releases) —
beta/RC builds included, which the `latest` link does not show:

| Platform | Asset |
| --- | --- |
| Windows | `*-setup.exe` (installer) or `*.msi` |
| macOS (Apple Silicon) | `*_aarch64.dmg` |
| macOS (Intel) | `*_x64.dmg` |
| Linux | `*.AppImage` (portable), `*.deb`, or `*.rpm` |

Builds are **unsigned** for now. On macOS, if the app is reported as damaged,
clear the quarantine attribute:

```sh
xattr -cr /Applications/Orrerix.app
```

(The install script does this for you.)

### Loomux and Orrerix install side by side

If you already had Loomux, installing Orrerix leaves **both** apps on your
machine. That is not something the installer can avoid: it identifies an
existing install by the product name — the Windows Add/Remove entry and install
directory, and the macOS bundle directory, are all named after it — so a
renamed app has nothing to recognise and installs fresh.

Nothing removes the old one for you, deliberately: it is a working app you
installed, and quietly deleting it is not the installer's call. Uninstall Loomux
whenever you are ready — Add/Remove Programs on Windows, or
`rm -rf /Applications/Loomux.app` on macOS. On Linux there is nothing to do:
the AppImage is a single file, and the one on your PATH is simply replaced.

**Until you do, plain `orrerix` launches the Loomux you already have.** That is
the same rule as always — a plain launch never installs anything over an
existing install, on any platform — and the launcher counts your Loomux as an
existing install on purpose, because that is what keeps it from downgrading you.
Run `orrerix update` to get the Orrerix build; after that, plain `orrerix`
launches Orrerix, because the launcher prefers the current app when both are
there.

Your data is not duplicated and nothing needs migrating — both builds read the
same profile directory, which moved separately and earlier.

### Upgrading from an earlier Orrerix build

The executable inside the install is now `orrerix.exe`; earlier Orrerix builds
carried `loomux.exe`. Installing over one of those replaces it in place and
removes the old executable for you — there is nothing to clean up, and no second
app appears in Add/Remove Programs.

Two things to know:

- **Quit Orrerix before installing it by hand.** If the old build is still
  running, its executable cannot be deleted, so it is left behind next to the
  new one — harmless, and cleared by the next install, but it stays until then.
  `orrerix update` refuses outright while the app is running rather than
  installing over it.
- **Re-pin your taskbar shortcut.** A shortcut you pinned yourself points at the
  old executable and stops working. The Start-menu and desktop shortcuts the
  installer created are updated for you; pin a fresh one from there.

Crash dumps written by Windows are named after the executable, so they are now
`orrerix.exe.<pid>.dmp` — see [Troubleshooting](troubleshooting.html).

## First launch

Open orrerix and you get a single terminal pane running your default shell — it
behaves like any native terminal, because under the hood it *is* one (real
ConPTY on Windows, forkpty on macOS/Linux, via WezTerm's PTY layer). Colors,
escape sequences, and wide characters render exactly as they would natively.

From here you can:

- **Split** the pane into a matrix — `Ctrl+Shift+E` (right) or `Ctrl+Shift+O`
  (down). See [Core concepts](core-concepts.html) for the whole grid model.
- **Pick a shell** on the **Terminal** kind of the welcome screen — PowerShell,
  Command Prompt, or Git Bash. Git Bash is offered only when Git for Windows is
  installed; otherwise it's shown disabled with that reason, and any pane still
  falls back to PowerShell rather than failing to start.
- **Name** a pane with `F2` so you can tell your agents apart.
- **Restore a past agent session** with the session browser (`Ctrl+Shift+P`) —
  it scans your machine for resumable Claude Code, Copilot CLI, and OpenCode
  sessions and drops the one you pick back into a pane, in its original folder.
  See the [session browser](features/session-browser.html).

## Your first agent pane

Orrerix is built to run AI coding agents, but it doesn't bundle them — it drives
the CLIs you already have installed. The three first-class ones are:

- **[Claude Code](https://claude.com/claude-code)** — the `claude` CLI.
- **[GitHub Copilot CLI](https://github.com/github/copilot-cli)** — the
  `copilot` CLI.
- **[OpenCode](https://opencode.ai/)** — the `opencode` CLI.

Make sure at least one is installed and on your `PATH`. Then, to open an agent
in a pane:

1. Open a new pane (`Ctrl+Shift+E`/`O` to split, `Ctrl+Shift+T` for a new tab).
   Every pane starts on the **welcome / pane-setup screen**.
2. Choose the **Agent** kind, pick the agent CLI and model, leave **Panes** at 1,
   and click **Create**.

The **Autopilot — pre-approve all tools** checkbox (on by default) launches the
agent with tools pre-approved so it stops prompting you to approve each edit or
command — Claude Code's native Auto mode plus pre-approved `git`/`gh`, or, for
Copilot, the same true autopilot mode an orchestration worker gets (`--autopilot
--allow-all-tools --allow-all-paths`). Copilot only opens its blocking "Enable
autopilot mode" dialog on the pane's first submit, which for a lone pane is your
own first Enter — so orrerix runs a watcher that answers it for you rather than
leaving it on screen. For OpenCode it's `--auto`, which answers each permission
ask itself as it comes up rather than opening a separate confirmation dialog, so
no watcher is needed on a fresh launch. Uncheck the box to launch in the CLI's
normal interactive mode. Orrerix never uses `--dangerously-skip-permissions`.
Your last choice is remembered for next time.

That posture survives a **restore**, too (an app restart, or resuming from the
session browser) — but only when orrerix launched the session itself *and*
recorded an unambiguous toggle state for it. For Claude that record is per
session, so it's never ambiguous; for Copilot — which hands orrerix no session id
at launch — it's per folder, so two sessions launched from the same folder with
the toggle flipped between them deliberately resolve to *no* flags rather than a
guess. A session with no such record comes back in plain interactive mode instead
of guessing; `Shift+Tab` still cycles it into autopilot by hand. OpenCode keeps
no restore-posture record yet, so a restored OpenCode pane always comes back
plain — same `Shift+Tab` fallback.

Want more than one agent? Set **Panes** above 1 on the Agent kind to spawn *N*
independent agent panes at once. And when you're ready to hand a whole queue of
work to a fleet that manages itself, that's the
[orchestration guide](orchestration.html).

## What you need installed

| For | Requirement |
| --- | --- |
| Running an agent pane | `claude`, `copilot`, and/or `opencode` on your `PATH` |
| The issues/PR view and the orchestration PR workflow | `gh` CLI, authenticated (`gh auth login`) |
| Voice prompts (Windows, opt-in) | a whisper.cpp runtime + a model — see [Voice prompts](features/voice-prompts.html) |

If a required CLI is missing, orrerix tells you inline rather than failing
silently — the launcher warns when a selected role's CLI isn't installed, and
the issues panel says so if `gh` isn't set up.
