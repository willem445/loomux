// Pure connect-gesture reducer + per-channel color/number assignment (#271) —
// channel.ts. Pins the arm/complete/cancel/self-click state machine and the
// deterministic (cache-free) channel color/number derivation.
import { test } from "node:test";
import assert from "node:assert/strict";
import { reduceConnect, channelNumber, channelColor, channelChipLabel, channelBadge, dropIfStale } from "../src/channel.ts";
import type { PendingConnect } from "../src/panemenu.ts";

const A: PendingConnect = { group: "g1", agentId: "w-1", name: "w-1" };
const B: PendingConnect = { group: "g2", agentId: "rev-3", name: "rev-3" };

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

test("connect-complete clears pending and hands back the connect effect with both identities", () => {
  const { pending, effect } = reduceConnect({ kind: "connect-complete", from: A, to: B }, A);
  assert.equal(pending, null);
  assert.deepEqual(effect, { kind: "connect", from: A, to: B });
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

test("channelNumber reads the backend-minted numeric suffix", () => {
  assert.equal(channelNumber("chan-1"), 1);
  assert.equal(channelNumber("chan-42"), 42);
});

test("a malformed channel id degrades to 0 rather than throwing — decoration, not a crash", () => {
  assert.equal(channelNumber("not-a-channel"), 0);
  assert.equal(channelNumber(""), 0);
});

test("channelColor and channelChipLabel are pure functions of the id — same id, same output, no cache", () => {
  assert.equal(channelColor("chan-3"), channelColor("chan-3"));
  assert.equal(channelChipLabel("chan-3"), "⇄3");
});

test("two DIFFERENT channels get visually distinct chip labels — the multi-channel requirement", () => {
  assert.notEqual(channelChipLabel("chan-1"), channelChipLabel("chan-2"));
});

test("the color palette wraps rather than throwing once channel numbers exceed the palette size", () => {
  // Must not throw, and must still return a defined color string.
  const c = channelColor("chan-999");
  assert.equal(typeof c, "string");
  assert.ok(c.length > 0);
});

test("channelBadge excludes the caller's own id from the peers list", () => {
  const members = [
    { agent_id: "w-1", name: "w-1" },
    { agent_id: "rev-3", name: "rev-3" },
    { agent_id: "orch-1", name: "orch-1" },
  ];
  const badge = channelBadge("chan-2", members, "rev-3");
  assert.deepEqual(badge.peers, ["w-1", "orch-1"]);
  assert.equal(badge.channelId, "chan-2");
  assert.equal(badge.label, "⇄2");
});
