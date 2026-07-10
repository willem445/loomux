// Whole-group resume planning (#194 P4, demo rounds 3–4) — groupresume.ts. The
// INPUT is the CAPTURED group panes (the orch panes live at close, read off the
// tab's dormant placeholders) — NEVER the backend's full historical roster. These
// pin that ONE Resume click plans exactly that captured set: orchestrator first,
// every resumable delegate rejoined, a no-transcript delegate skipped (not
// stranded), and nothing added beyond what was captured (the round-4 regression).
import { test } from "node:test";
import assert from "node:assert/strict";
import { planGroupResume, type GroupMember } from "../src/groupresume.ts";

// One CAPTURED group member (a dormant orch placeholder's recorded session + role).
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
