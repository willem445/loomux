// Whole-group resume planning (#194 P4, demo round 3) — groupresume.ts. Pins that
// ONE Resume click plans the entire group: orchestrator first, every resumable
// delegate rejoined, and a no-transcript delegate skipped (not stranded).
import { test } from "node:test";
import assert from "node:assert/strict";
import { planGroupResume, type GroupMember } from "../src/groupresume.ts";

const m = (sessionId: string, role: string): GroupMember => ({ sessionId, role });

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

test("no orchestrator in the roster → null (the caller falls back to the session browser)", () => {
  const plan = planGroupResume([m("w1", "worker")], () => true);
  assert.equal(plan.orchestrator, null);
  assert.deepEqual(plan.rejoin.map((x) => x.sessionId), ["w1"]);
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

test("with duplicate orchestrator records, a resumable one wins", () => {
  const members = [m("dead", "orchestrator"), m("alive", "orchestrator")];
  const plan = planGroupResume(members, (id) => id === "alive");
  assert.equal(plan.orchestrator?.sessionId, "alive");
});
