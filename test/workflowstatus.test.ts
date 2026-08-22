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
  default_branch: "main",
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

test("gateSummaryLine: a declared size limit is one of the clauses (#1174)", () => {
  // A clause the gate ENFORCES and the summary omits makes the summary a weaker
  // statement than the gate — the same defect as a silently-ignored `also:` token,
  // one surface out. The wording is the human one ("at most N changed lines"), not
  // the wire key.
  assert.equal(
    gateSummaryLine(status({ gate: gate({ max_diff_lines: 800 }) })),
    "merges to the default branch require: rev-orch + rev-ui + rev-tests · all-pass · ci-green · at most 800 changed lines"
  );
  // Undeclared says NOTHING — not "at most null", and not a default this pane made
  // up. Both spellings of undeclared, since the backend sends `null` and an older
  // one sends no key at all.
  const noLimit = "merges to the default branch require: rev-orch + rev-ui + rev-tests · all-pass · ci-green";
  assert.equal(gateSummaryLine(status()), noLimit);
  assert.equal(gateSummaryLine(status({ gate: gate({ max_diff_lines: null }) })), noLimit);
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

// #581: the relabel is three-way — the base ref a task records decides WHICH
// true sentence the human is told. None of these changes whether the merge is
// gated (the workflow gate applies to every merge of the PR, wherever it
// lands); they change only the story the board tells about it.

test("approveWillMerge: a base equal to the default branch keeps the default-branch warning", () => {
  const result = approveWillMerge(status({ default_branch: "main" }), { pr: "42", pr_base: "main" });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /gate needs rev-orch\/rev-ui\/rev-tests/);
  assert.doesNotMatch(result.reason ?? "", /sub-PR/);
});

test("approveWillMerge: a base that is NOT the default branch says sub-PR, and names it", () => {
  const result = approveWillMerge(status({ default_branch: "main" }), {
    pr: "42",
    pr_base: "integration/581",
  });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /sub-PR into integration\/581/);
  // The old text implied the HUMAN gate is what's holding this PR. It isn't:
  // the human grant is default-branch-only, so saying "won't merge" here was
  // the inaccuracy #581 is fixing.
  assert.doesNotMatch(result.reason ?? "", /won't merge/);
});

test("approveWillMerge: no recorded base falls back to the conservative default-branch warning", () => {
  // Every pre-#581 task, and any task whose author didn't record it.
  const result = approveWillMerge(status({ default_branch: "main" }), { pr: "42" });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /gate needs rev-orch\/rev-ui\/rev-tests/);
  assert.doesNotMatch(result.reason ?? "", /sub-PR/);
});

test("approveWillMerge: an unresolved default branch is unknown, never 'not the default'", () => {
  // The fail-conservative direction: with no default branch to compare
  // against, a recorded base cannot prove the PR is a sub-PR.
  const result = approveWillMerge(status({ default_branch: null }), {
    pr: "42",
    pr_base: "integration/581",
  });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /gate needs rev-orch\/rev-ui\/rev-tests/);
  assert.doesNotMatch(result.reason ?? "", /sub-PR/);
});

test("approveWillMerge: stray whitespace around a recorded base is not a different branch", () => {
  // Case is NOT normalized alongside it: git refs are case-sensitive, so
  // `Main` really would be another branch, and folding case would let a typo
  // read as the default branch.
  const result = approveWillMerge(status({ default_branch: "main" }), { pr: "42", pr_base: " main " });
  assert.equal(result.ok, false);
  assert.doesNotMatch(result.reason ?? "", /sub-PR/);
});

test("approveWillMerge: an origin/-prefixed record of the DEFAULT branch is not a sub-PR", () => {
  // rev-157 NB1: `pr_base` is agent-written and "the base ref" is as naturally
  // written `origin/main` as `main`. Against a resolved default of `main` the
  // raw comparison called that a sub-PR — a merge INTO the default branch
  // dressed up as harmless, the one direction worth spending code on.
  const result = approveWillMerge(status({ default_branch: "main" }), {
    pr: "42",
    pr_base: "origin/main",
  });
  assert.equal(result.ok, false);
  assert.doesNotMatch(result.reason ?? "", /sub-PR/);
  assert.match(result.reason ?? "", /gate needs rev-orch\/rev-ui\/rev-tests/);
});

test("approveWillMerge: an origin/-prefixed sub-PR base still reads as a sub-PR, named bare", () => {
  // The strip is a vocabulary normalization, not a special case for the default
  // branch: a genuine sub-PR recorded the same way must still relabel, and the
  // label must name the branch the way the rest of loomux does.
  const result = approveWillMerge(status({ default_branch: "main" }), {
    pr: "42",
    pr_base: "origin/integration/581",
  });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /sub-PR into integration\/581/);
  assert.doesNotMatch(result.reason ?? "", /origin\//);
});

test("approveWillMerge: only ONE leading origin/ is stripped — a doubled prefix is a typo, not a vocabulary", () => {
  const result = approveWillMerge(status({ default_branch: "main" }), {
    pr: "42",
    pr_base: "origin/origin/main",
  });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /sub-PR into origin\/main/, "repairing this would be guessing");
});

test("approveWillMerge: a branch merely NAMED origin/... is not mistaken for a remote prefix twice over", () => {
  // A real branch called `originals` must not lose characters to a prefix match
  // on `origin` — the strip keys on the `origin/` separator, not on `origin`.
  const result = approveWillMerge(status({ default_branch: "main" }), {
    pr: "42",
    pr_base: "originals",
  });
  assert.match(result.reason ?? "", /sub-PR into originals/);
});

test("approveWillMerge: an unsatisfiable gate outranks the sub-PR relabel", () => {
  // The gate applies to EVERY merge of the PR, integration branch included —
  // so "this session can't spawn the reviewers" is still the fact that decides
  // it, and the more specific message wins.
  const s = status({ gate: gate({ satisfiable: false, missing_blocks: ["rev-orch"] }) });
  const result = approveWillMerge(s, { pr: "42", pr_base: "integration/581" });
  assert.equal(result.ok, false);
  assert.match(result.reason ?? "", /gate unsatisfiable from this session/);
});

test("approveWillMerge: a recorded base cannot un-gate a task with no PR, or a gate-free group", () => {
  assert.deepEqual(approveWillMerge(status(), { pr: null, pr_base: "integration/581" }), { ok: true });
  assert.deepEqual(
    approveWillMerge(status({ gate: null }), { pr: "42", pr_base: "integration/581" }),
    { ok: true }
  );
});
