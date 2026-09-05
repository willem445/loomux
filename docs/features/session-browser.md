---
title: Session browser & editor
layout: default
parent: Features
nav_order: 5
---

# Session browser & editor launch
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

## Session browser

Press **`Ctrl+Shift+P`** (or the *sessions* button) to open the session browser.

The panel it opens in has **two tabs**. Sessions is this page; the second is the
[Agents tab](agents-tab.html), which lists the panes already open in this window
and what each one is doing. `Ctrl+Shift+P` and the *sessions* button always open
the Sessions tab; the *agents* button opens the other one. Switching between them
does not resize your panes — only opening and closing the panel does, exactly as
it always has.

A **Mine ⇄ Orchestration** control at the top picks which world you are looking
at, and orrerix remembers your choice:

- **Mine** — the sessions you started yourself: every pane you launched by hand,
  and nothing an orchestration minted.
- **Orchestration** — everything an orchestration group minted, with the
  **Orchestrations** list above it (the primary route back into a recorded
  group, described below).

The split is on whether orrerix recorded an orchestration identity for the
session, not on a list of role names — so a workflow that invents a new role
puts its sessions in **Orchestration**, where they belong, rather than quietly
mixing them in with your own.

### Orchestrations

Shown in **Orchestration** mode, above the session list. Every orchestration
group orrerix has a record of, on every agent CLI, newest
activity first with running groups at the top. **Resume** brings the whole group
back — same group id, state, task board and audit history, with fresh MCP
identity wired into the resumed orchestrator conversation.

This list is built from orrerix's own record of each group (`group.json` plus the
orchestrator row of `agents.json`), not from any CLI's session store. That is why
it is the reliable restart route: an OpenCode group's orchestrator session is
never in the session list below (see below), and Copilot's is there only once
orrerix has learned its session id.

A row without a **Resume** button says why it has none:

| What the row says | What happened | What to do |
| --- | --- | --- |
| *Running now* | The group has live agents in this window | Click **Focus** on the row — it brings that group's orchestrator pane back into view, out of the dock or out from behind a fullscreen pane, in whichever project tab holds it |
| *Session not yet identified* | Copilot and OpenCode mint their session ids after boot, and orrerix has not learned this one yet (or its watcher timed out). Claude Code and pi never show this row — orrerix assigns their ids before the pane starts | Wait for it. If the watcher timed out there is nothing to resume by hand — start a fresh orchestrator, which reattaches to this group's existing board and roster |
| *Recorded session is no longer in the … store* | The CLI's own history no longer holds that conversation | Start a fresh orchestrator — it reattaches to this group's existing board and roster |
| *This group's record could not be read* | The group's `group.json` is missing or damaged | Repair or remove that file; until then orrerix cannot tell which CLI ran the group |

### Sessions

Below that, the individual agent sessions orrerix found on this machine:

- **Claude Code** — `~/.claude/projects/*/*.jsonl` (titled by the first real
  prompt, resumed with `claude --resume <id>`).
- **Copilot CLI** — `~/.copilot/session-state/*/workspace.yaml` (resumed with
  `copilot --resume=<id>`) (#458).
- **OpenCode** — its own SQLite store, `~/.local/share/opencode/opencode.db`
  (`$XDG_DATA_HOME` and `$OPENCODE_DB` are honoured, exactly as opencode itself
  resolves them), resumed with `opencode --session <id>`.
- **pi** — `~/.pi/agent/sessions/--<your-folder>--/<timestamp>_<id>.jsonl`
  (`$PI_CODING_AGENT_SESSION_DIR` and `$PI_CODING_AGENT_DIR` are honoured, in
  that order, exactly as pi itself resolves them), titled by the first real
  prompt and resumed with `pi --session <id>`. If you point
  `$PI_CODING_AGENT_SESSION_DIR` somewhere, pi writes session files straight
  into it with no per-folder subdirectory — both shapes are read, so a store
  you have used both ways lists everything in it.

If you have moved your pi store using the `sessionDir` key in
`~/.pi/agent/settings.json` rather than either environment variable, orrerix
does not read that file and will list no pi sessions. It will not list *wrong*
ones.

Only *your own* opencode and pi sessions are listed — the ones a solo pane or
your own terminal created. Sessions belonging to an orchestration group live in
that group's own store and are reopened by restoring the group from the
**Orchestrations** list above, not as standalone panes: a bare
`opencode --session <id>` or `pi --session <id>` pane would come back with no
MCP tools and no task board.

Clicking a session opens a new pane in the session's original working directory
and resumes it there. The pane is auto-named from the session.

Clicking a **running** group's orchestrator session does not try to resume it —
there is nothing to resume, the conversation is open. It reveals that pane
instead, exactly as the **Focus** button above does. If the group is running
somewhere other than this window, the row says so rather than failing with the
backend's refusal. Worker and reviewer rows are unaffected: a running group
still rejoins them.

#### The name you gave the pane, and your notes

If you renamed the pane you ran a session in, that name is shown on the row
under the session's own title. It appears only when it adds something: a
session you never renamed, or one whose pane still carries the name orrerix
minted for it, shows just its title — the title *is* the fallback, never a
placeholder.

Every row also carries a small **notes** button on the right, with a count when
that session has notes. Click it for the same overlay the pane's own
[Notes button](../core-concepts.html#notes-on-an-agent-pane) opens, where you
can read, add and delete notes about that session. It works on a **dead**
session too: a note is your record *about* a session, and whether the session
can still be resumed is the CLI's business, not the note's.

The count is the number of notes orrerix has read for that session. If it
cannot read its notes file the button shows no number rather than a zero, and
says so when you hover it — a zero there would claim a session has no notes
when orrerix simply does not know.

**Orchestration sessions** in this list are marked with `ORCH` / `W` / `REV`
chips. Clicking a dead group's orchestrator session restores the *whole*
orchestration, exactly as the **Orchestrations** list does; worker/reviewer
sessions rejoin their group once it is running. Which route a click takes is
decided by the recorded membership the chip reflects, never by which CLI wrote
the session. See
[Restart after orrerix closes](../orchestration.html#persistence--restart).

#### Delegate sessions are hidden by default

A group mints a session per delegate and a fresh one on every rejoin, so a
machine that has run a few fleets accumulates hundreds of worker and reviewer
rows against the handful you would ever click. In **Orchestration** mode the
list therefore shows only **orchestrator** sessions by default; everything else
sits behind a **Show N hidden agent sessions** button under the list, which
toggles them all back on.

The two controls answer different questions and compose rather than replace each
other: the mode picks *whose* sessions, and this button then decides *how much*
of an orchestration you see. In **Mine** there are no delegates to hide, so the
button is not shown at all.

Nothing is filtered out of the *scan* — every session is still found, still
badged, and one click away. Restoring a group still brings its workers and
reviewers back: the orchestrator respawns them from the group's own roster,
which is why their individual rows are not a route you need.

The button counts what your current search left hidden, so it changes as you
type. It disappears entirely when nothing is hidden.

## Open in editor

Orrerix is a terminal, not an editor — so when you need to open files in a real
editor, the **`</>`** button in a pane header (or **`Alt+E`**) launches your
editor on that pane's current folder. The first time, you're asked for the editor
command; it's remembered after that.

- Set it to `code` (VS Code), `zed`, `subl`, or any command on your `PATH`, or a
  full path to the editor executable.
- The workspace folder is passed as the editor's sole argument, spawned detached
  — the editor keeps running independently of orrerix.
- Right-click the `</>` button any time to change the editor command.

If nothing is configured, or the editor can't be found/launched, orrerix shows a
short toast explaining what went wrong.
