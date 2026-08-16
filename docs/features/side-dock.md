---
title: Side dock
layout: default
parent: Features
nav_order: 9
---

# Side dock
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

Click **`⬔ dock`** in the top bar to open a panel down the right edge of the
window holding three tools — **Git**, **Files** and **Editor** — all pointed at
the folder of whichever pane you are currently working in. Click it again (or
the ✕ in the dock's header) to close it.

The dock is one panel for the whole window, not one per pane. Click a pane in
another split, or switch to another project tab, and the dock re-points itself
at that pane's folder.

## The three tabs

| Tab | What it is |
| --- | --- |
| **Git** | The same commit graph, diff preview and staging/commit surface as the [git view](git-view.html) — scoped to the dock's folder instead of one pane's. It refreshes when you select the tab, when you open the dock, and when you click back onto a pane in the same folder, so a commit you just made shows up. |
| **Files** | The file explorer: browse the folder, open a file in the application your OS associates with it, create, rename and delete. |
| **Editor** | loomux's own editor — a file tree, project search, and a text buffer for a quick read or a one-line fix. |

In the **Files** tab, right-clicking a file and choosing *Open in editor pane*
opens it in the dock's own **Editor** tab rather than taking a whole new pane —
the dock is the small-surface answer, so it keeps the work inside itself.

Each tab also has its own 📁 folder picker. Using one re-points the **whole
dock**, not just that tab, so the three never disagree about which folder you
are looking at.

## Following the active pane

The dock follows **which pane is active**, and it updates a moment after you
click — the short delay is deliberate, so walking the grid with `Alt+←↑↓→`
doesn't reload the git log once per keystroke.

Two things it deliberately does not do:

- **It does not move on its own.** The dock re-reads the folder only when you
  change which pane or which project tab is active — never on a timer, and never
  because something happened in a pane you were not looking at. Typing
  `cd ../other-project` in the pane you are already in does not move it. (The
  folder is read fresh at the moment you click, though, so if you `cd` and then
  come back to that pane later, the dock does land on the new folder. Use a
  tab's 📁 picker to point it somewhere by hand and it stays there until you
  click a pane in a different folder.)
- **It does not blank on a pane with no local folder.** An [SSH pane](ssh-panes.html)
  works on a directory on the remote machine, and a new empty pane has no folder
  yet; clicking either leaves the dock showing the last real folder it had.

## Unsaved edits stop the Editor tab from following

If the **Editor** tab is holding unsaved changes, re-pointing it would throw
them away — so it doesn't. The tab keeps your file, stops following, and shows a
notice naming the folder it is still on. Save (or discard) and it rejoins the
active pane the next time you click the tab.

Closing the dock never discards anything either: closing is hiding, and your
buffer, the loaded commit log and your place in the file tree are all still
there when you reopen it. Quitting loomux with unsaved edits in the dock's
editor asks first, the same as everywhere else — the confirm lists it as
`side dock editor`, with the folder it belongs to.

## It covers panes; it does not shrink them

The dock floats *over* the right-hand side of your grid rather than squeezing
it. That is on purpose: making room for it would mean resizing the terminals
underneath, and resizing a terminal makes full-screen CLI tools repaint and
dump duplicate frames into your scrollback. Nothing about opening, closing or
resizing the dock touches a running program.

The trade is that an open dock hides part of whatever is behind it. It starts
closed, it is one click away either direction, and it always leaves a strip of
grid uncovered no matter how wide you drag it.

**Resizing:** drag the dock's left edge. The width, whether it was open, and
which tab you were on are all remembered for next time. However wide you drag
it — and whatever width it was remembered at — it is capped so a strip of grid
always stays visible, including after you shrink the window.

## Relationship to the per-pane panels

The dock is separate from — and does not interfere with — the per-pane git
(`Alt+G`) and editor (`Alt+F`) overlays, or the panels you can dock inside a
single pane. Those belong to one pane and follow that pane. The side dock
belongs to the window and follows whichever pane you are in. You can use both
at once.
