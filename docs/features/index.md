---
title: Features
layout: default
nav_order: 6
has_children: true
---

# Feature pages

Deep dives on loomux's per-feature panels and tools. Each one floats *over* the
terminal it's scoped to and never resizes the PTY underneath. The pane-scoped
panels close with `Esc`; the window-level ones (the session browser, the side
dock) have their own toggle in the top bar, so that `Esc` keeps reaching your
shell.

- **[Project tabs](project-tabs.html)** — several project workspaces in one
  window; each a full split grid with its own dock, previews, and per-project
  pause (`Ctrl+Shift+T`).
- **[Git view](git-view.html)** — a commit graph, diff preview, and working-tree
  staging/commit, scoped to the pane's current repository (`Alt+G`).
- **[GitHub issues view](github-issues.html)** — browse and comment on issues and PRs,
  create issues, and hand them to the orchestrator with a label (`Alt+I`).
- **[Voice prompts](voice-prompts.html)** — local, opt-in push-to-talk dictation into
  any focused target (`Alt+S`). Includes the full Windows setup, the DLL gotcha,
  and tuning.
- **[Steering & attachments](steering.html)** — the collision-proof compose strip
  under an orchestrator pane, with screenshot attachments (`Alt+P`).
- **[Session browser & editor launch](session-browser.html)** — restore past agent
  sessions into a pane (`Ctrl+Shift+P`) and open a pane's folder in your editor
  (`Alt+E`).
- **[Progress timeline](progress-timeline.html)** — a read-only time axis of an
  orchestration group's work: agents, deliveries, reports, gates, and GitHub
  issue/PR lifecycle, with the coverage boundaries stated out loud (`Alt+W`).
- **[SSH panes](ssh-panes.html)** — a remote shell or agent CLI over your own ssh
  client, with saved connections that hold no credentials, dormant-until-you-click
  reconnect, and an honest account of what a remote pane cannot do.
- **[Side dock](side-dock.html)** — git, files and the editor in one collapsible
  panel down the right edge, pointed at whichever pane you are working in
  (`⬔ dock` in the top bar).
