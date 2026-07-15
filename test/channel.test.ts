// Pure connect-gesture reducer + per-channel color/number assignment (#271, plus the
// W3 addendum's directional model) — channel.ts. Pins the arm/complete/cancel/
// self-click/set-sender state machine and the deterministic (cache-free) channel
// color/number derivation, plus the directional channel badge.
import { test } from "node:test";
import assert from "node:assert/strict";
import { reduceConnect, channelColor, channelChipLabel, channelBadge, dropIfStale } from "../src/channel.ts";
import type { PendingConnect } from "../src/panemenu.ts";

const A: PendingConnect = { group: "g1", agentId: "w-1", name: "w-1", canSend: true, senderId: null, senderName: null, channelId: null };
const B: PendingConnect = { group: "g2", agentId: "rev-3", name: "rev-3", canSend: true, senderId: null, senderName: null, channelId: null };

test("connect-arm sets pending to the source and makes no backend call", () => {
  const { pending, effect } = reduceConnect({ kind: "connect-arm", source: A }, null);
  assert.deepEqual(pending, A);
  assert.deepEqual(effect, { kind: "none" });
});

test("connect-cancel clears pending and makes no backend call", () => {
  const { pending, effect } = reduceConnect({ kind: "connect-cancel" }, A);
  assert.equal(pending, null);
  assert.deepEqual(effect, { kind: "none" });
});

test("connect-complete clears pending and hands back the connect effect with both identities and the chosen direction", () => {
  const { pending, effect } = reduceConnect({ kind: "connect-complete", from: A, to: B, senderAgent: A.agentId }, A);
  assert.equal(pending, null);
  assert.deepEqual(effect, { kind: "connect", from: A, to: B, senderAgent: "w-1" });
});

test("connect-complete carries whichever senderAgent the completion item chose, not always 'from'", () => {
  const { effect } = reduceConnect({ kind: "connect-complete", from: A, to: B, senderAgent: B.agentId }, A);
  assert.deepEqual(effect, { kind: "connect", from: A, to: B, senderAgent: "rev-3" });
});

test("disconnecting an UNRELATED pane leaves an in-progress arm untouched", () => {
  const { pending, effect } = reduceConnect({ kind: "disconnect", pane: B }, A);
  assert.deepEqual(pending, A);
  assert.deepEqual(effect, { kind: "disconnect", group: B.group, agentId: B.agentId });
});

test("disconnecting the ARMED pane itself also cancels the gesture — nothing left to complete against", () => {
  const { pending, effect } = reduceConnect({ kind: "disconnect", pane: A }, A);
  assert.equal(pending, null);
  assert.deepEqual(effect, { kind: "disconnect", group: A.group, agentId: A.agentId });
});

test("set-sender never touches the pending-arm state — it's orthogonal to the connect gesture", () => {
  const armedElsewhere: PendingConnect = { ...B };
  const target: PendingConnect = { ...A, channelId: "chan-7" };
  const { pending, effect } = reduceConnect({ kind: "set-sender", pane: target }, armedElsewhere);
  assert.deepEqual(pending, armedElsewhere, "an unrelated armed gesture must survive a set-sender action");
  assert.deepEqual(effect, { kind: "set-sender", channelId: "chan-7", newSenderAgent: "w-1" });
});

test("set-sender on a pane with no channelId (shouldn't happen, defensive) is a no-op effect", () => {
  const { effect } = reduceConnect({ kind: "set-sender", pane: A }, null);
  assert.deepEqual(effect, { kind: "none" });
});

// ---------- stale-armed-source cleanup (#286 review finding 1) ----------
// The armed pane can close (or its agent can die) mid-gesture with no dispose
// hook of its own to un-arm it — dropIfStale is the pure decision the DOM shell
// (orchestration.ts's dropStalePending) applies on the next menu-open.

test("a still-alive armed source is left completely unchanged", () => {
  assert.equal(dropIfStale(A, true), A);
});

test("a dead armed source is dropped to null", () => {
  assert.equal(dropIfStale(A, false), null);
});

test("no armed source (already null) stays null regardless of liveness", () => {
  assert.equal(dropIfStale(null, false), null);
  assert.equal(dropIfStale(null, true), null);
});

// ---------- per-channel color/number (distinguishing concurrent channels) ----------
//
// #271 follow-up (PR #285 live-testing feedback): the chip's number/color derive
// from the backend-assigned `displayNumber` (mod.rs's `Channel.display_number`),
// NOT the channel id's `chan-N` suffix — that suffix is a monotonic counter that
// never stops climbing, even across a disconnect, so it kept showing "⇄2" for the
// only active channel right after chan-1 (the actual "⇄1") closed.

