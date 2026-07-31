// Whole-group resume planning (#194 P4, demo rounds 3–4) — groupresume.ts. The
// INPUT is the CAPTURED group panes (the orch panes live at close, read off the
// tab's dormant placeholders) — NEVER the backend's full historical roster. These
// pin that ONE Resume click plans exactly that captured set: orchestrator first,
// every resumable delegate rejoined, a no-transcript delegate skipped (not
// stranded), and nothing added beyond what was captured (the round-4 regression).
import { test } from "node:test";
import assert from "node:assert/strict";
import { planGroupResume, partitionByGroup, type GroupMember } from "../src/groupresume.ts";

// One CAPTURED group member (a dormant orch placeholder's recorded session + role,
// and — since #485 — the group its own record names).
const m = (sessionId: string, role: string, groupId?: string | null): GroupMember => ({
  sessionId,
  role,
  groupId,
});

test("plans the WHOLE group: orchestrator first, workers rejoined", () => {
  const members = [m("w1", "worker"), m("orch", "orchestrator"), m("w2", "worker")];
  const plan = planGroupResume(members, () => true); // all resumable
  assert.equal(plan.orchestrator?.sessionId, "orch", "orchestrator is separated out to run first");
  assert.deepEqual(
    plan.rejoin.map((x) => x.sessionId).sort(),
    ["w1", "w2"],
    "every delegate is planned for rejoin — not just the orchestrator (the demo bug)"
  );
  assert.deepEqual(plan.skipped, []);
});

test("fallback per member: a delegate with no transcript is skipped, not stranded", () => {
  // w2 was never prompted → no transcript → `--resume` would fail and strand a
  // dead pane, so it's skipped and reported instead.
  const members = [m("orch", "orchestrator"), m("w1", "worker"), m("w2", "worker")];
  const plan = planGroupResume(members, (id) => id !== "w2");
  assert.equal(plan.orchestrator?.sessionId, "orch");
  assert.deepEqual(plan.rejoin.map((x) => x.sessionId), ["w1"]);
  assert.deepEqual(plan.skipped.map((x) => x.sessionId), ["w2"]);
});

test("reviewers and planners rejoin too (any non-orchestrator delegate)", () => {
  const members = [m("orch", "orchestrator"), m("r1", "reviewer"), m("p1", "planner")];
  const plan = planGroupResume(members, () => true);
  assert.deepEqual(plan.rejoin.map((x) => x.role).sort(), ["planner", "reviewer"]);
});

test("the plan covers the ENTIRE set — one click, one atomic plan for every member", () => {
  const members = [
    m("orch", "orchestrator"),
    m("w1", "worker"),
    m("w2", "worker"),
    m("r1", "reviewer"),
  ];
  const plan = planGroupResume(members, (id) => id !== "w2");
  const planned = [
    ...(plan.orchestrator ? [plan.orchestrator.sessionId] : []),
    ...plan.rejoin.map((x) => x.sessionId),
    ...plan.skipped.map((x) => x.sessionId),
  ].sort();
  assert.deepEqual(planned, ["orch", "r1", "w1", "w2"], "no member is silently dropped from the plan");
});

test("captured-set in == planned-set out — a large historical roster is IRRELEVANT (round-4 regression)", () => {
  // The regression: the group had 10 sessions over its life (many long-killed
  // workers), but only the orchestrator + 1 worker were OPEN at close. The plan is
  // fed ONLY those 2 captured members, so exactly 2 come back — the roster's other
  // 8 are never an input and can't expand the set. (Session_roles's 10 rows never
  // reach this function; that's the whole fix.)
  const captured = [m("orch", "orchestrator"), m("w-live", "worker")];
  const plan = planGroupResume(captured, () => true);
  const planned = [
    ...(plan.orchestrator ? [plan.orchestrator.sessionId] : []),
    ...plan.rejoin.map((x) => x.sessionId),
    ...plan.skipped.map((x) => x.sessionId),
  ];
  assert.equal(planned.length, 2, "same number of panes out as captured in");
  assert.deepEqual(planned.sort(), ["orch", "w-live"], "exactly the captured members, nothing added");
});

test("no orchestrator in the roster → null (the caller falls back to the session browser)", () => {
  const plan = planGroupResume([m("w1", "worker")], () => true);
  assert.equal(plan.orchestrator, null);
  assert.equal(plan.orchestratorUnresumable, false, "there was no orchestrator at all");
  assert.deepEqual(plan.rejoin.map((x) => x.sessionId), ["w1"]);
});

test("a stale orchestrator (no transcript) is gated too → null + unresumable flag, not a dead pane", () => {
  // The orchestrator gets the same transcript check as delegates: if its own
  // session can't resume, the group can't relaunch cleanly, so the caller falls
  // back to the browser instead of resuming into a dead orchestrator pane.
  const plan = planGroupResume([m("orch", "orchestrator"), m("w1", "worker")], (id) => id !== "orch");
  assert.equal(plan.orchestrator, null, "not planned — its session is gone");
  assert.equal(plan.orchestratorUnresumable, true, "flagged so the caller can say why");
});

test("members without a session id are ignored", () => {
  const plan = planGroupResume([m("", "worker"), m("orch", "orchestrator")], () => true);
  assert.equal(plan.orchestrator?.sessionId, "orch");
  assert.deepEqual(plan.rejoin, []);
  assert.deepEqual(plan.skipped, []);
});

