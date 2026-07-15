// Pure pane-header context-menu model for the cross-workspace connect gesture (#271),
// including the W3 addendum's standalone-pane membership + directional (sender/receiver)
// model. DOM-free — mirrors filemenu.ts: the SHAPE of the menu (what's offered, on which
// pane, in what pending-arm/direction state) is decided and unit-tested here;
// contextmenu.ts renders it and orchestration.ts executes the fired action against the
// backend.
//
// THE GESTURE (human-only, explicit, opt-in — the issue's hard requirement). Right-click
// a free agent pane → "Connect…" arms it. Right-click a SECOND pane → the completion
// menu, which (W3) is now DIRECTIONAL: a fresh two-party connect offers two explicit-arrow
// items ("Connect: A → sends to → B" / "Connect: B → sends to → A"), each disabled with a
// reason if that side can't hold the sender role (no token — a delivery-only pane).
// Joining a channel that already has a sender offers only "Join as receiver — driven by
// {sender}" (B4: a newcomer can only ever join as a receiver). Right-clicking the ARMED
// pane again offers "Cancel connecting…" instead of a second arm — that's how a self-click
// cancels. Arming is only ever offered on a FREE pane; an already-connected pane is still a
// valid completion TARGET while another pane is armed — that's how a free THIRD pane joins
// an existing channel (multi-party), matching connect_agents' join rules (mod.rs): the
// direction of the GESTURE is always "arm the newcomer, complete on the target"; the
// direction of the CHANNEL (who may send) is a separate, explicit choice made at that
// completion moment.
//
// STANDALONE PANES (W3 part A). A standalone launcher pane now carries a channel identity
// too (`group: "__solo__"`, `agentId: "solo-N"`) once `orch_solo_prepare`/`orch_solo_bind`
// or `orch_solo_adopt` has run — so it stops hitting `NOT_CAPABLE_REASON` and is a normal
// connect target/source, full membership (claude/copilot) or delivery-only (everything
// else) exactly like an adopted pre-feature pane. `identity()` below is unchanged: the
// capability gate is still "does this pane have a group+agentId", it's just that solo panes
// now legitimately can.

import type { MenuItem } from "./contextmenu";

/** One pane's orchestration identity, as a connect action needs it — bound at
 *  arm/complete time (the same identity-vs-index discipline filemenu.ts's header
 *  describes for OpTarget), so a fired action carries a complete instruction rather
 *  than a pane reference that may have closed or rebound by the time it's read. */
export interface PaneIdentity {
  group: string;
  agentId: string;
  name: string;
  /** Whether this pane currently holds a channel-send-capable token (#271 W3
   *  addendum, part B6: "sender requires a token"). False for a delivery-only
   *  member — an adopted pre-feature pane, or a solo pane on a CLI with no MCP
   *  config seam (codex/gemini/opencode/custom). Gates whether this pane is
   *  eligible to be designated sender. */
  canSend: boolean;
  /** This pane's CURRENT channel's sender agent id/name, if it's already
   *  connected — `null` for a free pane. Drives the JOIN compatibility rule
   *  (B4): completing a connect against an already-connected pane can only
   *  ever add the newcomer as a receiver of that pane's EXISTING sender. */
  senderId: string | null;
  senderName: string | null;
  /** This pane's current channel id, or `null` if free — carried on the
   *  identity so a `set-sender` action (bound at menu-build time) has
   *  everything it needs without re-reading pane state later. */
  channelId: string | null;
}

/** The armed source of an in-progress connect gesture. There is at most one of these
 *  live at a time, globally, across every tab (channel.ts's `reduceConnect` is the
 *  state machine that maintains it). */
export type PendingConnect = PaneIdentity;

export type PaneMenuAction =
  | { kind: "connect-arm"; source: PaneIdentity }
  /** `senderAgent` is the explicit direction choice (B2) — always `from.agentId` or
   *  `to.agentId`, chosen at completion, never inferred from gesture order. */
  | { kind: "connect-complete"; from: PaneIdentity; to: PaneIdentity; senderAgent: string }
  | { kind: "connect-cancel" }
  | { kind: "disconnect"; pane: PaneIdentity }
  /** Human-only sender swap (B5) — "Make this pane the sender" on a
   *  token-holding receiver of an already-live channel. */
  | { kind: "set-sender"; pane: PaneIdentity };