test("channelColor and channelChipLabel are pure functions of the display number — same input, same output, no cache", () => {
  assert.equal(channelColor(3), channelColor(3));
  assert.equal(channelChipLabel(3), "⇄3");
});

test("two DIFFERENT display numbers get visually distinct chip labels — the multi-channel requirement", () => {
  assert.notEqual(channelChipLabel(1), channelChipLabel(2));
});

test("the color palette wraps rather than throwing once display numbers exceed the palette size", () => {
  // Must not throw, and must still return a defined color string.
  const c = channelColor(999);
  assert.equal(typeof c, "string");
  assert.ok(c.length > 0);
});

test("two distinct active channels (distinct display numbers) get distinct colors and labels", () => {
  // The reuse the backend performs (chan-1 closes, chan-3 mints as display 1)
  // must never leave two LIVE channels sharing a chip — this pins that two
  // different `displayNumber`s the frontend is handed always render distinctly.
  assert.notEqual(channelColor(1), channelColor(2));
  assert.notEqual(channelChipLabel(1), channelChipLabel(2));
});

test("channelBadge excludes the caller's own id from the peers list", () => {
  const members = [
    { agent_id: "w-1", name: "w-1", direction: "sender" as const, can_send: true, delivery_only: false },
    { agent_id: "rev-3", name: "rev-3", direction: "receiver" as const, can_send: false, delivery_only: false },
    { agent_id: "orch-1", name: "orch-1", direction: "receiver" as const, can_send: false, delivery_only: false },
  ];
  const badge = channelBadge("chan-2", 2, members, "rev-3");
  assert.deepEqual(badge.peers, ["w-1", "orch-1"]);
  assert.equal(badge.channelId, "chan-2");
  assert.equal(badge.label, "⇄2");
});

test("channelBadge's label/color come from displayNumber, NOT the channel id's numeric suffix", () => {
  // chan-7 (a high, ever-climbing id) reused down to display number 1 — the
  // chip must read "⇄1", never "⇄7".
  const members = [{ agent_id: "w-1", name: "w-1", direction: "sender" as const, can_send: true, delivery_only: false }];
  const badge = channelBadge("chan-7", 1, members, "someone-else");
  assert.equal(badge.label, "⇄1");
  assert.equal(badge.color, channelColor(1));
});

// ---------- directional badge fields (#271 W3 addendum, part C) ----------

test("the sender's own badge reads direction:sender, canSend:true, deliveryOnly:false", () => {
  const members = [
    { agent_id: "w-1", name: "w-1", direction: "sender" as const, can_send: true, delivery_only: false },
    { agent_id: "rev-3", name: "rev-3", direction: "receiver" as const, can_send: false, delivery_only: false },
  ];
  const badge = channelBadge("chan-2", 2, members, "w-1");
  assert.equal(badge.direction, "sender");
  assert.equal(badge.canSend, true);
  assert.equal(badge.deliveryOnly, false);
  assert.equal(badge.senderId, "w-1");
  assert.equal(badge.senderName, "w-1");
});

test("a receiver out of credit reads direction:receiver, canSend:false, but deliveryOnly:false — it WILL be able to reply", () => {
  const members = [
    { agent_id: "w-1", name: "w-1", direction: "sender" as const, can_send: true, delivery_only: false },
    { agent_id: "rev-3", name: "rev-3", direction: "receiver" as const, can_send: false, delivery_only: false },
  ];
  const badge = channelBadge("chan-2", 2, members, "rev-3");
  assert.equal(badge.direction, "receiver");
  assert.equal(badge.canSend, false);
  assert.equal(badge.deliveryOnly, false);
  assert.equal(badge.senderId, "w-1");
});

test("a delivery-only receiver reads deliveryOnly:true regardless of any momentary credit", () => {
  const members = [
    { agent_id: "w-1", name: "w-1", direction: "sender" as const, can_send: true, delivery_only: false },
    { agent_id: "solo-2", name: "solo codex", direction: "receiver" as const, can_send: false, delivery_only: true },
  ];
  const badge = channelBadge("chan-2", 2, members, "solo-2");
  assert.equal(badge.deliveryOnly, true);
  assert.equal(badge.canSend, false);
});

test("channelBadge falls back to a receive-only receiver if selfAgentId isn't found (defensive, should not happen live)", () => {
  const members = [{ agent_id: "w-1", name: "w-1", direction: "sender" as const, can_send: true, delivery_only: false }];
  const badge = channelBadge("chan-2", 2, members, "ghost-9");
  assert.equal(badge.direction, "receiver");
  assert.equal(badge.canSend, false);
  assert.equal(badge.deliveryOnly, true);
});