test("a duplicated session id is planned only once (belt-and-braces dedup)", () => {
  const members = [m("orch", "orchestrator"), m("w1", "worker"), m("w1", "worker")];
  const plan = planGroupResume(members, () => true);
  assert.deepEqual(plan.rejoin.map((x) => x.sessionId), ["w1"], "the duplicate row is dropped");
});

test("with duplicate orchestrator records OF THE SAME GROUP, a resumable one wins", () => {
  // Both records name group g — so they really are two records of one group's
  // orchestrator, and preferring the resumable one is safe. (#485 narrowed
  // this: without a recorded group, two orchestrators are indistinguishable
  // from two GROUPS, and picking one silently is the bug — see below.)
  const members = [m("dead", "orchestrator", "g"), m("alive", "orchestrator", "g")];
  const plan = planGroupResume(members, (id) => id === "alive", "g");
  assert.equal(plan.orchestrator?.sessionId, "alive");
  assert.equal(plan.ambiguous, false);
});

// ---------- #485: one plan is one group ----------

test("a member belonging to ANOTHER group is refused, never rejoined", () => {
  // THE BUG. A tab holding two groups handed every placeholder in it to one
  // plan: group B's worker was rejoined into group A, attaching it to an
  // orchestrator that never spawned it. It must land in `foreign` — planning
  // it is what made the contamination possible.
  const members = [
    m("orch-a", "orchestrator", "group-a"),
    m("w-a", "worker", "group-a"),
    m("w-b", "worker", "group-b"), // the other group's worker, same tab
  ];
  const plan = planGroupResume(members, () => true, "group-a");
  assert.deepEqual(plan.rejoin.map((x) => x.sessionId), ["w-a"], "only group A's own delegate rejoins");
  assert.deepEqual(plan.foreign.map((x) => x.sessionId), ["w-b"], "group B's worker is refused, not rejoined");
  assert.equal(plan.orchestrator?.sessionId, "orch-a");
});

test("the OTHER group's orchestrator is not silently dropped into this plan either", () => {
  // The second half of the same defect: with both orchestrators swept into one
  // plan, one was kept and the other vanished without a word. Partitioned by
  // group, each plan sees exactly its own — and B's is not "missing", it is
  // simply not this plan's business (it has its own card to resume from).
  const members = [m("orch-a", "orchestrator", "group-a"), m("orch-b", "orchestrator", "group-b")];
  const a = planGroupResume(members, () => true, "group-a");
  const b = planGroupResume(members, () => true, "group-b");
  assert.equal(a.orchestrator?.sessionId, "orch-a");
  assert.equal(b.orchestrator?.sessionId, "orch-b", "group B resumes its OWN orchestrator, not A's");
  assert.deepEqual(a.foreign.map((x) => x.sessionId), ["orch-b"]);
  assert.deepEqual(b.foreign.map((x) => x.sessionId), ["orch-a"]);
  assert.equal(a.ambiguous, false, "two orchestrators that NAME their groups are not ambiguous");
});

test("two orchestrators with no recorded group is refused LOUDLY, not resolved by preference", () => {
  // A pre-#485 snapshot records no group per pane, so a two-group tab is
  // indistinguishable from one group with a duplicate record. The old code
  // kept whichever was resumable and rejoined everything into it. Silence is
  // the failure mode #485 is about, so the whole set is refused instead and
  // the caller sends the human to the session browser.
  const members = [m("orch-1", "orchestrator"), m("orch-2", "orchestrator"), m("w1", "worker")];
  const plan = planGroupResume(members, (id) => id === "orch-2");
  assert.equal(plan.ambiguous, true);
  assert.equal(plan.orchestrator, null, "nothing is resumed on a guess");
  assert.equal(plan.orchestratorUnresumable, false, "not a stale-transcript failure — a different message");
});

test("a member that names no group still plans normally (pre-#485 snapshot)", () => {
  // Migration: a single-group tab saved by an older build has null everywhere
  // and must behave exactly as it did — one orchestrator, every delegate
  // rejoined, nothing refused.
  const members = [m("orch", "orchestrator"), m("w1", "worker"), m("w2", "worker")];
  const plan = planGroupResume(members, () => true, "group-a");
  assert.equal(plan.orchestrator?.sessionId, "orch");
  assert.deepEqual(plan.rejoin.map((x) => x.sessionId), ["w1", "w2"]);
  assert.deepEqual(plan.foreign, [], "unattributed is not the same as foreign");
  assert.equal(plan.ambiguous, false);
});

test("partitionByGroup splits a tab's placeholders by their OWN group", () => {
  const recs = [
    { id: "a1", groupId: "group-a" },
    { id: "b1", groupId: "group-b" },
    { id: "a2", groupId: "group-a" },
    { id: "legacy", groupId: null },
  ];
  const a = partitionByGroup(recs, "group-a");
  assert.deepEqual(a.mine.map((r) => r.id), ["a1", "a2"]);
  assert.deepEqual(a.others.map((r) => r.id), ["b1", "legacy"], "another group's AND the unattributed stay out");
  // A click on an unattributed placeholder claims only unattributed ones — it
  // must never sweep in a record that positively names a group.
  const legacy = partitionByGroup(recs, null);
  assert.deepEqual(legacy.mine.map((r) => r.id), ["legacy"]);
});

test("partitionByGroup treats blank the same as absent (never a group named \"\")", () => {
  const recs = [{ id: "blank", groupId: "  " }, { id: "absent" }, { id: "real", groupId: "g" }];
  const out = partitionByGroup(recs, null);
  assert.deepEqual(out.mine.map((r) => r.id), ["blank", "absent"]);
  assert.deepEqual(out.others.map((r) => r.id), ["real"]);
});
