// Pure pane-connect-menu model (#271, plus the W3 addendum's standalone-pane +
// directional model) — panemenu.ts. Pins the menu SHAPE across every pane/pending-arm
// state: free, connected, planner, solo, delivery-only, non-capable, the armed source
// itself, a fresh two-party directional completion, and a join onto an already-driven
// channel.
//
// #407 adds the promote-to-orchestrator item and its eligibility matrix at the
// bottom of this file — a SECOND, independent gesture on the same menu, which is
// why its tests assert on the promote item alone rather than on `kinds()` of the
// whole menu.
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildPaneMenu, type PaneConnectState, type PaneMenuItem, type PendingConnect } from "../src/panemenu.ts";

const free = (overrides: Partial<PaneConnectState> = {}): PaneConnectState => ({
  group: "g1",
  agentId: "w-1",
  name: "w-1",
  role: "worker",
  channelId: null,
  canSend: true,
  senderId: null,
  senderName: null,
  // #407: an orchestration worker pane — an agent pane, but never promotable
  // (it already belongs to a group). The promote fixtures below override these.
  isAgentPane: true,
  agentCli: "claude",
  sessionId: "11111111-2222-3333-4444-555555555555",
  workdir: "/repo/poc",
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
  // `isAgentPane: false` is what makes this a SHELL pane rather than an agent
  // pane whose adopt-on-connect failed. Before #407 nothing in the state told
  // those two apart, and this fixture meant both; now they diverge (an agent
  // pane keeps a promote item — see the #407 block below), so the fixture has
  // to say which one it is. The connect assertion itself is unchanged.
  const items = buildPaneMenu(free({ group: null, agentId: null, role: null, isAgentPane: false, agentCli: null }), null);
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
  // Connect first, then #407's promote item — a claude standalone pane is
  // exactly the shape both gestures apply to, so this is the one place the two
  // halves of the menu appear together.
  assert.deepEqual(kinds(items), ["connect-arm", "promote"]);
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

test("completing onto a RECEIVER of an already-driven channel offers ONLY ONE completion item — join-as-receiver, driven by the channel's actual sender (PR #289 review round 2, B1)", () => {
  // The completion TARGET here is itself a plain RECEIVER of its own channel
  // (senderId "w-9" !== its own agentId "w-1") — the exact shape the review
  // reproduced as broken: completing the gesture on a receiver, not the
  // sender. `senderAgent` in the resulting action is "w-9" (the channel's
  // real sender, a THIRD party neither pending's "orch-1" nor target's own
  // "w-1") — this is not a leftover implementation detail, it's the fix:
  // `connect_agents` now treats a join's `sender_agent` as a CONFIRMATION of
  // the existing sender, never a requirement that it be one of the two
  // panes this call names. Verified end-to-end against the real backend by
  // `join_completing_on_a_receiver_pane_succeeds_and_keeps_the_existing_sender`
  // (tests/orchestration.rs) — before the fix that integration test failed
  // with exactly the error this menu action used to trigger
  // ("sender_agent must be one of the two connected panes").
  //
  // The target also legitimately offers Disconnect + "Make this pane the
  // sender" — independent of the join/complete state above. The join rule
  // only constrains the COMPLETION item: exactly one, not two directional
  // choices, since the channel's sender is already fixed.
  const pending = pendingFrom(); // a free armed pane
  const target = free({ channelId: "chan-1", senderId: "w-9", senderName: "w-9" });
  const items = buildPaneMenu(target, pending);
  const completions = items.filter((i) => i.action?.kind === "connect-complete");
  assert.equal(completions.length, 1, "a join onto an already-driven channel offers only one completion item");
  assert.match(completions[0].label, /driven by w-9/);
  if (completions[0].action?.kind === "connect-complete") assert.equal(completions[0].action.senderAgent, "w-9");
});

test("completing directly onto the SENDER of an already-driven channel also offers exactly one join item, naming that same sender", () => {
  // The symmetric, already-working case: here the completion target IS the
  // channel's sender (senderId === its own agentId), so senderAgent in the
  // resulting action equals target's own id — a degenerate case of the same
  // rule the test above pins for a receiver target.
  const pending = pendingFrom();
  const target = free({ channelId: "chan-1", senderId: "w-1", senderName: "w-1" }); // target IS the sender
  const items = buildPaneMenu(target, pending);
  const completions = items.filter((i) => i.action?.kind === "connect-complete");
  assert.equal(completions.length, 1);
  assert.match(completions[0].label, /driven by w-1/);
  if (completions[0].action?.kind === "connect-complete") assert.equal(completions[0].action.senderAgent, "w-1");
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

// ---------- promote to orchestrator (#407 slice B, plan step 6) ----------
//
// The eligibility matrix. Promotion is a SECOND gesture on this menu, decided
// independently of the connect state: a pane with no channel identity at all
// (adopt-on-connect failed, or the pane predates channel tools) is still a
// perfectly promotable claude session, so the promote item must be built OUTSIDE
// the connect short-circuits rather than after them.

/** A standalone claude agent pane that has been adopted as `__solo__` — the
 *  common shape at menu-build time, since `adoptIfEligible` runs before every
 *  right-click. */
const soloPane = (overrides: Partial<PaneConnectState> = {}): PaneConnectState =>
  free({ group: "__solo__", agentId: "solo-3", name: "poc", role: "solo", ...overrides });

const promoteOf = (items: PaneMenuItem[]): PaneMenuItem | undefined =>
  items.find((i) => /^Promote/.test(i.label));

test("#407: a standalone claude pane with a session and a workdir offers Promote, carrying everything the backend call needs", () => {
  const items = buildPaneMenu(soloPane(), null);
  const promote = promoteOf(items);
  assert.ok(promote, "a standalone claude pane must offer the promote item");
  assert.equal(promote.disabled, undefined);
  assert.match(promote.label, /orchestrator/i);
  assert.deepEqual(promote.action, {
    kind: "promote",
    repo: "/repo/poc",
    sessionId: "11111111-2222-3333-4444-555555555555",
    cli: "claude",
    // The `__solo__` identity the promotion retires (PromoteConfig.soloAgentId).
    soloAgentId: "solo-3",
  });
});

test("#407: an agent pane with NO channel identity yet still offers promote — the connect short-circuit must not swallow it", () => {
  // `adoptIfEligible` is best-effort: when it fails, the pane has no group/agentId
  // and `buildPaneMenu` returns the not-capable CONNECT item. Promotion does not
  // need a channel identity at all (soloAgentId is optional backend-side), so the
  // item has to survive that arm — this is the ordering trap the plan named.
  const pane = free({ group: null, agentId: null, role: null, name: "poc" });
  const items = buildPaneMenu(pane, null);
  const promote = promoteOf(items);
  assert.ok(promote, "an un-adopted claude agent pane is still promotable");
  assert.equal(promote.disabled, undefined);
  assert.deepEqual(promote.action, {
    kind: "promote",
    repo: "/repo/poc",
    sessionId: "11111111-2222-3333-4444-555555555555",
    cli: "claude",
    soloAgentId: null, // nothing to retire — there is no solo identity
  });
  // …and the connect half is unchanged: still exactly one disabled Connect item.
  const connect = items.filter((i) => !i.separator && i !== promote);
  assert.equal(connect.length, 1);
  assert.equal(connect[0].disabled, true);
});

test("#407: a non-claude agent pane offers promote DISABLED, naming the v1 limit rather than silently omitting it", () => {
  const items = buildPaneMenu(soloPane({ agentCli: "copilot" }), null);
  const promote = promoteOf(items);
  assert.ok(promote);
  assert.equal(promote.disabled, true);
  assert.equal(promote.action, undefined, "a disabled item must never carry a fireable action");
  assert.match(promote.reason ?? "", /claude/i);
});

test("#407: an agent pane whose session id loomux has not learned offers promote DISABLED — a promotion resumes a conversation", () => {
  const items = buildPaneMenu(soloPane({ sessionId: null }), null);
  const promote = promoteOf(items);
  assert.ok(promote);
  assert.equal(promote.disabled, true);
  assert.equal(promote.action, undefined);
  assert.match(promote.reason ?? "", /conversation|session/i);
});

test("#407: an agent pane with no working directory offers promote DISABLED — its cwd becomes the group's repo", () => {
  const items = buildPaneMenu(soloPane({ workdir: null }), null);
  const promote = promoteOf(items);
  assert.ok(promote);
  assert.equal(promote.disabled, true);
  assert.equal(promote.action, undefined);
  assert.match(promote.reason ?? "", /director|repositor/i);
});

test("#407: a pane that is already an orchestration member has NO promote item at all", () => {
  // Two conflicting role contracts in one session is what the backend's
  // `promote-already-managed` refuses; the frontend refuses it by never offering
  // the gesture, so a delegate's menu doesn't grow a permanently-dead row.
  for (const role of ["worker", "reviewer", "orchestrator"]) {
    const items = buildPaneMenu(free({ role }), null);
    assert.equal(promoteOf(items), undefined, `a ${role} pane must not offer promote`);
  }
});

test("#407: a planner pane offers neither promote nor connect — its single disabled item is unchanged", () => {
  const items = buildPaneMenu(free({ role: "planner" }), null);
  assert.equal(promoteOf(items), undefined);
  assert.equal(items.length, 1);
  assert.match(items[0].reason ?? "", /planner/i);
});

test("#407: a command pane that is not an agent CLI has no promote item either (rev-1 N1)", () => {
  // `isAgentPane` is "was launched with a command", which a `npm run dev` or
  // `htop` pane satisfies too. Those have no recognized CLI, and a greyed row
  // telling a build watcher that promotion is Claude-only is exactly the
  // permanently-dead row the absent-vs-disabled rule exists to avoid.
  const items = buildPaneMenu(free({ group: null, agentId: null, role: null, agentCli: null }), null);
  assert.equal(promoteOf(items), undefined);
});

test("#407: a shell / content pane has no promote item — promotion is an AGENT-pane gesture", () => {
  const items = buildPaneMenu(
    free({ group: null, agentId: null, role: null, isAgentPane: false, agentCli: null, sessionId: null }),
    null
  );
  assert.equal(promoteOf(items), undefined);
  assert.equal(items.length, 1, "a non-agent pane keeps its single not-capable item");
  assert.equal(items[0].disabled, true);
});

test("#407: promote is offered alongside an in-progress connect gesture, not swallowed by it", () => {
  // A pending arm rewrites the CONNECT half of the menu entirely (completion
  // items instead of "Connect…"); promotion is orthogonal and must survive.
  const items = buildPaneMenu(soloPane(), pendingFrom());
  assert.ok(promoteOf(items), "an armed connect elsewhere must not hide promote");
  assert.equal(kinds(items).filter((k) => k === "connect-complete").length, 2);
});
