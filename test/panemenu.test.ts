// Pure pane-connect-menu model (#271) — panemenu.ts. Pins the menu SHAPE across every
// pane/pending-arm state: free, connected, planner, non-capable, the armed source
// itself, and a free/connected pane while an arm is live elsewhere.
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildPaneMenu, type PaneConnectState, type PendingConnect } from "../src/panemenu.ts";

const free = (overrides: Partial<PaneConnectState> = {}): PaneConnectState => ({
  group: "g1",
  agentId: "w-1",
  name: "w-1",
  role: "worker",
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

test("right-clicking the ARMED pane again offers Cancel, not a second arm (self-click cancels)", () => {
  const pane = free();
  const pending: PendingConnect = { group: pane.group!, agentId: pane.agentId!, name: pane.name };
  const items = buildPaneMenu(pane, pending);
  assert.deepEqual(kinds(items), ["connect-cancel"]);
});

test("a DIFFERENT free pane, while another is armed, offers Connect-here (complete) — not a second arm", () => {
  const pending: PendingConnect = { group: "g0", agentId: "orch-1", name: "orch-1" };
  const items = buildPaneMenu(free(), pending);
  assert.deepEqual(kinds(items), ["connect-complete"]);
  const action = items[0].action;
  assert.equal(action?.kind, "connect-complete");
  if (action?.kind === "connect-complete") {
    assert.deepEqual(action.from, pending);
    assert.deepEqual(action.to, { group: "g1", agentId: "w-1", name: "w-1" });
  }
});

test("an ALREADY-CONNECTED pane is still a valid completion target — how a third pane joins (multi-party)", () => {
  const pending: PendingConnect = { group: "g0", agentId: "w-9", name: "w-9" };
  const items = buildPaneMenu(free({ channelId: "chan-1" }), pending);
  assert.deepEqual(kinds(items), ["connect-complete", "disconnect"]);
});

test("a connected pane with no pending arm offers only Disconnect — arming never starts from a connected pane", () => {
  const items = buildPaneMenu(free({ channelId: "chan-1" }), null);
  assert.deepEqual(kinds(items), ["disconnect"]);
});

test("disconnect carries the pane's own identity, not the peer's", () => {
  const items = buildPaneMenu(free({ channelId: "chan-1", agentId: "rev-2", name: "rev-2" }), null);
  const action = items[0].action;
  assert.equal(action?.kind, "disconnect");
  if (action?.kind === "disconnect") assert.deepEqual(action.pane, { group: "g1", agentId: "rev-2", name: "rev-2" });
});
