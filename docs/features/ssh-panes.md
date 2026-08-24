---
title: SSH panes
layout: default
parent: Features
nav_order: 8
---

# SSH panes
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

An **SSH pane** is a pane whose process is *your own* `ssh` client, connected to
a remote host — optionally starting an agent CLI on the far end. It behaves like
any other terminal pane: it splits, docks, drags, maximizes, takes a name, and
scrolls back.

Two things it is *not*, and both are load-bearing:

- **Nothing orrerix-side runs on the remote host.** There is no daemon, no
  service, no protocol, no remote state orrerix owns. The pane's child process is
  a local `ssh.exe`; everything past that is between OpenSSH and the host you
  configured.
- **An SSH pane is a solo pane.** It can display and drive a remote agent, but it
  can never be a member of an orchestration group. See
  [What doesn't work over SSH](#what-doesnt-work-over-ssh-and-why) — that
  boundary is enforced in code, not just documented.

You could always type `ssh host` into a Terminal pane, and that still works. What
this pane kind adds is saved connections, a first-class launch of a remote agent
CLI (with a resumable Claude Code session), restore across app restarts, and
honest degradation of every orrerix feature that needs a *local* filesystem.

## What you need

The Windows **OpenSSH client** (`ssh.exe`). orrerix looks for it on `PATH` first —
so a newer OpenSSH, or a wrapper you put ahead of the inbox client, wins, exactly
as it does for every other program on your machine — and then in the inbox
install directory (`%SystemRoot%\System32\OpenSSH`), which covers a stripped
`PATH`.

If neither turns up anything, **Create is refused** with:

> No ssh client found — orrerix looked on PATH and in the Windows OpenSSH install
> (System32\OpenSSH). Install the OpenSSH Client optional feature, or put ssh.exe
> on PATH.

That is deliberate: a pane that opened and died on its first line would tell you
much less.

## Creating one

On a pane's welcome screen, pick **SSH — a remote shell or agent CLI over your own
ssh client**, then fill in the section that appears:

| Field | What it is |
| --- | --- |
| **Connection** | The saved connection to launch, or **New connection…**. Picking one fills every field below from it; the fields *are* the connection's editor, so what you launch is what gets saved. |
| **Name** | The label the picker shows. Free text, and not an identity — renaming a connection keeps the panes that use it pointed at it. |
| **Destination** | `user@host`, a bare `host`, or a `Host` alias from your own `ssh_config`. Required. One field rather than separate user/host boxes, because an alias has neither — and an alias is how your `ProxyJump`, `IdentityFile` and `User` come along for free. |
| **Port** | `-p`. Blank means orrerix passes nothing and your ssh config decides. |
| **Identity file** | `-i`. A **path** to a private key — never the key itself (see [below](#credentials-orrerix-holds-none)). Blank means your ssh config decides. |
| **Remote shell** | **POSIX (sh/bash/zsh)** or **cmd.exe** — which shell's quoting rules the remote command is built for. A declaration, not a guess; see [Remote shell](#remote-shell-a-declaration-not-a-detection). |
| **Keepalive (s)** | `ServerAliveInterval`. Blank emits *nothing at all*, so your own ssh config's keepalive settings win untouched. |
| **Extra ssh flags** | Space-separated argv words passed to `ssh` verbatim and in order (`-J jump.example.net`). The escape hatch for anything the fields above don't model. |
| **Remote CLI** | The agent CLI to start on the far host, or **None — a plain login shell**. |
| **Remote folder** | A directory on the **remote** host to `cd` into before the remote CLI starts. |

Then **Create**. The pane converts in place, the ssh client starts, and whatever
your ssh setup wants to ask — host-key confirmation, a password, a 2FA code —
renders in the pane, because the pane is a terminal.

The connection is saved on Create, whether it was new or edited — best-effort: if
the file can't be written, the pane still connects and you are not blocked on the
record of it.

### Nothing you type is silently dropped

A value the saved connection would refuse is **refused at Create**, with the
reason, instead of being quietly discarded on the way to `ssh`:

- a **Port** or **Keepalive** outside its range (1–65535 and 1–86400) — you are
  told, rather than connecting on port 22 and wondering;
- an **Identity file** that is actually key material — refused: that field takes a
  path, and orrerix will not write a key into the connections file (see below).

A **Remote folder** with **Remote CLI = None** is the one case that is neither
refused nor honoured: there is no remote command for the `cd` to prefix, so the
launch warns inline —

> ⚠ Remote folder applies only when a remote CLI is set — a plain login shell
> starts wherever the remote host puts it. The folder stays saved on this
> connection.

— and keeps the value, because it becomes correct the moment you pick a CLI.
Synthesizing a login shell (`cd … && exec $SHELL -l`) would mean *guessing* the
remote's login shell, which is exactly what **Remote shell** exists to avoid.

## Credentials: orrerix holds none

Authentication is entirely your ssh setup's business — `ssh_config`, `ssh-agent`,
and interactive prompts in the pane. orrerix passes no `BatchMode`, so a prompt is
free to appear and you answer it in the pane. This mirrors how orrerix treats
GitHub: it stores no token and shells out to your authenticated `gh`.

Saved connections live in **`%APPDATA%\loomux\sshprofiles.json`**, a plain,
hand-editable JSON file next to `tabs.json` and `settings.json`. It holds
hostnames, ports, the *path* of an identity file, a remote folder, a CLI name and
your extra flags. **orrerix never writes a password or a passphrase into it**, and
two structural guards keep that true rather than merely stated:

1. **Read and write are both allowlists.** Only the declared fields are read in,
   and only those fields are written out — so a `password` key hand-added to the
   file cannot survive one load/save cycle.
2. **The identity file is checked to be a path.** Paste a PEM blob into that field
   and it is refused, rather than written into the JSON.

Guard 2 has one honest gap, stated here rather than left to be inferred: it is a
**line-break test** (plus an armour-header test as a belt), because every real
key wraps its base64 body across lines — so a key body pasted as a *single line
with no header* looks exactly like a path and gets through. Nothing orrerix does
produces that shape, and the value is then handed to `ssh -i` as a filename that
does not exist, which fails loudly rather than storing anything. The guard fails
closed on everything a key actually looks like; it is not a content classifier.

Editing the file by hand is fine. A malformed *entry* is dropped on load rather
than taking the whole list with it; a file that won't parse as JSON at all is
renamed aside to `sshprofiles.corrupt.json` and orrerix starts from an empty
list — the same treatment `tabs.json` gets.

Launching a connection rewrites that whole file, so orrerix will not write it
until it has read it back. If your saved list has not loaded yet, the save waits
for it; if orrerix could not read the file at all — not corrupt, just unreadable
— the connection you just launched is **not saved**, rather than becoming the
only line in the file. The pane still opens either way. Re-picking **SSH** in the
launcher retries the read.

> **The trust boundary, stated plainly.** Anyone who can write
> `sshprofiles.json` can make your `ssh` do anything your `ssh` can do — the
> **Extra ssh flags** field reaches `ssh` as raw argv, exactly like a line in your
> own `~/.ssh/config`. That is accepted, not overlooked: it is your own AppData,
> and an attacker who can write there can already do worse. It is the reason the
> fields are *not* filtered against a list of "dangerous" options — that would be
> theatre over a file you control, while breaking legitimate flags.
> `doc/design/ssh-panes.md` argues it in full.

## Running an agent on the far end

**Remote CLI** offers the same catalog the Agent pane kind offers (minus *custom*,
whose command line is a *local* one you own), plus **None — a plain login shell**.
The name you pick is run on the remote host as written; orrerix's catalog decides
what *flags* it can add, not what the remote machine is allowed to have installed.
A name orrerix doesn't recognize warns and still runs:

> ⚠ "beam" isn't a CLI orrerix knows — it will be run on the remote host exactly as
> written, with no session id and no autopilot flags.

What actually reaches the far host, with **Remote CLI = Claude Code** and a
**Remote folder** of `/srv/app` on a POSIX remote, is **one** remote-command
string — every token individually quoted, so nothing in it can be re-read as
another argument:

```
cd '/srv/app' && exec 'claude' '--session-id' '<uuid>'
```

**Claude Code is the only remote CLI that gets a session id**, and for a
structural reason rather than a preference: Claude's session identity is a value
orrerix puts *on the command line*, which travels through ssh untouched. Every
other CLI's session id is *discovered* by reading a store on the machine the CLI
runs on — a mechanism that reaches your machine, not the far host. So a remote
Copilot or OpenCode pane runs fine and records **no** session id, which is what
makes its reconnect honest instead of a `--resume` of an id nobody can look up.

**No autopilot flags and no MCP/channel tools are applied to a remote CLI.** The
Autopilot and Channel-tools toggles belong to the Agent kind and are hidden here:
orrerix's MCP server listens on loopback only and its per-agent config reaches only
children orrerix spawns itself, so neither could be delivered to a process on
another machine.

### Remote shell: a declaration, not a detection

The remote's default shell is genuinely unknowable from here — probing it costs a
round trip and can still be wrong (a forced command, a `chsh`'d account) — so you
declare it, and the remote command is quoted for what you declared.

- **POSIX (sh/bash/zsh)** — single-quoted, the provably-safe scheme.
- **cmd.exe** — double-quote doubling, which is cmd.exe's own convention. Two
  inputs it cannot express are refused with a message rather than mangled: a
  token containing a newline (cmd.exe reads only the first line of a `/C` command
  and silently drops the rest) and one ending in `\`.

A remote whose sshd `DefaultShell` is **PowerShell is not supported in v1** — its
quoting rules are neither of the above. Reach such a host as a plain login shell
(**Remote CLI = None**), which needs no quoting at all.

## Restarting, disconnecting, reconnecting

**An SSH pane never connects by itself.** Not on restore, not after a drop.

### After an app restart

A restored SSH pane comes back as a **dormant card with a Reconnect button**.
Two independent reasons, neither of which applies to a local shell: the CLI on the
far end is an agent on *someone else's machine*, so an automatic reconnect spends
**remote** credits with nobody watching; and a host that is down, asleep, or behind
a VPN that isn't up yet would put a TCP connect on the boot path, where it may not
fail for a minute.

### After the connection drops

A dropped link makes `ssh` exit non-zero. The pane **stays open** — the bytes it is
holding ("Connection to host closed", a timeout, a refused key) are the explanation
— and a **Reconnect** card floats *over* that terminal rather than replacing it.
**Dismiss** drops the offer and leaves the pane exactly as it is.

A clean exit closes the pane as usual: typing `exit` on the far end, or a remote
CLI finishing, is not a disconnection.

### What Reconnect does

It re-derives everything from the **saved connection**, never from a captured
command line:

- your local ssh client is **re-probed**, so a machine that has gained (or lost)
  one since is read correctly;
- the connection is **re-read from `sshprofiles.json`**, so an edit you made
  between boots (a new port, a different remote folder) applies to the reconnect;
- a recorded Claude session is **resumed** (`claude --resume <id>`) when the
  connection still names Claude Code; otherwise it is a plain fresh connect.

If a recorded session could not be resumed — because you switched that connection
to a CLI whose session identity orrerix cannot carry — the pane says so rather than
letting you discover it by asking an agent about work it has no memory of.

**Reconnect fresh** is offered beside it when there *is* a recorded session: it
starts a new remote session instead of resuming, which is what you want when the
remote conversation is gone (deleted on the far host) and every resume fails.

If the saved connection has been **deleted**, the card looks exactly as it always
does — Reconnect is live, and **Reconnect fresh** is still offered if a session was
recorded — and the refusal arrives when you **click**:

> This pane's saved SSH connection no longer exists — it was removed, or the
> connections file was reset. Open a new SSH pane to connect again.

There is nothing left to reconnect *with*, because the pane records the
*connection* and not a command line, and inventing one from a stale command line
would connect you somewhere you removed on purpose. The one case that says so at
mount, without a click, is a pane whose record carries no connection at all — an
`ssh` entry hand-written into `tabs.json` without an `sshProfileId`.

Two things Reconnect does not preserve: the **scrollback** of the dead session
(the terminal is reset before the new client starts, so the old session's tail
cannot paint over the new one's first bytes), and anything the remote process was
doing that did not survive the disconnection. orrerix kills the local ssh client
when the pane closes; what the far host does with the session at that point is
the far host's business — which is precisely why a resumable Claude session is
worth having.

## What doesn't work over SSH, and why

Everything below is a *local-filesystem* assumption, wired off deliberately
rather than left to produce a wrong answer:

| Feature | On an SSH pane |
| --- | --- |
| **Folder chip / folder picker** | Hidden. The pane's local working directory is home, and the directory the remote shell reports names a filesystem this machine cannot see. A picker would choose a *local* path and `cd` a shell on another machine into it. |
| **Branch chip** | Never populates — same reason. |
| **Git view** (`Alt+G`) | Opens, and never shows a repository. It has no local folder to read, so it sits on its placeholder — *"Waiting for the shell to report its folder…"* — which on an SSH pane is a wait that never ends, since the folder the remote shell reports is deliberately ignored. Misleading wording, but the alternative (pointing it at whatever local path happened to match a remote one) would be worse than showing nothing. |
| **External git watching** | Never registered. A watch here would either resolve to nothing or report an unrelated *local* repo's changes as though they were the remote one's. |
| **Session browser, usage meter, transcripts** | A remote session never appears. All three read stores on the machine the CLI ran on, and that machine isn't this one. |
| **The tab's agent counter** | An SSH pane counts for nothing. The CLI on the far end may well be an agent — but not one this orrerix spawned, supervises, or can account for. |
| **Orchestration membership** | Refused outright. See below. |

Typing *into* the pane works normally, including paste: it is a transparent byte
stream.

### The orchestration boundary

**An SSH pane can never be an orchestration group member, and the group spawn
surfaces never offer one.** This is a refusal, not a degradation, because none of
the machinery degrades gracefully:

- **Worktrees** are local directories made by local `git`;
- **the MCP server** is loopback-only, and its per-agent config reaches only
  children orrerix spawns itself — so a remote agent could not report at all;
- **the `gh` shim** — which is what *enforces* the merge gate — also reaches only
  locally-spawned children, so a remote `gh` would face no gate. That is a
  security regression, and it is the reason this line is drawn hard;
- **briefs** written by a local orchestrator name local paths.

The refusal is enforced in code at the pane, before any process starts, and
restore has no field that could smuggle an orchestration identity back in: an
`ssh` entry hand-edited into `tabs.json` claiming `role: "worker"` still restores
as an ordinary dormant SSH card.

**So: in v1 an SSH pane is display-only** — a remote solo pane you drive yourself.
Every "make *X* work remotely" follow-up (remote sessions in the browser, remote
usage, remote orchestration members, a tunnelled MCP server) needs an orrerix
process on the far end, which is the **remote engine** ([#888]), not this feature.

[#888]: https://github.com/willem445/orrerix/issues/888
