---
title: Home
layout: default
nav_order: 1
---

# Orrerix documentation
{: .no_toc }

A dead-simple terminal multiplexer for AI agent management — without the bloat.
{: .fs-6 .fw-300 }

[Get started](getting-started.html){: .btn .btn-primary .fs-5 .mb-4 .mb-md-0 .mr-2 }
[Download the latest release](https://github.com/willem445/loomux/releases/latest){: .btn .fs-5 .mb-4 .mb-md-0 }

---

The name comes from the **orrery**: a desk-sized geared model of the solar
system, like the Whipple Museum's [Grand Orrery](https://www.whipplemuseum.cam.ac.uk/explore-whipple-collections/astronomy/grand-orrery) — George Adams, London,
c. 1750 — where every planet and moon runs its own track at its own period, and
the whole model stays in phase because one mechanism drives all of it. Here it
is a matrix of terminal panes, each carrying an agent (or just a shell), with
one orchestrator holding them in phase.

Orrerix gives you Windows Terminal–class smoothness with the multiplexing
features it lacks: instant matrix splits, nameable panes, a native session
browser that restores Claude Code, GitHub Copilot CLI, and OpenCode sessions
straight into a pane, and — the headline feature — a built-in
**orchestrator/worker** workflow for running a small fleet of AI agents, each
in its own visible pane, that you gatekeep only at review and merge.

![An orrerix window with several agent panes](https://raw.githubusercontent.com/willem445/loomux/main/sample.jpg)

## What's here

- **[Getting started](getting-started.html)** — install, first launch, first agent pane.
- **[Core concepts](core-concepts.html)** — panes, the split grid, and the full
  keyboard-shortcut table.
- **[Orchestration guide](orchestration.html)** — agent groups, the task board, and
  the `agent-ready` / `agent-investigation` label handshake.
- **[Autonomous & supervised modes](autonomous-mode.html)** — the idle-tick
  autonomous mode, the token budget, auto-merge / auto-release, and supervised
  dangerous mode.
- **Feature pages** — [git view](features/git-view.html),
  [GitHub issues view](features/github-issues.html),
  [voice prompts](features/voice-prompts.html),
  [steering & attachments](features/steering.html), the
  [session browser & editor launch](features/session-browser.html), and the
  [side dock](features/side-dock.html).
- **[Troubleshooting](troubleshooting.html)** — the classics: whisper DLLs, `gh`
  auth, mic permission, disk.

## For contributors

This site is the **user** guide. If you want to build on orrerix, the developer
docs stay in the repository:

- [`README.md`](https://github.com/willem445/loomux/blob/main/README.md) — the
  pitch, the stack, and the build/run commands.
- [`doc/design/architecture.md`](https://github.com/willem445/loomux/blob/main/doc/design/architecture.md)
  — the source tree, module by module, and the extension seams.
- [`CLAUDE.md`](https://github.com/willem445/loomux/blob/main/CLAUDE.md) — the
  hard constraints and code conventions for working in this codebase.
- [`doc/design/`](https://github.com/willem445/loomux/tree/main/doc/design) —
  per-feature design notes (why things are built the way they are).

> This documentation describes only what ships on `main` today. Where a feature
> is still in flight, the page says so rather than describing something that
> isn't there yet.
