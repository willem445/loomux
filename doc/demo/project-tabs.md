# Project tabs — prototype walkthrough (#63)

> **Status: PROTOTYPE, Option A.** This is a demo build for direction feedback,
> not a finished feature. All five phases are now wired end-to-end at
> prototype/demo quality (see the honest limits near the end). Do **not** merge —
> see the draft PR.

## What this is

Each **project tab** is a full workspace — its own split grid of panes and its
own minimize dock. Switching tabs swaps the whole workspace in and out. The
design (`Option A` in the issue #63 investigation) reuses loomux's existing,
tested pieces:

- **Grid/Pane are unchanged.** A tab wraps one `Grid` (`src/grid.ts`) exactly as
  the app always had, so splitting, dragging, minimizing, maximizing, agent
  panes, and orchestration all keep working *inside* a tab.
- **Switching is `display:none`, not teardown.** An inactive tab is hidden, not
  detached — its panes and scrollback stay alive, its PTYs keep running
  (they're backend-owned). Hiding drops every pane to zero width, which is the
  same mechanism maximize uses to avoid ConPTY resize repaints (see the
  invariant below).

## Architecture (phase 1–2)

| Piece | File | Role |
| --- | --- | --- |
| `TabManager` | `src/tabs.ts` | Ordered tab list, active tab, never-zero-tabs, phase-3 routing seams. DOM-free / unit-tested. |
| `Workspace` | `src/workspace.ts` | One tab = a `Grid` + its own dock; hides via `display:none`, drops WebGL when hidden, snapshots its viewport for previews. |
| `TabBar` | `src/tabbar.ts` | The tab strip: switch, close, new (+), rename, color, attention dot, status chip, hover thumbnail, right-click pause menu. |
| wiring | `src/main.ts` | The old module-scope single `grid` is gone; everything acts on `tabs.activeWorkspace.grid`. Owns the `OrchWiring` router + persistence + preview timer. |
| shortcuts | `src/shortcuts.ts` | new / close / next / prev tab. |
| fit guard | `src/panefit.ts` | The pure, tested "hidden ⇒ no resize" decision. |
| routing/preview | `src/tabroute.ts` | Pure: cross-tab attention → tab badge, focus-switches-tab, preview throttle. |
| persistence | `src/tabstore.ts` | Pure encode/decode of the saved tab set; stored in `localStorage` (`loomux.tabs`). |

## The load-bearing invariant (CLAUDE.md constraint 1)

A hidden tab's panes must issue **no** PTY resize — resizing ConPTY repaints the
screen into scrollback on the Win10 inbox conhost. Hidden containers report
zero width, and the pure `shouldResizePty` (`src/panefit.ts`) returns `false` for
any zero-width pane. This is the exact maximize precedent, now covering tabs.

- `test/panefit.test.ts` — a zero-width pane never resizes, even when the fitted
  size "changed".
- `test/tabs.test.ts` — switching only ever sets non-active tabs invisible
  (never re-shows them while inactive); switching never disposes a tab.

## Demo steps (phases 1–2)

**Tabs & panes (phase 1)**

1. Launch loomux. You start with one default tab (**Tab 1**) holding one pane —
   identical to before.
2. Press **Ctrl+Shift+T** (or click **+** in the tab strip) → a second tab opens
   and activates, with its own fresh pane.
3. Split panes in each tab (**Ctrl+Shift+E / O**), toggle agent mode
   (**Ctrl+Shift+A**) and open an agent pane. Each tab keeps its own layout.
4. Switch tabs with **Ctrl+Shift+[** / **Ctrl+Shift+]** (or click a tab). The
   other tab's terminals keep running in the background — scroll history is
   intact when you switch back, and nothing repaints/reflows on switch.
5. Maximize (**Ctrl+Shift+M**), minimize (**Alt+M**), and the dock all work
   per-tab.
6. **Ctrl+Shift+K** (or the tab's ✕) closes a tab and kills its panes. Closing
   the **last** tab is refused — there's always at least one.

**Rename & color (phase 2)**

7. Double-click a tab's name → inline rename (Enter commits, Esc cancels), the
   same UX as pane rename.
8. Click the small color dot on a tab → pick one of the shared group colors, a
   custom color, or **default**. The active tab shows the accent on its top edge.

**Orchestration ↔ tabs (phase 3)**

9. Turn on agent mode (**Ctrl+Shift+A**) and launch an **orchestrator** from the
   launcher. It opens in a **new project tab named for the repo** (not the tab
   you were on). Its workers spawn **into that same tab** as the backend requests
   them — switch away and they still land in the project's tab.
10. Trigger a worker that blocks on the human (e.g. an agent asking a question)
    and switch to another tab. The blocked agent's tab shows a **pulsing dot**
    on its strip entry (red for `blocked`, amber otherwise) — reusing the same
    attention mapping as the pane header / dock chip. Cross-tab, so a hidden
    project surfaces its ask.
11. When the orchestrator focuses an agent (or you restore a session), loomux
    **switches to that agent's tab first, then focuses the pane**.
12. End the orchestration — its (now-dead) panes close in the owning tab.

**Status, preview & pause (phases 4–5)**

13. A project tab shows a live **status chip**: `✦<agents> · $<cost>` from the
    group summary/usage, refreshed on a timer.
14. Hover a **background** tab → a small **thumbnail** of its viewport (a text
    snapshot of the terminal, refreshed on switch-away and on a throttle). It is
    a snapshot string, never a live/laid-out pane (that would re-arm the resize
    storm the whole design avoids).
15. **Right-click** a project tab → **Pause project** / **Resume project**
    (`pauseGroup`/`resumeGroup`) to hold or resume prompt delivery and contain
    unattended spend; a paused tab shows a **⏸**. The menu also has rename/close.
16. **Restart loomux** — your tabs come back with their **names, colors, and
    group bindings**. See the limits below.

## Honest limits (prototype)

- **Session/pane rehydration on restart (phase 5).** Persistence restores the
  tab *shells* — name, color, and which orchestration group each tab owns — to
  `localStorage`. It does **not** revive the live agent panes/PTYs: a restored
  project tab comes back with a plain shell, and its bound group only truly
  reconnects when you restore that group's session from the session browser
  (which now routes into the correct tab). Full layout persistence is out of
  scope for the prototype.
- **Preview is text, not pixels.** The thumbnail strips ANSI and shows the last
  lines of the serialized viewport — enough to recognize a tab at a glance, not
  a pixel-accurate mini-terminal.
- **Background tabs created while hidden keep a WebGL context** until first
  shown-then-hidden; hidden *active-then-switched* tabs drop it immediately.
  A minor resource nicety, not a correctness issue.
- **Status polling** hits the backend every few seconds per group-bound tab;
  fine for a handful of projects.

## Tests

- `test/tabs.test.ts` — TabManager: add/remove/switch, active invariant,
  never-zero-tabs, switch-is-hide-not-dispose, group/pty routing seams.
- `test/panefit.test.ts` — the no-resize invariant (hidden ⇒ no resize).
- `test/tabroute.test.ts` — cross-tab attention badge, focus-switches-tab,
  preview throttle.
- `test/tabstore.test.ts` — persistence encode/decode round-trip + validation.
