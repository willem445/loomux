// The repo's OWN `.orrerix/workflow.yml` (#222), checked against the pane's reader
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
import { existsSync, readFileSync } from "node:fs";
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
const text = readFileSync(new URL("../.orrerix/workflow.yml", import.meta.url), "utf8");
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
    ["orchestrator", "planner", "worker-std", "worker-adv", "rev-std", "rev-final", "process"]
  );
  // Two worker tiers, and the STANDARD one FIRST — which is the opposite default
  // from the roster this replaced, and deliberate. The first block of a class is
  // what a bare `spawn_agent(kind: "worker")` resolves to, and this file's own
  // rule 3 ("CLASSIFY THE WORKER AT INTAKE") is that the orchestrator chooses the
  // tier when it writes the brief; the fallback is therefore the cheap one, so an
  // unrouted task is one nobody classified rather than one that silently cost
  // Opus. `process` is worker-kind too (#324) but role_hint-gated, so it is
  // excluded from this default-tier pin.
  const tiers = workflow.blocks.filter((b) => b.kind === "worker" && !b.role_hint);
  assert.deepEqual(
    tiers.map((b) => [b.id, b.cli, b.model]),
    [
      ["worker-std", "opencode", "openrouter/z-ai/glm-5.3-flash"],
      ["worker-adv", "claude", "opus"],
    ],
    "the tiers are the demo: a cheap default worker, and a strong one for work with judgment in it"
  );
  // process (#324): role_hint pairs with the kind it requires — the worker-side
  // half of that rule is exercised end to end by this real file. (The
  // planner-side half — role_hint: advisor — moved to the synthetic fixture
  // below when the advisor block left the roster; the rule outlives the block.)
  // THE REVIEWER LANES, and the one ordering property the roster now leans on.
  // `block_for(Role::Reviewer)` resolves a bare `spawn_agent(kind: "reviewer")` to
  // the FIRST reviewing block in roster order, so rev-std must be declared ahead
  // of rev-final: rev-final is the ONCE-LAST validator (rule 1), so an unrouted
  // review request landing on it would both spend the expensive lane first and
  // break the sequencing the whole roster is built around. Same shape as the
  // worker-tier pin above, and stated as an index rather than as a set so a
  // reordering edit fails here rather than silently changing what a bare spawn
  // does.
  const reviewers = workflow.blocks.filter((b) => b.kind === "reviewer");
  assert.deepEqual(
    reviewers.map((b) => [b.id, b.cli, b.model]),
    [
      ["rev-std", "opencode", "openrouter/z-ai/glm-5.3-flash"],
      ["rev-final", "claude", "opus"],
    ],
    "the every-round lane is declared first; the strong final validator runs once, last"
  );
  assert.equal(
    reviewers[0].id,
    "rev-std",
    "a bare spawn_agent(kind: \"reviewer\") must reach the lane that runs every round"
  );
  // The model id is pinned in FULL on purpose, and the loop below covers every
  // opencode block (both tiers of it — the default worker AND the every-round
  // reviewer), not just the reviewers. `default_model("opencode", …)` is
  // deliberately empty — opencode has no vendor-neutral alias, its ids are
  // `provider_id/model_id` — so a block that dropped the `openrouter/` half would
  // spawn against a model that does not exist. This asserts the `/` specifically,
  // which is the character #722 had to widen `sanitize_model` to admit; the
  // pattern allows a second one because this provider's own model ids carry it
  // (`openrouter` + `z-ai/glm-5.3-flash`).
  const viaOpencode = workflow.blocks.filter((b) => b.cli === "opencode");
  assert.ok(viaOpencode.length > 0, "the cheap tier is the point of this roster — it must have opencode blocks");
  for (const b of viaOpencode) {
    assert.match(b.model ?? "", /^[a-z0-9-]+\/[a-z0-9./-]+$/, `${b.id}: an opencode model id names its provider`);
  }

  const processPro = workflow.blocks.find((b) => b.id === "process");
  assert.deepEqual([processPro?.kind, processPro?.role_hint], ["worker", "process"]);
  // Every delegate carries a repo-authored persona, and it is a FILE in
  // `.github/agents/` — the copilot-native convention — so a block flipped to
  // `cli: copilot` gets `--agent <name>` natively instead of a kickoff paste.
  for (const b of workflow.blocks) {
    if (b.kind === "orchestrator") {
      assert.equal(b.profile, undefined, "the trust root may never carry a repo persona");
      continue;
    }
    if (b.kind === "planner" && !b.profile) continue; // the bare planner: loomux's own contract is enough
    assert.match(b.profile ?? "", /^\.github\/agents\/[a-z-]+\.md$/, `${b.id} needs a persona file`);
    assert.equal(b.prompt, undefined, `${b.id}: a persona file and an inline prompt are exclusive`);
  }
});

