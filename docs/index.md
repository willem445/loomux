---
title: Home
layout: default
nav_order: 1
---

# Loomux documentation
{: .no_toc }

A dead-simple terminal multiplexer for AI agent management — without the bloat.
{: .fs-6 .fw-300 }

[Get started](getting-started.html){: .btn .btn-primary .fs-5 .mb-4 .mb-md-0 .mr-2 }
[Download the latest release](https://github.com/willem445/loomux/releases/latest){: .btn .fs-5 .mb-4 .mb-md-0 }

---

*Loom* + *mux*: a loom is the frame that holds every thread in place while the
fabric is woven — here, the frame holding a matrix of terminal panes, each one
carrying an agent (or just a shell).

Loomux gives you Windows Terminal–class smoothness with the multiplexing
features it lacks: instant matrix splits, nameable panes, a native session
browser that restores Claude Code and GitHub Copilot CLI sessions straight into
a pane, and — the headline feature — a built-in **orchestrator/worker** workflow
for running a small fleet of AI agents, each in its own visible pane, that you
gatekeep only at review and merge.

![A loomux window with several agent panes](https://raw.githubusercontent.com/willem445/loomux/main/sample.jpg)

## What's here

- **[Getting started](getting-started.html)** — install, first launch, first agent pane.
- **[Core concepts](core-concepts.html)** — panes, the split grid, and the full
  keyboard-shortcut table.
- **[Orchestration guide](orchestration.html)** — agent groups, the task board, and
  the `agent-ready` / `agent-investigation` label handshake.
- **Feature pages** — [git view](features/git-view.html),
  [GitHub issues view](features/github-issues.html),
  [voice prompts](features/voice-prompts.html),
  [steering & attachments](features/steering.html), and the
  [session browser & editor launch](features/session-browser.html).
- **[Troubleshooting](troubleshooting.html)** — the classics: whisper DLLs, `gh`
  auth, mic permission, disk.

## For contributors

This site is the **user** guide. If you want to build on loomux, the developer
docs stay in the repository:

- [`README.md`](https://github.com/willem445/loomux/blob/main/README.md) — the
  stack, the build/run commands, and the architecture map.
- [`CLAUDE.md`](https://github.com/willem445/loomux/blob/main/CLAUDE.md) — the
  hard constraints and code conventions for working in this codebase.
- [`doc/design/`](https://github.com/willem445/loomux/tree/main/doc/design) —
  per-feature design notes (why things are built the way they are).

> This documentation describes only what ships on `main` today. Where a feature
> is still in flight, the page says so rather than describing something that
> isn't there yet.
