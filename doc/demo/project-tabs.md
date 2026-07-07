# Project tabs — prototype walkthrough (#63)

> **Status: PROTOTYPE, Option A.** This is a demo build for direction feedback,
> not a finished feature. Phases 1–2 (this doc) are functional; phases 3–5 are
> stubbed with `TODO(#63 phase N)` seams. Do **not** merge — see the draft PR.

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
| `Workspace` | `src/workspace.ts` | One tab = a `Grid` + its own dock, in a container that hides via `display:none`. |
| `TabBar` | `src/tabbar.ts` | The tab strip: switch, close, new (+), rename (dbl-click), color swatch. |
| wiring | `src/main.ts` | The old module-scope single `grid` is gone; everything acts on `tabs.activeWorkspace.grid`. |
| shortcuts | `src/shortcuts.ts` | new / close / next / prev tab. |
| fit guard | `src/panefit.ts` | The pure, tested "hidden ⇒ no resize" decision. |

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

## Honest stubs / seams for phases 3–5 (worker B)

- **Orchestration routing (phase 3).** `orch-spawn-request` / `orch-focus` /
  `orch-attention` / `orch-group-ended` currently resolve to the **active** tab
  (`OrchTargetResolver` in `src/orchestration.ts`) — so agents open in whatever
  tab is focused. Phase 3 makes this route by `group_id` / `pty_id`: spawn into
  the group's own tab (auto-created on first sight), and badge a background
  tab whose agent needs attention. `TabManager.bindGroup` / `bindPty` /
  `workspaceForGroup` / `workspaceForPty` are the seams (already unit-tested).
- **Status + preview (phase 4).** No per-tab agent count / cost / attention
  badge or terminal thumbnail yet.
- **Persistence + per-tab pause (phase 5).** Tab set is in-memory only; nothing
  is restored across restarts.
