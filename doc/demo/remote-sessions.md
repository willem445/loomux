# Demo: remote sessions (SSH + tmux)

Status: **prototype** (issue #122, Part A). Direction demo, not polish.

This walks through running a persistent remote session in a loomux pane and
proving the detach/reattach behaviour that makes it useful for long-running
agents. It's the "clear-winner" **A1** from the planner's investigation on
#122: a remote session pane is just an ordinary loomux pane whose child is
`ssh -t <host> tmux new -A -s <session>`.

## What you'll see

- A loomux pane running a shell (or an agent) on a remote Linux host.
- Closing the pane (or yanking the network) **leaves the session alive** on the
  host — tmux holds the process, its scrollback, and its layout.
- Reopening the same host + session **reattaches** it, history intact.

## Prerequisites (the human's setup — not automated)

These are yours to set up once; loomux deliberately doesn't touch credentials.

1. **A reachable host with an SSH server** you can log into. `user@host` should
   work from a normal terminal.
2. **Key-based auth already working** — `ssh user@host` connects without a
   password prompt (an `ssh-agent` key or `~/.ssh/config` identity). Loomux uses
   your existing ssh client and its config; it never asks for or stores a
   password. (A password/passphrase prompt *will* still render in the pane and
   you can type it, but key auth is the intended flow.)
3. **`tmux` installed on the remote** — `ssh user@host tmux -V` prints a
   version.
4. **The local OpenSSH client** — present on Windows 10/11 but an *optional
   feature*. If it's missing, the launcher warns you; enable it via
   *Settings ▸ Apps ▸ Optional features ▸ OpenSSH Client*.
5. *(Optional, for the agent story)* Any agent CLI you plan to run
   (`claude`, `copilot`, …) is **installed and logged in on the remote**. Its
   credentials live there, not on your laptop.

## Steps

### 1. Open a remote session

1. Open the **New agent pane** dialog (the way you normally add a pane in agent
   mode).
2. Set **Mode → Remote session (SSH + tmux)**.
3. Fill in:
   - **Host**: `user@host`
   - **Session**: e.g. `demo` (leave blank to accept the suggested default)
   - **Remote directory** *(optional)*: e.g. `/home/user/project` — only used
     the first time the session is created.
4. Click **Launch**.

The pane opens and you should land at a shell prompt **on the remote host**,
inside a fresh tmux session named `demo`. (tmux's green status bar at the bottom
of the pane is the giveaway that you're attached.)

Run something long-lived so the reattach is visually obvious — e.g.:

```sh
watch -n1 date        # a ticking clock, or
top                   # or start your agent CLI here: claude / copilot
```

### 2. Detach and prove persistence

Pick either detach path:

- **Close the pane** in loomux (`Ctrl+Shift+W`), or
- **Kill the link** — disable Wi-Fi / pull the cable for a moment.

Either way, loomux's ssh child dies but **tmux on the remote does not** — the
`watch`/`top`/agent keeps running server-side. (You can confirm from any
terminal: `ssh user@host tmux ls` lists `demo` still attached-or-detached.)

### 3. Reattach from a fresh pane

1. Open the **New agent pane** dialog again → **Mode → Remote session**.
2. The **Host** and **Session** are pre-filled from your recent targets (this is
   the "one-click reattach" — the target was remembered). If not, type the same
   `user@host` and `demo`.
3. **Launch.**

You reattach to the **same** session: the clock has kept ticking, `top` shows
uninterrupted uptime, your agent has kept working while you were gone. That's
`tmux new -A` (attach-or-create) doing the work — no separate "resume" command,
no lost scrollback.

## Failure modes (what a rough edge looks like)

For a prototype the raw ssh/tmux error text in the pane is considered acceptable
signal; only the local-client check is pre-flighted:

| Situation | What you see |
| --- | --- |
| Local `ssh` client missing | Inline warning in the dialog *before* launch, with the enable-OpenSSH hint; launch is blocked. |
| Host unreachable / DNS fails | The pane shows ssh's own `ssh: Could not resolve hostname …` / `Connection timed out`, then exits. |
| Auth not set up | ssh's password prompt (or `Permission denied (publickey)`) renders in the pane. |
| `tmux` missing on the remote | ssh connects, then `bash: tmux: command not found` in the pane. |
| Bad host / injection attempt | The dialog rejects a malformed host (e.g. a leading `-`) up front; a session name with shell metacharacters is sanitized to a safe tmux name rather than executed. |

## What's stubbed / out of scope

This prototype is intentionally narrow:

- **No remote orchestration groups.** A remotely-run agent can't reach the
  loomux MCP server (bound to `127.0.0.1` on *your* machine) and has no group
  dir/task board/watchdog. Remote panes are agents you steer **by hand**, not a
  remote orchestrated group. Making orchestration remote is the deferred **A4**
  "loomux daemon on the remote" design.
- **No web/phone monitoring or steering.** That's **Part B** of #122, a separate
  security-sensitive build (Tailscale-serve, typed intents, append-only audit) —
  not touched here.
- **No credential handling** beyond what your own ssh config / agent provides.
- **No session browser chips yet.** Reattach discovery is via the launcher's
  remembered recent targets, not the local-transcript session browser (which
  scans Claude/Copilot files and doesn't know about remote tmux servers).

## How it maps to the code

- `src/remote.ts` — pure argv assembly + validation (the injection-guarded
  `buildRemoteInvocation`) and recent-target persistence. Unit-tested in
  `test/remote.test.ts`.
- `src/launcher.ts` — the **Remote session** mode in the New-agent-pane dialog.
- `src-tauri/src/cliprobe.rs` `probe_ssh` — locates the OpenSSH client
  (PATH + the Windows optional-feature path); it never runs ssh.
- Spawn path — unchanged: the pane goes through the existing #110 direct-`argv`
  path in `pty.rs`; a remote ssh pane is just another ConPTY child.
