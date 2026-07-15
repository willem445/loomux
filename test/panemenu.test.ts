// Pure pane-connect-menu model (#271, plus the W3 addendum's standalone-pane +
// directional model) — panemenu.ts. Pins the menu SHAPE across every pane/pending-arm
// state: free, connected, planner, solo, delivery-only, non-capable, the armed source
// itself, a fresh two-party directional completion, and a join onto an already-driven
// channel.
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildPaneMenu, type PaneConnectState, type PendingConnect } from "../src/panemenu.ts";

const free = (overrides: Partial<PaneConnectState> = {}): PaneConnectState => ({
  group: "g1",
  agentId: "w-1",
  name: "w-1",
  role: "worker",
  channelId: null,
  canSend: true,
  senderId: null,
  senderName: null,
  ...overrides,
});

const pendingFrom = (overrides: Partial<PendingConnect> = {}): PendingConnect => ({
  group: "g0",
  agentId: "orch-1",
  name: "orch-1",
  canSend: true,
  senderId: null,
  senderName: null,
  channelId: null,
  ...overrides,
});

const kinds = (items: ReturnType<typeof buildPaneMenu>) => items.filter((i) => !i.separator).map((i) => i.action?.kind);

test("a free, MCP-capable pane with no pending arm offers only Connect (arm)", () => {
  const items = buildPaneMenu(free(), null);
  assert.deepEqual(kinds(items), ["connect-arm"]);
});

test("a non-orchestration pane (shell/content) offers a single disabled item, never a live action", () => {
  const items = buildPaneMenu(free({ group: null, agentId: null, role: null }), null);
  assert.equal(items.length, 1);
  assert.equal(items[0].disabled, true);
  assert.ok(items[0].reason && items[0].reason.length > 0);
  assert.equal(items[0].action, undefined);
});

test("a planner pane offers a single disabled item naming why, even though it has an agent id", () => {
  const items = buildPaneMenu(free({ role: "planner" }), null);
  assert.equal(items.length, 1);
  assert.equal(items[0].disabled, true);
  assert.match(items[0].reason ?? "", /planner/i);
});

test("a standalone solo pane with a channel identity is capable — it offers Connect like any other agent pane", () => {
  const solo = free({ group: "__solo__", agentId: "solo-3", role: "solo", canSend: true });
  const items = buildPaneMenu(solo, null);
  assert.deepEqual(kinds(items), ["connect-arm"]);
});

test("right-clicking the ARMED pane again offers Cancel, not a second arm (self-click cancels)", () => {
  const pane = free();
  const pending: PendingConnect = pendingFrom({ group: pane.group!, agentId: pane.agentId!, name: pane.name });
  const items = buildPaneMenu(pane, pending);
  assert.deepEqual(kinds(items), ["connect-cancel"]);
});

test("a fresh two-party connect (neither side has a channel yet) offers BOTH directional items", () => {
  const pending = pendingFrom();
  const items = buildPaneMenu(free(), pending);
  assert.deepEqual(kinds(items), ["connect-complete", "connect-complete"]);
  const [a, b] = items.map((i) => i.action);
  if (a?.kind === "connect-complete" && b?.kind === "connect-complete") {
    // One item names the ARMED pane as sender, the other names THIS pane —
    // both directions offered, the human picks which arrow is correct.
    assert.deepEqual([a.senderAgent, b.senderAgent].sort(), ["orch-1", "w-1"]);
    assert.deepEqual(a.from, pending);
    assert.deepEqual(a.to, { group: "g1", agentId: "w-1", name: "w-1", canSend: true, senderId: null, senderName: null, channelId: null });
  }
  assert.ok(items.every((i) => !i.disabled), "both sides can send — neither item should be disabled");
});

