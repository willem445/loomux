---
title: GitHub issues view
layout: default
parent: Features
nav_order: 2
---

# GitHub issues view
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

Press **`Alt+I`** (or the ◉ icon in a pane header) to overlay a GitHub **issues**
panel on the pane, scoped to the repository the shell is currently in — the
durable upstream work queue, alongside the [git view](git-view.html). Like the git
view it floats over the terminal and **never resizes it**; press `Esc` (or ✕) to
return.

## Authentication

It reads and writes through the authenticated **`gh` CLI** — loomux stores no
token or secret; `gh` uses whatever `gh auth login` you already have. If `gh`
isn't installed, or you haven't logged in, the panel says so with a one-line hint
instead of failing calls. (See [Troubleshooting](../troubleshooting.html#gh-not-found-or-not-authenticated).)

## Issues ⇄ PRs

The header carries an **Issues ⇄ PRs** toggle. Both lists share the same filter,
sort (newest-updated first), and detail pane. **PR mode is read-only** — you can
browse, open, and **comment** on PRs, but the panel never labels, merges, or
approves them (do that on GitHub or in the git view).

## What you can do

- **Browse** the repo's open issues or PRs (number, title, labels, and when each
  was last updated), newest first. The **filter box** matches on number, title,
  or label. Issue rows already carrying an agent go-signal label are marked with
  an accent stripe; PR rows show their head branch.
- **Open a detail view** by clicking a row: the full description, the whole
  comment thread, and a box to **add a comment** (`Ctrl+Enter` posts; the thread
  refreshes after). `Esc` (or ←) returns to the list. Commenting works on both
  issues and PRs. All GitHub-authored text (descriptions, comments) is rendered
  as plain text, never HTML.
- **Create** an issue (＋) from a title and optional body. `Ctrl+Enter` submits.
  (Creating is issues-only; PR mode has no ＋.)
- **Copy** any issue's URL (⧉) to the clipboard.

## Handing an issue to the orchestrator

Toggle a label directly on the row:

- **ready** applies `agent-ready` (start work), and
- **investigate** applies `agent-investigation` (research + a plan).

That's the whole handshake — a running orchestrator on this repo polls open
issues and pulls any so-labelled onto its board. No orchestrator needs to be
running when you label, since the label is durable on GitHub and picked up
whenever one next starts here. An `agent-managed` label (set by an orchestrator
that already owns the issue) is shown read-only.

If the repo doesn't have these agent labels yet, loomux **creates the one you
toggle on first use** (with its standard color and description) — so the
handshake works on a fresh repo without any manual label setup. Only these
allow-listed labels are ever created. See the
[orchestration guide](../orchestration.html#the-label-handshake) for what happens
next.

## Refreshing

The panel refreshes on open, when you switch mode, and on the ↻ button — a single
cheap `gh issue list` / `gh pr list` call (and a `gh {issue,pr} view` when you
open a detail), with no background polling.
