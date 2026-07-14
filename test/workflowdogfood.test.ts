// The repo's OWN `.loomux/workflow.yml` (#222), checked against the pane's reader
// and validator — the two things a human sees when they open it in loomux.
//
// loomux dogfoods its own feature, which is only worth anything if the file it ships
// is a file the app is happy with. So this reads the real one off disk (not a
// fixture: a fixture would drift the moment someone edits the workflow) and asserts
// it opens with ZERO findings — errors *and* warnings, because a warning here means
// the graph loomux would draw of its own workflow has a block nothing points at.
//
// The backend half of this pin lives in `src-tauri/tests/workflow.rs`
// (`the_repos_own_workflow_file_parses_clean_against_the_real_parser`). Both halves
// exist because the two parsers are deliberately separate: the pane's is an editor
// giving live feedback on text a human is typing, the backend's is the engine. A file
// that only one of them accepts is a file the human is being lied to about — which is
// precisely the drift this test catches, forever.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  parseWorkflow,
  validateWorkflow,
  deriveGraph,
  serializeWorkflow,
  formatWorkflowText,
} from "../src/workflowmodel.ts";
import { rewriteImpact, rewriteImpactMessage } from "../src/workflowpane.ts";

const text = readFileSync(new URL("../.loomux/workflow.yml", import.meta.url), "utf8");

test("the repo's own workflow opens in the pane with no findings", () => {
  const { workflow, findings: syntax } = parseWorkflow(text);
  const findings = [...syntax, ...validateWorkflow(workflow)];
  assert.deepEqual(
    findings.map((f) => `${f.severity} ${f.code}: ${f.message}`),
    [],
    "loomux's own workflow file must be clean in loomux's own pane"
  );
  assert.equal(workflow.version, 1);
});

test("the roster is the one the repo means to run", () => {
  const { workflow } = parseWorkflow(text);
  // Ids, not names: an id is what an edge, a gate and `spawn_agent(block:)`
  // reference, so renaming a display name must never break this pin — and a
  // renamed *id* must, because it breaks the gate.
  assert.deepEqual(
    workflow.blocks.map((b) => b.id),
    ["orchestrator", "planner", "worker-deep", "worker-quick", "rev-orch", "rev-ui", "rev-tests"]
  );
  // Two worker tiers, and the deep one FIRST: the first block of a class is what a
  // bare `spawn_agent(kind: "worker")` resolves to, and the safe default for an
  // unrouted task is the model that can handle ambiguity.
  const workers = workflow.blocks.filter((b) => b.kind === "worker");
  assert.deepEqual(
    workers.map((b) => [b.id, b.model]),
    [
      ["worker-deep", "opus"],
      ["worker-quick", "haiku"],
    ],
    "the tiers are the demo: a deep worker on the strong model, a quick one on the cheap one"
  );
  // Every delegate carries a repo-authored persona, and it is a FILE in
  // `.github/agents/` — the copilot-native convention — so a block flipped to
  // `cli: copilot` gets `--agent <name>` natively instead of a kickoff paste.
  for (const b of workflow.blocks) {
    if (b.kind === "orchestrator") {
      assert.equal(b.profile, undefined, "the trust root may never carry a repo persona");
      continue;
    }
    if (b.kind === "planner") continue; // loomux's own planner contract is enough
    assert.match(b.profile ?? "", /^\.github\/agents\/[a-z-]+\.md$/, `${b.id} needs a persona file`);
    assert.equal(b.prompt, undefined, `${b.id}: a persona file and an inline prompt are exclusive`);
  }
});

test("the merge gate waits for every lane, because an abstention is a pass", () => {
  const { workflow } = parseWorkflow(text);
  const gate = workflow.gates.merge;
  assert.ok(gate, "the point of the dogfood file is that the human can demo the gate");
  assert.deepEqual(gate.reviewers, ["rev-orch", "rev-ui", "rev-tests"]);
  // NOT a threshold. These reviewers are lane-scoped, and one whose lane a PR doesn't
  // touch records a `pass` immediately rather than staying silent — so a `threshold: 2`
  // is satisfied by the two fastest abstentions while the only in-lane reviewer (the
  // slowest, by design) is still working. all-pass costs nothing: all three are spawned
  // on every PR anyway, and the out-of-lane ones pass in one turn.
  assert.equal(gate.require, "all-pass");
  assert.equal(gate.threshold, undefined, "an all-pass gate takes no threshold");
  assert.deepEqual(gate.also, ["ci-green"]);
  const kinds = new Map(workflow.blocks.map((b) => [b.id, b.kind]));
  for (const r of gate.reviewers) {
    assert.equal(kinds.get(r), "reviewer", `${r} must be a reviewer — only reviewers record verdicts`);
  }
});