test("a delivery-only side of a fresh connect is disabled as sender, with a reason, but still offered as the OTHER direction", () => {
  const pending = pendingFrom({ canSend: false }); // the armed pane has no token
  const items = buildPaneMenu(free(), pending);
  assert.equal(items.length, 2);
  const asPendingSender = items.find((i) => i.action?.kind === "connect-complete" && i.action.senderAgent === "orch-1");
  const asThisSender = items.find((i) => i.action?.kind === "connect-complete" && i.action.senderAgent === "w-1");
  assert.equal(asPendingSender?.disabled, true, "the delivery-only pane can't be designated sender");
  assert.ok(asPendingSender?.reason && /receive-only|token/i.test(asPendingSender.reason));
  assert.equal(asThisSender?.disabled, undefined, "the OTHER pane (has a token) is still offered as sender");
});

test("completing onto a pane already in a channel with a sender offers ONLY ONE completion item — join-as-receiver, driven by that sender", () => {
  // The target is itself a plain RECEIVER of its own channel (senderId "w-9" !==
  // its own agentId "w-1"), so it also legitimately offers Disconnect + "Make
  // this pane the sender" — independent of the join/complete state. The join
  // rule only constrains the COMPLETION item: exactly one, not two directional
  // choices, since the channel's sender is already fixed.
  const pending = pendingFrom(); // a free armed pane
  const target = free({ channelId: "chan-1", senderId: "w-9", senderName: "w-9" });
  const items = buildPaneMenu(target, pending);
  const completions = items.filter((i) => i.action?.kind === "connect-complete");
  assert.equal(completions.length, 1, "a join onto an already-driven channel offers only one completion item");
  assert.match(completions[0].label, /driven by w-9/);
  if (completions[0].action?.kind === "connect-complete") assert.equal(completions[0].action.senderAgent, "w-9");
});

test("an ALREADY-CONNECTED pane (with a resolved sender) is still a valid completion target — how a third pane joins (multi-party)", () => {
  const pending = pendingFrom({ group: "g0", agentId: "w-9", name: "w-9" });
  const items = buildPaneMenu(free({ channelId: "chan-1", senderId: "w-1", senderName: "w-1" }), pending);
  assert.deepEqual(kinds(items), ["connect-complete", "disconnect"]);
});

test("a connected pane with no pending arm and no resolved sender offers only Disconnect — arming never starts from a connected pane", () => {
  const items = buildPaneMenu(free({ channelId: "chan-1" }), null);
  assert.deepEqual(kinds(items), ["disconnect"]);
});

test("disconnect carries the pane's own identity, not the peer's", () => {
  const pane = free({ channelId: "chan-1", agentId: "rev-2", name: "rev-2", senderId: "w-1", senderName: "w-1" });
  const items = buildPaneMenu(pane, null);
  const action = items[0].action;
  assert.equal(action?.kind, "disconnect");
  if (action?.kind === "disconnect") {
    assert.equal(action.pane.group, "g1");
    assert.equal(action.pane.agentId, "rev-2");
    assert.equal(action.pane.name, "rev-2");
  }
});

test("a token-holding RECEIVER of a live channel also gets 'Make this pane the sender'", () => {
  const pane = free({ channelId: "chan-1", agentId: "rev-2", name: "rev-2", canSend: true, senderId: "w-1", senderName: "w-1" });
  const items = buildPaneMenu(pane, null);
  assert.deepEqual(kinds(items), ["disconnect", "set-sender"]);
});

test("the current SENDER never gets 'Make this pane the sender' offered on itself", () => {
  const pane = free({ channelId: "chan-1", agentId: "w-1", name: "w-1", canSend: true, senderId: "w-1", senderName: "w-1" });
  const items = buildPaneMenu(pane, null);
  assert.deepEqual(kinds(items), ["disconnect"]);
});

test("a delivery-only RECEIVER never gets 'Make this pane the sender' — it has no token", () => {
  const pane = free({ channelId: "chan-1", agentId: "rev-2", name: "rev-2", canSend: false, senderId: "w-1", senderName: "w-1" });
  const items = buildPaneMenu(pane, null);
  assert.deepEqual(kinds(items), ["disconnect"]);
});
