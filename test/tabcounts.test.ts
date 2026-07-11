// Deterministic per-tab counting (#194 P4) — tabcounts.ts. Pins the fix for the
// demo bug (unreliable agent counter) and the live/dormant orchestration markers.
import { test } from "node:test";
import assert from "node:assert/strict";
import { tabCounts, type TabPaneInfo } from "../src/tabcounts.ts";

const p = (kind: TabPaneInfo["kind"], live = true): TabPaneInfo => ({ kind, live });

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
  });
});

test("an empty tab (only a welcome pane) counts zero and shows no markers", () => {
  const c = tabCounts([p("terminal", false)], false);
  assert.deepEqual(c, { agents: 0, liveOrch: false, dormantOrch: false });
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
