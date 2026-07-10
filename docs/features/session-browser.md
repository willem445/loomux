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
It scans the local machine for resumable agent sessions:

- **Claude Code** — `~/.claude/projects/*/*.jsonl` (titled by the first real
  prompt, resumed with `claude --resume <id>`).
- **Copilot CLI** — `~/.copilot/session-state/*/workspace.yaml` (resumed with
  `copilot --resume <id>`).

Clicking a session opens a new pane in the session's original working directory
and resumes it there. The pane is auto-named from the session.

**Orchestration sessions** are marked with `ORCH` / `W` / `REV` chips. Clicking a
dead group's orchestrator session restores the *whole* orchestration — same group
id, state, task board, and audit history — with fresh MCP identity wired in.
Worker/reviewer sessions rejoin their group when it's running. See
[Restart after loomux closes](../orchestration.html#persistence--restart).

## Open in editor

Loomux is a terminal, not an editor — so when you need to open files in a real
editor, the **`</>`** button in a pane header (or **`Alt+E`**) launches your
editor on that pane's current folder. The first time, you're asked for the editor
command; it's remembered after that.

- Set it to `code` (VS Code), `zed`, `subl`, or any command on your `PATH`, or a
  full path to the editor executable.
- The workspace folder is passed as the editor's sole argument, spawned detached
  — the editor keeps running independently of loomux.
- Right-click the `</>` button any time to change the editor command.

If nothing is configured, or the editor can't be found/launched, loomux shows a
short toast explaining what went wrong.
