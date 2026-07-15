// Pure connect-gesture reducer + per-channel color/number assignment for #271's
// cross-workspace channel UI. DOM-free (node:test loadable), mirrors watchline.ts /
// orchbadge.ts. Holds no state itself: there is at most one armed connect gesture live
// at a time, globally, across every tab, and orchestration.ts keeps that single
// module-level `pending` variable — calling `reduceConnect` on every fired
// `PaneMenuAction` to get the next pending state plus whatever backend call (if any)
// needs to happen. Keeping the reducer pure (explicit state in, next state + effect
// out) is what makes the arm/complete/cancel/self-click machine unit-testable without
// simulating a DOM (CLAUDE.md's testing convention).

import type { PaneMenuAction, PendingConnect } from "./panemenu";
import type { PaneChannelBadge } from "./pane";

export type ConnectEffect =
  | { kind: "none" }
  | { kind: "connect"; from: PendingConnect; to: PendingConnect }
  | { kind: "disconnect"; group: string; agentId: string };

/** One fired pane-menu action → the next pending-arm state, plus what to actually do.
 *  `connect-arm` and `connect-cancel` only change the pending state (arming is a pure
 *  UI gesture — no backend call until a SECOND pane completes it). `connect-complete`
 *  clears pending and hands back the `connect` effect; `disconnect` hands back the
 *  `disconnect` effect and ALSO clears pending if the disconnected pane happened to be
 *  the armed source (there is nothing left to complete against). */
export function reduceConnect(
  action: PaneMenuAction,
  pending: PendingConnect | null
): { pending: PendingConnect | null; effect: ConnectEffect } {
  switch (action.kind) {
    case "connect-arm":
      return { pending: action.source, effect: { kind: "none" } };
    case "connect-cancel":
      return { pending: null, effect: { kind: "none" } };
    case "connect-complete":
      return { pending: null, effect: { kind: "connect", from: action.from, to: action.to } };
    case "disconnect":
      return {
        pending: pending && pending.agentId === action.pane.agentId ? null : pending,
        effect: { kind: "disconnect", group: action.pane.group, agentId: action.pane.agentId },
      };
  }
}

// ---------- per-channel color/number chip (#271: distinguish concurrent channels) ----------

// Reuses orchbadge.ts's GROUP_COLORS palette values (kept as a separate literal here,
// not an import: orchbadge.ts's palette is keyed by insertion-order group id, this one
// by a channel's OWN numeric suffix — different indexing scheme, same visual set, and
// importing would suggest a coupling that doesn't exist).
const CHANNEL_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e"];

/** Channel ids are backend-minted `chan-N` (mod.rs's `channel_seq`, a monotonic
 *  `AtomicU32`, never reused) — so, unlike orchbadge.ts's per-group colors (arbitrary
 *  ids, needing an insertion-order cache), a channel's color/number is a pure function
 *  of its OWN id: no cache, no reset-between-tests seam needed. Falls back to 0 for a
 *  malformed id (a payload from a future backend shape) rather than throwing — a
 *  channel chip is decoration, never worth crashing the pane header over. */
export function channelNumber(channelId: string): number {
  const m = /^chan-(\d+)$/.exec(channelId);
  return m ? parseInt(m[1], 10) : 0;
}

export function channelColor(channelId: string): string {
  return CHANNEL_COLORS[channelNumber(channelId) % CHANNEL_COLORS.length];
}

/** The pane header chip's text for a channel — short enough to sit before the title
 *  without crowding the role badge, and numbered (not just colored) so the indicator
 *  still disambiguates concurrent channels for a human who can't easily tell two
 *  similar accent colors apart. */
export function channelChipLabel(channelId: string): string {
  return `⇄${channelNumber(channelId)}`;
}

/** Build the pane header's channel badge (pane.ts's `setConnected` input) from a
 *  channel id and its member list, for whichever member `selfAgentId` is — used both
 *  by the live `orch-channel` event handler and by the on-open rehydration read
 *  (`channelForPane`), so the two paths can't render the chip differently. */
export function channelBadge(
  channelId: string,
  members: readonly { agent_id: string; name: string }[],
  selfAgentId: string
): PaneChannelBadge {
  return {
    channelId,
    color: channelColor(channelId),
    label: channelChipLabel(channelId),
    peers: members.filter((m) => m.agent_id !== selfAgentId).map((m) => m.name),
  };
}