test("every block is on the declared path — the graph loomux draws has no orphans", () => {
  const { workflow } = parseWorkflow(text);
  const graph = deriveGraph(workflow);
  // `isolated`/`unreachable` are warnings in the validator, and the file is already
  // asserted findings-free above; this says the same thing about the derived graph,
  // which is what the pane actually renders. An orphan block is a delegate the flow
  // forgot — the fan-out someone meant to wire and didn't.
  assert.equal(graph.nodes.length, workflow.blocks.length);
  assert.ok(graph.edges.length > 0, "the declared happy path must actually be declared");
});

// ---------- and now the pane can WRITE it (#222 v2) ----------

test("a canonical save preserves the workflow's MEANING, exactly", () => {
  // What serialization actually guarantees, and all it guarantees: the workflow that comes back
  // is the workflow that went in — every block, persona, edge and gate — and the canonical form
  // is stable, so saving twice is a no-op.
  const { workflow } = parseWorkflow(text);
  const saved = serializeWorkflow(workflow);
  const reread = parseWorkflow(saved);

  assert.deepEqual(reread.findings, [], "a saved copy must still be clean");
  assert.deepEqual(reread.workflow, workflow, "…and must mean exactly what the original meant");
  assert.equal(serializeWorkflow(reread.workflow), saved, "…and saving it twice must be a no-op");
});

test("a canonical save REWRITES this file — and the pane says so before it does", () => {
  // The honest version of what this test used to claim (rev-15 F6). The old one asserted that a
  // save "does not churn the file" — but it compared the canonical form against ITSELF and never
  // against the bytes on disk, so it could not fail, and the property it was named after is
  // FALSE: the shipped workflow is not canonical. It is deliberately-committed documentation —
  // the comments explain the roster and the .github/agents/ convention — and a canonical
  // re-serialize drops every one of them.
  //
  // So this asserts the truth instead, and then asserts the guard that makes the truth
  // survivable: the pane warns, once, before the first save that would do it.
  const { workflow } = parseWorkflow(text);
  const canonical = serializeWorkflow(workflow);

  assert.notEqual(canonical, text, "the shipped file is NOT in canonical form — it has comments");

  const commentsOnDisk = text.split(/\r?\n/).filter((l) => /^\s*#/.test(l)).length;
  assert.ok(commentsOnDisk > 20, `the file's comments are load-bearing (${commentsOnDisk} lines)`);

  // The guard: a form or canvas edit re-serializes, and the human is told what that costs BEFORE
  // it happens — not left to find it in `git diff`.
  const impact = rewriteImpact(text, canonical, (t) => formatWorkflowText(t) === t);
  assert.ok(impact, "saving canonical text over this file must raise a warning");
  assert.ok(impact.reformats, "…it is a whole-file rewrite");
  assert.ok(
    impact.droppedComments >= 20,
    `…and it drops the comments (${impact.droppedComments} lines)`
  );
  assert.match(rewriteImpactMessage(impact, ".loomux/workflow.yml"), /comments on \d+ lines/);

  // And the case that must stay SILENT: a file loomux itself wrote is already canonical, so
  // saving it costs nothing and asks nothing.
  assert.equal(rewriteImpact(canonical, canonical, (t) => formatWorkflowText(t) === t), null);
});

// Comment-preserving serialization would make this whole trade go away, and it is a real
// feature — the comments in this very file are the argument for it. It needs its own design and
// its own review, so it is filed as a follow-up rather than smuggled in here. Until it lands,
// the contract is: the YAML tab saves exactly what you type; the form and the canvas rewrite the
// file, and say so first.
