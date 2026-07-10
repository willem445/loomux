---
title: Project tabs
layout: default
parent: Features
nav_order: 6
---

# Project tabs
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

The [split grid](../core-concepts.html#the-split-grid) is *one* workspace.
**Project tabs** give you several. Each tab is a whole workspace — its own split
grid of panes and its own minimize dock — and switching tabs swaps the entire
workspace in and out, so you can keep several projects side by side without their
panes competing for screen space.

Like every loomux panel, tabs never resize the terminal underneath: a background
tab is **hidden, not torn down**, so its PTYs keep running and switching never
repaints a terminal (the same no-resize promise as maximize).

## Open, switch, close

- **New tab** — `Ctrl+Shift+T`, or the **+** at the end of the tab strip. It
  opens on the [welcome / pane-setup screen](../getting-started.html) where you
  pick the pane's kind (Agent, Orchestrator, or Terminal) — never a blank tab.
- **Switch** — click a tab, or page with `Ctrl+Shift+[` (previous) and
  `Ctrl+Shift+]` (next).
- **Close** — `Ctrl+Shift+K`, or the tab's **✕**. There's always at least one
  tab; closing the last one is refused.

Closing a tab that runs an **orchestration project** (it owns live agents) asks
for a two-step confirm first — the ✕ turns into **✕?**; click again to end the
project's agents. Plain terminal tabs close immediately.

## Background tabs keep running

An inactive tab is hidden with `display:none`, not detached — its terminals keep
streaming and its scroll history stays intact, so switching back is instant and
lossless. Agents in a background tab run untouched; their PTYs are owned by the
backend, independent of which tab is on screen.

## Name & color

- **Rename** — double-click a tab's name and type (Enter commits, Esc cancels),
  the same inline edit as pane rename.
- **Color** — click the small dot on a tab to pick an accent: one of the shared
  group colors, a custom color, or **default**. The accent marks the tab so
  projects are easy to tell apart at a glance.

## Orchestration lands in its own tab

Launch an orchestrator (welcome screen → **Orchestrator + workers**) and it opens
a **new project tab named for the repo**, rather than taking over the tab you're
on. Its workers spawn **into that tab** as the backend requests them — even while
you're looking at another project.

- A worker that **blocks on you** (a permission prompt, a question) in a *hidden*
  tab raises an unmistakable **`⚠ blocked`** / **`⚠ waiting`** chip on that tab's
  strip entry — the same label the pane header shows — so a background project
  can't hide its ask. Click the tab to jump straight to the pane.
- The tab shows a live **`✦agents · $cost`** status chip pulled from the group.
- When the orchestrator focuses an agent (or you restore its session), loomux
  **switches to that agent's tab first, then focuses the pane**.

See the [orchestration guide](../orchestration.html) for the group workflow
itself.

## Live preview

Hover a **background** tab to get a live thumbnail that composites the tab's
**whole layout** — every pane, arranged like its split, with terminal colors and
spacing intact — refreshed a few times a second so a running prompt streams in as
you watch.

The preview is a text snapshot of each pane's in-memory buffer, **never a live
terminal** — so it costs no PTY resize and honors the no-resize rule. All
mini-panes render at one consistent, readable text size; a very large pane crops
to its cell rather than shrinking illegibly. Big grids are capped (extra panes
show a small placeholder); docked panes aren't shown.

## Pause a project

Right-click a tab and choose **Pause project** to hold prompt / kickoff delivery
to that project's agents, so they idle out and stop spending while you're away;
**Resume project** re-enables delivery. A paused tab shows a **⏸**. This is the
per-tab form of the group pause described in the
[orchestration guide](../orchestration.html).

## What survives a restart

Your tabs persist across launches — their **names, colors, order, the active
tab, and each tab's bound orchestration group** — saved to durable app storage
(not the browser's), so clearing webview data doesn't lose them.

Live agent panes are **not** auto-revived on boot: relaunching every orchestrator
and worker on startup would spawn a process storm and spend credits without your
say-so. Instead a restored tab **remembers its group**, so when you restore that
group's session from the [session browser](session-browser.html) it re-inhabits
the correct tab. Restored tabs come back holding a plain shell as a placeholder
until then.
