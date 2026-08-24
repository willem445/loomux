// Unit tests for orchestration group-membership selection. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { managerAbsenceNotice, panesInGroup, planGroupMinimize } from "../src/group.ts";

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

// ── the declared-but-absent manager notice (#1433, #1161 M5) ──
//
// What these are FOR: #1433's two premortem items are "the launch-time manager
// spawn can fail and nobody tells the human" and "nothing reopens a manager
// that died". The chosen answer to both is one NOTICE, and the properties worth
// pinning are the ones that make it that answer rather than a decoration — it
// says nothing for the common case, it fires on a group whose manager is gone
// however it went, and it names the route back rather than implying something
// automatic is coming.

test("a group that declares no manager says nothing", () => {
  // The overwhelmingly common case: every default group and every workflow
  // without a manager block. A notice here would be a permanent line on almost
  // every group panel in the app.
  assert.equal(managerAbsenceNotice(false, 0), null);
});

test("a declared manager that IS live says nothing either", () => {
  // The healthy case. Pinned beside the one above so the predicate cannot be
  // satisfied by "never say anything", which is how an absence-only assertion
  // passes vacuously.
  assert.equal(managerAbsenceNotice(true, 1), null);
});

test("declared and none live is the one case that speaks", () => {
  const n = managerAbsenceNotice(true, 0);
  assert.ok(n, "a declared manager with no live pane must be surfaced");
  assert.match(n.text, /manager/i, n.text);
  assert.match(n.text, /not open|absent|gone/i, n.text);
});

test("the notice names the route back, and does not promise a reopen", () => {
  // The load-bearing half. `docs/features/manager.md` promises the human that
  // closing this pane is allowed, so nothing reopens it — and a notice that
  // read like a transient error would leave them waiting for a repair that is
  // never coming. It has to say where to go instead.
  const n = managerAbsenceNotice(true, 0);
  assert.ok(n);
  assert.match(n.title, /session browser/i, `the way back must be named: ${n.title}`);
  assert.doesNotMatch(
    n.title,
    /reopening|will reopen|retrying|will retry/i,
    `nothing automatic reopens a manager — the notice must not imply one does: ${n.title}`
  );
});
