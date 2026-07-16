// Pure per-tab agent/orchestration counting (#194). DOM-free so the tab-bar
// counter — which the demo found unreliable (it sometimes rendered, sometimes
// not, with a stray "+0") — is deterministic and unit-tested (test/tabcounts.test.ts).
// The tab bar (tabbar.ts) feeds this the tab's live pane classification (from
// the grid) plus whether the tab owns an orchestration group; it renders what
// comes back and never counts from a flaky backend poll again.
//
// THE BUG this replaces: the counter was driven ONLY by a 4-second backend poll
// of a tab's bound group (groupSummary.live_agents) — so a tab with plain agent
// panes and no group showed nothing, a group not yet known to the backend showed
// nothing (the poll's try/catch skipped it), and a just-opened tab flashed 0.
// Counting the panes actually open in the tab is exact and immediate.

/** One pane's contribution to its tab's counts. Derived from the live pane
 *  (kind + whether it has a running PTY); welcome/dormant panes report
 *  `live: false` so they add nothing to the agent count. */
export interface TabPaneInfo {
  /** "files" (#214), "editor" and "git" (#217), "workflow" (#222), and "plugin" (#360
   *  Slice D) are the PTY-less CONTENT panes. None is an agent, and none ever will be,
   *  so — like a terminal — they contribute nothing to the count below, no matter what
   *  `live` says. The count keys off the KIND, not off `live`: a viewer that is fully
   *  functional (and so honestly reports live) must not thereby claim to be a running
   *  agent. The workflow pane is the sharpest case of that: it is ABOUT agents without
   *  being one — a plugin pane is a close second, hosting arbitrary third-party UI. */
  kind: "terminal" | "agent" | "orch" | "files" | "editor" | "git" | "workflow" | "plugin";
  /** True when the pane has a running PTY — a live terminal/agent. False for a
   *  setup (welcome) pane or a dormant restore placeholder (no process yet). A
   *  content pane has no process at all; it reports `live: true` because it is
   *  fully functional content, and the count ignores its kind regardless. */
  live: boolean;
  /** The cross-workspace channel (#271) this pane currently belongs to, or
   *  null/absent. Only ever set on an agent/orch pane — content and terminal
   *  panes have no MCP identity to join a channel with. */
  connectedChannel?: string | null;
}

/** What the tab strip renders for one tab. */
export interface TabCounts {
  /** Live agents open in this tab: plain agent panes plus live orchestration
   *  panes. Terminals and dormant/welcome panes never count. */
  agents: number;
  /** A live orchestration session lives in this tab → the orchestration-active
   *  icon (feature #4). A tab can mix normal agents and orchestration, so this
   *  is independent of `agents`. */
  liveOrch: boolean;
  /** The tab holds a DORMANT orchestration group — it's bound to a group that
   *  isn't currently live in any pane (a restored-but-not-resumed group), or it
   *  carries a dormant orch restore placeholder → the static ORCH marker. Never
   *  set at the same time as `liveOrch` (a live group wins). */
  dormantOrch: boolean;
  /** Distinct cross-workspace channels (#271) any pane in this tab currently
   *  belongs to — the tab-strip dot's count, so a tab spanning two separate
   *  channels shows "2" rather than collapsing to one indicator. A hidden tab's
   *  connected pane is otherwise invisible until you switch to it (its header
   *  chip is the per-pane indicator; this is the cross-tab one). */
  connectedChannels: number;
}

/** Count a tab's live agents and classify its orchestration state.
 *
 *  @param panes      every pane in the tab (visible AND docked), classified.
 *  @param groupBound whether the tab owns an orchestration group (TabManager's
 *                    groupForWorkspace) — the binding survives a restore even
 *                    when the group's panes haven't been revived, which is
 *                    exactly the dormant-group case the static marker flags. */
export function tabCounts(panes: readonly TabPaneInfo[], groupBound: boolean): TabCounts {
  let agents = 0;
  let liveOrch = false;
  let dormantOrchPane = false;
  const channelIds = new Set<string>();
  for (const p of panes) {
    if (p.kind === "agent") {
      if (p.live) agents++;
    } else if (p.kind === "orch") {
      if (p.live) {
        agents++;
        liveOrch = true;
      } else {
        dormantOrchPane = true;
      }
    }
    if (p.connectedChannel) channelIds.add(p.connectedChannel);
  }
  // Static ORCH marker: a bound-but-not-live group, or a dormant orch placeholder
  // in the layout. Suppressed the moment any orch pane is live — then the live
  // icon speaks for the tab instead.
  const dormantOrch = !liveOrch && (groupBound || dormantOrchPane);
  return { agents, liveOrch, dormantOrch, connectedChannels: channelIds.size };
}
