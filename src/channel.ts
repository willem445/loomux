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
  /** `senderAgent` (#271 W3 addendum, part B2) is the explicit direction choice made
   *  at completion — always `from.agentId` or `to.agentId` (or, on a join, whichever
   *  agent already drives that channel), never inferred from gesture order. */
  | { kind: "connect"; from: PendingConnect; to: PendingConnect; senderAgent: string }
  | { kind: "disconnect"; group: string; agentId: string }
  /** Human-only sender swap (B5): reassign an already-live channel's sender without
   *  reconnecting. */
  | { kind: "set-sender"; channelId: string; newSenderAgent: string };

/** One fired pane-menu action → the next pending-arm state, plus what to actually do.
 *  `connect-arm` and `connect-cancel` only change the pending state (arming is a pure
 *  UI gesture — no backend call until a SECOND pane completes it). `connect-complete`
 *  clears pending and hands back the `connect` effect (with its direction); `disconnect`
 *  hands back the `disconnect` effect and ALSO clears pending if the disconnected pane
 *  happened to be the armed source (there is nothing left to complete against);
 *  `set-sender` doesn't touch the pending-arm state at all — it's a mutation on an
 *  already-live channel, orthogonal to the connect gesture. */
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
      return {
        pending: null,
        effect: { kind: "connect", from: action.from, to: action.to, senderAgent: action.senderAgent },
      };
    case "disconnect":
      return {
        pending: pending && pending.agentId === action.pane.agentId ? null : pending,
        effect: { kind: "disconnect", group: action.pane.group, agentId: action.pane.agentId },
      };
    case "set-sender":
      return {
        pending,
        effect: action.pane.channelId
          ? { kind: "set-sender", channelId: action.pane.channelId, newSenderAgent: action.pane.agentId }
          : { kind: "none" },
      };
  }
}

/** Drop the pending arm if its source pane is no longer alive (#286 review
 *  finding 1): the armed pane can close — or its agent can die, kept open only
 *  as an exit banner — mid-gesture, and `pending` is a plain identity value
 *  with no dispose hook of its own to un-arm it. `isAlive` is supplied by the
 *  caller (orchestration.ts, from `Pane.isDisposed`) since a pane's liveness
 *  isn't something a DOM-free module can observe itself; this only decides
 *  what the result SHOULD be once that fact is known. Called lazily on the
 *  next menu-open rather than wired to a close callback — the backend would
 *  reject a completion against a dead agent either way (ids are never
 *  reused), so the only real bug was the stale "pairs with `<dead name>`"
 *  label, and the very next right-click, anywhere, is exactly when that label
 *  would next be shown. */
export function dropIfStale(pending: PendingConnect | null, isAlive: boolean): PendingConnect | null {
  return pending && !isAlive ? null : pending;
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

/** One entry in a channel's member list, as the backend's `channel_members_json`
 *  serializes it (mod.rs) — `direction`/`can_send`/`delivery_only` are the #271 W3
 *  addendum's directional fields (part B7/A4). */
export interface ChannelBadgeMember {
  agent_id: string;
  name: string;
  direction?: "sender" | "receiver";
  can_send?: boolean;
  delivery_only?: boolean;
}

/** Build the pane header's channel badge (pane.ts's `setConnected` input) from a
 *  channel id and its member list, for whichever member `selfAgentId` is — used both
 *  by the live `orch-channel` event handler and by the on-open rehydration read
 *  (`channelForPane`), so the two paths can't render the chip differently.
 *
 *  `direction`/`canSend`/`deliveryOnly` describe THIS pane (`selfAgentId`'s own entry),
 *  not the peers — they drive the chip's arrow (outward for sender, inward for
 *  receiver) and the "receive-only" variant (#271 W3 addendum, part C). Default to a
 *  receiver with no send capability if `selfAgentId` isn't found in `members` (should
 *  not happen for a live channel — a defensive fallback, not a real UI state). */
export function channelBadge(
  channelId: string,
  members: readonly ChannelBadgeMember[],
  selfAgentId: string
): PaneChannelBadge {
  const me = members.find((m) => m.agent_id === selfAgentId);
  const senderMember = members.find((m) => m.direction === "sender");
  return {
    channelId,
    color: channelColor(channelId),
    label: channelChipLabel(channelId),
    peers: members.filter((m) => m.agent_id !== selfAgentId).map((m) => m.name),
    direction: me?.direction ?? "receiver",
    canSend: me?.can_send ?? false,
    deliveryOnly: me?.delivery_only ?? true,
    senderId: senderMember?.agent_id ?? null,
    senderName: senderMember?.name ?? null,
  };
}
