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
It has two parts: an **Orchestrations** list at the top, and a list of individual
agent sessions below it.

### Orchestrations

Every orchestration group orrerix has a record of, on every agent CLI, newest
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
| *Running now* | The group has live agents in this window | Focus its orchestrator pane |
| *Session not yet identified* | Copilot and OpenCode mint their session ids after boot, and orrerix has not learned this one yet (or its watcher timed out) | Wait for it. If the watcher timed out there is nothing to resume by hand — start a fresh orchestrator, which reattaches to this group's existing board and roster |
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

Only *your own* opencode sessions are listed — the ones a solo pane or your own
terminal created. Sessions belonging to an orchestration group live in that
group's own store and are reopened by restoring the group from the
**Orchestrations** list above, not as standalone panes: a bare
`opencode --session <id>` pane would come back with no MCP tools and no task
board.

Clicking a session opens a new pane in the session's original working directory
and resumes it there. The pane is auto-named from the session.

**Orchestration sessions** in this list are marked with `ORCH` / `W` / `REV`
chips. Clicking a dead group's orchestrator session restores the *whole*
orchestration, exactly as the **Orchestrations** list does; worker/reviewer
sessions rejoin their group once it is running. Which route a click takes is
decided by the recorded membership the chip reflects, never by which CLI wrote
the session. See
[Restart after orrerix closes](../orchestration.html#persistence--restart).

#### Agent sessions are hidden by default

A group mints a session per delegate and a fresh one on every rejoin, so a
machine that has run a few fleets accumulates hundreds of worker and reviewer
rows against the handful you would ever click. By default the list shows only
**your own sessions and orchestrator sessions**; everything else sits behind a
**Show N hidden agent sessions** button under the list, which toggles them all
back on.

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
