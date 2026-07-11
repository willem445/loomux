---
title: Core concepts
layout: default
nav_order: 3
---

# Core concepts
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

## Panes

A **pane** is one slot in the grid. Most panes are a terminal — a real PTY
running a shell, an agent CLI, or anything else you'd run in a terminal, with
full color, escape-sequence, and wide-character fidelity. Panes can be **named**
(`F2`, or double-click the title) so a wall of agents stays legible.

### Pane kinds

Every pane starts on the **welcome screen**, where you pick what it becomes.
There is no global mode — each pane declares its own kind:

| Kind | What it is |
| --- | --- |
| **Agent** | A coding-agent CLI (Claude, Copilot, or your own command). Optionally fans out to *N* panes, each in its own git worktree. |
| **Orchestrator + workers** | An orchestrator pane plus idle workers, in its own project tab, with guardrails. See the [orchestration guide](orchestration). |
| **Terminal** | A plain shell — PowerShell, Command Prompt, or Git Bash. |
| **File explorer** | A native-style **file manager** rooted at a folder you choose. |

### The file explorer pane

A **file explorer** pane is loomux's Windows-Explorer equivalent, living inside a
pane. Pick a folder and you get a real file manager: browse it, open things, and
do the usual housekeeping — without leaving loomux or opening an OS Explorer
window per project.

- **Browse** — double-click a folder to go in; the breadcrumb and the **↑** button
  take you back out. `Backspace` (or `Alt+←`) goes up, arrow keys move the
  selection, `Enter` opens.
- **Double-click a file → it opens in your default app for that extension**, exactly
  like Explorer. A `.png` goes to your image viewer, a `.pdf` to your PDF reader,
  a `.docx` to Word. Loomux doesn't open it and has no opinion about its type.
- **New folder** (`Ctrl+Shift+N`), **rename** (`F2`), **delete** (`Del`).
- On Windows, delete goes to the **Recycle Bin**, so a mis-click is recoverable —
  and the confirmation says so. On macOS/Linux there's no bin, so it's permanent,
  and the confirmation says *that* instead. It never promises an undo you don't have.
- **Hidden** toggle — shows hidden files, and widens the Go-to-file index to include
  git-ignored paths (`node_modules`, build output).

This is **not** the in-app editor. That's the `Alt+F` overlay, it still works
everywhere, and it's the right tool for a quick look or a one-line fix. The
explorer is the one for *"get this file into the application that owns it."*

#### Go to file

The **Go to file** box finds a file by **name**, anywhere under the pane's root.
It's built to be instant: the folder's paths are indexed once in the background,
and each keystroke filters that index in memory.

- Type any part of a name or path — matching is plain substring, case-insensitive.
- **Several terms, separated by spaces, must all match** somewhere in the path:
  `pane rest` finds `src/panerestore.ts`, and `src pane` finds `src/pane.ts`.
- `↑` / `↓` pick a result, `Enter` opens it **in its default app**, `Esc` clears the
  box. Opening a hit also navigates you to its folder with it selected, so you end
  up somewhere useful rather than back where you started.

If more files match than the list shows, the count above it tells you — results are
never cut silently. (The same box is in the `Alt+F` editor too, where `Enter` opens
the file *in the editor* instead.)

#### The rest of the pane

It has no terminal underneath and never starts a process. That means the
terminal-oriented chrome is gone from its header (no folder or branch chip; the
git, issues, and file-editor overlays don't apply — `Alt+G` / `Alt+I` will tell you
so). Everything else is a normal pane: it splits, drags, docks, maximizes, renames,
and comes back on session restore at the same folder. It is **not** an agent, so it
never counts toward a tab's agent badge.

