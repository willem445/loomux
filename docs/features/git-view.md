---
title: Git view
layout: default
parent: Features
nav_order: 1
---

# Git view
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

Press **`Alt+G`** (or the ⑂ icon in a pane header) to overlay a git panel on the
pane, scoped to the repository the shell is currently in — a commit graph, a diff
preview, and the working-tree changes with staging and commit. It floats over the
terminal and never resizes it; press `Esc` (or ✕) to return.

## Layout

Drag the divider **between the graph and diff** (or **above the changes strip**)
to resize those sub-panes — handy for wide diffs or busy branch graphs. Each
divider remembers its position across sessions, and neither side can collapse
below a usable minimum. These dividers only redistribute space *inside* the
panel; its outer size never changes, so the terminal's PTY is never resized.

## Toolbar

Top-right of the graph:

| Button | Does |
| --- | --- |
| ↓ | **Pull** the current branch — fast-forward only, so it never creates a surprise merge; a diverged branch reports the conflict instead. |
| ↑ | **Push** the current branch. If it has no upstream yet, you're offered to publish it to the remote and set tracking. |
| ↻ | **Fetch** from all remotes (with prune) and refresh the view. |

## Branches, commits, and tags

- Click the **branch name** in the header to switch branches — the menu lists
  every local branch plus remote-tracking branches. Checking out a remote branch
  creates a local branch tracking it (or switches to the existing local branch of
  that name).
- **Right-click a commit** for its actions: checkout (detached), create a branch
  or tag here, cherry-pick / revert / merge / rebase onto the current branch, or
  copy the commit hash or subject.
- **Right-click a branch/tag chip** to check it out directly (double-click works
  too).

## Safety

History-changing operations (cherry-pick, revert, merge, rebase) ask for
confirmation first. If any of them hit a conflict, loomux **aborts** the
operation and leaves your working tree exactly as it was, reporting the conflict
— it never leaves you in a half-finished, conflicted state to untangle. Resolve
those in a terminal.

## Reacts to outside changes

The view (and the pane's header branch chip) also react to changes made
**outside** the pane's shell — a `git checkout`, commit, or stage run from VS
Code or another terminal shows up within a couple of seconds, without you having
to press Enter in the pane. A lightweight backend watch samples the repo's `.git`
metadata (HEAD, index, refs) once a second while a pane has the view open, and
feeds the same throttled refresh a shell prompt would; it stops when the pane
closes.
