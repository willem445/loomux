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

## Worktrees

If the repository has [git worktrees](https://git-scm.com/docs/git-worktree)
(loomux creates one per agent session during orchestration), a **worktree chip**
appears in the header next to the repo name. Click it to switch the whole view —
graph, commits, working-tree changes, diffs, and branch — to any listed worktree,
or back to the primary checkout, **without leaving the pane or opening a new
session**. This is the quick way to see what an agent's worktree has been up to:
its history, its unstaged files, its commits.

- **Opening the view from inside a worktree** (the normal case for an
  orchestration agent pane, whose shell already sits in its own worktree) shows
  **that** worktree by default — the view follows the pane. Use the chip to look
  at any other worktree, or pin the primary.
- The chip names the worktree you're viewing; it turns the accent color when
  you're off the primary tree, so it's obvious the view is scoped elsewhere. Its
  tooltip shows the full path and branch.
- The primary checkout is labelled *(primary)* in the menu. A bare repository
  entry, or one whose directory is gone — whether git flagged it or loomux saw
  it vanish — is listed but disabled *(missing)*.
- The selection sticks across refreshes. If the worktree is pruned or removed
  while you're viewing it — even by a plain `rm -rf` that git hasn't noticed —
  the view **fails soft** to the default (the pane's own worktree if it sits in a
  live one, otherwise the primary) and tells you; that dead entry is then
  disabled in the menu. Switching the pane into a different repository resets the
  selection.
- External changes inside a *selected* worktree refresh on the next shell prompt
  in the pane or when you press **↻** — the once-a-second auto-watch tracks the
  pane's own repo.

### Read-only by default, with an explicit unlock

A worktree you didn't check out yourself is very likely a **live agent's** — and
staging, committing, discarding, or checking out under a running agent can break
its work (a discard destroys uncommitted changes; a checkout flips its branch
mid-task). So a **non-primary worktree opens read-only**: you can browse its
history, status, diffs, and branch, but every write affordance (stage/unstage,
commit, discard, checkout, push/pull, cherry-pick/revert/merge/rebase, branch/tag
create) is hidden or disabled, with a **🔒 read-only** badge in the header.

Click the badge to **unlock writes** for that worktree (it turns **🔓 writable**).
The unlock is scoped to that one selection and is dropped the moment you switch
worktrees — re-selecting it is read-only again, so you never leave writes armed
on a tree you've moved away from. The **primary checkout keeps full write access**
exactly as before.

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
