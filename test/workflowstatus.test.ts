// Workflow-mode status derivations (#316) — the pure logic behind Slice C's
// lifecycle chrome and the task-board Approve button. What these tests
// defend: the "Approve cannot succeed, say so up front" rule (#316 design ask
// 1) and the satisfiability-warning rule (#316's second stance, "never
// silently arm a gate this session cannot satisfy") both have to hold for
// every shape `orch_workflow_status` can actually return, not just the happy
// one.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  approveWillMerge,
  gateExitsMessage,
  gateSatisfiabilityWarning,
  gateSummaryLine,
  workflowModeLabel,
} from "../src/workflowstatus.ts";
import type { WorkflowGateStatus, WorkflowStatus } from "../src/orchestration.ts";

const gate = (o: Partial<WorkflowGateStatus> = {}): WorkflowGateStatus => ({
  require: "all-pass",
  reviewers: ["rev-orch", "rev-ui", "rev-tests"],
  also: ["ci-green"],
  satisfiable: true,
  missing_blocks: [],
  ...o,
});

const status = (o: Partial<WorkflowStatus> = {}): WorkflowStatus => ({
  advanced: true,
  name: "loomux",
  blocks: [],
  gate: gate(),
  ...o,
});

test("workflowModeLabel: toggle off is the built-in roster, whatever the repo declares", () => {
  assert.equal(workflowModeLabel(status({ advanced: false, name: "loomux" })), "Standard roster");
});

test("workflowModeLabel: on with a resolved name uses it", () => {
  assert.equal(workflowModeLabel(status({ advanced: true, name: "loomux" })), "loomux");
});

test("workflowModeLabel: on but the name read came back empty falls back to a fixed label", () => {
  assert.equal(workflowModeLabel(status({ advanced: true, name: "" })), "Workflow mode");
});

test("gateSummaryLine: no gate armed is null, not an empty sentence", () => {
  assert.equal(gateSummaryLine(status({ gate: null })), null);
});

test("gateSummaryLine: reviewers + all-pass + also-conditions, in the demo's own wording", () => {
  assert.equal(
    gateSummaryLine(status()),
    "merges to the default branch require: rev-orch + rev-ui + rev-tests · all-pass · ci-green"
  );
});

test("gateSummaryLine: a threshold requirement reads as a pass count, not the raw wire string", () => {
  const line = gateSummaryLine(status({ gate: gate({ require: "threshold 2", also: [] }) }));
  assert.equal(
    line,
    "merges to the default branch require: rev-orch + rev-ui + rev-tests · at least 2 pass"
  );
});

test("gateSatisfiabilityWarning: satisfiable gate is quiet", () => {
  assert.equal(gateSatisfiabilityWarning(status()), null);
});

test("gateSatisfiabilityWarning: no gate at all is quiet", () => {
  assert.equal(gateSatisfiabilityWarning(status({ gate: null })), null);
});

test("gateSatisfiabilityWarning: one missing block reads as singular", () => {
  const s = status({ gate: gate({ satisfiable: false, missing_blocks: ["rev-orch"] }) });
  assert.equal(
    gateSatisfiabilityWarning(s),
    "gate names rev-orch — this session can't spawn it; merges will bounce."
  );
});

test("gateSatisfiabilityWarning: multiple missing blocks read as plural", () => {
  const s = status({ gate: gate({ satisfiable: false, missing_blocks: ["rev-orch", "rev-ui"] }) });
  assert.equal(
    gateSatisfiabilityWarning(s),
    "gate names rev-orch, rev-ui — this session can't spawn them; merges will bounce."
  );
});

test("gateExitsMessage: names all three exits (run reviewers / toggle off / GitHub UI)", () => {
  const msg = gateExitsMessage();
  assert.match(msg, /reviewer/i);
  assert.match(msg, /toggle workflow (mode )?off/i);
  assert.match(msg, /github/i);
});

test("approveWillMerge: no gate armed always succeeds", () => {
  assert.deepEqual(approveWillMerge(status({ gate: null }), { pr: "42" }), { ok: true });
});

test("approveWillMerge: a task with no PR is never blocked by the gate", () => {
  assert.deepEqual(approveWillMerge(status(), { pr: null }), { ok: true });
});

test("approveWillMerge: gate armed + a PR-bearing task cannot succeed, even when satisfiable", () => {
  const result = approveWillMerge(status(), { pr: "42" });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /rev-orch\/rev-ui\/rev-tests/);
});

test("approveWillMerge: an unsatisfiable gate gets its own distinct reason, not the generic one", () => {
  const s = status({ gate: gate({ satisfiable: false, missing_blocks: ["rev-orch"] }) });
  const result = approveWillMerge(s, { pr: "42" });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /gate unsatisfiable from this session/);
});