test("every declared reviewer lane is named by the gate or by a routing rule, because an abstention is a pass", () => {
  const { workflow } = parseWorkflow(text);
  const gate = workflow.gates.merge;
  assert.ok(gate, "the point of the dogfood file is that the human can demo the gate");
  // THE SAFETY PROPERTY — and it is NAMEDNESS, not reachability, so say so rather
  // than overclaim. Under `all-pass` an abstention counts as a pass, so a
  // reviewer-kind block in the roster that neither `gates.merge.reviewers` nor any
  // `routing:` rule NAMES can never enter the required set at all: it would sit
  // there looking wired while the gate opened without it. That is what the
  // assertion below catches, and all of it.
  //
  // What it does NOT catch, found in review (rev-final N4) rather than by the
  // author: a rule whose `paths:` match nothing still NAMES its reviewer, so the
  // lane is named and required on no PR — the same end state, arrived at from the
  // other side. Measured there by pointing rule 1 at `zzz-no-such-dir/**` and
  // watching this suite stay 9/9 green. The `routing rules must be able to FIRE`
  // block further down is the partial close, with its own residual pinned.
  //
  // "In the gate" is a UNION of two lists, and that is the roster's design rather
  // than a loosening: `rev-std` is static (every PR), `rev-final` is REQUIRED BY
  // ROUTING (#1176) on the paths a prose review cannot judge — code, tests, CI,
  // manifests, doc/design. A docs-only PR that runs rev-std alone is the rule
  // working (#1952), not a hole.
  const declaredReviewers = workflow.blocks.filter((b) => b.kind === "reviewer").map((b) => b.id);
  const namedBy = (g: typeof gate): Set<string> =>
    new Set([...(g?.reviewers ?? []), ...(g?.routing ?? []).flatMap((r) => r.reviewers)]);
  const unnamed = (w: typeof workflow): string[] => {
    const named = namedBy(w.gates.merge);
    return w.blocks.filter((b) => b.kind === "reviewer" && !named.has(b.id)).map((b) => b.id);
  };
  assert.deepEqual(unnamed(workflow), [], "every declared reviewer lane is named by the gate");
  // …and the SPLIT itself, pinned positively, so a lane sliding out of the static
  // list into nothing — or the routing block emptying — fails here and not only in
  // the generic assertion above.
  assert.deepEqual(gate.reviewers, ["rev-std"], "the static lane is the one that runs every round");
  assert.deepEqual(
    [...new Set((gate.routing ?? []).flatMap((r) => r.reviewers))].sort(),
    ["rev-final"],
    "…and the routing rules add exactly the final validator, on the paths rule 2 names"
  );
  assert.ok((gate.routing ?? []).length > 0, "the routing block is what makes rev-final reachable at all");
  assert.deepEqual(declaredReviewers, ["rev-std", "rev-final"]);

  // ROUTING RULES MUST BE ABLE TO FIRE — the partial close on the namedness/reachability
  // gap above, and the reason it is only partial is stated rather than left for the next
  // reader to discover. Full reachability ("does this glob match a file a PR could touch")
  // needs a glob engine and the repo's tracked-file list, and this suite is DOM-free pure
  // modules with neither. What it can check without either is that a glob ROOTED at a
  // literal path is rooted at one that EXISTS — which is exactly the shape a directory
  // rename or a typo produces, i.e. the arrival route the review's premortem named
  // (`src/**` narrowed to `src/orchestration/**` during a refactor that moved it).
  const literalRoot = (glob: string): string | null => {
    const wild = glob.search(/[*?[]/);
    if (wild < 0) return glob; // no wildcard at all: the path itself must exist
    const upto = glob.slice(0, wild);
    const cut = upto.slice(0, upto.lastIndexOf("/") + 1).replace(/\/$/, "");
    return cut || null; // `**/Cargo.toml` has no literal root — the residual, pinned below
  };
  // The check's own POSITIVE CONTROL, in-test: the exact shape review found (a rooted
  // glob naming a directory that does not exist) must be one this check would refuse.
  // Without this the loop below passes just as well when `literalRoot` returns null for
  // everything and nothing is ever verified.
  assert.equal(literalRoot("zzz-no-such-dir/**"), "zzz-no-such-dir");
  assert.equal(existsSync(new URL("../zzz-no-such-dir", import.meta.url)), false, "…and it really is absent, so the arm below has teeth");

  let rootsChecked = 0;
  for (const rule of gate.routing ?? []) {
    let checkedInRule = 0;
    for (const p of rule.paths) {
      const root = literalRoot(p);
      if (root === null) continue;
      assert.ok(
        existsSync(new URL(`../${root}`, import.meta.url)),
        `routing path ${JSON.stringify(p)} is rooted at ${JSON.stringify(root)}, which does not exist — the rule can never fire`
      );
      rootsChecked++;
      checkedInRule++;
    }
    // POPULATION CONTROL, counted at the VERIFIED site: a rule made entirely of unrooted
    // globs would sail through the loop above having certified nothing.
    assert.ok(
      checkedInRule > 0,
      `every routing rule needs at least one path this check can verify: ${JSON.stringify(rule.paths)}`
    );
  }
  assert.ok(rootsChecked > 0, "…and some path was actually verified, not zero of them");

  // THE RESIDUAL, PERFORMED rather than merely disclosed (CLAUDE.md's escape-hatch rule) —
  // and performing it is what corrected it. Two shapes LOOK like the blind spot; only one
  // is one, and the first draft of this comment named the wrong one:
  //
  //  * a rule made ONLY of unrooted globs is CAUGHT — not by the existence check, which
  //    skips them, but by the per-rule population control above. Measured: mutating rule 1
  //    to `["**/nope.zzz"]` reddens this test. Worth writing down, because the obvious
  //    reading of "an unrooted glob is not checked" is that such a rule slips through, and
  //    it does not.
  //  * what DOES slip through is a glob whose literal root EXISTS but which matches no
  //    file. `src/**/*.zzz` roots at `src`, which is there, so the check passes it while
  //    the rule can still never fire. This check verifies the ROOT, not a match; closing
  //    that last step needs the glob engine and the tracked-file list this suite has
  //    neither of. Measured: that mutation leaves the suite green.
  assert.equal(literalRoot("**/nope.zzz"), null, "an unrooted glob has no root to check…");
  assert.equal(literalRoot("src/**/*.zzz"), "src", "…but a rooted-yet-unmatchable glob IS checked, and passes — the real blind spot");
  assert.equal(existsSync(new URL("../src", import.meta.url)), true, "…because its root really does exist, which is all this check asks");
  assert.equal(literalRoot("**/Cargo.toml"), null, "…and the shipped file really does contain an unrooted path");
  assert.deepEqual(
    (gate.routing ?? []).map((r) => r.paths.filter((p) => literalRoot(p) === null).length),
    [0, 0, 0, 1],
    "…in exactly one rule, which carries three rooted paths beside it — so the blind spot is not load-bearing here"
  );

  // POSITIVE CONTROL — the assertion above passes just as well against a check that
  // never ran, so this performs the one edit it exists to catch: drop rev-final from
  // EVERY routing rule and leave everything else alone. The mutation is asserted to
  // have LANDED (a `replace` whose anchor missed exits happily and leaves a suite
  // green for the wrong reason), and the mutated file is asserted to still parse
  // clean — so the red below is about the RULE, not about a file the pane rejects.
  const anchor = /reviewers: \[rev-final\]/g;
  const hits = text.match(anchor) ?? [];
  assert.equal(hits.length, 4, "the four routing rules that require the final lane");
  const mutated = text.replace(anchor, "reviewers: [rev-std]");
  assert.notEqual(mutated, text, "the mutation landed");
  assert.equal((mutated.match(anchor) ?? []).length, 0, "…on every rule, not just the first");
  const after = parseWorkflow(mutated);
  assert.deepEqual([...after.findings, ...validateWorkflow(after.workflow)], [], "the mutant is a file the pane blesses");
  assert.deepEqual(
    unnamed(after.workflow),
    ["rev-final"],
    "a declared lane that no rule and no gate names must fail — that is the whole point"
  );

  assert.equal(gate.require, "all-pass");
  assert.equal(gate.threshold, undefined, "an all-pass gate takes no threshold");
  // ci-green: the PR's own checks. body-unchanged (#565/#634): the squash record
  // is pinned to what the reviewer approved.
  assert.deepEqual(gate.also, ["ci-green", "body-unchanged"]);
});

test("role_hint: advisor still pairs only with the planner kind (synthetic — the rule outlives the roster)", () => {
  // The advisor block left the dogfood roster (never spawned across two live
  // batches), but the planner-side half of the role_hint-pairs-with-kind rule
  // (#250/#324) keeps end-to-end coverage via this synthetic fixture.
  const fixture = [
    "version: 1",
    "blocks:",
    "  - id: helper",
    "    kind: planner",
    "    role_hint: advisor",
  ].join("\n");
  // The minimal fixture legitimately warns about graph shape (no edges); the
  // property under test is the PAIRING, so filter to hint-related findings.
  const hintFindings = (w: string) => {
    const parsed = parseWorkflow(w);
    return [...parsed.findings, ...validateWorkflow(parsed.workflow)].filter((f) =>
      /hint/i.test(`${f.code} ${f.message}`)
    );
  };
  assert.deepEqual(hintFindings(fixture), [], "planner + advisor is the legal pairing");
  const ok = parseWorkflow(fixture);
  assert.deepEqual(
    [ok.workflow.blocks[0].kind, ok.workflow.blocks[0].role_hint],
    ["planner", "advisor"]
  );
  assert.ok(
    hintFindings(fixture.replace("kind: planner", "kind: worker")).length > 0,
    "advisor on a worker kind must be a finding, never coerced"
  );
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
  //
  // "MEANING" is the word that has to be precise here, and the canonical form's own contract
  // supplies it: every list that REFERENCES a block (an edge's fan-out, `gates.merge.reviewers`)
  // is normalized into ROSTER order on the way out — `sortByBlocks`, argued at length in
  // `connectToGate`'s docblock ("SEAT ORDER IS NOT THE HUMAN'S") and pinned on minimal fixtures
  // in `test/workflowmodel.test.ts` ("a fan-out collapses to one entry per source, its targets in
  // ROSTER order" and "As a SET, not a sequence — and that is a property, not a concession"). It
  // is what makes two humans who wire the same graph in a different order get the same file. So
  // the graph is compared as a SET here, and the ORDERING is asserted separately, as the
  // direction it is supposed to move in — which is strictly stronger than the sequence equality
  // this used to assert, because a serializer that DROPPED an edge fails the set check whatever
  // it does to the order.
  const { workflow } = parseWorkflow(lfText);
  const saved = serializeWorkflow(workflow);
  const reread = parseWorkflow(saved);

  assert.deepEqual(reread.findings, [], "a saved copy must still be clean");

  // Everything that is not the edge list comes back deepEqual — blocks, personas, the gate and
  // its routing, and every policy section.
  assert.deepEqual(
    { ...reread.workflow, edges: [] },
    { ...workflow, edges: [] },
    "…and must mean exactly what the original meant"
  );

  // The graph itself: the same edges, none invented, none lost.
  const key = (e: { from: string; to: string }): string => `${e.from}->${e.to}`;
  assert.deepEqual([...reread.workflow.edges.map(key)].sort(), [...workflow.edges.map(key)].sort());
  assert.equal(reread.workflow.edges.length, workflow.edges.length, "no edge invented, none lost");

  assert.equal(serializeWorkflow(reread.workflow), saved, "…and saving it twice must be a no-op");

  // The normalization, pinned in the direction it moves: every fan-out in the SAVED file is in
  // roster order. A serializer that stopped sorting fails here rather than silently making the
  // file a function of the human's clicking sequence again.
  const rosterIndex = new Map(workflow.blocks.map((b, i) => [b.id, i]));
  const fanouts = (w: typeof workflow): [string, string[]][] => {
    const byFrom = new Map<string, string[]>();
    for (const e of w.edges) byFrom.set(e.from, [...(byFrom.get(e.from) ?? []), e.to]);
    return [...byFrom.entries()];
  };
  for (const [from, to] of fanouts(reread.workflow)) {
    assert.deepEqual(
      to,
      [...to].sort((a, b) => (rosterIndex.get(a) ?? 0) - (rosterIndex.get(b) ?? 0)),
      `${from}: the canonical file lists a fan-out in roster order`
    );
  }
  // THE CONTROL that keeps the set comparison above from being a tautology: the shipped file
  // must actually EXERCISE the reorder, or "same set, different order" is a property nothing
  // here witnesses. Today `worker-std -> [rev-std, worker-adv]` is authored out of roster order
  // and comes back as `[worker-adv, rev-std]`. If a future edit puts every fan-out in the
  // file into roster order, this fails first and on purpose: move the divergence onto a
  // synthetic fixture rather than deleting the pin.
  assert.notDeepEqual(
    fanouts(workflow),
    fanouts(reread.workflow),
    "the shipped file must keep authored order and roster order DIFFERENT, or the set comparison above witnesses nothing"
  );
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
  assert.match(rewriteImpactMessage(impact, ".orrerix/workflow.yml"), /comments on \d+ lines/);

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
  // `worker-adv` is the SECOND worker tier, so the `# -- workers:` header sits above its
  // untouched sibling rather than above the block being edited — which is what makes the
  // "every OTHER block's comments" claim in the title a discrimination and not a coincidence.
  // (Editing `worker-std`, the first tier, legitimately costs that one header line: the model's
  // documented bar is "untouched regions keep their comments", and a comment directly above the
  // edited block is not an untouched region. That is what the `- 1` tolerance below is for.)
  const edited = {
    ...workflow,
    blocks: workflow.blocks.map((b) => (b.id === "worker-adv" ? { ...b, model: "sonnet" } : b)),
  };
  const out = serializeWorkflowPreserving(edited, text);

  assert.deepEqual(parseWorkflow(out).workflow, edited, "the edit itself round-trips");

  // The file header, the untouched blocks' own comments, and both section headers survive —
  // only the roster in general was touched, not edges or gates, and not the OTHER blocks.
  assert.match(out, /# CHEAP-TIER ROSTER/, "the file preamble survives");
  assert.match(out, /CLASSIFY THE WORKER AT INTAKE/, "…all ~90 lines of it, not just the first");
  assert.match(out, /-- workers: classified at intake/, "the untouched sibling worker's section header survives");
  assert.match(out, /-- reviewers: rev-std runs every round/, "the reviewers' section header survives");
  assert.match(out, /S5 dogfood \(#1778\)/, "the driver block's comment survives");
  assert.match(out, /^edges:/m, "the edges section is untouched");
  assert.match(out, /^# ADVISORY/m, "…and keeps its own header comment");
  assert.match(out, /^# ENFORCED/m, "the gates section keeps its header comment too");
  assert.match(out, /application code, tests, the E2E lane/, "a comment NESTED inside the gate's routing survives");
  assert.match(out, /lessons\.md ENTRY/, "…and so does the block trailing the last section");

  const commentLines = out.split("\n").filter((l) => /^\s*#/.test(l)).length;
  const originalCommentLines = text.split("\n").filter((l) => /^\s*#/.test(l)).length;
  assert.ok(originalCommentLines > 100, `the file's comments are load-bearing (${originalCommentLines} lines)`);
  assert.ok(
    commentLines >= originalCommentLines - 1,
    `a one-field edit must not cost more than its own block's comment (had ${originalCommentLines}, now ${commentLines})`
  );

  // The rewrite-impact guard (Format's guard, not save's — see the test above) would not even
  // fire for this: it isn't a whole-file canonical rewrite, just one changed field.
  const impact = rewriteImpact(text, out, (t) => formatWorkflowText(t) === t);
  assert.equal(impact, null, "an ordinary field edit is not the reformat Format's guard exists for");
});