If the folder is gone when a session is restored (deleted, renamed, or on a drive
that isn't mounted), that pane comes back as the welcome screen with a message
instead of an empty listing — pick a new folder and carry on.

## The split grid

Loomux arranges panes as a **matrix**, not a lopsided staircase:

- **Split right** (`Ctrl+Shift+E`) adds a pane beside the current one.
- **Split down** (`Ctrl+Shift+O`) adds one below.

Splitting again *in the same direction* adds a sibling column or row, so
repeated splits build an even grid instead of nesting ever-smaller boxes.

Drag the divider between two panes to **resize** them.

### Rearranging without re-splitting

Panes get cramped fast once an orchestrator opens one per agent, so the grid can
be rearranged in place:

- **Drag to reorder or move** — grab a pane by its header and drag it over
  another. A snap preview shows where it will land:
  - drop on the **middle** to *swap* the two panes, or
  - drop on an **edge** (left/right/top/bottom half) to move the pane there,
    re-splitting the target.

  Release to drop, or press `Esc` to cancel. Swapping two equally-sized slots
  never resizes their terminals, so no scrollback is disturbed.
- **Maximize** (`Ctrl+Shift+M` or the ⤢ button) blows one pane up to fill the
  grid; the same shortcut (or the ⤡ restore button) puts it back. The other
  panes are hidden rather than shrunk, so they don't repaint. Maximize is
  **sticky**: when the orchestrator spawns an agent in the background, the new
  pane joins the grid underneath without dropping you out of fullscreen.
- **Minimize** (`Alt+M` or the — button) parks a pane in the **dock** strip at
  the bottom of the grid — it keeps running. Click its chip to bring it back, or
  the chip's ✕ to close it for good.
- **Fold a whole group** — an orchestrator pane has a fold toggle (the stacked
  panes icon) that minimizes *every* worker/reviewer pane in its group to the
  dock at once, leaving just the orchestrator. Click again to restore them all.
  Handy once a big group has opened a pane per agent and you want the screen
  back. (More in the [orchestration guide](orchestration.html).)

> **Why overlays, never re-splits, for the git/issues/board/audit panels:**
> resizing a PTY forces the program inside it to repaint, which pollutes
> scrollback. Loomux's feature panels float *over* the terminal instead, so the
> PTY box never changes size. You'll see this promise repeated across the
> feature pages — it's a core design rule.

## Project tabs

The split grid above is *one* workspace. **Project tabs** give you several: each
tab is a whole workspace — its own split grid and minimize dock — and switching
tabs swaps the entire workspace in and out, so you can keep several projects side
by side without their panes competing for space.

- **New tab** `Ctrl+Shift+T` (or the **+** in the tab strip); **close** it with
  `Ctrl+Shift+K` (or its ✕); page between tabs with `Ctrl+Shift+[` / `Ctrl+Shift+]`.
- A background tab is **hidden, not torn down** — its terminals keep running and
  its scrollback stays intact, and switching never repaints a terminal (the same
  no-resize promise as maximize).
- Launch an orchestrator and it opens **its own repo-named tab**; a blocked agent
  in a hidden tab raises an alert on its tab so a background project can't hide
  its ask.

Full details — rename/color, live previews, per-project pause, and what survives
a restart — are on the **[Project tabs](features/project-tabs.html)** feature page.

## Copy & paste

- **Copy / paste** — `Ctrl+Shift+C` / `Ctrl+Shift+V` (`Ctrl+V` also pastes).
- A CLI running in a pane (e.g. an agent that says "copied to clipboard") copies
  straight to your **system** clipboard too, via OSC 52 — no manual re-select
  needed.

## Keyboard shortcuts

The single source of truth for keybindings is `src/shortcuts.ts` in the repo;
this table mirrors it.

| Action | Shortcut |
| --- | --- |
| Split right | `Ctrl+Shift+E` (or ◫ in a pane header) |
| Split down | `Ctrl+Shift+O` (or ⬓) |
| Close pane | `Ctrl+Shift+W` (or ✕) |
| New project tab | `Ctrl+Shift+T` (or **+** in the tab strip) |
| Close project tab | `Ctrl+Shift+K` (or the tab's ✕) |
| Prev / next tab | `Ctrl+Shift+[` / `Ctrl+Shift+]` (or click a tab) |
| Rename pane | `F2`, or double-click its title |
| Move focus | `Alt+←/→/↑/↓` (or click) |
| Resize panes | drag the divider between them |
| Reorder / move panes | drag a pane by its header |
| Maximize pane | `Ctrl+Shift+M` (or ⤢); same keys restore |
| Minimize pane | `Alt+M` (or —); restore from the dock |
| Session browser | `Ctrl+Shift+P` (or the *sessions* button) |
| Open in editor | `Alt+E` (or the `</>` button in a pane header) |
| Git view | `Alt+G` (or the ⑂ icon) |
| GitHub issues view | `Alt+I` (or the ◉ icon) |
| Voice prompt | `Alt+S` (push-to-talk; `Esc` cancels) |
| Copy / paste | `Ctrl+Shift+C` / `Ctrl+Shift+V` (`Ctrl+V` also pastes) |

Orchestrator panes add a few more (steering strip, task board, audit viewer,
lifecycle panel) — those live in the [orchestration guide](orchestration.html).

## Stack (what a pane actually is)

- **Backend:** Rust + [Tauri 2](https://tauri.app) +
  [`portable-pty`](https://crates.io/crates/portable-pty) (WezTerm's PTY layer)
  — real ConPTY on Windows, forkpty on macOS/Linux.
- **Frontend:** [xterm.js](https://xtermjs.org) (the emulator VS Code uses) with
  the WebGL renderer + Unicode 11 addon, vanilla TypeScript, Vite. No UI
  framework.

On Windows the installer ships one prebuilt, MIT-licensed runtime — a modern
**ConPTY host** (`conpty.dll` + `OpenConsole.exe`) for clean terminal resize.
Voice input's whisper.cpp runtime is **not** shipped (it would add ~150 MB); it's
an opt-in download covered on the [voice prompts](features/voice-prompts.html) page.
