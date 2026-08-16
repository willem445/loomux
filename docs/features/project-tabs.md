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

Like every orrerix panel, tabs never resize the terminal underneath: a background
tab is **hidden, not torn down**, so its PTYs keep running and switching never
repaints a terminal (the same no-resize promise as maximize).

## Open, switch, close

- **New tab** — `Ctrl+Shift+T`, or the **+** at the end of the tab strip. It
  opens on the [welcome / pane-setup screen](../getting-started.html) where you
  pick the pane's kind (Agent, Orchestrator, or Terminal) — never a blank tab.
- **Switch** — click a tab, or page with `Ctrl+Shift+[` (previous) and
  `Ctrl+Shift+]` (next).
- **Reorder** — drag a tab to a new spot in the strip; drop it to the left or
  right half of another tab to land it there. `Ctrl+Alt+Shift+[` /
  `Ctrl+Alt+Shift+]` move the active tab one slot left/right without a mouse.
  The new order is saved with everything else a tab carries — it survives a
  restart.
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
- The tab shows a live **`✦agents · $cost`** chip. The **agent count is exact** —
  it counts the agent panes actually open in that tab (normal agents *and* live
  orchestration panes); the cost comes from the group.
- A tab running orchestration shows a **`⛓`** marker; a tab holding a **dormant**
  (restored-but-not-resumed) group shows a static **`ORCH`** chip. A tab can mix
  normal agents and orchestration, so these are independent of the agent count.
- When the orchestrator focuses an agent (or you restore its session), orrerix
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
**Resume project** re-enables delivery. A paused tab shows a **⏸**. Deliveries
that arrive while you're paused are held rather than dropped, and are delivered
when you resume — see the
[orchestration guide](../orchestration.html), which describes the group pause
this is the per-tab form of, including what happens to a very long pause.

## Restore your session on launch

Reopen orrerix with a saved session and it asks first: a **"Restore your last
session?"** splash with **Restore** and **Start fresh**. Tick *Remember my
choice* and future launches skip the splash and do what you picked; leave it
unticked to be asked again next launch. Pressing **Esc** is a non-committal
*Start fresh* — it never remembers and **leaves your saved session on disk**, so
the splash comes back next launch (a stray Escape can't quietly wipe your
session). There's no prompt when there's nothing worth restoring — you go
straight to a fresh welcome tab.

**Restore** brings back every tab — names, colors, order, the active tab, its
group binding — **and each tab's full pane layout**, split for split, with the
divider positions you'd dragged. Each pane comes back by kind:

- **Terminals** re-spawn a fresh shell in their recorded folder and shell kind
  (PowerShell / cmd / Git Bash) — instant, nothing to resume.
- **Agent panes** (Claude) **auto-resume their session** — the CLI reopens with
  its prior context loaded, into the idle TUI. Resuming **spends nothing until
  you send a prompt**, and orrerix never replays one for you. If the recorded
  session has no saved conversation (you closed it before sending a prompt, or
  the transcript was deleted), the pane comes back as a **fresh** session in the
  same spot — same folder, same agent — instead of erroring; a best-effort CLI
  with no resumable session at all comes back as a dormant pane with a **Start**
  button. That card is styled as a clear error state — a red accent, a warning
  icon, and a heading that says so — not just another neutral dormant card,
  since "nothing to resume" is worth noticing rather than looking identical to
  a group that's simply waiting for a click; the explanation of what happened
  stays visible underneath, and **Start** is still the prominent next action.
- **SSH panes** come back **dormant**, with a **Reconnect** button. Nothing dials
  out until you click it: the agent CLI at the far end runs on someone else's
  machine and spends *their* credits, and a host that's asleep or behind a VPN
  would otherwise stall startup on a connection that can take a minute to fail.
  Reconnect rebuilds the connection from the **saved profile**, so a connection
  you've edited since (a new port, a different remote folder) reconnects with the
  edit — and if you deleted the saved connection, the card says so rather than
  guessing at one. With Claude Code on the far end the reconnect **resumes the
  remote session** it recorded; other CLIs come back on a new conversation,
  because only Claude's session id travels on the command line where orrerix can
  put it. The same **Reconnect** offer appears if a live connection drops
  mid-session — the pane stays open with the output that explains why (a timeout,
  a closed connection, a refused key) and the offer floats over it, so you read
  the reason before deciding. Ending the session yourself (`exit` on the far end)
  closes the pane as usual.
- **Orchestration panes** come back **dormant**, with a **Resume group** button —
  reviving a whole group can spawn workers and spend credits, so that stays a
  deliberate, human-triggered action. The tab keeps its group binding and shows
  the `ORCH` marker until you resume. **Resume group** is one-click consent for
  the **whole group**: the orchestrator relaunches (task board, MCP identity), and
  then every worker/reviewer that had an active session **rejoins** the group and
  resumes into its idle TUI — re-registered so the orchestrator can message it,
  and spending nothing until it's given work. An idle agent that never started a
  conversation isn't restored (there's nothing to resume); the orchestrator can
  respawn one on demand. Clicking **Resume group** is acknowledged immediately —
  the button disables and spins right away, before the relaunch (session lookup,
  MCP config, PTY spawn, CLI boot) finishes — so there's no dead-feeling pause
  where nothing on screen tells you the click landed. If the group's orchestrator
  session turns out not to be resumable, the card switches to the same error
  styling as the agent case above (with the specific reason) instead of quietly
  reverting to looking like an untouched "Resume group" card; a toast covers the
  in-the-moment notice, and the card itself is what's still there if you come
  back to the tab later.
- **A tab holding two orchestration groups** (split an orchestrator tab and
  launch a second orchestrator into it, or restore a group into a tab that
  already has one) restores **both**, separately: each group gets its own
  **Resume group** card, and each card brings back that group's own
  orchestrator and its own workers. Resuming one leaves the other's card
  exactly where it was, so a group you haven't resumed yet is never mistaken
  for one that's gone. A worker is only ever rejoined into the group its own
  recorded session belongs to — if something asks for it under a different
  group, the restore **fails with a message** instead of attaching it to the
  wrong orchestrator. (An older saved tab may not record which group each pane
  belonged to; if such a tab holds two orchestrators, the card says so and
  points you at the session browser, where each session's group is known,
  rather than guessing at one.)

**Start fresh** opens a single blank welcome tab and leaves the rest behind.

Everything is saved to durable app storage (not the browser's), so clearing
webview data doesn't lose it. What is *never* captured is the live terminal
buffer/scrollback or the process itself — a pane is re-created or resumed from
its record, so its on-screen history from last session is gone (the process died
with the app). See the [design note](https://github.com/willem445/loomux/blob/main/doc/design/session-restore.md).
