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
  serializeWorkflowPreserving,
  formatWorkflowText,
} from "../src/workflowmodel.ts";
import { rewriteImpact, rewriteImpactMessage } from "../src/workflowpane.ts";

// The RAW bytes, whatever line ending this checkout actually has — a Windows checkout may have
// CRLF (`core.autocrlf`). `serializeWorkflowPreserving` keeps the original's own line ending
// (#233 non-blocking #3), so testing byte-for-byte against THIS is the honest claim regardless
// of what platform the suite runs on. `serializeWorkflow` (the fully canonical rewrite Format
// uses) always emits `\n` — no original text to take a convention from — so tests that compare
// against ITS output use `lfText` instead.
const text = readFileSync(new URL("../.loomux/workflow.yml", import.meta.url), "utf8");
const lfText = text.replace(/\r\n/g, "\n");

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
  const { workflow } = parseWorkflow(lfText);
  const saved = serializeWorkflow(workflow);
  const reread = parseWorkflow(saved);

  assert.deepEqual(reread.findings, [], "a saved copy must still be clean");
  assert.deepEqual(reread.workflow, workflow, "…and must mean exactly what the original meant");
  assert.equal(serializeWorkflow(reread.workflow), saved, "…and saving it twice must be a no-op");
});

test("the EXPLICIT Format action still rewrites this file wholesale — and still warns first", () => {
  // `serializeWorkflow` (what the Format button uses) is still a full, comment-dropping
  // rewrite on purpose — see its own docblock. The shipped file is deliberately-committed
  // documentation (60+ comment lines explaining the roster and the `.github/agents/`
  // convention), so asking for the fully canonical form still costs something, and the pane
  // still says so before it happens (`rewriteImpact`, used from the Format action since #233 —
  // see `workflowview.ts`'s `confirmFormatRewrite`).
  const { workflow } = parseWorkflow(lfText);
  const canonical = serializeWorkflow(workflow);

  assert.notEqual(canonical, lfText, "the shipped file is NOT in canonical form — it has comments");

  const commentsOnDisk = lfText.split(/\r?\n/).filter((l) => /^\s*#/.test(l)).length;
  assert.ok(commentsOnDisk > 20, `the file's comments are load-bearing (${commentsOnDisk} lines)`);

  const impact = rewriteImpact(lfText, canonical, (t) => formatWorkflowText(t) === t);
  assert.ok(impact, "an explicit Format over this file must raise a warning");
  assert.ok(impact.reformats, "…it is a whole-file rewrite");
  assert.ok(
    impact.droppedComments >= 20,
    `…and it drops the comments (${impact.droppedComments} lines)`
  );
  assert.match(rewriteImpactMessage(impact, ".loomux/workflow.yml"), /comments on \d+ lines/);

  // And the case that must stay SILENT: a file loomux itself wrote is already canonical, so
  // formatting it costs nothing and asks nothing.
  assert.equal(rewriteImpact(canonical, canonical, (t) => formatWorkflowText(t) === t), null);
});

// ---------- and now an ordinary form/canvas edit does NOT eat the comments (#233) ----------
//
// This is the pin the rest of #233's tests build on: an actual save through the pane calls
// `serializeWorkflowPreserving(model, previousBufferText)`, not `serializeWorkflow`. The two
// tests above and below together are the whole story — Format still asks, because it is still
// a deliberate full rewrite; an ordinary edit through the form or canvas no longer needs to.

test("re-serializing this file with NOTHING changed reproduces it exactly", () => {
  const { workflow } = parseWorkflow(text);
  assert.equal(serializeWorkflowPreserving(workflow, text), text);
});

test("editing one block's model keeps every other block's comments — and the section headers", () => {
  const { workflow } = parseWorkflow(text);
  const edited = {
    ...workflow,
    blocks: workflow.blocks.map((b) => (b.id === "worker-quick" ? { ...b, model: "opus" } : b)),
  };
  const out = serializeWorkflowPreserving(edited, text);

  assert.deepEqual(parseWorkflow(out).workflow, edited, "the edit itself round-trips");

  // The file header, the untouched blocks' own comments, and both section headers survive —
  // only the roster in general was touched, not edges or gates, and not the OTHER blocks.
  assert.match(out, /loomux's own agent workflow/, "the file preamble survives");
  assert.match(out, /The orchestrator is loomux's trust root/, "an untouched block's comment survives");
  assert.match(out, /the two worker tiers/, "the comment on the untouched sibling worker survives");
  assert.match(out, /three focused reviewers, one per real review lane/, "the reviewers' comment survives");
  assert.match(out, /^edges:/m, "the edges section is untouched");
  assert.match(out, /^# ADVISORY/m, "…and keeps its own header comment");
  assert.match(out, /^# ENFORCED/m, "the gates section keeps its header comment too");

  const commentLines = out.split("\n").filter((l) => /^\s*#/.test(l)).length;
  const originalCommentLines = text.split("\n").filter((l) => /^\s*#/.test(l)).length;
  assert.ok(
    commentLines >= originalCommentLines - 1,
    `a one-field edit must not cost more than its own block's comment (had ${originalCommentLines}, now ${commentLines})`
  );

  // The rewrite-impact guard (Format's guard, not save's — see the test above) would not even
  // fire for this: it isn't a whole-file canonical rewrite, just one changed field.
  const impact = rewriteImpact(text, out, (t) => formatWorkflowText(t) === t);
  assert.equal(impact, null, "an ordinary field edit is not the reformat Format's guard exists for");
});
