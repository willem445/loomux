// Unit tests for orchestration pane-badge / identity derivation (issue #75).
// The bug: every spawned worker's pane badge read "W 1" / every reviewer "REV
// 1" — a per-GROUP ordinal — while the task board and roster showed the unique
// registry id (w-2, rev-5, ...), so a human couldn't cross-reference a pane to
// its board row. The fix derives the badge label from the real minted agent id.
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { badgeFor, agentSeq, metaForGroup, resetGroupMeta } from "../src/orchbadge.ts";

type Role = "orchestrator" | "worker" | "reviewer" | "planner";
const req = (group_id: string, agent_id: string, role: Role) => ({ group_id, agent_id, role });

test.beforeEach(() => resetGroupMeta());

// --- The core regression: distinct agents get distinct badges ------------

test("two workers in the SAME group get DISTINCT badges from their real ids", () => {
  // The bug: both were "W 1" because the label used the group's ordinal.
  const w2 = badgeFor(req("g1", "w-2", "worker"));
  const w8 = badgeFor(req("g1", "w-8", "worker"));
  assert.equal(w2.label, "W 2");
  assert.equal(w8.label, "W 8");
  assert.notEqual(w2.label, w8.label, "same-group workers must not share a badge");
});

test("badge seq cross-references 1:1 to the task-board/roster id", () => {
  // Board/roster show `w-7`; the badge shows `W 7` — same discriminating number.
  for (const [id, label] of [
    ["w-7", "W 7"],
    ["rev-5", "REV 5"],
    ["orch-1", "ORCH 1"],
    ["plan-3", "PLAN 3"],
  ] as const) {
    const role = ({ w: "worker", rev: "reviewer", orch: "orchestrator", plan: "planner" } as const)[
      id.slice(0, id.lastIndexOf("-")) as "w" | "rev" | "orch" | "plan"
    ];
    assert.equal(badgeFor(req("g1", id, role)).label, label, `${id} → ${label}`);
    assert.equal(agentSeq(id), label.split(" ")[1], `${id} seq`);
  }
});

test("each role maps to its short uppercase tag", () => {
  assert.equal(badgeFor(req("g", "w-1", "worker")).label.split(" ")[0], "W");
  assert.equal(badgeFor(req("g", "rev-1", "reviewer")).label.split(" ")[0], "REV");
  assert.equal(badgeFor(req("g", "orch-1", "orchestrator")).label.split(" ")[0], "ORCH");
  assert.equal(badgeFor(req("g", "plan-1", "planner")).label.split(" ")[0], "PLAN");
});

test("an unknown role degrades to AGENT, never throws", () => {
  const b = badgeFor(req("g", "x-9", "spectator" as Role));
  assert.equal(b.label, "AGENT 9");
});

// --- agentSeq parsing edge cases -----------------------------------------

test("agentSeq extracts the numeric suffix", () => {
  assert.equal(agentSeq("w-2"), "2");
  assert.equal(agentSeq("rev-15"), "15");
});

test("agentSeq falls back to the whole id when it isn't prefix-seq shaped", () => {
  // Never silently drop identity: a hand-named or legacy id shows in full.
  assert.equal(agentSeq("orchestrator"), "orchestrator");
  assert.equal(agentSeq("w-"), "w-");
  assert.equal(agentSeq("my-agent-a"), "my-agent-a");
});

// --- The title tooltip carries the FULL id + group -----------------------

test("badge title spells out role, full id, and group for hover disambiguation", () => {
  const b = badgeFor(req("group-xyz", "w-7", "worker"));
  assert.equal(b.title, "worker · w-7 · group group-xyz");
});

// --- Group color: pairing cue, stable per group --------------------------

test("all agents in a group share one accent color (the group-pairing cue)", () => {
  const orch = badgeFor(req("g1", "orch-1", "orchestrator"));
  const worker = badgeFor(req("g1", "w-2", "worker"));
  const rev = badgeFor(req("g1", "rev-3", "reviewer"));
  assert.equal(orch.color, worker.color);
  assert.equal(worker.color, rev.color);
});

test("distinct groups get distinct colors in first-seen order", () => {
  const a = badgeFor(req("gA", "w-1", "worker"));
  const b = badgeFor(req("gB", "w-2", "worker"));
  assert.notEqual(a.color, b.color);
  // Re-deriving gA keeps its original color (order is remembered, not re-minted).
  assert.equal(badgeFor(req("gA", "w-9", "worker")).color, a.color);
});

test("the color palette wraps after it is exhausted rather than going undefined", () => {
  // 6 colors in the palette; the 7th group must still get a defined color.
  let seventh = "";
  for (let i = 1; i <= 7; i++) seventh = metaForGroup(`grp-${i}`).color;
  assert.equal(metaForGroup("grp-1").color, seventh, "7th group wraps onto the 1st color");
  assert.ok(seventh.startsWith("#"));
});

// --- Restore / rejoin: badge reflects whatever id the registry assigned ---

test("a rejoined session shows the NEW minted id (badge tracks the registry, not a cache)", () => {
  // On rejoin the backend mints a fresh seq (spawn_agent_ex always allocates),
  // so a worker that was w-2 comes back as, say, w-11. The pane must show the
  // id it currently has — the badge is derived per-open, so it does.
  const before = badgeFor(req("g1", "w-2", "worker"));
  const afterRejoin = badgeFor(req("g1", "w-11", "worker"));
  assert.equal(before.label, "W 2");
  assert.equal(afterRejoin.label, "W 11");
  assert.equal(afterRejoin.color, before.color, "same group → same color across restore");
});

// --- Rename independence: a re-bind re-badge can't clobber a human rename --

test("badgeFor ignores the pane name — re-badging never touches a renamed title", () => {
  // The human renames the pane TITLE; the badge is a separate chip derived
  // only from (group, id, role). Passing a `name` (as OrchSpawnRequest carries)
  // must not change the badge, and repeated derivation is stable — so a bind /
  // re-bind that re-runs badgeFor can never overwrite the human's title.
  const withName = badgeFor({ group_id: "g1", agent_id: "w-4", role: "worker", name: "my-renamed-pane" } as never);
  const withoutName = badgeFor(req("g1", "w-4", "worker"));
  assert.deepEqual(withName, withoutName);
  // Deterministic across repeated calls (idempotent re-bind).
  assert.deepEqual(badgeFor(req("g1", "w-4", "worker")), withoutName);
});
