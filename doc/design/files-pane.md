# Design: the file-explorer pane kind (`files`)

Status: implemented (issue #214).

## Problem

The file editor from #174 is an **overlay**: `Alt+F` floats a tree + editor over a
pane's terminal, and `Esc` puts it away. That is the right shape for "quick look,
one-line fix" — but the issue asks for the other shape. Quoting it: *"to avoid
needing to open a bunch of separate file explorers."* The user wants to **park** a
browser next to their agents and leave it there, on a folder of their choosing,
across restarts.

An overlay can't be that. It belongs to a terminal, it hides that terminal while
it's open, and it goes away. So #214 makes the explorer a **pane kind** — a
fourth peer to Agent, Orchestrator, and Terminal in the welcome screen.

## The core idea: a pane that is *content*, not a *process*

Every pane before this one was ultimately a PTY with chrome around it. A files
pane has **no PTY, ever** — its content *is* the `FileEditView`.

This is less of a leap than it sounds, because #194 already built the machinery:
a welcome (setup-state) pane and a dormant restore placeholder are both panes
that exist, sit in the split tree, and hold no process. `Pane.startFiles()` is
the third member of that family, and it follows the same recipe:

- the terminal is **never opened** (`term.open()` is not called), so there is no
  ConPTY and therefore nothing that could ever be resized — CLAUDE.md hard
  constraint #1 holds by construction rather than by care;
- `.pane-term` stays in the flow, empty, and `.pane-files` covers it exactly the
  way `.pane-welcome` does. The grid, the dock, drag-to-reorder, maximize and the
  divider math are all untouched, because from their point of view nothing is
  unusual about this pane;
- `filesRoot !== null` **is** the kind flag, and it doubles as the pane's `cwd`,
  so "open in editor" (`Alt+E`) and `capture()` both target the folder on screen.

Two latent bugs fell out of the audit and are fixed here for *every* PTY-less
pane, not just the new one: `tryWebgl()` and `serializeViewportHtml()` now bail
when `term.element` is unset. Both previously threw-and-caught on a welcome or
dormant pane whenever its tab was shown or hover-previewed.

## What the overlays do — and why they're off

Every pane overlay (git, issues, tasks, audit, group, file editor) is sized
*from the terminal*: `overlayClamp` measures `termEl.clientHeight`, and
`updateTermShift` reads the live `.xterm-screen` to nudge the cursor row out from
under the panel. A files pane has no terminal, so those measurements are
meaningless and a panel would open into a zero-height box.

Making them work needs a **second sizing model** for overlays that don't assume a
terminal underneath. That is real work and it isn't what #214 is about, so the
overlays are cleanly **off** on a files pane rather than half-working:

- the git / issues / file-editor buttons carry a `pty-only` class and are hidden
  by `.pane.is-files` (as are the folder and branch chips, which describe a
  *shell's* live cwd and branch);
- the hotkey path is answered by `Pane.refuseOverlay()` with an honest toast,
  rather than silently no-oping;
- `Alt+F` on a files pane just focuses it — the pane already *is* the file editor,
  and a toast saying otherwise would be absurd.

**The git view over a files root is the one worth revisiting** — it's the natural
next ask, and it's a tracked follow-up on #214, not an oversight.

## Reusing `FileEditView` without forking it

The view is used **as-is**, including #207's streaming search. Two optional hooks
on `FileEditHost` carry the whole difference, and both exist because the overlay
semantics genuinely have no answer for the embedded case:

| Hook | Why |
| --- | --- |
| `embedded: true` | Drops the view's own ✕ and its `Esc`-to-close binding. There is nothing to close *back to* — the pane's ✕ closes it — and closing a pane on a stray `Escape` with unsaved edits in the buffer would be a nasty surprise. |
| `onRootChanged(root)` | The header's folder picker re-roots the *pane*: title and persisted record follow, so a restore reopens what was actually on screen. An overlay host deliberately does **not** implement this — there, browsing is view-local by design and must not disturb the terminal or a running agent. |

## Validating the root — at both ends

A files pane rooted at a folder that isn't there renders an empty tree and no
explanation. A terminal or agent in a bad cwd at least fails loudly in its own
output; this would just look broken. So the root is validated for real, twice:

- **at setup** — the welcome form probes it and, on failure, shows an inline error
  and puts the cursor back in the field. Same treatment a missing CLI gets.
- **at restore** — the folder may have been deleted, renamed, or be on a drive
  that isn't mounted this boot. The slot **fails soft to the welcome form** with a
  toast naming the missing folder, and the rest of the layout restores around it.

The probe is `ftRootIsDir()`, which reuses `ft_list_dir` with an empty `rel`:
`safe_resolve` already stats the root and rejects a missing or non-directory path
with `not-found`. That is exactly the question being asked, so **no backend change
was needed** — no new command, no new Rust.

## Not an agent

A files pane reports `kind: "files"` to `tabcounts.ts`, which ignores it: the
per-tab agent badge counts agents, and this is a viewer. It reports `live: true`
(it *is* fully functional the moment it exists), which is precisely why
`tabcounts` keys off **kind, not `live`** — a tab full of file explorers must not
claim to be running agents that don't exist. There's a test pinning that.

## Module map

| Piece | File | Role |
| --- | --- | --- |
| Kind + root validation | `src/panesetup.ts` | `PaneKind` gains `"files"`; `planPaneSetup` requires a root (no home fallback — a tree over `~` is never what anyone meant). Unit-tested. |
| The pane | `src/pane.ts` | `startFiles()`, `isFiles`, `workdir`, `refuseOverlay()`, the `liveKind`/`capture`/`tabPaneInfo` arms. DOM-coupled → hand-validated. |
| Placement | `src/grid.ts` | `openFilesPane()` — like `openWelcomePane`, but content instead of a form. Synchronous: there's no process to await. |
| Embedding hooks | `src/fileedit.ts` | `FileEditHost.embedded` / `onRootChanged`. |
| Root probe | `src/fileapi.ts` | `ftRootIsDir()` over the existing `ft_list_dir`. |
| Persistence | `src/tabstore.ts` | `PersistedPaneKind` gains `"files"`; the root rides in `cwd`. No version bump. Unit-tested. |
| Restore policy | `src/panerestore.ts` | The `open-files` action (root may be null → caller fails soft). Unit-tested. |
| Counting | `src/tabcounts.ts` | `"files"` in the kind union; never counted. Unit-tested. |
| Wiring | `src/main.ts`, `src/launcher.ts` | The kind picker, the submit branch, the restore branch, and the folder-field seed. |

## A small bonus: the folder field is seeded from context

The welcome form's path field now defaults to the working directory of the pane
you split *from* (or the tab's active pane) — its shell cwd, agent worktree, or
files root — falling back to the recent-repo default as before. It matters most
here: a file explorer opened beside an agent should root at *that agent's
worktree*, not at whatever repo you last launched app-wide.
