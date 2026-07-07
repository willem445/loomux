# Design: remote sessions (SSH + tmux)

Status: **prototype** (issue #122, Part A). Direction demo for the human to
test — not merged behaviour.

## Problem

Users want to run agents (or shells) on a persistent remote host — a 24/7 box —
connect from loomux, disconnect, and reopen later to see progress, with history
intact. The planner's read-only investigation on #122 compared SSH+tmux, dtach,
tmux control-mode, and a full remote daemon, and landed on a clear winner for v1.

## The one fact that makes this cheap

Loomux's PTY layer is already transport-agnostic. `pty.rs` spawns a child under
a local ConPTY and streams it to xterm.js; it does not care *what* the child is.
`spawn_pty` already accepts a structured `argv` and spawns it directly (issue
#110). So **a pane whose child is `ssh.exe user@host …` is, to loomux, an
ordinary local pane** — reader/writer/killer/Job-Object plumbing all unchanged.
The remote PTY is a Linux forkpty that tmux owns; loomux only ever sees ssh's
*local* ConPTY. That also means remote resize happens remotely (invisible to our
scrollback), so hard-constraint #1 ("never resize the PTY for UI") is respected
by construction.

## Approach (A1)

The pane child is:

```
ssh -t <host> tmux new -A -s <session> [-c <remote-dir>]
```

- `-t` forces PTY allocation so tmux gets a real terminal over the link.
- `tmux new -A` is **attach-or-create**: it attaches an existing session or
  creates one. This makes **reattach idempotent** — reopening with the same
  host + session resumes it. tmux holds server-side scrollback, process state,
  and layout across any client disconnect (pane close *or* network drop).
- `-c <remote-dir>` sets the start directory, used only when the session is
  first created.

No new crates (respects hard-constraint #2 — no getrandom). No new spawn path.

## Where the logic lives, and the injection surface

Argv assembly is the one place raw user input becomes a command, so it's a pure,
unit-tested module (`src/remote.ts`, `test/remote.test.ts`) — the same instinct
as `git.rs`/`gh.rs` guarding `-`-prefixed args. Three inputs, three postures:

1. **Host** (`[user@]host`) — becomes a *distinct argv element* to `ssh.exe`
   (the direct-spawn path passes argv straight to `CreateProcess`, no local
   shell), so there's no *local* shell-injection surface. The real risk is ssh
   **option injection**: a host like `-oProxyCommand=calc` would be read as a
   flag. Mitigation: **reject** a host that starts with `-` or contains anything
   outside `[A-Za-z0-9._@-]` (and at most one `@`). Hosts can't be meaningfully
   "sanitized", so they're rejected with an actionable message.

2. **Session name** — interpolated into the *remote* shell command
   `tmux new -A -s <name>` (ssh runs that string via the remote shell), so this
   *is* a remote-shell-injection surface. Mitigation: **sanitize** to
   `[A-Za-z0-9_-]` (tmux itself forbids `.`/`:` anyway), collapsing invalid runs
   to `-`. Sanitizing (not rejecting) keeps the field friendly — `my session` →
   `my-session`, `x; reboot` → `x-reboot` — while guaranteeing the output is a
   single inert shell word.

3. **Remote directory** — single-quoted into `-c '<dir>'` in the remote command.
   Wrapping in single quotes makes `$`/backticks literal; the only escape is a
   `'` itself, so we **reject** `'`, `"`, `` ` ``, `$`, `\`, and control chars.
   Empty → omitted.

The module returns both an `argv` (primary — what actually runs via the
direct-spawn path) and a `command` shell-string fallback for the rare case where
`ssh.exe` can't be resolved to a native image.

## Client-presence probe

Windows 10/11 ship the OpenSSH client but it's an *optional feature* that may be
absent. `probe_ssh` (`cliprobe.rs`) resolves the executable via PATH (fresh
registry PATH, so an SSH installed after loomux started still counts) and then
the well-known `%SystemRoot%\System32\OpenSSH\ssh.exe` fallback. It does **not**
run ssh — unlike the agent-CLI probe, `ssh` has no `--help` and prints usage to
stderr with a non-zero exit, which the generic probe would misread as "missing".
The launcher warns inline before a doomed launch; downstream errors (host
unreachable, tmux missing) are left to render as plain text in the pane, which
is acceptable prototype signal.

## Reattach UX / persistence

Recent targets (`{host, session, cwd}`) are persisted in `localStorage`
(`src/remote.ts`), deduped by **host+session** (the reattach identity — cwd only
matters on create), most-recent-first, capped at 8. The launcher pre-fills the
last target and offers a host datalist, so reattaching a known session is one
click. This mirrors where the rest of the launcher's state lives
(`agents.ts`), rather than the transcript-scanning session browser
(`sessions.rs`), which only knows about local Claude/Copilot files and has no
notion of a remote tmux server.

## Deliberate non-goals (stated for the PR)

- **No remote orchestration groups.** The MCP server binds `127.0.0.1` on the
  local machine and group state lives in local AppData; a remotely-run agent
  can't reach either. Remote panes are agents you steer by hand. Making
  orchestration remote is the deferred **A4** "loomux daemon on the remote"
  design (a wire protocol + remote bootstrap + a crate audit) — a v2+ effort.
- **No Part B** (web/phone monitor + steer) — a separate, security-sensitive
  build.
- **No credential handling** beyond the user's own ssh config / agent. Agent
  CLIs authenticate on the remote.
- **dtach fallback (A2)** documented by the planner but not built; tmux's
  server-side scrollback is the better default.
