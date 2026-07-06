// Unit tests for orchestration group-membership selection. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { panesInGroup, planGroupMinimize } from "../src/group.ts";

// A minimal stand-in for a Pane: only the field the selector reads, plus a
// `minimized` marker used to assert visibility is irrelevant to selection.
const pane = (orchGroupId: string | null, minimized = false) => ({ orchGroupId, minimized });

test("selects only panes in the given group", () => {
  const panes = [pane("g1"), pane("g2"), pane("g1"), pane(null)];
  const picked = panesInGroup(panes, "g1");
  assert.equal(picked.length, 2);
  assert.ok(picked.every((p) => p.orchGroupId === "g1"));
});

test("a minimized group pane is still selected (the group-ended fix)", () => {
  const visible = pane("g1");
  const docked = pane("g1", true); // minimized — must not escape a group end
  const other = pane("g2");
  const picked = panesInGroup([visible, docked, other], "g1");
  assert.deepEqual(picked, [visible, docked]);
  assert.ok(picked.includes(docked), "minimized pane in the group is closed too");
});

test("panes with no group are never selected", () => {
  assert.deepEqual(panesInGroup([pane(null), pane(null)], "g1"), []);
});

test("no members yields an empty set, not a throw", () => {
  assert.deepEqual(panesInGroup([pane("g2")], "g1"), []);
  assert.deepEqual(panesInGroup([], "g1"), []);
});

// --- planGroupMinimize: the #46 fold/restore toggle decision ---

// A group member as the toggle sees it: role + docked state, tagged with a name
// so assertions can name the exact targets picked.
const member = (
  name: string,
  orchGroupId: string | null,
  orchRole: string | null,
  minimized = false
) => ({ name, orchGroupId, orchRole, minimized });

const names = (ps: { name: string }[]) => ps.map((p) => p.name).sort();

test("any visible member → minimize all visible members", () => {
  const panes = [
    member("orch", "g1", "orchestrator"),
    member("w1", "g1", "worker"),
    member("rev", "g1", "reviewer"),
  ];
  const plan = planGroupMinimize(panes, "g1");
  assert.equal(plan?.action, "minimize");
  assert.deepEqual(names(plan!.targets), ["rev", "w1"]);
});

test("minimize never targets the orchestrator itself", () => {
  const panes = [
    member("orch", "g1", "orchestrator"),
    member("w1", "g1", "worker"),
  ];
  const plan = planGroupMinimize(panes, "g1");
  assert.ok(!plan!.targets.some((p) => p.orchRole === "orchestrator"));
});

test("partially folded group still minimizes — folds the remaining visible ones", () => {
  const panes = [
    member("orch", "g1", "orchestrator"),
    member("w1", "g1", "worker", true), // already docked
    member("w2", "g1", "worker"), // still visible
  ];
  const plan = planGroupMinimize(panes, "g1");
  assert.equal(plan?.action, "minimize");
  assert.deepEqual(names(plan!.targets), ["w2"], "only the visible one is folded");
});

test("all members docked → restore every member", () => {
  const panes = [
    member("orch", "g1", "orchestrator"),
    member("w1", "g1", "worker", true),
    member("rev", "g1", "reviewer", true),
  ];
  const plan = planGroupMinimize(panes, "g1");
  assert.equal(plan?.action, "restore");
  assert.deepEqual(names(plan!.targets), ["rev", "w1"]);
});

test("orchestrator's docked state is irrelevant to the decision", () => {
  // Even if the orchestrator pane were somehow minimized, a visible worker
  // still drives a minimize, and the orchestrator is never a target.
  const panes = [
    member("orch", "g1", "orchestrator", true),
    member("w1", "g1", "worker"),
  ];
  const plan = planGroupMinimize(panes, "g1");
  assert.equal(plan?.action, "minimize");
  assert.deepEqual(names(plan!.targets), ["w1"]);
});

test("only an orchestrator (no workers/reviewers) → null, nothing to toggle", () => {
  const panes = [member("orch", "g1", "orchestrator")];
  assert.equal(planGroupMinimize(panes, "g1"), null);
});

test("a group with no members at all → null", () => {
  assert.equal(planGroupMinimize([member("w", "g2", "worker")], "g1"), null);
  assert.equal(planGroupMinimize([], "g1"), null);
});

test("only the requested group's members are considered", () => {
  const panes = [
    member("orch1", "g1", "orchestrator"),
    member("w1", "g1", "worker", true),
    member("orch2", "g2", "orchestrator"),
    member("w2", "g2", "worker"), // visible, but a different group
  ];
  // g1's workers are all docked → restore, and g2's visible worker must not
  // flip g1 into a minimize.
  const plan = planGroupMinimize(panes, "g1");
  assert.equal(plan?.action, "restore");
  assert.deepEqual(names(plan!.targets), ["w1"]);
});
