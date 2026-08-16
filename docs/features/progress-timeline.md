---
title: Progress timeline
layout: default
parent: Features
nav_order: 7
---

# Progress timeline
{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

---

Press **`Alt+W`** (or the timeline icon — dots over an axis — in the pane
header) on any
orchestration pane to overlay a **time axis of what your group has been doing**:
agents spawning and exiting, prompts delivered, reports coming back, task-board
transitions, review verdicts and merge gates — plus, from GitHub itself, when
issues were opened and closed and when PRs were opened, merged, or closed
unmerged.

It is **read-only**. It starts nothing, changes nothing, and adds no background
work of its own: it re-reads the same audit log the [audit
viewer](../orchestration.html) shows, plus one read-only `gh` query. Like every
other panel it floats over the terminal and never resizes it — press `Esc` (or
✕) to return, or dock it beside the terminal (see below).

The button appears on **every** pane in an orchestration group — the
orchestrator's and every worker's — because the data is the whole group's.

## Reading the chart

Each dot is one event, placed at the instant it happened. Dots are grouped into
**lanes** by category, top to bottom:

| Lane | What lands there |
| --- | --- |
| **group** | the group being created, resumed, paused, ended — and incoming intake signals |
| **agents** | agents spawned and exited |
| **work** | prompts delivered to an agent, reports coming back, task-board status changes |
| **gates** | review verdicts, merge gates, release gates |
| **GitHub** | issues opened/closed, PRs opened/merged/closed-unmerged |
| **ops** | everything with no lane of its own — delivery plumbing, compaction, watch registrations. **Off by default** |

When several events land in the same spot, they collapse into one **cluster**
dot labelled with how many it holds. Hover any dot for a one-line summary;
**click** it to list its events underneath the chart, and click a row there to
see the raw record behind it.

## Choosing what you see

- **Window presets** — `1h`, `6h`, `12h` (the default), `24h`, `72h`, `All`.
  `All` stretches back to the oldest event that was loaded.
- **Category chips** — one per lane. Click to hide or show that lane. Turning
  a lane off never unloads anything; the chart says which lanes are off,
  underneath it.
- **▶ follow** — re-poll for new activity. GitHub is re-read on a much slower
  cadence than the audit log (it shells out to `gh`), and **⟳** forces both
  immediately.

## What it will tell you it *isn't* showing

A chart is uniquely good at looking complete, so this one states its own
boundaries in plain sentences under the axis. You may see any of:

- **the audit log is loaded at its cap** — the newest 5000 entries are in;
  anything older than the instant named is not.
- **audit coverage starts at *T*** — the window reaches further back than the
  log does. Empty space to the left of that instant means *not recorded*, not
  *nothing happened*.
- **GitHub issues/PRs capped at the 100 most recently active**, together with
  **complete back to *T*** — nothing left out by that cap has been active since
  *T*.
- **GitHub activity unavailable** — the `gh` read failed (not signed in, no
  network, not a GitHub repo). The audit half of the chart is unaffected and
  still complete; only issue and PR dots are missing.
- ***n* events carried no usable timestamp** — some agent shims fall back to a
  zero timestamp when they can't read the clock. Those events are counted here
  rather than plotted at 1970.
- ***n* audit rows could not be read** — a corrupted line never blanks the view.
- **lanes switched off** — which categories you have hidden.

If a cluster holds more events than the detail list shows, it says how many it
left out. Nothing here is ever silently dropped.

## Docking it beside the terminal

The ⬒ button docks the timeline to the **left**, **right** or **bottom** edge of
the pane instead of floating it, with a draggable divider — the same mechanism
every other panel uses (up to three docked at once, one per edge). A docked
timeline re-draws itself to whatever width you give it.

If the timeline is docked on an orchestrator pane when you quit, it comes back
docked on the same edge next launch. Your window preset and category chips are
not restored — you get the 12-hour default again, the same way a restored audit
log comes back without its filters.

## What it can't tell you

- **Only what the audit log and GitHub recorded.** It doesn't read agent
  transcripts, so "the agent thought about X for ten minutes" isn't here.
- **A merge gate is not a merge.** Orrerix audits its *permission* to merge and
  then runs `gh`, which can still fail. A dot reading "merge allowed" means
  exactly that; only a GitHub `merged` dot means the PR actually merged.
- **A first kickoff and a later delivery look the same**, because the audit
  record for them is the same shape. Both appear as deliveries, labelled with
  who sent them.