export type PaneMenuItem = MenuItem<PaneMenuAction>;

/** The slice of a pane's state the menu needs — a structural subset of `Pane`'s
 *  orchestration fields, not an import of the `Pane` class (keeps this module
 *  DOM-free and node:test-loadable; see filemenu.ts's header for why a tested
 *  module can't value-import a DOM-touching sibling). */
export interface PaneConnectState {
  /** null for a pane with no channel identity at all — a shell/content pane, or a
   *  standalone launcher pane that hasn't been prepared/bound/adopted yet. Once a
   *  standalone pane HAS a channel identity (W3), this is `"__solo__"`. */
  group: string | null;
  agentId: string | null;
  name: string;
  /** "orchestrator" | "worker" | "reviewer" | "planner" | "solo", or null alongside a
   *  null `group`/`agentId`. */
  role: string | null;
  /** The channel this pane currently belongs to, or null if free. */
  channelId: string | null;
  /** Whether this pane currently holds a channel-send-capable token — see
   *  `PaneIdentity.canSend`. Irrelevant (and meaningless) for a pane with no
   *  identity at all. */
  canSend: boolean;
  /** This pane's current channel's sender, if connected — see `PaneIdentity`. */
  senderId: string | null;
  senderName: string | null;
}

const NOT_CAPABLE_REASON =
  "This pane has no connectable agent — only orchestrator, worker, reviewer, and standalone agent panes can join a channel.";
const PLANNER_REASON =
  "A planner's pane closes as soon as it reports done, so it can never join a channel.";
const CANT_BE_SENDER_REASON =
  "This pane is receive-only — it has no channel token, so it can't be the sender.";

function identity(p: PaneConnectState): PaneIdentity | null {
  return p.group !== null && p.agentId !== null
    ? {
        group: p.group,
        agentId: p.agentId,
        name: p.name,
        canSend: p.canSend,
        senderId: p.senderId,
        senderName: p.senderName,
        channelId: p.channelId,
      }
    : null;
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
    // A JOIN happens when EITHER side already belongs to a live channel — that
    // channel's sender is fixed, so the only compatible completion is "join as
    // receiver, driven by the existing sender" (B4). Both sides can't already be
    // connected here: `buildPaneMenu` never offers Connect-here as a target's
    // OWN pending-arm source (self-click is handled above), and the caller
    // (orchestration.ts) only shows this menu against a target that isn't
    // itself the armed pane.
    const existingSenderId = pending.senderId ?? id.senderId;
    const existingSenderName = pending.senderId ? pending.senderName : id.senderName;
    if (existingSenderId) {
      items.push({
        label: `Join as receiver — driven by ${existingSenderName ?? existingSenderId}`,
        action: { kind: "connect-complete", from: pending, to: id, senderAgent: existingSenderId },
      });
    } else {
      const pendingAsSender: PaneMenuItem = {
        label: `Connect: ${pending.name} → sends to → ${id.name}`,
        action: { kind: "connect-complete", from: pending, to: id, senderAgent: pending.agentId },
      };
      if (!pending.canSend) {
        pendingAsSender.disabled = true;
        pendingAsSender.reason = CANT_BE_SENDER_REASON;
      }
      const idAsSender: PaneMenuItem = {
        label: `Connect: ${id.name} → sends to → ${pending.name}`,
        action: { kind: "connect-complete", from: pending, to: id, senderAgent: id.agentId },
      };
      if (!id.canSend) {
        idAsSender.disabled = true;
        idAsSender.reason = CANT_BE_SENDER_REASON;
      }
      items.push(pendingAsSender, idAsSender);
    }
  } else if (!pane.channelId) {
    items.push({ label: "Connect…", action: { kind: "connect-arm", source: id } });
  }

  // Disconnect is independent of the arm/complete state above — a connected pane
  // can be both a valid join TARGET (a pending arm elsewhere) and disconnectable.
  if (pane.channelId && !isPendingSource) {
    items.push({ label: "Disconnect", action: { kind: "disconnect", pane: id } });
    // "Make this pane the sender" (B5): only for a token-holding receiver of an
    // already-live channel — never for the current sender itself, never for a
    // delivery-only pane.
    if (pane.senderId && pane.senderId !== id.agentId && id.canSend) {
      items.push({ label: "Make this pane the sender", action: { kind: "set-sender", pane: id } });
    }
  }

  return items;
}
