// Deterministic per-tab counting (#194 P4) — tabcounts.ts. Pins the fix for the
// demo bug (unreliable agent counter) and the live/dormant orchestration markers.
import { test } from "node:test";
import assert from "node:assert/strict";
import { tabCounts, type TabPaneInfo } from "../src/tabcounts.ts";

const p = (kind: TabPaneInfo["kind"], live = true, connectedChannel: string | null = null): TabPaneInfo => ({
  kind,
  live,
  connectedChannel,
});

test("counts only LIVE agent panes — terminals and welcome/dormant panes add nothing", () => {
  const c = tabCounts([p("agent"), p("agent"), p("terminal"), p("agent", false)], false);
  assert.equal(c.agents, 2);
  assert.equal(c.liveOrch, false);
  assert.equal(c.dormantOrch, false);
});

test("file-explorer panes are NOT agents — they never touch the count (#214)", () => {
  // A files pane is a viewer with no process. It is reported `live: true` (it IS
  // functional), which is exactly why this has to be pinned: if the counter ever
  // keyed off `live` instead of `kind`, a tab of file explorers would claim to be
  // running agents that don't exist.
  const c = tabCounts([p("files"), p("files"), p("agent")], false);
  assert.equal(c.agents, 1);
  assert.equal(c.liveOrch, false);
  assert.equal(c.dormantOrch, false);
});

test("a tab of nothing but file explorers reports no agents and no orch markers", () => {
  assert.deepEqual(tabCounts([p("files"), p("files")], false), {
    agents: 0,
    liveOrch: false,
    dormantOrch: false,
    connectedChannels: 0,
  });
});

test("editor and git panes are NOT agents either — same rule, same reason (#217)", () => {
  // The exclusion is keyed on KIND, so it extends to every content pane by construction
  // rather than by remembering. All three report `live: true` (they ARE functional), which
  // is precisely why: a counter that keyed off `live` would show a tab of viewers as a tab
  // of running agents — the exact class of bug tabcounts.ts was written to end.
  const c = tabCounts([p("editor"), p("git"), p("files"), p("agent")], false);
  assert.equal(c.agents, 1);
  assert.equal(c.liveOrch, false);
  assert.equal(c.dormantOrch, false);

  assert.deepEqual(tabCounts([p("editor"), p("git")], false), {
    agents: 0,
    liveOrch: false,
    dormantOrch: false,
    connectedChannels: 0,
  });
});

test("an empty tab (only a welcome pane) counts zero and shows no markers", () => {
  const c = tabCounts([p("terminal", false)], false);
  assert.deepEqual(c, { agents: 0, liveOrch: false, dormantOrch: false, connectedChannels: 0 });
});

test("live orchestration panes count as agents AND flag the live-orch icon", () => {
  const c = tabCounts([p("orch"), p("orch"), p("agent")], true);
  assert.equal(c.agents, 3); // 2 live orch + 1 agent
  assert.equal(c.liveOrch, true);
  assert.equal(c.dormantOrch, false); // live wins over the static marker
});

test("a tab can MIX normal agents and live orchestration (the feature's premise)", () => {
  const c = tabCounts([p("agent"), p("orch")], true);
  assert.equal(c.agents, 2);
  assert.equal(c.liveOrch, true);
});

test("a group-bound tab with no live orch pane shows the static ORCH (dormant) marker", () => {
  // The restored-but-not-resumed group: the tab kept its group binding but its
  // panes haven't been revived. This is exactly what the marker is for.
  const c = tabCounts([], true);
  assert.equal(c.agents, 0);
  assert.equal(c.liveOrch, false);
  assert.equal(c.dormantOrch, true);
});

test("a dormant orch restore placeholder flags the marker even without a group binding", () => {
  const c = tabCounts([p("orch", false)], false);
  assert.equal(c.dormantOrch, true);
  assert.equal(c.liveOrch, false);
});

test("liveOrch and dormantOrch are never both set (a live group supersedes the marker)", () => {
  const c = tabCounts([p("orch", false), p("orch", true)], true);
  assert.equal(c.liveOrch, true);
  assert.equal(c.dormantOrch, false);
});

test("a plain (no group) tab of agents never shows an orch marker", () => {
  const c = tabCounts([p("agent"), p("agent")], false);
  assert.equal(c.agents, 2);
  assert.equal(c.liveOrch, false);
  assert.equal(c.dormantOrch, false);
});

// ---------- cross-workspace channels (#271): the tab-strip dot ----------

test("a tab with no connected panes reports zero connected channels", () => {
  const c = tabCounts([p("agent"), p("orch")], true);
  assert.equal(c.connectedChannels, 0);
});

test("one connected pane counts as one channel", () => {
  const c = tabCounts([p("agent", true, "chan-1"), p("terminal")], false);
  assert.equal(c.connectedChannels, 1);
});

test("two panes in the SAME channel still count as one — distinct channels, not connected panes", () => {
  const c = tabCounts([p("agent", true, "chan-1"), p("orch", true, "chan-1")], true);
  assert.equal(c.connectedChannels, 1);
});

test("panes in two DIFFERENT channels count as two — the multi-channel case the dot exists for", () => {
  const c = tabCounts([p("agent", true, "chan-1"), p("agent", true, "chan-2")], false);
  assert.equal(c.connectedChannels, 2);
});

test("a content/terminal pane's connectedChannel (always null) never inflates the count", () => {
  const c = tabCounts([p("files"), p("editor"), p("git"), p("terminal")], false);
  assert.equal(c.connectedChannels, 0);
});
