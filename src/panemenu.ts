// Pure pane-header context-menu model for the cross-workspace connect gesture (#271).
// DOM-free — mirrors filemenu.ts: the SHAPE of the menu (what's offered, on which pane,
// in what pending-arm state) is decided and unit-tested here; contextmenu.ts renders it
// and orchestration.ts executes the fired action against the backend.
//
// THE GESTURE (human-only, explicit, opt-in — the issue's hard requirement). Right-click
// a free agent pane → "Connect…" arms it. Right-click a SECOND pane → "Connect here"
// completes the channel. Right-clicking the ARMED pane again offers "Cancel connecting…"
// instead of a second arm — that's how a self-click cancels. Arming is only ever offered
// on a FREE pane; an already-connected pane is still a valid completion TARGET while
// another pane is armed — that's how a free THIRD pane joins an existing channel
// (multi-party), matching connect_agents' join rules (mod.rs, W1's backend PR): the
// direction of the gesture is always "arm the newcomer, complete on the target".

import type { MenuItem } from "./contextmenu";

/** One pane's orchestration identity, as a connect action needs it — bound at
 *  arm/complete time (the same identity-vs-index discipline filemenu.ts's header
 *  describes for OpTarget), so a fired action carries a complete instruction rather
 *  than a pane reference that may have closed or rebound by the time it's read. */
export interface PaneIdentity {
  group: string;
  agentId: string;
  name: string;
}

/** The armed source of an in-progress connect gesture. There is at most one of these
 *  live at a time, globally, across every tab (channel.ts's `reduceConnect` is the
 *  state machine that maintains it). */
export type PendingConnect = PaneIdentity;

export type PaneMenuAction =
  | { kind: "connect-arm"; source: PaneIdentity }
  | { kind: "connect-complete"; from: PaneIdentity; to: PaneIdentity }
  | { kind: "connect-cancel" }
  | { kind: "disconnect"; pane: PaneIdentity };

export type PaneMenuItem = MenuItem<PaneMenuAction>;

/** The slice of a pane's state the menu needs — a structural subset of `Pane`'s
 *  orchestration fields, not an import of the `Pane` class (keeps this module
 *  DOM-free and node:test-loadable; see filemenu.ts's header for why a tested
 *  module can't value-import a DOM-touching sibling). */
export interface PaneConnectState {
  /** null for a pane with no orchestration identity — a shell/content pane, or a
   *  standalone launcher pane (#271 v1 excludes both: neither has an MCP identity
   *  to reach `channel_send`/`channel_status` with). */
  group: string | null;
  agentId: string | null;
  name: string;
  /** "orchestrator" | "worker" | "reviewer" | "planner", or null alongside a null
   *  `group`/`agentId`. */
  role: string | null;
  /** The channel this pane currently belongs to, or null if free. */
  channelId: string | null;
}

const NOT_CAPABLE_REASON =
  "This pane has no connectable agent — only orchestrator, worker, and reviewer panes can join a channel.";
const PLANNER_REASON =
  "A planner's pane closes as soon as it reports done, so it can never join a channel.";

function identity(p: PaneConnectState): PaneIdentity | null {
  return p.group !== null && p.agentId !== null ? { group: p.group, agentId: p.agentId, name: p.name } : null;
}

/** Build the pane header's context menu.
 *
 *  `pending` is the currently-armed connect source (channel.ts's module-level state,
 *  threaded in by the caller), or null if no gesture is in progress. Every action this
 *  returns carries the full identity it needs — see the module header. */
export function buildPaneMenu(pane: PaneConnectState, pending: PendingConnect | null): PaneMenuItem[] {
  const id = identity(pane);
  if (!id) return [{ label: "Connect", disabled: true, reason: NOT_CAPABLE_REASON }];
  if (pane.role === "planner") return [{ label: "Connect", disabled: true, reason: PLANNER_REASON }];

  const items: PaneMenuItem[] = [];
  const isPendingSource = pending !== null && pending.agentId === id.agentId;

  if (isPendingSource) {
    items.push({ label: "Cancel connecting…", action: { kind: "connect-cancel" } });
  } else if (pending) {
    items.push({
      label: `Connect here — pairs with ${pending.name}`,
      action: { kind: "connect-complete", from: pending, to: id },
    });
  } else if (!pane.channelId) {
    items.push({ label: "Connect…", action: { kind: "connect-arm", source: id } });
  }

  // Disconnect is independent of the arm/complete state above — a connected pane
  // can be both a valid join TARGET (a pending arm elsewhere) and disconnectable.
  if (pane.channelId && !isPendingSource) {
    items.push({ label: "Disconnect", action: { kind: "disconnect", pane: id } });
  }

  return items;
}
