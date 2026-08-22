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
| **Editor** | orrerix's own editor — a file tree, project search, and a text buffer for a quick read or a one-line fix. |

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
there when you reopen it. Quitting orrerix with unsaved edits in the dock's
editor asks first, the same as everywhere else — the confirm lists it as
`side dock editor`, with the folder it belongs to.

## Opening it makes room: your panes resize to share the row

The dock takes its own column down the right-hand side, so opening it shrinks
your panes to fit beside it and closing it gives the space straight back —
the same thing the [session browser](session-browser.html) does on the left.
Nothing is hidden behind it.

That does mean the terminals in view are resized when you open or close the
dock, which full-screen tools notice: expect the same one-off repaint you get
from the session browser, once per pane per click, not a continuous cost.
Moving between panes with the dock already open costs nothing at all — the
dock re-points itself inside a column that has not moved.

It starts closed, so a fresh window is all terminal until you ask for the dock.

**Resizing:** drag the dock's left edge. The width, whether it was open, and
which tab you were on are all remembered for next time. However wide you drag
it — and whatever width it was remembered at — your terminals keep a usable
strip, including after you shrink the window or open the session browser as
well: when room runs short it is the dock that gives up width, never the panes.

**In a window too narrow for both:** with the session browser open as well,
there is a point — around a half-screen window on a 1366-wide laptop — where a
dock narrow enough to fit is too narrow to read. Rather than show you a sliver,
the dock hides itself and the **`⬔ dock`** button says why. Close the session
browser, or widen the window, and it comes straight back exactly as you left
it; it never forgets that you had it open.

## Relationship to the per-pane panels

The dock is separate from — and does not interfere with — the per-pane git
(`Alt+G`) and editor (`Alt+F`) overlays, or the panels you can dock inside a
single pane. Those belong to one pane and follow that pane. The side dock
belongs to the window and follows whichever pane you are in. You can use both
at once.
