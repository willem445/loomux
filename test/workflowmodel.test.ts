// Unit tests for the pure workflow model (#222): reading `.loomux/workflow.yml`,
// writing it back canonically, deriving its graph, and — the part that earns its
// keep — the PRE-RUN VALIDATION pass that every workflow tool surveyed in the #222
// investigation skipped.
//
// These test what the pane promises the human, not how it is written: that a file
// survives a round-trip unchanged, that a canonical save doesn't churn the diff, that
// a broken file still OPENS (as stubs + findings, never a refusal), and that each
// validation rule fires on the mistake it exists to catch and stays quiet otherwise.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  parseWorkflow,
  serializeWorkflow,
  serializeWorkflowPreserving,
  validateWorkflow,
  analyzeWorkflow,
  formatWorkflowText,
  deriveGraph,
  removeBlockAt,
  nextBlockId,
  starterWorkflow,
  scaffoldWorkflowText,
  connectBlocks,
  disconnectBlocks,
  connectionError,
  addBlock,
  newBlock,
  isValidBlockId,
  isBlockKind,
  allowDenialReason,
  personaDenialReason,
  isReviewingBlock,
  isWorkflowCli,
  isValidIntakeLabel,
  isValidResourceName,
  sanitizeAllowPattern,
  roleHintsForKind,
  hasErrors,
  BLOCK_KINDS,
  ROLE_HINTS,
  INTAKE_SOURCES,
  ID_MAX_CHARS,
  RESOURCES_MAX,
  RESOURCE_SLOTS_MAX,
  MERGE_QUEUE_MAX_BATCH_MIN,
  WORKFLOW_VERSION,
  roleHintRequires,
  type Workflow,
  type Finding,
  type FindingCode,
} from "../src/workflowmodel.ts";
import { knobState, type CliKnobs } from "../src/selectorknobs.ts";

/** The schema sketch from the #222 investigation (§4), verbatim in spirit: the file the
 *  feature was designed around. If this stops reading, the feature is broken. */
const SAMPLE = `# <repo>/.loomux/workflow.yml
version: 1
name: focused-review

blocks:
  - id: planner
    name: Planner
    kind: planner
    cli: claude
    model: opus

  - id: worker
    name: Worker
    kind: worker
    cli: copilot
    profile: .github/agents/worker.md
    model: auto

  - id: rev-security
    name: Security review
    kind: reviewer
    cli: claude
    model: opus
    prompt: |
      Review ONLY for security defects: injection, authz, secrets, path traversal.
      Ignore style and perf — other reviewers cover those.

  - id: rev-tests
    name: Test-quality review
    kind: reviewer
    cli: claude
    model: sonnet
    prompt: |
      Review ONLY test quality: do the tests exercise intent?

edges:
  - { from: planner, to: worker }
  - { from: worker,  to: [rev-security, rev-tests] }

gates:
  merge:
    require: all-pass
    reviewers: [rev-security, rev-tests]
    also: [ci-green]
`;

const codes = (findings: readonly Finding[]): FindingCode[] => findings.map((f) => f.code);
const has = (findings: readonly Finding[], code: FindingCode): boolean =>
  findings.some((f) => f.code === code);

// ---------- reading the schema ----------

test("reads every part of the §4 schema", () => {
  const { workflow, findings } = parseWorkflow(SAMPLE);
  assert.deepEqual(findings, [], "the reference schema must parse cleanly");

  assert.equal(workflow.version, 1);
  assert.equal(workflow.name, "focused-review");
  assert.deepEqual(
    workflow.blocks.map((b) => b.id),
    ["planner", "worker", "rev-security", "rev-tests"]
  );

  const worker = workflow.blocks[1]!;
  assert.equal(worker.kind, "worker");
  assert.equal(worker.cli, "copilot");
  assert.equal(worker.profile, ".github/agents/worker.md", "a profile: path is the Copilot native --agent form");
  assert.equal(worker.prompt, undefined);

  const sec = workflow.blocks[2]!;
  assert.equal(sec.model, "opus");
  assert.match(sec.prompt ?? "", /^Review ONLY for security defects/);
  assert.match(sec.prompt ?? "", /Ignore style and perf/, "a block scalar keeps its line breaks");

  // The fan-out `to: [a, b]` becomes one flat edge per target — that is what reachability
  // and in-degree are asked of.
  assert.deepEqual(workflow.edges, [
    { from: "planner", to: "worker" },
    { from: "worker", to: "rev-security" },
    { from: "worker", to: "rev-tests" },
  ]);

  assert.deepEqual(workflow.gates.merge, {
    require: "all-pass",
    reviewers: ["rev-security", "rev-tests"],
    also: ["ci-green"],
  });
});

test("a comment is never mistaken for content, and a # inside a prompt survives", () => {
  const { workflow } = parseWorkflow(`version: 1
name: x   # the workflow's name
blocks:
  - id: rev
    name: Rev
    kind: reviewer
    cli: claude
    prompt: |
      # Checklist
      Check the auth path.
`);
  assert.equal(workflow.name, "x");
  assert.equal(workflow.blocks[0]!.prompt, "# Checklist\nCheck the auth path.\n");
});

// ---------- round-trip + canonical stability ----------

test("model → text → model is lossless", () => {
  const original = parseWorkflow(SAMPLE).workflow;
  const reread = parseWorkflow(serializeWorkflow(original)).workflow;
  assert.deepEqual(reread, original);
});

test("formatting is idempotent — a canonical save never churns the diff", () => {
  const once = formatWorkflowText(SAMPLE);
  const twice = formatWorkflowText(once);
  assert.equal(twice, once, "formatting an already-canonical file must be a no-op");
  // And a cosmetically different file with the same meaning canonicalizes to the SAME
  // text — the whole point of having one shape.
  const reordered = SAMPLE.replace("    name: Planner\n", "").replace(
    "  - id: planner\n",
    "  - id: planner\n    name: Planner\n"
  );
  assert.equal(formatWorkflowText(reordered), once);
});

test("keys this build doesn't know survive a round-trip", () => {
  // A file written by a NEWER loomux must not be silently stripped by an older pane —
  // the form would otherwise delete a field the user's backend depends on.
  const text = `version: 1
retries: 3
blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
    timeout: 900
`;
  const w = parseWorkflow(text).workflow;
  assert.deepEqual(w.extra, { retries: 3 });
  assert.deepEqual(w.blocks[0]!.extra, { timeout: 900 });
  const out = serializeWorkflow(w);
  assert.match(out, /^retries: 3$/m);
  assert.match(out, /^ {4}timeout: 900$/m);
  assert.deepEqual(parseWorkflow(out).workflow, w);
});

test("canonical form fixes key order and orders references by the roster", () => {
  const w = parseWorkflow(`version: 1
blocks:
  - cli: claude
    kind: reviewer
    id: rev-b
    name: B
  - id: rev-a
    name: A
    kind: reviewer
    cli: claude
  - id: worker
    name: W
    kind: worker
    cli: claude
edges:
  - { from: worker, to: rev-a }
  - { from: worker, to: rev-b }
gates:
  merge:
    require: all-pass
    reviewers: [rev-a, rev-b]
`).workflow;
  const out = serializeWorkflow(w);
  // Fixed key order per block…
  assert.match(out, /- id: rev-b\n {4}name: B\n {4}kind: reviewer\n {4}cli: claude/);
  // …blocks keep their AUTHORED order (re-sorting the roster on save would churn the
  // very diff the canonical form exists to keep legible)…
  assert.deepEqual(
    parseWorkflow(out).workflow.blocks.map((b) => b.id),
    ["rev-b", "rev-a", "worker"]
  );
  // …and a fan-out collapses to one entry per source, its targets in ROSTER order
  // (rev-b is declared first), not alphabetical order.
  assert.match(out, /- \{ from: worker, to: \[rev-b, rev-a\] \}/);
  assert.match(out, /reviewers: \[rev-b, rev-a\]/);
});

test("a prompt's trailing newline is preserved exactly", () => {
  const withNl: Workflow = {
    ...starterWorkflow(),
    blocks: [{ id: "r", name: "R", kind: "reviewer", cli: "claude", model: "", prompt: "a\nb\n" }],
    edges: [],
    gates: {},
  };
  const withoutNl: Workflow = {
    ...withNl,
    blocks: [{ ...withNl.blocks[0]!, prompt: "a\nb" }],
  };
  assert.match(serializeWorkflow(withNl), /prompt: \|\n/);
  assert.match(serializeWorkflow(withoutNl), /prompt: \|-\n/);
  assert.equal(parseWorkflow(serializeWorkflow(withNl)).workflow.blocks[0]!.prompt, "a\nb\n");
  assert.equal(parseWorkflow(serializeWorkflow(withoutNl)).workflow.blocks[0]!.prompt, "a\nb");
});

// ---------- the flow-context quoting bug (rev-5 F1) ----------
//
// The emitter serves BOTH block context (`name: …`) and FLOW context (`reviewers: [a, b]`,
// an unknown key's array or map), and in flow context `, [ ] { }` are STRUCTURAL. Quoting
// only for block context meant an ordinary form edit — every one of which re-serializes the
// file — silently destroyed any value containing one. These are the values that actually
// occur: `allow` patterns of exactly this shape are what the backend's agent profiles carry.

test("a comma inside a flow-emitted value does not split it into two", () => {
  const w = starterWorkflow();
  w.gates.merge!.also = ["Bash(gh pr view --json title,body)", "ci-green"];
  const reread = parseWorkflow(serializeWorkflow(w)).workflow;
  assert.deepEqual(
    reread.gates.merge!.also,
    ["Bash(gh pr view --json title,body)", "ci-green"],
    "a comma is structural in a flow list — unquoted, this came back as three conditions"
  );
});

test("braces and brackets inside a flow-emitted value do not destroy it", () => {
  // Unquoted, the mid-string `}` closed the flow collection early, the reader threw, and the
  // whole value came back as `null` — with a bogus syntax finding on a line the pane itself
  // had just written.
  const w = starterWorkflow();
  w.gates.merge!.also = ["fmt{x}", "arr[0]", "map{a: b}"];
  const out = serializeWorkflow(w);
  const { workflow: reread, findings } = parseWorkflow(out);
  assert.deepEqual(reread.gates.merge!.also, ["fmt{x}", "arr[0]", "map{a: b}"]);
  assert.deepEqual(findings, [], "and it must not report a syntax error against its own output");
});

test("unknown keys holding arrays and maps survive a round-trip, structural characters and all", () => {
  // The PR's stated guarantee — "an older pane never strips a newer file's fields" — is only
  // true if it holds for the values those fields actually carry. The original unknown-key
  // test used scalars only (`retries: 3`), which is exactly the hole this closes.
  const text = `version: 1
blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
    tools: ["fmt{x}", "Read"]
    allow: ["Bash(gh pr view --json title,body)", "Bash(git status)"]
    limits: { cpu: 2, note: "a,b" }
`;
  const w = parseWorkflow(text).workflow;
  assert.deepEqual(w.blocks[0]!.extra, {
    tools: ["fmt{x}", "Read"],
    limits: { cpu: 2, note: "a,b" },
  });
  // `allow:` is a KNOWN field since #880 (it was a `RawBlock` field all along — the pane
  // just never had a name for it), so it leaves `extra` and lands on the block. The quoted
  // comma is why it was worth putting in this test at all, and that half is unchanged: the
  // pattern has to survive as ONE entry, not two.
  assert.deepEqual(w.blocks[0]!.allow, [
    "Bash(gh pr view --json title,body)",
    "Bash(git status)",
  ]);
  const out = serializeWorkflow(w);
  const reread = parseWorkflow(out);
  assert.deepEqual(reread.findings, [], "the serialized form must re-read cleanly");
  assert.deepEqual(reread.workflow, w, "…and identically — a form edit must not eat a field it doesn't know");
  // Twice, because the corruption in the original bug only appeared on the SECOND read.
  assert.equal(serializeWorkflow(reread.workflow), out);
});

test("an escaped backslash is not re-read as the start of another escape (rev-6 F8)", () => {
  // A Windows path is the obvious carrier, and it is one form edit away: `C:\new,dir` emits
  // as "C:\\new,dir", and unescaping in the wrong order expanded the `\n` — of the escaped
  // BACKSLASH plus the letter n — into a newline before the `\\` could collapse. It read back
  // as `C:` + newline + `ew,dir`. (The comma is what drags a path into the quoted path at
  // all, so this only became reachable when F1 widened quoting.)
  for (const raw of ["C:\\new,dir", "C:\\temp\\{x}", "a\\\\b", 'quote " and \\ backslash, comma']) {
    const w = starterWorkflow();
    w.gates.merge!.also = [raw];
    w.blocks[0]!.model = raw;
    const reread = parseWorkflow(serializeWorkflow(w)).workflow;
    assert.equal(reread.gates.merge!.also[0], raw, `flow: ${JSON.stringify(raw)}`);
    assert.equal(reread.blocks[0]!.model, raw, `block: ${JSON.stringify(raw)}`);
  }
  // Real escapes still decode — the fix must not turn \n into a literal "n".
  assert.equal(parseWorkflow('version: 1\nname: "a\\nb\\tc"').workflow.name, "a\nb\tc");
});

test("a KEY carrying structural characters survives too (rev-6 F9)", () => {
  // The value side of this was F1; the key side is the same bug with the pair swapped. An
  // unknown key's nested map is arbitrary data from a newer loomux — its keys are as free as
  // its values, and emitting them raw split or truncated the map on re-read.
  const w = starterWorkflow();
  w.blocks[0]!.extra = {
    limits: { "cpu,mem": 2, "brace{}": "x", "colon: here": true },
    "top,key": ["a,b"],
  };
  const out = serializeWorkflow(w);
  const reread = parseWorkflow(out);
  assert.deepEqual(reread.findings, [], "the pane must not report a syntax error on its own output");
  assert.deepEqual(reread.workflow.blocks[0]!.extra, {
    limits: { "cpu,mem": 2, "brace{}": "x", "colon: here": true },
    "top,key": ["a,b"],
  });
  assert.equal(serializeWorkflow(reread.workflow), out, "…and it stays stable");
});

test("a value that would change meaning unquoted is quoted", () => {
  const w: Workflow = {
    version: 1,
    name: "yes: really",
    blocks: [{ id: "w", name: "1.5", kind: "worker", cli: "claude", model: "" }],
    edges: [],
    gates: {},
  };
  const reread = parseWorkflow(serializeWorkflow(w)).workflow;
  assert.equal(reread.name, "yes: really");
  assert.equal(reread.blocks[0]!.name, "1.5", "a numeric-looking NAME must come back a string");
});

test("a tab-indented file is reported, not silently accepted (rev-5 F2)", () => {
  // YAML forbids tabs in indentation, so the backend validator will refuse this file. A pane
  // that reports `valid` on a file the spawn then rejects is worse than one that says
  // nothing — the human is told their workflow is good and the run fails anyway.
  const { findings } = analyzeWorkflow("version: 1\nblocks:\n\t- id: w\n");
  const tab = findings.find((f) => f.code === "yaml-syntax" && /tab/i.test(f.message));
  assert.ok(tab, "a tab in the indentation must produce a finding");
  assert.equal(tab!.line, 3, "and it must say which line");
  // Reported ONCE, not once per re-peek of the same line.
  assert.equal(findings.filter((f) => /tab/i.test(f.message)).length, 1);
});

test("a tab INSIDE a prompt is content, and stays content", () => {
  // The guard is about indentation. A prompt body is text — a tab in it is the user's tab.
  const { workflow, findings } = analyzeWorkflow(`version: 1
blocks:
  - id: rev
    name: R
    kind: reviewer
    cli: claude
    prompt: |
      col1\tcol2
`);
  assert.equal(workflow.blocks[0]!.prompt, "col1\tcol2\n");
  assert.deepEqual(codes(findings), []);
});

test("a prompt whose first line is indented round-trips (rev-5 F3)", () => {
  // Straight out of the form's textarea: a code snippet, an indented checklist. A bare `|`
  // is read back by dedenting to the first content line's indent, which ate exactly this.
  for (const prompt of ["  indented\nplain\n", "\n  after a blank line\n", "\tstarts with a tab\n"]) {
    const w = starterWorkflow();
    w.blocks[2]!.prompt = prompt;
    const out = serializeWorkflow(w);
    assert.equal(
      parseWorkflow(out).workflow.blocks[2]!.prompt,
      prompt,
      `prompt ${JSON.stringify(prompt)} must survive`
    );
    assert.equal(serializeWorkflow(parseWorkflow(out).workflow), out, "…and stay stable");
  }
});

test("an empty roster serializes to something that re-reads as an empty roster (rev-5 F4)", () => {
  // Delete the last block in the form and the pane used to report a YAML-shape error against
  // text it had just written itself (a bare `blocks:` is YAML null).
  const empty: Workflow = { version: 1, name: "x", blocks: [], edges: [], gates: {} };
  const out = serializeWorkflow(empty);
  const { workflow, findings } = analyzeWorkflow(out);
  assert.deepEqual(workflow.blocks, []);
  assert.deepEqual(codes(findings), ["no-blocks"], "the honest error, and ONLY the honest error");
  // A hand-authored bare `blocks:` means the same thing and must not be a shape error either.
  assert.deepEqual(codes(analyzeWorkflow("version: 1\nblocks:\n").findings), ["no-blocks"]);
});

// ---------- the empty-state bug (v2) ----------

test("a BOM does not make a valid workflow look broken", () => {
  // A workflow file written by a Windows editor starts with U+FEFF. The reader took it as
  // part of the first KEY, so `version: 1` arrived as a key named "﻿version" and the pane
  // reported `version-missing` against a file the human could see was correct — and the
  // character is INVISIBLE, so nothing in the error could lead them to the cause.
  const { workflow, findings } = analyzeWorkflow("﻿" + SAMPLE);
  assert.deepEqual(codes(findings), []);
  assert.equal(workflow.version, 1);
  assert.equal(workflow.blocks.length, 4);
});

test("the scaffold is a valid workflow, and canonicalizes to the same one", () => {
  // What a repo with no workflow gets when the human asks for one. If this stops parsing
  // clean, every new workflow in the world starts life with a finding on it.
  const { workflow, findings } = analyzeWorkflow(scaffoldWorkflowText("0.9.0"));
  assert.deepEqual(codes(findings), [], "a scaffold that isn't valid is a scaffold that lies");
  assert.deepEqual(
    workflow.blocks.map((b) => b.id),
    ["planner", "worker", "reviewer"]
  );
  assert.deepEqual(workflow.edges, [
    { from: "planner", to: "worker" },
    { from: "worker", to: "reviewer" },
  ]);
  assert.deepEqual(workflow.gates.merge, { require: "all-pass", reviewers: ["reviewer"], also: [] });
  // A TYPED field since #880, not an unknown key riding `extra` — the pane knows this key
  // now, which is also why the scaffold stays finding-free with unknown keys reported.
  assert.equal(workflow.authored_with, "0.9.0");
  assert.equal(workflow.extra, undefined);
  // It is the same workflow the model's starter describes — the commented file and the
  // programmatic one must not drift into being two different pipelines.
  const starter = starterWorkflow("0.9.0");
  assert.deepEqual(workflow.blocks.map((b) => b.id), starter.blocks.map((b) => b.id));
  assert.deepEqual(workflow.edges, starter.edges);
  // And a form edit (which re-serializes) produces canonical text that still round-trips.
  const canonical = serializeWorkflow(workflow);
  assert.equal(serializeWorkflow(parseWorkflow(canonical).workflow), canonical);
});

// ---------- graph edit operations (v2: the canvas edits the file) ----------

test("drawing an edge, then re-reading the file, gives back the edge you drew", () => {
  // The round-trip the editable canvas rests on: a gesture → the model → the canonical file
  // → the model again, with the same GRAPH. If this doesn't hold, the canvas is lying about
  // the file.
  const w = starterWorkflow();
  const connected = connectBlocks(w, "planner", "reviewer");
  assert.deepEqual(connected.edges.at(-1), { from: "planner", to: "reviewer" });

  const reread = parseWorkflow(serializeWorkflow(connected)).workflow;
  // As a SET, not a sequence — and that is a property, not a concession: the canonical form
  // groups edges by source in roster order, so the file's edge order is a function of the
  // workflow rather than of the order the human happened to draw them in. Two people who draw
  // the same graph in a different order get the same file, and neither sees a diff from the
  // other's clicking sequence.
  const key = (e: { from: string; to: string }): string => `${e.from}->${e.to}`;
  assert.deepEqual(new Set(reread.edges.map(key)), new Set(connected.edges.map(key)));
  assert.equal(reread.edges.length, connected.edges.length, "no edge invented, none lost");
  assert.deepEqual(codes(validateWorkflow(reread)), []);
});

test("an edge that would be nonsense is refused before it is drawn, not after", () => {
  // A canvas that lets you complete the gesture and THEN says the edge was invalid has
  // wasted the gesture and left you to undo it.
  const w = starterWorkflow();
  assert.equal(connectionError(w, "planner", "reviewer"), null, "a legal edge has no error");
  assert.match(connectionError(w, "worker", "worker") ?? "", /itself/);
  assert.match(connectionError(w, "worker", "ghost") ?? "", /doesn't exist/);
  assert.match(connectionError(w, "", "worker") ?? "", /needs an id/);
  assert.match(connectionError(w, "planner", "worker") ?? "", /already exists/, "planner→worker is already drawn");

  // And the operation enforces it too, not only the pre-check — the canvas is the first line
  // of defence, not the only one.
  assert.deepEqual(connectBlocks(w, "worker", "worker").edges, w.edges);
  assert.deepEqual(connectBlocks(w, "worker", "ghost").edges, w.edges);
  assert.deepEqual(connectBlocks(w, "planner", "worker").edges, w.edges, "no duplicate edge");
});

test("erasing an edge takes the edge and nothing else", () => {
  const w = starterWorkflow();
  const cut = disconnectBlocks(w, "worker", "reviewer");
  assert.deepEqual(cut.edges, [{ from: "planner", to: "worker" }]);
  assert.deepEqual(cut.blocks, w.blocks, "the blocks it joined are untouched");
  // The reviewer is now unwired, which the validator says out loud — as a WARNING, because
  // edges are advisory and the workflow still runs.
  const f = validateWorkflow(cut);
  assert.equal(hasErrors(f), false);
  assert.ok(f.some((x) => x.code === "isolated-block" && x.blockId === "reviewer"));
});

test("a block created on the canvas keeps the id the human gave it", () => {
  // §4's first commitment. Dify mints `node_1720794829558`; n8n keys the graph by the display
  // NAME so a rename silently breaks it. A block created here gets a human id, edges name that
  // id, and a rename touches nothing.
  const w = addBlock(starterWorkflow(), newBlock("rev-security", "Security review"));
  const wired = connectBlocks(w, "worker", "rev-security");
  const reread = parseWorkflow(serializeWorkflow(wired)).workflow;
  const made = reread.blocks.find((b) => b.id === "rev-security")!;
  assert.equal(made.name, "Security review");
  assert.equal(made.kind, "reviewer");
  assert.ok(reread.edges.some((e) => e.from === "worker" && e.to === "rev-security"));

  // Renaming it (display only) leaves every reference alone — the property the id buys.
  const renamed: Workflow = {
    ...reread,
    blocks: reread.blocks.map((b) => (b.id === "rev-security" ? { ...b, name: "Sec" } : b)),
  };
  assert.deepEqual(parseWorkflow(serializeWorkflow(renamed)).workflow.edges, reread.edges);
  assert.deepEqual(codes(validateWorkflow(renamed)), []);
});

test("a canvas-authored workflow serializes canonically and stays stable", () => {
  // Build one entirely through the edit ops — the way the canvas does — and it must produce
  // the same shape as a hand-written file: canonical, idempotent, no findings.
  let w = starterWorkflow("0.9.0");
  w = addBlock(w, newBlock("rev-perf", "Perf review"));
  w = connectBlocks(w, "worker", "rev-perf");
  w = disconnectBlocks(w, "planner", "worker");
  w = connectBlocks(w, "planner", "worker");
  const once = serializeWorkflow(w);
  assert.equal(serializeWorkflow(parseWorkflow(once).workflow), once, "GUI-authored files format like any other");
  assert.deepEqual(analyzeWorkflow(once).findings.filter((f) => f.severity === "error"), []);
});

// ---------- comment-preserving serialization (#233) ----------
//
// `serializeWorkflow` is the FULL rewrite — it never carried comments, and it still doesn't;
// that is what `formatWorkflowText` and the Format button ask for on purpose. What follows is
// `serializeWorkflowPreserving`: same model, but handed the ORIGINAL text too, so it can reuse
// whatever it didn't change instead of reformatting the whole file every time.

const COMMENTED = `# who runs, and why
version: 1
name: focused-review

blocks:
  # the planner goes first
  - id: planner
    name: Planner
    kind: planner
    cli: claude
    model: opus

  - id: worker          # opens the PR
    name: Worker
    kind: worker
    cli: claude

# ADVISORY — the declared happy path
edges:
  - { from: planner, to: worker }

# ENFORCED — nothing merges without this
gates:
  merge:
    require: all-pass
    reviewers: [planner]
`;

test("an untouched file re-serializes to itself, byte for byte", () => {
  const { workflow } = parseWorkflow(COMMENTED);
  assert.equal(serializeWorkflowPreserving(workflow, COMMENTED), COMMENTED);
});

test("editing one block's field keeps every OTHER block's comments, and the section headers", () => {
  const { workflow } = parseWorkflow(COMMENTED);
  const edited: Workflow = {
    ...workflow,
    blocks: workflow.blocks.map((b) => (b.id === "worker" ? { ...b, model: "opus" } : b)),
  };
  const out = serializeWorkflowPreserving(edited, COMMENTED);
  assert.match(out, /# who runs, and why/, "the file preamble survives");
  assert.match(out, /# the planner goes first/, "an untouched block's own comment survives");
  assert.match(out, /# ADVISORY — the declared happy path/, "the edges section header survives");
  assert.match(out, /# ENFORCED — nothing merges without this/, "the gates section header survives");
  // The edited block's own trailing comment is the one thing that is allowed to go — it is
  // the node that changed, and #233's bar is "edited nodes serialize cleanly", not lossless.
  assert.doesNotMatch(out, /# opens the PR/);
  assert.deepEqual(parseWorkflow(out).workflow, edited, "and the edit itself must round-trip");
});

test("a prompt whose own last line looks like a comment survives editing a SIBLING (#233 B2)", () => {
  // `isSignificantLine` treats a `#`-starting line as trivia to peel — correct for an ACTUAL
  // comment, wrong for a `|` block scalar's body, where `#` is just a character the prompt
  // happens to contain. Peeling it as if it were commentary on the NEXT block silently moves it
  // there; if that next block is the one that gets edited (regenerated canonically), the line
  // never comes back — the reviewer's exact repro.
  const text = `version: 1
blocks:
  - id: a
    name: A
    kind: worker
    cli: claude
    prompt: |
      Do the work.
      # trailing checklist marker, not a comment
  - id: b
    name: B
    kind: worker
    cli: claude
`;
  const { workflow } = parseWorkflow(text);
  const promptBefore = workflow.blocks[0]!.prompt;
  assert.match(promptBefore ?? "", /# trailing checklist marker/, "sanity: the real reader keeps it as content");

  const edited = { ...workflow, blocks: workflow.blocks.map((b) => (b.id === "b" ? { ...b, model: "opus" } : b)) };
  const out = serializeWorkflowPreserving(edited, text);
  const reread = parseWorkflow(out).workflow;
  assert.equal(reread.blocks[0]!.prompt, promptBefore, "block a's prompt — untouched — must survive intact");
  assert.deepEqual(reread, edited);
});

test("adding a block regenerates only the new entry — every existing one is untouched text", () => {
  const { workflow } = parseWorkflow(COMMENTED);
  const added = addBlock(workflow, newBlock("rev", "Reviewer", "reviewer"));
  const out = serializeWorkflowPreserving(added, COMMENTED);
  assert.match(out, /# the planner goes first/);
  assert.match(out, /# opens the PR/);
  // Round-tripped through the ordinary parser convention on BOTH sides (a fresh `newBlock()`
  // has no `extra` key at all; a parsed one always carries `extra: undefined` explicitly —
  // an unrelated quirk of `readBlock`, not something this test is about).
  assert.deepEqual(parseWorkflow(out).workflow, parseWorkflow(serializeWorkflow(added)).workflow);
});

test("removing a block drops only its own segment — the rest, including comments, is untouched", () => {
  const { workflow } = parseWorkflow(COMMENTED);
  const removed = removeBlockAt(workflow, workflow.blocks.findIndex((b) => b.id === "worker"));
  const out = serializeWorkflowPreserving(removed, COMMENTED);
  assert.match(out, /# the planner goes first/, "the untouched block's comment survives");
  assert.doesNotMatch(out, /id: worker\b/, "the removed block itself is gone");
  assert.deepEqual(parseWorkflow(out).workflow, removed);
  // Its edges and gate seat go with it (removeBlockAt's own contract) — and since the edges/
  // gates sections themselves changed, THEIR comments are the honest cost of that edit.
  assert.doesNotMatch(out, /# ADVISORY/);
});

test("an edge added or removed regenerates the edges CONTENT, but keeps that section's own header comment", () => {
  // The section header ("# ADVISORY …") introduces the CONCEPT of the edges section, not any
  // one edge in it — dropping it every time a single edge is rewired cost far more than the
  // edit itself touched (#233 non-blocking #1). Only the fan-out entries fall back to canonical.
  const { workflow } = parseWorkflow(COMMENTED);
  const rewired = connectBlocks(workflow, "worker", "planner");
  const out = serializeWorkflowPreserving(rewired, COMMENTED);
  assert.match(out, /# who runs, and why/);
  assert.match(out, /# the planner goes first/);
  assert.match(out, /# opens the PR/);
  assert.match(out, /# ADVISORY — the declared happy path/, "the edges section HEADER survives its own content changing");
  assert.match(out, /# ENFORCED — nothing merges without this/, "gates is untouched and keeps its header");
  assert.deepEqual(parseWorkflow(out).workflow, rewired);
});

test("emptying the edge list entirely omits the section rather than leaving a bare header", () => {
  const { workflow } = parseWorkflow(COMMENTED);
  const cleared = { ...workflow, edges: [] };
  const out = serializeWorkflowPreserving(cleared, COMMENTED);
  assert.doesNotMatch(out, /^edges:/m, "no edges left — nothing to hang the header on");
  assert.deepEqual(parseWorkflow(out).workflow, cleared);
});

test("a name change loses only the front section's own trivia (none here), not the rest", () => {
  const { workflow } = parseWorkflow(COMMENTED);
  const renamed = { ...workflow, name: "renamed" };
  const out = serializeWorkflowPreserving(renamed, COMMENTED);
  assert.match(out, /# who runs, and why/, "the file preamble is document-level, kept regardless");
  assert.match(out, /# the planner goes first/);
  assert.deepEqual(parseWorkflow(out).workflow, renamed);
});

test("preserving-serializing is idempotent over its own output", () => {
  const { workflow } = parseWorkflow(COMMENTED);
  const edited: Workflow = {
    ...workflow,
    blocks: workflow.blocks.map((b) => (b.id === "worker" ? { ...b, model: "opus" } : b)),
  };
  const once = serializeWorkflowPreserving(edited, COMMENTED);
  const twice = serializeWorkflowPreserving(edited, once);
  assert.equal(twice, once);
});

test("a file from a NEWER loomux (version: 2) is still editable — its comments are not silently eaten (#233 B3)", () => {
  // `version-unsupported` is an ERROR finding, but the file is still READABLE — the view keeps
  // the form enabled through it (`syntaxBroken` only cares about `yaml-syntax`/`not-a-mapping`).
  // Before this fix, `serializeWorkflowPreserving` gated its fallback on `hasErrors` (any error
  // finding at all), so a version-2 file — the one case the codebase explicitly designs for
  // surviving an older pane (`extra` pass-through) — silently full-canonicalized on the very
  // first edit, for a reason the human was never shown.
  const text = `# a note the file's comments carry
version: 2
blocks:
  - id: a
    name: A
    kind: worker
    cli: claude
`;
  const { workflow, findings } = parseWorkflow(text);
  assert.ok(findings.some((f) => f.code === "version-unsupported"), "sanity: this finding fires");

  const edited = { ...workflow, blocks: [{ ...workflow.blocks[0]!, model: "opus" }] };
  const out = serializeWorkflowPreserving(edited, text);
  assert.match(out, /# a note the file's comments carry/, "the comment must not be silently eaten");
  assert.deepEqual(parseWorkflow(out).workflow, edited);
});

test("original text that doesn't parse falls back to the ordinary canonical rewrite, never a guess", () => {
  const w = starterWorkflow();
  const broken = "version: 1\nblocks:\n\t- id: w\n"; // a tab in the indentation — a syntax finding
  assert.equal(serializeWorkflowPreserving(w, broken), serializeWorkflow(w));
});

test("an empty original text still produces a valid file that round-trips", () => {
  // Empty text has no syntax error (`isUnreadable` is about READABILITY, not about every
  // finding — #233 B3), so this goes through the ordinary preserving path rather than a
  // hard-coded "brand new file" shortcut; there is simply nothing to reuse, so every piece
  // regenerates canonically. What matters is that it's still a correct, round-trip-safe file.
  const w = starterWorkflow();
  const out = serializeWorkflowPreserving(w, "");
  assert.deepEqual(parseWorkflow(out).workflow, parseWorkflow(serializeWorkflow(w)).workflow);
});

test("a block sequence indented to something other than loomux's own 2 spaces is preserved AT that indent", () => {
  // #233 non-blocking #2: a regenerated (edited/added) item is emitted at the FILE's own marker
  // indent, not a hardcoded one — so it never has to choose between corrupting the sequence
  // (mixing two indents) and reformatting the whole roster just because one field changed.
  const text = `version: 1
blocks:
    - id: w
      name: W
      kind: worker
      cli: claude

    - id: w2
      name: W2
      kind: worker
      cli: claude
`;
  const { workflow } = parseWorkflow(text);
  const edited = {
    ...workflow,
    blocks: workflow.blocks.map((b) => (b.id === "w" ? { ...b, model: "opus" } : b)),
  };
  const out = serializeWorkflowPreserving(edited, text);
  assert.deepEqual(parseWorkflow(out).workflow, edited);
  // The untouched sibling (w2) is reused verbatim at its original indent…
  assert.match(out, /\n {4}- id: w2\n {6}name: W2\n/);
  // …and the regenerated one matches that SAME indent, not a hardcoded 2.
  assert.match(out, /\n {4}- id: w\n {6}name: W\n {6}kind: worker\n {6}cli: claude\n {6}model: opus\n/);
});

test("a block sequence at column 0 (same indent as `blocks:` itself) is understood, not misread as new keys", () => {
  // #233 B1: `blocks:` with nothing after it may be followed by its own sequence at the SAME
  // column — legal YAML the reader (`afterKey`, above) already accepts. A structural scan that
  // treated each `- id: …` as a bogus new top-level key spliced roster content into `front` and
  // silently discarded everything after the first misread line on re-parse.
  const text = `version: 1
blocks:
- id: a
  name: A
  kind: worker
  cli: claude
- id: b
  name: B
  kind: worker
  cli: claude
`;
  const { workflow } = parseWorkflow(text);
  assert.equal(workflow.blocks.length, 2, "sanity: the real reader sees both blocks");

  // A total no-op must reproduce the file exactly — the strongest form of "not destructive".
  assert.equal(serializeWorkflowPreserving(workflow, text), text);

  // And an edit to one of them must not lose the other, or silently drop the roster.
  const edited = {
    ...workflow,
    blocks: workflow.blocks.map((b) => (b.id === "b" ? { ...b, model: "opus" } : b)),
  };
  const out = serializeWorkflowPreserving(edited, text);
  assert.deepEqual(parseWorkflow(out).workflow, edited);
});

test("an ORPHAN column-0 dash sequence (no owning key at all) safely falls back — nothing is lost", () => {
  // Round 2: the same-column fix above only recognizes a `- …` line as sequence CONTENT when it
  // directly follows an empty-rest key (`blocks:` with nothing after the colon). A `- id: a`
  // line with NO such key before it at all — nobody wrote `blocks:` — is an ORPHAN: `splitKey`
  // still returns a "key" for it (`- id`, since the text contains a `: `), and reading THAT as a
  // fresh top-level key is the same B1 mistake with no governing key to blame it on. The real
  // reader's `mapping()` stops here too (filed separately as its own issue: it does so SILENTLY,
  // with no finding) — so `orig.workflow.blocks` is already empty by the time this scan sees it.
  const text = `version: 1
- id: a
  name: A
  kind: worker
  cli: claude
`;
  const { workflow: orig } = parseWorkflow(text);
  assert.equal(orig.blocks.length, 0, "sanity: the real reader never reads this as a roster at all");

  // A block added through the form (`orig` had none) must survive being written back and
  // reloaded — not get silently swallowed by a scan that trusted the orphan dash as a key.
  const withBlock = addBlock(orig, newBlock("w", "W"));
  const out = serializeWorkflowPreserving(withBlock, text);
  const reloaded = parseWorkflow(out).workflow;
  assert.deepEqual(reloaded.blocks.map((b) => b.id), ["w"], "the added block must survive a reload");
});

test("no double blank line when a regenerated item follows one whose scalar ran to the segment's end", () => {
  // Round 2: a `prompt: |` that is the LAST field of an item, followed by exactly one blank
  // line before the next item, is ambiguous — the blank line could be trailing content of the
  // scalar (which the real reader's own chomping would discard) or the ordinary separator
  // before the next item. `opaqueScalarIndices` used to leave it "stuck" inside the (never
  // properly closed) scalar for the rest of the segment, so it stayed as unpeelable content of
  // item `a` — and when item `b` was then regenerated, the synthetic separator this function
  // always inserts before a regenerated item stacked a SECOND blank line on top of it.
  const text = `version: 1
blocks:
  - id: a
    name: A
    kind: worker
    cli: claude
    prompt: |
      line one

  - id: b
    name: B
    kind: worker
    cli: claude
`;
  const { workflow } = parseWorkflow(text);
  const edited = {
    ...workflow,
    blocks: workflow.blocks.map((b) => (b.id === "b" ? { ...b, model: "opus" } : b)),
  };
  const out = serializeWorkflowPreserving(edited, text);
  assert.doesNotMatch(out, /\n\n\n/, "at most one blank line between the two items");
  assert.deepEqual(parseWorkflow(out).workflow, edited);
});

test("emptying the roster keeps the section's own header comment, not just a bare `blocks: []`", () => {
  const text = `version: 1
# BLOCKS — the agents a run may use, closed-set kind:
blocks:
  - id: a
    name: A
    kind: worker
    cli: claude
`;
  const { workflow } = parseWorkflow(text);
  const emptied = { ...workflow, blocks: [] };
  const out = serializeWorkflowPreserving(emptied, text);
  assert.match(out, /# BLOCKS — the agents a run may use, closed-set kind:/);
  assert.match(out, /^blocks: \[\]$/m);
  assert.deepEqual(parseWorkflow(out).workflow, emptied);
});

test("CRLF line endings are preserved end to end, on every platform (#233 non-blocking #3)", () => {
  // A 5-line fixture with EXPLICIT `\r\n`, so this is pinned independent of what line ending
  // the test runner's own checkout happens to have (the dogfood test exercises the real file's
  // actual bytes, which on a Linux CI runner may be LF even though this repo targets Windows).
  const text = "version: 1\r\nblocks:\r\n  - id: w\r\n    name: W\r\n    kind: worker\r\n";
  const { workflow } = parseWorkflow(text);

  assert.equal(serializeWorkflowPreserving(workflow, text), text, "a no-op must reproduce it byte for byte");

  const edited = { ...workflow, blocks: [{ ...workflow.blocks[0]!, cli: "claude" }] };
  const out = serializeWorkflowPreserving(edited, text);
  assert.ok(out.includes("\r\n"), "CRLF survives an edit too");
  assert.ok(!/[^\r]\n/.test(out), "no bare LF snuck in anywhere");
  assert.deepEqual(parseWorkflow(out).workflow, edited);
});

// ---------- broken files still open ----------

test("a file that cannot be fully understood still opens, with findings", () => {
  const { workflow, findings } = analyzeWorkflow(`version: 1
blocks:
  - id: mystery
    name: Mystery
    kind: superuser
    cli: goose
`);
  // The block is a STUB, not a dropped row: a block you cannot see is a block you
  // cannot repair (the ComfyUI import-failure class the design note names).
  assert.equal(workflow.blocks.length, 1);
  assert.equal(workflow.blocks[0]!.id, "mystery");
  assert.ok(has(findings, "unknown-kind"));
  assert.ok(has(findings, "unknown-cli"));
});

test("a syntax error is a finding on a line, not a thrown parse", () => {
  const { findings, workflow } = analyzeWorkflow(`version: 1
blocks:
  - id: w
    name: W
    kind: worker
    cli: [claude
`);
  const syntax = findings.find((f) => f.code === "yaml-syntax");
  assert.ok(syntax, "an unterminated flow list must report as a finding");
  assert.equal(syntax!.line, 6, "and it must say WHICH line");
  assert.equal(workflow.blocks.length, 1, "the rest of the file still loads");
});

test("an unexpected top-level `-` is a finding, not a silent truncation (#270)", () => {
  // The reader used to treat ANY `-`-prefixed line as "a sequence at this level ends the
  // mapping" — correct when handing a same-indent sequence off to an enclosing key, but
  // `mapping(0)` (called once, from `document()`) has no enclosing key to hand off to. It
  // just stopped, silently, and everything from that line to EOF vanished with zero findings.
  const { workflow, findings } = analyzeWorkflow(`version: 1
- id: a
  name: A
  kind: worker
  cli: claude
`);
  const syntax = findings.find((f) => f.code === "yaml-syntax");
  assert.ok(syntax, "an orphan top-level dash must report as a finding");
  assert.equal(syntax!.line, 2, "and it must say WHICH line");
  assert.equal(workflow.blocks.length, 0, "there was no `blocks:` key at all — nothing to read");
});

test("a top-level `blocks:` roster is still read after an orphan `-` line earlier in the file", () => {
  // The reader recovers: it consumes the whole orphan sequence (reporting it once) and keeps
  // reading the rest of the document as a mapping, rather than treating the entire remainder
  // as lost.
  const { workflow, findings } = analyzeWorkflow(`version: 1
- id: orphan
  name: Orphan
blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
`);
  assert.ok(findings.find((f) => f.code === "yaml-syntax"));
  assert.deepEqual(
    workflow.blocks.map((b) => b.id),
    ["w"],
    "the real roster after the orphan line is still read, not dropped too"
  );
});

test("an empty file is a workflow with nothing in it, not an error page", () => {
  const { findings, workflow } = analyzeWorkflow("");
  assert.equal(workflow.blocks.length, 0);
  assert.ok(has(findings, "no-blocks"));
  assert.ok(!has(findings, "yaml-syntax"));
});

// ---------- validation: one rule at a time ----------

test("the reference workflow validates clean", () => {
  assert.deepEqual(codes(analyzeWorkflow(SAMPLE).findings), []);
  assert.deepEqual(codes(validateWorkflow(starterWorkflow())), []);
});

test("kind must be a capability class — a workflow can never invent one", () => {
  const w = starterWorkflow();
  w.blocks[0]!.kind = "superuser";
  const f = validateWorkflow(w);
  assert.ok(has(f, "unknown-kind"));
  assert.equal(f.find((x) => x.code === "unknown-kind")!.blockId, "planner");
  // Every declared class is accepted, so the rule cannot drift from the enum.
  for (const kind of BLOCK_KINDS) {
    const ok = starterWorkflow();
    ok.blocks[0]!.kind = kind;
    assert.ok(!has(validateWorkflow(ok), "unknown-kind"), `${kind} must be accepted`);
  }
});

test("cli must be one loomux can actually spawn", () => {
  const w = starterWorkflow();
  w.blocks[1]!.cli = "goose";
  assert.ok(has(validateWorkflow(w), "unknown-cli"));
});

test("duplicate and malformed block ids are caught", () => {
  const dup = starterWorkflow();
  dup.blocks[1]!.id = "planner";
  assert.ok(has(validateWorkflow(dup), "block-id-duplicate"));

  const bad = starterWorkflow();
  bad.blocks[0]!.id = "Rev Security!";
  assert.ok(has(validateWorkflow(bad), "block-id-invalid"));

  const missing = starterWorkflow();
  missing.blocks[0]!.id = "";
  assert.ok(has(validateWorkflow(missing), "block-id-missing"));

  assert.ok(isValidBlockId("rev-security"));
  assert.ok(isValidBlockId("rev_2"));
  assert.ok(!isValidBlockId("2rev"));
  assert.ok(!isValidBlockId("rev security"));
  assert.ok(!isValidBlockId("../etc"));
});

test("an edge to a block that doesn't exist is caught before anything spawns", () => {
  const w = starterWorkflow();
  w.edges.push({ from: "worker", to: "rev-perf" });
  const f = validateWorkflow(w);
  assert.ok(has(f, "edge-unknown-block"));
  assert.match(f.find((x) => x.code === "edge-unknown-block")!.message, /rev-perf/);

  const self = starterWorkflow();
  self.edges.push({ from: "worker", to: "worker" });
  assert.ok(has(validateWorkflow(self), "edge-self"));
});

test("a gate that could never open is an error, not a runtime surprise", () => {
  // The reviewer it names doesn't exist…
  const ghost = starterWorkflow();
  ghost.gates.merge!.reviewers = ["rev-perf"];
  assert.ok(has(validateWorkflow(ghost), "gate-unknown-reviewer"));

  // …it names a block that isn't a reviewer (only a reviewer records a verdict)…
  const notRev = starterWorkflow();
  notRev.gates.merge!.reviewers = ["worker"];
  assert.ok(has(validateWorkflow(notRev), "gate-not-a-reviewer"));

  // …it needs more passes than there are reviewers…
  const greedy = starterWorkflow();
  greedy.gates.merge = { require: "threshold", threshold: 2, reviewers: ["reviewer"], also: [] };
  assert.ok(has(validateWorkflow(greedy), "gate-bad-threshold"));

  // …a threshold gate with no threshold…
  const noN = starterWorkflow();
  noN.gates.merge = { require: "threshold", reviewers: ["reviewer"], also: [] };
  assert.ok(has(validateWorkflow(noN), "gate-bad-threshold"));

  // …it gates on nothing at all…
  const empty = starterWorkflow();
  empty.gates.merge!.reviewers = [];
  assert.ok(has(validateWorkflow(empty), "gate-no-reviewers"));

  // …or it requires something we don't know how to enforce.
  const odd = starterWorkflow();
  odd.gates.merge!.require = "vibes";
  assert.ok(has(validateWorkflow(odd), "gate-unknown-require"));

  // A well-formed threshold gate is clean.
  const good = starterWorkflow();
  good.blocks.push({ id: "rev-2", name: "R2", kind: "reviewer", cli: "claude", model: "" });
  good.edges.push({ from: "worker", to: "rev-2" });
  good.gates.merge = { require: "threshold", threshold: 2, reviewers: ["reviewer", "rev-2"], also: [] };
  assert.deepEqual(codes(validateWorkflow(good)), []);
});

test("the small-batch clause survives a round trip and refuses a limit that limits nothing (#1174)", () => {
  // `MergeGate` has NO unknown-key bag, so a gate key the parser does not read is a
  // line the next form edit silently DELETES. That is the failure this covers, and
  // it is why the assertion is a round trip rather than a parse.
  const text = `version: 1
blocks:
  - id: worker
    kind: worker
    cli: claude
  - id: reviewer
    kind: reviewer
    cli: claude
gates:
  merge:
    require: all-pass
    reviewers: [reviewer]
    max_diff_lines: 800
`;
  const parsed = parseWorkflow(text).workflow;
  assert.equal(parsed.gates.merge!.max_diff_lines, 800);
  const round = parseWorkflow(serializeWorkflow(parsed)).workflow;
  assert.equal(round.gates.merge!.max_diff_lines, 800, "a save must not drop the limit");
  assert.deepEqual(codes(validateWorkflow(parsed)), [], "a declared limit is a clean file");

  // Undeclared stays undeclared — the emitter writes no line, so opening the gate
  // form on a repo with no limit cannot invent one.
  const none = starterWorkflow();
  assert.equal(none.gates.merge!.max_diff_lines, undefined);
  assert.ok(!serializeWorkflow(none).includes("max_diff_lines"));

  // 0 is a file the ENGINE refuses, so the pane must say so rather than bless it.
  const zero = starterWorkflow();
  zero.gates.merge!.max_diff_lines = 0;
  assert.ok(has(validateWorkflow(zero), "gate-bad-max-diff-lines"));
  const fractional = starterWorkflow();
  fractional.gates.merge!.max_diff_lines = 1.5;
  assert.ok(has(validateWorkflow(fractional), "gate-bad-max-diff-lines"));
  // …and a positive whole number is clean, so the two assertions above cannot be
  // passing against a rule that simply flags everything.
  const ok = starterWorkflow();
  ok.gates.merge!.max_diff_lines = 1;
  assert.deepEqual(codes(validateWorkflow(ok)), []);

  // A non-number in the FILE is a finding at parse time, never a coerced value.
  const bad = parseWorkflow(text.replace("max_diff_lines: 800", "max_diff_lines: eight"));
  assert.ok(bad.findings.some((f) => f.code === "gate-bad-max-diff-lines"));
});

test("a block declaring both a prompt and a profile is ambiguous", () => {
  const w = starterWorkflow();
  w.blocks[2]!.prompt = "Review the auth path.";
  w.blocks[2]!.profile = ".github/agents/rev.md";
  assert.ok(has(validateWorkflow(w), "prompt-and-profile"));
});

test("role_hint requires its matching capability class (#250/#324)", () => {
  // advisor -> planner, process -> worker. Mirrors the backend's
  // `role_hint_requires` (workflow.rs) so this pane's pre-run pass never
  // disagrees with what the real parser would say.
  const advisorOk = starterWorkflow();
  advisorOk.blocks[0]!.role_hint = "advisor"; // blocks[0] is the planner
  assert.deepEqual(codes(validateWorkflow(advisorOk)), []);

  const processOk = starterWorkflow();
  processOk.blocks[1]!.role_hint = "process"; // blocks[1] is the worker
  assert.deepEqual(codes(validateWorkflow(processOk)), []);

  // The mismatched pairing is a NAMED finding, not a silent no-op.
  const mismatched = starterWorkflow();
  mismatched.blocks[1]!.role_hint = "advisor"; // worker, not planner
  const f = validateWorkflow(mismatched);
  assert.ok(has(f, "role-hint-wrong-kind"));
  assert.equal(f.find((x) => x.code === "role-hint-wrong-kind")!.blockId, "worker");

  const mismatched2 = starterWorkflow();
  mismatched2.blocks[0]!.role_hint = "process"; // planner, not worker
  assert.ok(has(validateWorkflow(mismatched2), "role-hint-wrong-kind"));

  // An unrecognized value is its own finding, never coerced to the nearest hint.
  const bogus = starterWorkflow();
  bogus.blocks[0]!.role_hint = "supervisor";
  assert.ok(has(validateWorkflow(bogus), "role-hint-unknown"));

  // Absent is clean — today's behavior, byte for byte.
  assert.deepEqual(codes(validateWorkflow(starterWorkflow())), []);
});

test("role_hint case handling matches the backend's lowercasing (#250/#324 rider)", () => {
  // `role_hint_requires` (workflow.rs) trims and lowercases before comparing, so
  // `role_hint: Advisor` parses clean on the real engine. This pane's pre-run pass
  // must agree, or it flags a file the real parser accepts as broken.
  assert.equal(roleHintRequires("Advisor"), "planner");
  assert.equal(roleHintRequires("ADVISOR"), "planner");
  assert.equal(roleHintRequires(" process "), "worker");
  assert.equal(roleHintRequires("Process"), "worker");
  assert.equal(roleHintRequires("supervisor"), undefined, "still rejected, never coerced");

  const w = starterWorkflow();
  w.blocks[0]!.role_hint = "Advisor"; // blocks[0] is the planner
  assert.deepEqual(
    codes(validateWorkflow(w)),
    [],
    "a capitalized role_hint the real parser accepts must not be flagged as unknown here"
  );
});

test("role_hint: liaison requires kind: reviewer (#891)", () => {
  // The human-facing pane rides the reviewer capability class. Mirrors the
  // backend's `role_hint_requires`, including its trim + lowercase, so this
  // pane never flags a file the real parser accepts.
  assert.equal(roleHintRequires("liaison"), "reviewer");
  assert.equal(roleHintRequires(" Liaison "), "reviewer");
  assert.equal(roleHintRequires("liason"), undefined, "a typo is rejected, never coerced");

  // On a reviewer block the gate does not name: no hint finding, no gate finding.
  const ok = starterWorkflow();
  ok.blocks.push({
    id: "human",
    name: "Liaison",
    kind: "reviewer",
    cli: "claude",
    model: "",
    role_hint: "liaison",
  });
  ok.edges.push({ from: "worker", to: "human" });
  assert.deepEqual(
    codes(validateWorkflow(ok)).filter((c) => c.startsWith("role-hint") || c.startsWith("gate-")),
    []
  );

  // On every other kind it is the same named finding — "requires reviewer" is a
  // claim about the whole set of kinds it is NOT allowed on, so check them all.
  for (const [i, kind] of [
    [0, "planner"],
    [1, "worker"],
  ] as const) {
    const bad = starterWorkflow();
    bad.blocks[i]!.role_hint = "liaison";
    const f = validateWorkflow(bad);
    assert.ok(has(f, "role-hint-wrong-kind"), `liaison on a ${kind} block must be flagged`);
    assert.equal(f.find((x) => x.code === "role-hint-wrong-kind")!.blockId, kind);
  }
});

test("a merge gate may not name a liaison as one of its reviewers (#891)", () => {
  // A liaison IS reviewer-kind, so it slips past the "not a reviewer" check —
  // but it never records a verdict, so a gate naming one could never open.
  // The backend refuses the same file at parse; this pane must not call it fine.
  const w = starterWorkflow();
  w.blocks[2]!.role_hint = "liaison"; // blocks[2] is the block the gate names
  const f = validateWorkflow(w);
  assert.ok(has(f, "gate-not-a-reviewer"), `a gate naming a liaison must be flagged: ${codes(f)}`);
  assert.match(
    f.find((x) => x.code === "gate-not-a-reviewer")!.message,
    /liaison/,
    "and the message must say why, not just that the block is wrong"
  );

  // The control: the same document without the hint is clean, so the finding
  // above is attributable to the liaison rule and not to the fixture.
  assert.ok(!has(validateWorkflow(starterWorkflow()), "gate-not-a-reviewer"));
});

// ---------- #1161 M1: the `manager` kind ----------

/** `starterWorkflow()` plus a manager block, wired so it is not also flagged
 *  isolated — the shape a repo declaring one actually writes. */
const withManager = (id = "manager"): Workflow => {
  const w = starterWorkflow();
  w.blocks.push({ id, name: "Manager", kind: "manager", cli: "claude", model: "opus" });
  w.edges.push({ from: id, to: "planner" });
  return w;
};

test("kind: manager is a class this pane knows (#1161)", () => {
  // The mirror of the backend's `kind_from_str`. If these drift, the pane
  // paints a file the real engine accepts red — or, worse, saves one it
  // refuses.
  assert.ok(BLOCK_KINDS.includes("manager"));
  assert.ok(isBlockKind("manager"));
  assert.deepEqual(codes(validateWorkflow(withManager())), [], "a declared manager is an ordinary roster");

  // The negative control, and the half that makes the line "closed" rather than
  // "we added one more": a near-miss is still refused.
  assert.ok(!isBlockKind("managers"));
});

test("at most one manager block (#1161)", () => {
  const two = withManager();
  two.blocks.push({ id: "second-desk", name: "Desk 2", kind: "manager", cli: "claude", model: "" });
  two.edges.push({ from: "second-desk", to: "planner" });
  const f = validateWorkflow(two);
  assert.ok(has(f, "manager-not-unique"), `a second manager must be flagged: ${codes(f)}`);
  // Both ids, not just the later one — the second declaration is no more wrong
  // than the first, and the author needs to see which two they wrote.
  const msg = f.find((x) => x.code === "manager-not-unique")!.message;
  assert.match(msg, /manager/);
  assert.match(msg, /second-desk/);

  // The control: one manager is fine, so the finding is about the SECOND.
  assert.ok(!has(validateWorkflow(withManager()), "manager-not-unique"));
});

test("a merge gate may not name the manager as one of its reviewers (#1161)", () => {
  const w = withManager();
  w.gates.merge!.reviewers = ["manager"];
  const f = validateWorkflow(w);
  assert.ok(has(f, "gate-not-a-reviewer"), `a gate naming the manager must be flagged: ${codes(f)}`);
  // And it must say what the manager IS — an author who named it was reaching
  // for "the human signs off", which is real and which this gate cannot
  // express. "that block's kind is manager" would describe a type error.
  assert.match(f.find((x) => x.code === "gate-not-a-reviewer")!.message, /human/);

  // The control: the same document with the real reviewer on the gate is clean.
  assert.ok(!has(validateWorkflow(withManager()), "gate-not-a-reviewer"));
});

test("a manager block may not declare allow: (#1161 D1)", () => {
  // The pane's mirror of the engine's D1 refusal. `allowDenialReason` is the
  // one predicate the form and the validation pass share, so they cannot
  // disagree about which kinds may pre-approve a tool.
  assert.ok(allowDenialReason("manager"));
  const w = withManager();
  w.blocks[3]!.allow = ["Bash(gh pr merge *)"];
  const f = validateWorkflow(w);
  assert.ok(has(f, "allow-not-permitted"), `${codes(f)}`);
  assert.equal(f.find((x) => x.code === "allow-not-permitted")!.blockId, "manager");

  // The control: a reviewer with the same pattern keeps it — a reviewer has its
  // shell by design, so the refusal above is about the class.
  assert.equal(allowDenialReason("reviewer"), null);
});

test("a loomux-owned block may not declare a persona (#1161 D1, review N5)", () => {
  // The pane's mirror of `persona_allowed` / `parse_workflow`'s refusal. The
  // engine fails the WHOLE FILE over this, so a pane reporting it clean lets an
  // author save a workflow that silently launches on the built-in roster with
  // no finding to explain where their roster went.
  for (const kind of ["manager", "orchestrator"] as const) {
    for (const key of ["prompt", "profile"] as const) {
      const w = withManager();
      const i = w.blocks.findIndex((b) => b.kind === "manager");
      w.blocks[i]! = { ...w.blocks[i]!, kind, [key]: key === "prompt" ? "Say it is fine." : ".github/agents/x.md" };
      const f = validateWorkflow(w);
      assert.ok(has(f, "persona-not-permitted"), `${key} on a ${kind} block: ${codes(f)}`);
      assert.match(f.find((x) => x.code === "persona-not-permitted")!.message, new RegExp(key));
    }
  }

  // The controls, and they are what keep this from being "no block may carry a
  // persona". A reviewer's persona is the entire point of the workflow feature,
  // and a PLANNER's is the pairing that matters most: a planner may carry one
  // while being denied `allow:`, so the two rules are not co-extensive and the
  // predicates must stay separate.
  assert.equal(personaDenialReason("reviewer"), null);
  assert.equal(personaDenialReason("planner"), null);
  assert.equal(allowDenialReason("planner") === null, false, "…but a planner still may not allow:");
  const ok = withManager();
  ok.blocks[2]! = { ...ok.blocks[2]!, prompt: "Review for security." }; // the reviewer
  assert.ok(!has(validateWorkflow(ok), "persona-not-permitted"));
});

test("the manager never reviews, so it can never satisfy a gate (#1161)", () => {
  // `isReviewingBlock` is what the gate's reviewer checkboxes offer and what
  // switching the gate on fills in. A manager answering true there would make
  // the editor author, in one keystroke, the exact file the test above flags.
  assert.equal(isReviewingBlock({ kind: "manager" }), false);
  assert.equal(isReviewingBlock({ kind: "reviewer" }), true, "the non-vacuity control");
});

test("isReviewingBlock separates the blocks that review from the class they ride (#891)", () => {
  // The pane's mirror of the backend's `is_reviewing_block`. It is what the
  // merge-gate reviewer list offers and what switching the gate ON fills in —
  // and the pairing that matters is the last two: a liaison is reviewer-KIND,
  // so a `kind` filter cannot tell them apart and the editor would author a
  // file `validateWorkflow` flags `gate-not-a-reviewer` on the same keystroke.
  assert.equal(isReviewingBlock({ kind: "reviewer" }), true);
  assert.equal(isReviewingBlock({ kind: "worker" }), false);
  assert.equal(isReviewingBlock({ kind: "reviewer", role_hint: "process" }), true,
    "another hint on a reviewer subtracts nothing — only the liaison does");
  assert.equal(isReviewingBlock({ kind: "reviewer", role_hint: "liaison" }), false);
  // Trimmed and case-folded, like `roleHintRequires` and the backend parser: a
  // file the real engine reads as a liaison must not read as a reviewer here.
  assert.equal(isReviewingBlock({ kind: "reviewer", role_hint: " Liaison " }), false);

  // The property the two call sites depend on, asserted directly rather than
  // through the DOM they live in: filtering a real roster leaves exactly the
  // blocks a merge gate may name.
  const w = starterWorkflow();
  w.blocks[2]!.role_hint = "liaison";
  const gateable = w.blocks.filter(isReviewingBlock).map((b) => b.id);
  assert.ok(!gateable.includes(w.blocks[2]!.id), `the liaison must not be offered: ${gateable}`);
  assert.deepEqual(
    validateWorkflow({ ...w, gates: { merge: { require: "all-pass", reviewers: gateable, also: [] } } })
      .filter((f) => f.code === "gate-not-a-reviewer"),
    [],
    "a gate filled from this predicate must validate clean — that is the whole point of it"
  );
});

test("role_hint round-trips through serialize/parse unchanged", () => {
  const w = starterWorkflow();
  w.blocks[0]!.role_hint = "advisor";
  const reread = parseWorkflow(serializeWorkflow(w)).workflow;
  assert.equal(reread.blocks[0]!.role_hint, "advisor");
  // ...and a block that never declared one stays undefined, not "".
  assert.equal(reread.blocks[1]!.role_hint, undefined);
  // Formatting twice is still a no-op with the field present.
  assert.equal(serializeWorkflow(reread), serializeWorkflow(w));
});

test("a block nothing wires up is a warning, not a hard error", () => {
  const w = starterWorkflow();
  w.blocks.push({ id: "rev-perf", name: "Perf", kind: "reviewer", cli: "claude", model: "" });
  const f = validateWorkflow(w);
  const isolated = f.find((x) => x.code === "isolated-block");
  assert.ok(isolated, "a reviewer nobody points at will never be asked to review");
  assert.equal(isolated!.severity, "warning", "edges are advisory — this must not block a run");
  assert.equal(isolated!.blockId, "rev-perf");
  assert.equal(hasErrors(f), false);
});

test("unreachable and entry-less graphs are reported; a rework loop is not", () => {
  // A block only reachable through a cycle it isn't part of an entry for.
  const stranded = starterWorkflow();
  stranded.blocks.push({ id: "rev-2", name: "R2", kind: "reviewer", cli: "claude", model: "" });
  stranded.blocks.push({ id: "rev-3", name: "R3", kind: "reviewer", cli: "claude", model: "" });
  stranded.edges.push({ from: "rev-2", to: "rev-3" }, { from: "rev-3", to: "rev-2" });
  assert.ok(has(validateWorkflow(stranded), "unreachable-block"));

  // The worker ⇄ reviewer REWORK LOOP is how loomux actually works — a cycle must not
  // be a finding on its own.
  const loop = starterWorkflow();
  loop.edges.push({ from: "reviewer", to: "worker" });
  const f = validateWorkflow(loop);
  assert.deepEqual(codes(f), [], "the rework loop is legitimate, not a defect");

  // But a graph where EVERY block is pointed at has nowhere to start.
  const closed: Workflow = {
    version: 1,
    name: "",
    blocks: [
      { id: "a", name: "A", kind: "worker", cli: "claude", model: "" },
      { id: "b", name: "B", kind: "reviewer", cli: "claude", model: "" },
    ],
    edges: [
      { from: "a", to: "b" },
      { from: "b", to: "a" },
    ],
    gates: {},
  };
  assert.ok(has(validateWorkflow(closed), "no-entry-block"));
});

test("the version is checked before anything else trusts the shape", () => {
  assert.ok(has(parseWorkflow("blocks: []").findings, "version-missing"));
  assert.ok(has(parseWorkflow(`version: ${WORKFLOW_VERSION + 1}\nblocks: []`).findings, "version-unsupported"));
});

// ---------- the derived graph ----------

test("the graph layers the declared path and flags what doesn't resolve", () => {
  const g = deriveGraph(parseWorkflow(SAMPLE).workflow);
  // Layers hold block INDICES (rev-5 F5) — the roster's rows, not their ids.
  assert.deepEqual(g.layers, [[0], [1], [2, 3]]);
  assert.ok(g.nodes.every((n) => n.known));
  assert.ok(g.edges.every((e) => e.resolved));
  assert.deepEqual(g.gates, [
    { name: "merge", require: "all-pass", threshold: undefined, reviewers: ["rev-security", "rev-tests"] },
  ]);

  const broken = parseWorkflow(SAMPLE).workflow;
  broken.edges.push({ from: "worker", to: "ghost" });
  broken.blocks[0]!.kind = "superuser";
  const bg = deriveGraph(broken);
  assert.equal(bg.nodes.find((n) => n.block.id === "planner")!.known, false);
  assert.equal(bg.edges.find((e) => e.to === "ghost")!.resolved, false);
});

test("broken blocks each get their OWN node in the graph (rev-5 F5)", () => {
  // Keyed by id, two id-less stubs (both "") mapped to ONE position and rendered stacked, so
  // a file with two broken blocks showed one — in the view whose whole job is to show you the
  // file. Same for a duplicate-id pair.
  const stubs: Workflow = {
    version: 1,
    name: "",
    blocks: [
      { id: "", name: "stub A", kind: "worker", cli: "claude", model: "" },
      { id: "", name: "stub B", kind: "reviewer", cli: "claude", model: "" },
      { id: "dupe", name: "first", kind: "reviewer", cli: "claude", model: "" },
      { id: "dupe", name: "second", kind: "reviewer", cli: "claude", model: "" },
    ],
    edges: [],
    gates: {},
  };
  const g = deriveGraph(stubs);
  assert.equal(g.nodes.length, 4);
  assert.deepEqual(
    g.nodes.map((n) => n.index),
    [0, 1, 2, 3],
    "every row is its own node, whatever its id says"
  );
  // …and no two nodes share a slot: the flattened layers hold each index exactly once.
  const placed = g.layers.flat();
  assert.deepEqual([...placed].sort((a, b) => a - b), [0, 1, 2, 3]);
});

test("a file whose blocks have no ids at all makes no claim about entry points (rev-5 F6)", () => {
  // With no ids there is no graph to reason about — every edge is dangling, and
  // `edge-unknown-block` has already said so. "Every block is pointed at by another" was
  // neither true nor useful here.
  const w: Workflow = {
    version: 1,
    name: "",
    blocks: [
      { id: "", name: "a", kind: "worker", cli: "claude", model: "" },
      { id: "", name: "b", kind: "reviewer", cli: "claude", model: "" },
    ],
    edges: [{ from: "a", to: "b" }],
    gates: {},
  };
  const f = validateWorkflow(w);
  assert.ok(!has(f, "no-entry-block"));
  assert.ok(has(f, "block-id-missing"), "the finding that IS true still fires");
  assert.ok(has(f, "edge-unknown-block"));
});

test("a cyclic graph still layers (it must never spin)", () => {
  const w = starterWorkflow();
  w.edges.push({ from: "reviewer", to: "worker" });
  const g = deriveGraph(w);
  assert.equal(g.nodes.length, 3);
  assert.ok(g.layers.length >= 1);
});

// ---------- editing helpers ----------

test("a new block's id is unique and derived from its name", () => {
  const w = starterWorkflow();
  assert.equal(nextBlockId(w, "Security review"), "security-review");
  assert.equal(nextBlockId(w, "Worker"), "worker-2", "an id already in use gets suffixed, never reused");
  assert.equal(nextBlockId(w, "!!!"), "block");
  assert.ok(isValidBlockId(nextBlockId(w, "2nd reviewer")));
});

test("a created workflow records which loomux wrote it — and only a created one (rev-5 F7)", () => {
  // §4's "record the loomux version that authored it" (Langflow's last_tested_version
  // lesson). Written EXACTLY ONCE, at creation.
  const created = starterWorkflow("0.8.0");
  assert.match(serializeWorkflow(created), /^authored_with: 0\.8\.0$/m);
  assert.deepEqual(codes(validateWorkflow(created)), [], "and it is not itself a finding");

  // No version to hand → no key. An `authored_with: unknown` would be worse than an absent one.
  assert.ok(!serializeWorkflow(starterWorkflow()).includes("authored_with"));

  // On an EXISTING file it round-trips verbatim and is never restamped: opening a workflow
  // written by an older build and changing a model must not also rewrite the version line.
  const older = parseWorkflow(`version: 1
authored_with: 0.6.1
blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
`).workflow;
  assert.equal(older.authored_with, "0.6.1");
  assert.equal(older.extra, undefined, "a key loomux itself writes is never an unknown one");
  older.blocks[0]!.model = "opus"; // the ordinary form edit
  assert.match(serializeWorkflow(older), /^authored_with: 0\.6\.1$/m, "preserved, not restamped");
});

test("deleting a block takes every reference to it with it", () => {
  const w = starterWorkflow();
  const after = removeBlockAt(w, 2); // the reviewer
  assert.deepEqual(
    after.blocks.map((b) => b.id),
    ["planner", "worker"]
  );
  assert.deepEqual(after.edges, [{ from: "planner", to: "worker" }]);
  assert.deepEqual(after.gates.merge!.reviewers, [], "the gate must not keep gating on a block that's gone");
  // …and the result is therefore free of dangling references — which is the entire
  // point: a delete that left them behind would turn one click into three errors.
  assert.ok(!has(validateWorkflow(after), "edge-unknown-block"));
  assert.ok(!has(validateWorkflow(after), "gate-unknown-reviewer"));
});

test("deleting a broken block deletes THAT block — not everything shaped like it", () => {
  // The two cases the pane is guaranteed to meet, because they are exactly the ones the
  // validation pass is complaining about when the human reaches for Delete.
  //
  // Two id-LESS stubs: deleting one must not take the other. (An id-keyed delete would
  // remove "every block whose id is empty" — i.e. both.)
  const stubs: Workflow = {
    version: 1,
    name: "",
    blocks: [
      { id: "", name: "first stub", kind: "worker", cli: "claude", model: "" },
      { id: "", name: "second stub", kind: "reviewer", cli: "claude", model: "" },
    ],
    edges: [],
    gates: {},
  };
  const left = removeBlockAt(stubs, 0);
  assert.deepEqual(
    left.blocks.map((b) => b.name),
    ["second stub"]
  );

  // A DUPLICATE id survives its own deletion — the twin still answers to it — so the edges
  // and the gate that name it are still meaningful and must NOT be stripped.
  const dupes = starterWorkflow();
  dupes.blocks.push({ id: "reviewer", name: "Reviewer (copy)", kind: "reviewer", cli: "claude", model: "" });
  const after = removeBlockAt(dupes, 3);
  assert.deepEqual(after.edges, dupes.edges, "the surviving twin still answers to that id");
  assert.deepEqual(after.gates.merge!.reviewers, ["reviewer"]);
  assert.deepEqual(codes(validateWorkflow(after)), [], "and the duplicate is resolved by the delete");
});

// ---------- model knobs: effort / context (#687) ----------

/** The `agent_cli_knobs` replies the pane fetches per CLI, verbatim from
 *  `CLI_CAPS` (crates/loomux-engine/src/model.rs) — the pane never mirrors a
 *  vendor fact, it asks. */
const KNOBS: Record<string, CliKnobs> = {
  claude: {
    cli: "claude",
    known: true,
    effort: { values: ["low", "medium", "high", "xhigh", "max"], note: "--effort <level> is a session-scoped flag" },
    context: { values: ["1m"], note: "the [1m] model-alias suffix (sonnet[1m])" },
  },
  copilot: {
    cli: "copilot",
    known: true,
    effort: { values: [], note: "copilot reads effortLevel from ~/.copilot/settings.json" },
    context: { values: [], note: "copilot's context window is an interactive-only control (/context)" },
  },
};

/** The lookup the pane hands to `validateWorkflow`: what has been fetched so
 *  far, and `null` for a CLI whose reply hasn't landed. */
const lookup = (cli: string, model: string) =>
  KNOBS[cli] ? knobState(KNOBS[cli]!, cli, model) : null;

test("effort:/context: are real keys the pane reads, not unknown ones it merely keeps", () => {
  // Before #687 these landed in `extra` — preserved, but invisible to the form and
  // to validation. The engine parses them, so the pane must too.
  const { workflow, findings } = parseWorkflow(`version: 1
blocks:
  - id: worker
    name: Worker
    kind: worker
    cli: claude
    model: opus
    effort: xhigh
    context: 1m
`);
  assert.deepEqual(
    findings.map((f) => f.code),
    []
  );
  const b = workflow.blocks[0]!;
  assert.equal(b.effort, "xhigh");
  assert.equal(b.context, "1m");
  assert.equal(b.extra, undefined, "a known key must not ALSO be carried as an unknown one");
});

test("the knobs round-trip through serialize/parse, and a block without them stays clean", () => {
  const w = starterWorkflow();
  w.blocks[1]!.effort = "max"; // blocks[1] is the worker
  w.blocks[1]!.context = "1m";
  const text = serializeWorkflow(w);
  assert.match(text, /^ {4}effort: max$/m);
  assert.match(text, /^ {4}context: 1m$/m);
  const reread = parseWorkflow(text).workflow;
  assert.equal(reread.blocks[1]!.effort, "max");
  assert.equal(reread.blocks[1]!.context, "1m");
  // A block that declared neither stays undefined, not "" — the same distinction
  // role_hint keeps, and what makes "absent = the CLI's default" survive a save.
  assert.equal(reread.blocks[0]!.effort, undefined);
  assert.equal(reread.blocks[0]!.context, undefined);
  // Serializing twice is still a no-op with the fields present.
  assert.equal(serializeWorkflow(reread), serializeWorkflow(w));
  // ...and a file that pins nothing is unchanged, byte for byte.
  assert.equal(serializeWorkflow(starterWorkflow()), serializeWorkflow(parseWorkflow(serializeWorkflow(starterWorkflow())).workflow));
});

test("a knob the block's CLI cannot honor is a finding quoting that CLI's own reason", () => {
  // The engine REFUSES this file (`validate_knob`, workflow.rs). A pane that
  // reported it clean would send the human to a launch that falls back to the
  // built-in roster with no idea why.
  const w = starterWorkflow();
  w.blocks[1]!.cli = "copilot";
  w.blocks[1]!.effort = "xhigh";
  const f = validateWorkflow(w, lookup);
  assert.ok(has(f, "knob-unavailable"));
  const finding = f.find((x) => x.code === "knob-unavailable")!;
  assert.equal(finding.blockId, "worker");
  assert.equal(finding.severity, "error");
  assert.match(finding.message, /effort/);
  assert.match(finding.message, /~\/\.copilot\/settings\.json/, "the vendor's reason, not 'unsupported'");
});

test("a value outside the CLI's own vocabulary is a finding that names the vocabulary", () => {
  const w = starterWorkflow();
  w.blocks[1]!.effort = "banana";
  const f = validateWorkflow(w, lookup);
  assert.ok(has(f, "knob-unavailable"));
  assert.match(f.find((x) => x.code === "knob-unavailable")!.message, /low, medium, high, xhigh, max/);
  // The legal values are clean, and so is an empty one (= the CLI's default).
  for (const level of ["low", "medium", "high", "xhigh", "max", ""]) {
    const ok = starterWorkflow();
    ok.blocks[1]!.effort = level;
    assert.deepEqual(codes(validateWorkflow(ok, lookup)), [], `effort: ${level || "(empty)"} is legal`);
  }
});

test("context: is gated on the MODEL, not just the CLI (#709 carried finding)", () => {
  // `model: haiku` + `context: 1m` composes `--model haiku[1m]` — an alias the
  // vendor docs do not define (model-config §Extended context lists Fable 5,
  // Sonnet 5, Opus 4.6+ and Sonnet 4.6). The launcher can't produce it (the
  // control is disabled); a hand-written file can, and this is where that is
  // caught, at the surface a human authors it in.
  const w = starterWorkflow();
  w.blocks[1]!.model = "haiku";
  w.blocks[1]!.context = "1m";
  const f = validateWorkflow(w, lookup);
  assert.ok(has(f, "knob-unavailable"));
  assert.match(f.find((x) => x.code === "knob-unavailable")!.message, /haiku\[1m\]/);

  // The same block on a 1M-capable model is clean...
  const ok = starterWorkflow();
  ok.blocks[1]!.model = "sonnet";
  ok.blocks[1]!.context = "1m";
  assert.deepEqual(codes(validateWorkflow(ok, lookup)), []);
  // ...and effort is NOT gated with it: a model that lacks a level falls back to
  // the highest it supports (model-config §Adjust effort level).
  const effortOnHaiku = starterWorkflow();
  effortOnHaiku.blocks[1]!.model = "haiku";
  effortOnHaiku.blocks[1]!.effort = "max";
  assert.deepEqual(codes(validateWorkflow(effortOnHaiku, lookup)), []);
});

test("with no capability data the knob checks DEFER — they never guess", () => {
  // The pane fetches caps asynchronously, and a block may name a CLI it hasn't
  // fetched. Same deferral the real parser makes for a block with no explicit
  // `cli:` (workflow.rs): check what you can know here, and let the layer that
  // knows the resolved CLI check the rest.
  const w = starterWorkflow();
  w.blocks[1]!.cli = "copilot";
  w.blocks[1]!.effort = "xhigh";
  assert.deepEqual(codes(validateWorkflow(w)), [], "no caps in hand = no finding invented");
  assert.deepEqual(
    codes(validateWorkflow(w, (cli, model) => (cli === "claude" ? knobState(KNOBS.claude!, cli, model) : null))),
    [],
    "caps for the OTHER cli in hand is still no answer about this one"
  );
  // And a file that pins no knobs is unaffected whether caps are present or not.
  assert.deepEqual(codes(validateWorkflow(starterWorkflow(), lookup)), []);
});

// ---------- opencode as a spawnable block cli (#722) ----------

/** opencode's `agent_cli_knobs` reply, verbatim from `CLI_CAPS`
 *  (crates/loomux-engine/src/model.rs). A
 *  hand-copied literal, like every other fixture here — the note text's fidelity
 *  to the Rust source (including the `--variant` / `model-determined` substrings
 *  the assertions below match on) is pinned by `selectorknobs.test.ts`' last
 *  test, which reads `CLI_CAPS` back. What THIS file asks of it is only that a
 *  finding quotes the CLI's own reason rather than saying "unsupported". */
const OPENCODE_KNOBS: CliKnobs = {
  cli: "opencode",
  known: true,
  effort: {
    values: [],
    note: "opencode's reasoning effort is a model VARIANT: a session flag on `opencode run` (--variant) but absent from the TUI loomux spawns, and settable per-agent in loomux's generated config (agent.<name>.variant, observed values minimal|high|max) — the seam exists, but the per-model vocabulary is provider-specific and unverified against a live run, so loomux does not write it yet",
  },
  context: {
    values: [],
    note: "opencode's context window is model-determined; no session-scoped variant switch is documented or present in the TUI's options",
  },
};

const lookupWithOpencode = (cli: string, model: string) =>
  cli === "opencode" ? knobState(OPENCODE_KNOBS, cli, model) : lookup(cli, model);

test("a block may run cli: opencode, model and all (#722)", () => {
  // The backend spawns it (`SUPPORTED_CLIS`), so a pane that flagged the file
  // would send a human to fix a file that is already correct — and the pane's
  // block editor would not even offer the CLI in its dropdown.
  assert.ok(isWorkflowCli("opencode"));
  const w = starterWorkflow();
  w.blocks[1]!.cli = "opencode";
  w.blocks[1]!.model = "opencode/deepseek-v4-flash-free";
  assert.deepEqual(codes(validateWorkflow(w, lookupWithOpencode)), []);
  // Through the text the human actually writes, provider `/` and all — parse,
  // validate, serialize, re-read, with the id intact at every step.
  const src = `version: 1
blocks:
  - id: rev-oc
    name: Second opinion
    kind: reviewer
    cli: opencode
    model: opencode/deepseek-v4-flash-free
`;
  const { workflow, findings } = parseWorkflow(src);
  assert.deepEqual(
    findings.map((f) => f.code),
    []
  );
  assert.equal(workflow.blocks[0]!.cli, "opencode");
  assert.equal(workflow.blocks[0]!.model, "opencode/deepseek-v4-flash-free");
  const reread = parseWorkflow(serializeWorkflow(workflow)).workflow;
  assert.equal(reread.blocks[0]!.model, "opencode/deepseek-v4-flash-free", "the provider prefix must survive a save");
  // And the roster still rejects what the backend cannot spawn — widening the
  // list must not have turned the check into a rubber stamp.
  const bad = starterWorkflow();
  bad.blocks[1]!.cli = "opencodex";
  assert.ok(has(validateWorkflow(bad), "unknown-cli"));
});

test("an opencode block declaring effort: is a finding quoting opencode's own reason (#722)", () => {
  // The real engine refuses it (`validate_knob`), because opencode's TUI has no
  // variant flag. The pane has to say the same thing, in opencode's words — and
  // must NOT quietly accept a level it cannot deliver just because the CLI is
  // newly spawnable.
  const w = starterWorkflow();
  w.blocks[1]!.cli = "opencode";
  w.blocks[1]!.effort = "high";
  const f = validateWorkflow(w, lookupWithOpencode);
  assert.deepEqual(codes(f), ["knob-unavailable"]);
  const finding = f.find((x) => x.code === "knob-unavailable")!;
  assert.equal(finding.blockId, "worker");
  assert.equal(finding.severity, "error");
  assert.match(finding.message, /--variant/, "the vendor's reason, not 'unsupported'");
  // Same for context, and a block that pins neither stays clean.
  const ctx = starterWorkflow();
  ctx.blocks[1]!.cli = "opencode";
  ctx.blocks[1]!.context = "1m";
  assert.match(
    validateWorkflow(ctx, lookupWithOpencode).find((x) => x.code === "knob-unavailable")!.message,
    /model-determined/
  );
  const clean = starterWorkflow();
  clean.blocks[1]!.cli = "opencode";
  assert.deepEqual(codes(validateWorkflow(clean, lookupWithOpencode)), []);
});

// ---------- the full config surface: intake / merge_queue / resources / allow (#880) ----------
//
// `intake:` (#382), `merge_queue:` (#581), `resources:` (#858) and block `allow:` (#222)
// are all real fields of the engine's wire schema that this model had no name for — they
// landed in the unknown-key bag, so the pane rendered a file that declared them exactly
// like a file that didn't, and a form edit re-emitted them as one flattened flow mapping.
// These pin the whole surface: read into the model, emitted back, and — the part that
// matters most for a hand-written file — left alone when the edit was somewhere else.

const FULL_SURFACE = `# the whole schema, commented
version: 1
name: everything
authored_with: 0.9.9

# WHERE WORK COMES FROM (#382)
intake:
  source: github-labels
  labels:
    ready: go-build
    investigate: go-look

# THE QUEUE (#581)
merge_queue:
  enabled: true
  max_batch: 5
  checks_timeout_minutes: 90

blocks:
  - id: worker
    name: Worker
    kind: worker
    cli: claude
    allow: ["Bash(gh pr view --json title,body)"]

  - id: rev
    name: Reviewer
    kind: reviewer
    cli: claude

edges:
  - { from: worker, to: rev }

gates:
  merge:
    require: all-pass
    reviewers: [rev]

# WHAT AGENTS TAKE TURNS ON (#858)
resources:
  build:
    slots: 1
    max_hold_minutes: 45
`;

test("every section of the wire schema reads into the model — none of them is an unknown key (#880)", () => {
  const { workflow, findings } = analyzeWorkflow(FULL_SURFACE);
  assert.deepEqual(codes(findings), [], "a file using the documented schema must be clean");
  assert.equal(workflow.authored_with, "0.9.9");
  assert.deepEqual(workflow.intake, {
    source: "github-labels",
    labels: { ready: "go-build", investigate: "go-look" },
  });
  assert.deepEqual(workflow.merge_queue, {
    enabled: true,
    max_batch: 5,
    checks_timeout_minutes: 90,
  });
  assert.deepEqual(workflow.resources, { build: { slots: 1, max_hold_minutes: 45 } });
  // The comma lives INSIDE the quoted pattern: one entry, not two. `allow:` is the field
  // whose absence from this model was the whole reason the schema manifest exists.
  assert.deepEqual(workflow.blocks[0]!.allow, ["Bash(gh pr view --json title,body)"]);
  assert.equal(workflow.extra, undefined, "nothing at the top level is unknown any more");
  assert.equal(workflow.blocks[0]!.extra, undefined);
  // Absent stays absent: an undeclared label must not be filled in with its built-in
  // default, or the next save silently PINS what the file meant to inherit.
  assert.equal(workflow.intake!.labels!.owned, undefined);
});

test("the whole surface survives a canonical round-trip, twice", () => {
  const { workflow } = parseWorkflow(FULL_SURFACE);
  const out = serializeWorkflow(workflow);
  const reread = parseWorkflow(out);
  assert.deepEqual(reread.findings, []);
  assert.deepEqual(reread.workflow, workflow, "a Format must not drop a section it can read");
  assert.equal(serializeWorkflow(reread.workflow), out, "…and must be idempotent");
});

test("a commented file with intake: and merge_queue: survives an unrelated block edit byte-for-byte outside the edited block (#880)", () => {
  const { workflow } = parseWorkflow(FULL_SURFACE);
  const edited: Workflow = {
    ...workflow,
    blocks: workflow.blocks.map((b) => (b.id === "rev" ? { ...b, model: "opus" } : b)),
  };
  const out = serializeWorkflowPreserving(edited, FULL_SURFACE);
  assert.deepEqual(parseWorkflow(out).workflow, edited, "the edit itself must round-trip");

  // Cut the ONE item that changed out of both texts; what is left has to be identical —
  // not "still has the comments", identical. That is the claim a human actually cares
  // about when they open a file they hand-wrote and the pane saves it.
  const cutEditedBlock = (text: string): string => {
    const lines = text.split("\n");
    const start = lines.findIndex((l) => l.includes("- id: rev"));
    assert.ok(start > 0, "the fixture must contain the block being edited");
    let end = start + 1;
    while (end < lines.length && !/^\S/.test(lines[end]!)) end++;
    return [...lines.slice(0, start), ...lines.slice(end)].join("\n");
  };
  assert.equal(
    cutEditedBlock(out),
    cutEditedBlock(FULL_SURFACE),
    "everything outside the edited block must be the ORIGINAL text, byte for byte"
  );
});

test("editing intake: rewrites intake: and nothing else, and no section is relocated (#880)", () => {
  const { workflow } = parseWorkflow(FULL_SURFACE);
  const edited: Workflow = {
    ...workflow,
    intake: { ...workflow.intake, labels: { ...workflow.intake!.labels, ready: "ready-now" } },
  };
  const out = serializeWorkflowPreserving(edited, FULL_SURFACE);
  assert.deepEqual(parseWorkflow(out).workflow, edited);
  assert.match(out, /ready: ready-now/);
  // The comment ABOVE a section is about the section, not about the field that changed.
  assert.match(out, /# WHERE WORK COMES FROM \(#382\)/);
  assert.match(out, /# THE QUEUE \(#581\)/);
  assert.match(out, /# WHAT AGENTS TAKE TURNS ON \(#858\)/);
  assert.match(out, /# the whole schema, commented/);
  // And the file's own ORDER is kept — this fixture, like the repo's own workflow, writes
  // merge_queue: above blocks:, and an edit must not move it to the bottom.
  assert.ok(
    out.indexOf("merge_queue:") < out.indexOf("blocks:"),
    `merge_queue: must stay where the file put it:\n${out}`
  );
});

test("a typed section keeps the position the file gave it when an unrelated block is edited (#880)", () => {
  const text = `version: 1

# the queue, declared FIRST because that is the order this file reads in
merge_queue:
  enabled: true

blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
`;
  const { workflow } = parseWorkflow(text);
  assert.deepEqual(workflow.merge_queue, { enabled: true }, "read as a section, not an unknown key");
  const edited: Workflow = { ...workflow, blocks: [{ ...workflow.blocks[0]!, model: "opus" }] };
  const out = serializeWorkflowPreserving(edited, text);
  assert.ok(
    out.indexOf("merge_queue:") < out.indexOf("blocks:"),
    `a section is written where the FILE wrote it, not where the emitter would have:\n${out}`
  );
  assert.match(out, /# the queue, declared FIRST/);
  assert.deepEqual(parseWorkflow(out).workflow, edited);
});

test("an unknown key is an error finding — and is still preserved verbatim (#880)", () => {
  const text = `version: 1
promt: whoops
blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
    retires: 3
`;
  const { workflow, findings } = analyzeWorkflow(text);
  const unknown = findings.filter((f) => f.code === "unknown-key");
  assert.equal(unknown.length, 2, "one per key, wherever it sits");
  assert.ok(unknown.every((f) => f.severity === "error"));
  assert.match(unknown[0]!.message, /"promt"/);
  assert.match(
    unknown[0]!.message,
    /will not load/,
    "the finding must say what actually happens: this build's engine refuses the WHOLE file"
  );
  assert.equal(unknown[1]!.blockId, "w", "a block's unknown key is shown next to the block");

  // Warned, never deleted. A newer loomux may have written the key, and silently dropping
  // a human's line to make a warning go away is worse than the warning.
  assert.equal(serializeWorkflowPreserving(workflow, text), text);
  const canonical = serializeWorkflow(workflow);
  assert.match(canonical, /promt: whoops/);
  assert.match(canonical, /retires: 3/);
});

test("a gate loomux does not enforce is NOT an unknown key — gates: is a map, not a struct", () => {
  // The engine reads `gates:` as `BTreeMap<String, RawGate>`, so a gate it has no
  // machinery for still parses; only `merge` is enforced. Reporting it would be the pane
  // inventing a refusal the engine never makes — the mirror image of the failure
  // `unknown-key` exists for.
  const text = `version: 1
blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
gates:
  release:
    require: all-pass
    reviewers: [w]
`;
  const { findings } = analyzeWorkflow(text);
  assert.deepEqual(codes(findings).filter((c) => c === "unknown-key"), []);
});

test("a section written as something other than a mapping is a finding, not a silent drop", () => {
  const { findings } = analyzeWorkflow(`version: 1
intake: nope
merge_queue: 3
blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
`);
  const shape = findings.filter((f) => f.code === "section-not-a-mapping");
  assert.deepEqual(
    shape.map((f) => f.message.split(":")[0]),
    ["intake", "merge_queue"]
  );
});

test("a wrongly-typed field is rejected, never coerced (#880)", () => {
  // The engine's serde refuses each of these outright. A pane that read `soon` as "the
  // default" would be describing a queue policy nobody wrote.
  const { workflow, findings } = analyzeWorkflow(`version: 1
merge_queue:
  enabled: yep
  max_batch: soon
blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
    allow: Bash(git push)
`);
  assert.deepEqual(
    findings.filter((f) => f.code === "section-bad-value").map((f) => f.message.split(":")[0]),
    // Parse order: the roster is read before the policy sections.
    ["blocks[0].allow", "merge_queue.enabled", "merge_queue.max_batch"]
  );
  assert.equal(workflow.merge_queue!.enabled, undefined, "not coerced to false");
  assert.equal(workflow.merge_queue!.max_batch, undefined);
  assert.equal(workflow.blocks[0]!.allow, undefined, "not coerced to a one-item list");
});

test("a section declared EMPTY stays declared — `intake: {}` is not the same as no intake:", () => {
  // A bare `intake:` is YAML null, which the engine's `Option<RawIntake>` reads as absent —
  // so emitting one back would delete a section someone deliberately wrote. `{}` is the
  // spelling that means "declared, all defaults", and it is what this model emits.
  const declared = parseWorkflow(
    "version: 1\nintake: {}\nblocks:\n  - id: w\n    kind: worker\n"
  ).workflow;
  assert.deepEqual(declared.intake, {});
  const out = serializeWorkflow(declared);
  assert.match(out, /^intake: \{\}$/m);
  assert.deepEqual(parseWorkflow(out).workflow.intake, {});

  const absent = parseWorkflow("version: 1\nintake:\nblocks:\n  - id: w\n    kind: worker\n");
  assert.equal(absent.workflow.intake, undefined, "a bare key means the section is not declared");
  assert.doesNotMatch(serializeWorkflow(absent.workflow), /intake/);
});

// ---------- the config surface the PANE can now edit (#1020) ----------
//
// #880 gave the model a name for every field of the wire schema; the pane could still only
// edit half of them. These pin the half that was missing — `allow:`, `role_hint:`, and the
// three policy sections — as RULES rather than as form wiring, because the form is DOM and
// the rules are what a form must not be able to break:
//
//   * what a picker may offer is derived from the same statement that validates it, so a new
//     role hint cannot appear in one and not the other;
//   * a value the engine REFUSES is a finding here (the pane must not bless a file that will
//     not load), and one it silently REWRITES is a warning (the pane must not let a pattern
//     reach an agent as something other than what the file says);
//   * declaring a section and then undeclaring it leaves the file exactly as it was.

const workflowWith = (body: string): string =>
  `version: 1\nname: t\n\nblocks:\n  - id: w\n    name: W\n    kind: worker\n    cli: claude\n${body}`;

test("the role_hint offer is DERIVED from the pairing rule, for every kind (#1020)", () => {
  // The property, over the whole cross product rather than over today's two hints: a hint
  // this function offers for a kind must validate clean on that kind, and one it withholds
  // must be refused. A hardcoded picker passes this today and fails it the day a third hint
  // lands — which is the day it would otherwise start lying.
  for (const kind of BLOCK_KINDS) {
    const offered = roleHintsForKind(kind);
    for (const hint of ROLE_HINTS) {
      const w = parseWorkflow(
        `version: 1\nblocks:\n  - id: b\n    kind: ${kind}\n    cli: claude\n    role_hint: ${hint}\n`
      ).workflow;
      const roleFindings = validateWorkflow(w).filter((f) => f.code.startsWith("role-hint-"));
      if (offered.includes(hint)) {
        assert.deepEqual(roleFindings, [], `${kind} is offered ${hint} — it must validate`);
      } else {
        assert.deepEqual(
          codes(roleFindings),
          ["role-hint-wrong-kind"],
          `${kind} is not offered ${hint} — the parser must refuse it`
        );
      }
    }
  }
  // …and every hint is offered SOMEWHERE. A hint no kind can carry would be one the form can
  // never spell, which is a different bug from the two above and just as silent.
  for (const hint of ROLE_HINTS) {
    assert.ok(
      BLOCK_KINDS.some((k) => roleHintsForKind(k).includes(hint)),
      `no kind offers ${hint}`
    );
  }
});

test("allow: is refused on the two kinds the engine refuses it on, and only those (#1020)", () => {
  const on = (kind: string): Finding[] =>
    validateWorkflow(
      parseWorkflow(
        `version: 1\nblocks:\n  - id: b\n    kind: ${kind}\n    cli: claude\n    allow: ["Bash(npm test)"]\n`
      ).workflow
    ).filter((f) => f.code === "allow-not-permitted");

  // The trust root: a repo file may not pre-approve the tools of the one agent that runs
  // unsupervised. The read-only class: `allow: Bash(python *)` is a shell that writes files.
  assert.equal(on("orchestrator").length, 1);
  assert.equal(on("planner").length, 1);
  // A reviewer keeps its shell by design (running the tests IS the job) and a worker holds
  // the whole surface anyway — the engine allows both, so the pane must not invent a rule.
  assert.deepEqual(on("worker"), []);
  assert.deepEqual(on("reviewer"), []);
  // An unrecognized kind is already reported as one; a second finding explains nothing.
  assert.deepEqual(on("superuser"), []);
});

test("an allow: pattern the engine would silently rewrite is a warning, not silence (#1020)", () => {
  const { findings } = analyzeWorkflow(
    workflowWith(`    allow: ["Bash(gh pr view --json title,body)", "Bash(echo $HOME)", "$$$"]\n`)
  );
  const sanitized = findings.filter((f) => f.code === "allow-sanitized");
  // The first pattern is clean — commas and parens are in the engine's alphabet — so it must
  // NOT be flagged, or the warning becomes noise on every real file.
  assert.equal(sanitized.length, 2, sanitized.map((f) => f.message).join(" | "));
  assert.match(sanitized[0]!.message, /Bash\(echo HOME\)/, "says what will actually be applied");
  assert.match(sanitized[1]!.message, /dropped/, "a pattern with nothing left is dropped entirely");
  assert.deepEqual(
    sanitized.map((f) => f.severity),
    ["warning", "warning"],
    "the file still loads — the engine rewrites rather than refuses"
  );
  assert.deepEqual(
    sanitized.map((f) => f.blockId),
    ["w", "w"],
    "reported on the block, so the pane can show it in that block's own form"
  );
});

test("sanitizeAllowPattern mirrors the engine's alphabet, both ways (#1020)", () => {
  // Kept, because a real tool pattern needs them: parens, colon, star, dot, slash, comma,
  // interior spaces, dashes, underscores.
  assert.equal(
    sanitizeAllowPattern("Bash(gh pr view --json title,body)"),
    "Bash(gh pr view --json title,body)"
  );
  assert.equal(sanitizeAllowPattern("mcp__loomux/report:*"), "mcp__loomux/report:*");
  // Filtered — every one of these reaches the CLI's flag as something else.
  assert.equal(sanitizeAllowPattern('Bash(echo "$X" | tee f)'), "Bash(echo X  tee f)");
  assert.equal(sanitizeAllowPattern("  Bash(ls)  "), "Bash(ls)");
  // Nothing usable left = the entry is dropped, not passed through empty.
  assert.equal(sanitizeAllowPattern("!!!"), null);
  assert.equal(sanitizeAllowPattern("   "), null);
});

test("an intake source the parser refuses is a finding, and the empty one is not (#1020)", () => {
  const source = (v: string): Finding[] =>
    analyzeWorkflow(
      `version: 1\nintake:\n  source: ${v}\nblocks:\n  - id: w\n    kind: worker\n    cli: claude\n`
    ).findings.filter((f) => f.code === "intake-unknown-source");
  for (const ok of INTAKE_SOURCES) assert.deepEqual(source(ok), [], ok);
  // The engine trims and lowercases before matching, so the pane must not disagree.
  assert.deepEqual(source("GitHub-Labels"), []);
  assert.deepEqual(source('""'), [], "an empty source means inherit, which is always legal");
  const bad = source("jira");
  assert.equal(bad.length, 1);
  assert.equal(bad[0]!.section, "intake", "routed to the section that can fix it");
  assert.match(bad[0]!.message, /github-labels/, "names the vocabulary");
});

test("an intake label the parser refuses is a finding — including the leading-dash one (#1020)", () => {
  const { findings } = analyzeWorkflow(`version: 1
intake:
  labels:
    ready: agent-ready
    hold: -force
    owned: "agent managed"
blocks:
  - id: w
    kind: worker
    cli: claude
`);
  const bad = findings.filter((f) => f.code === "intake-bad-label");
  // `-force` is the one that is NOT obvious: the alphabet permits a dash freely, but the hold
  // spelling becomes a positional argument to `gh label create`, where a leading dash is read
  // as a flag. `agent-ready` (an interior dash) must stay clean.
  assert.deepEqual(
    bad.map((f) => f.message.split(":")[0]),
    ["intake.labels.owned", "intake.labels.hold"]
  );
  assert.deepEqual(
    bad.map((f) => f.section),
    ["intake", "intake"]
  );
});

test("a policy number outside the engine's bounds is a finding, not a clean bill of health (#1020)", () => {
  const { findings } = analyzeWorkflow(`version: 1
merge_queue:
  max_batch: 0
  checks_timeout_minutes: 999
resources:
  build:
    slots: 65
  lint:
    max_hold_minutes: 481
  "no good":
    slots: 2
blocks:
  - id: w
    kind: worker
    cli: claude
`);
  const range = findings.filter((f) => f.code === "section-out-of-range");
  assert.deepEqual(
    range.map((f) => f.message.split(":").slice(0, 1).join(":")),
    [
      "merge_queue.max_batch",
      "merge_queue.checks_timeout_minutes",
      "resources.build.slots",
      "resources.lint.max_hold_minutes",
    ]
  );
  // `max_batch: 0` is a REFUSAL on the engine; `checks_timeout_minutes` is CLAMPED. The pane
  // says which is which, because "your file will not load" and "your file will not do what it
  // says" are different sentences.
  assert.deepEqual(
    range.map((f) => f.severity),
    ["error", "warning", "error", "error"]
  );
  assert.deepEqual(
    range.map((f) => f.section),
    ["merge_queue", "merge_queue", "resources", "resources"]
  );
  const names = findings.filter((f) => f.code === "resource-name-invalid");
  assert.equal(names.length, 1, "a name the engine rejects rather than rewrites");
  assert.match(names[0]!.message, /no good/);
});

test("too many resources is the engine's own cap, mirrored (#1020)", () => {
  const many = Object.fromEntries(
    Array.from({ length: RESOURCES_MAX + 1 }, (_, i) => [`r${i}`, { slots: 1 }])
  );
  const w: Workflow = { ...starterWorkflow(), resources: many };
  const over = validateWorkflow(w).filter((f) => f.code === "section-out-of-range");
  assert.equal(over.length, 1);
  assert.match(over[0]!.message, new RegExp(String(RESOURCES_MAX)));
  // Exactly at the cap is fine — an off-by-one here would refuse a legal file.
  const at: Workflow = {
    ...starterWorkflow(),
    resources: Object.fromEntries(Object.entries(many).slice(0, RESOURCES_MAX)),
  };
  assert.deepEqual(
    validateWorkflow(at).filter((f) => f.code === "section-out-of-range"),
    []
  );
});

test("a clean policy surface stays clean — none of the new rules fires on the documented schema", () => {
  const { findings } = analyzeWorkflow(FULL_SURFACE);
  assert.deepEqual(codes(findings), [], "the #880 fixture uses every section, legally");
});

test("declaring a section and undeclaring it again leaves the file byte-for-byte (#1020)", () => {
  // The edge case the form's enable-toggle has to get right: tick `merge_queue:` on, change
  // your mind, untick it — and the file that never declared one still doesn't. Declared-only
  // emission is what makes that true, and this is the round-trip that proves it rather than
  // asserting it.
  const original = `# a hand-written file
version: 1
name: t

blocks:
  - id: w
    name: W
    kind: worker
    cli: claude
`;
  const { workflow } = parseWorkflow(original);
  const on: Workflow = { ...workflow, merge_queue: { enabled: true } };
  const withQueue = serializeWorkflowPreserving(on, original);
  assert.match(withQueue, /^merge_queue:$/m, "the toggle really did write the section");
  assert.match(withQueue, /^ {2}enabled: true$/m);

  const off: Workflow = { ...workflow };
  delete off.merge_queue;
  assert.equal(
    serializeWorkflowPreserving(off, original),
    original,
    "untick must leave an undeclared file undeclared, comments and all"
  );
  // …and the same for the other two sections, which get the same toggle.
  for (const section of ["intake", "resources"] as const) {
    const declared: Workflow = { ...workflow, [section]: {} };
    const text = serializeWorkflowPreserving(declared, original);
    assert.match(text, new RegExp(`^${section}: \\{\\}$`, "m"));
    const undeclared: Workflow = { ...workflow };
    delete undeclared[section];
    assert.equal(serializeWorkflowPreserving(undeclared, original), original, section);
  }
});

test("the three sections a form writes survive a round-trip through their own text (#1020)", () => {
  // What the forms actually produce, read back: every field the pane can now set has to come
  // back as itself, or the next render shows something the human didn't type.
  const { workflow } = parseWorkflow(
    "version: 1\nblocks:\n  - id: w\n    kind: worker\n    cli: claude\n"
  );
  const edited: Workflow = {
    ...workflow,
    intake: { source: "board", labels: { ready: "go", hold: "wait_here" } },
    merge_queue: {
      enabled: true,
      max_batch: MERGE_QUEUE_MAX_BATCH_MIN,
      checks_timeout_minutes: 45,
    },
    resources: { build: { slots: RESOURCE_SLOTS_MAX }, docs: { max_hold_minutes: 5 } },
  };
  const text = serializeWorkflow(edited);
  const reread = parseWorkflow(text);
  assert.deepEqual(reread.findings, []);
  assert.deepEqual(reread.workflow, edited);
  assert.deepEqual(
    validateWorkflow(reread.workflow).filter((f) => f.severity === "error"),
    []
  );
});

// ---------- empty ↔ block: a section that gains or loses its last child (#1090) ----------
//
// The bug, from the #1018 demo: a file with an empty `resources: {}` given its first resource
// through the form came back as
//
//     resources: {}
//       catfish: {}
//
// — the preserving serializer reused the original key line, which commits the section to the
// inline empty-mapping form, and then wrote block children under it. Not YAML, so the pane
// declared its own output unreadable and disabled the form. Every section reused through
// `pushSection` had it, plus `blocks:`; the inverse direction (last child removed, key line
// left as a bare `resources:`) is the silent half — a bare key is YAML null, i.e. undeclared.

/** A file with one worker and nothing else, ready to have a section spliced onto it. */
const oneWorker = (section: string): string =>
  `version: 1
name: t

blocks:
  - id: w
    name: W
    kind: worker
    cli: claude

${section}
`;

/** Serialize `edited` against `original`, then assert the result both PARSES cleanly and reads
 *  back as exactly the model that was written — the two halves the pane depends on. */
const roundTrip = (edited: Workflow, original: string, why: string): string => {
  const out = serializeWorkflowPreserving(edited, original);
  const reread = parseWorkflow(out);
  assert.deepEqual(codes(reread.findings), [], `${why}\n--- emitted ---\n${out}`);
  assert.deepEqual(reread.workflow, edited, `${why}\n--- emitted ---\n${out}`);
  return out;
};

test("a section gaining its first child stops being an empty map (#1090)", () => {
  const cases: { section: string; edit: (w: Workflow) => Workflow; body: RegExp }[] = [
    {
      section: "resources: {}",
      edit: (w) => ({ ...w, resources: { catfish: {} } }),
      body: /^ {2}catfish: \{\}$/m,
    },
    {
      section: "intake: {}",
      edit: (w) => ({ ...w, intake: { source: "board" } }),
      body: /^ {2}source: board$/m,
    },
    {
      section: "merge_queue: {}",
      edit: (w) => ({ ...w, merge_queue: { enabled: true } }),
      body: /^ {2}enabled: true$/m,
    },
  ];
  for (const c of cases) {
    const original = oneWorker(c.section);
    const { workflow } = parseWorkflow(original);
    const out = roundTrip(c.edit(workflow), original, `${c.section} + one child`);
    const key = c.section.split(":")[0]!;
    assert.match(out, new RegExp(`^${key}:$`, "m"), "the key line must lose its `{}`");
    assert.match(out, c.body);
  }
});

test("a section losing its last child goes back to `{}`, not to nothing (#1090)", () => {
  // The silent inverse: leaving the bare `resources:` header behind re-reads as YAML null, so
  // the section a human deliberately kept (empty) would be gone on the next open — exactly what
  // `emitMappingSection` writes `{}` to prevent, one save later.
  const cases: { section: string; edit: (w: Workflow) => Workflow }[] = [
    { section: "resources:\n  catfish:\n    slots: 2", edit: (w) => ({ ...w, resources: {} }) },
    { section: "intake:\n  source: board", edit: (w) => ({ ...w, intake: {} }) },
    { section: "merge_queue:\n  enabled: true", edit: (w) => ({ ...w, merge_queue: {} }) },
  ];
  for (const c of cases) {
    const original = oneWorker(c.section);
    const { workflow } = parseWorkflow(original);
    const out = roundTrip(c.edit(workflow), original, `${c.section} − its last child`);
    const key = c.section.split(":")[0]!;
    assert.match(out, new RegExp(`^${key}: \\{\\}$`, "m"), "still declared, now empty");
  }
});

test("an empty flow sequence gaining its first item becomes a block header too (#1090)", () => {
  // `blocks: []` is this file's own empty-roster spelling (rev-5 F4) and `edges: []`/`gates: {}`
  // are shapes a hand-written file can carry, so all three can be handed their first entry.
  const emptyRoster = "version: 1\nname: t\n\nblocks: []\n";
  const { workflow } = parseWorkflow(emptyRoster);
  // The block comes from the READER (parsing a file that already has one) rather than a
  // hand-written literal, so the round-trip compares models, not optional-field spellings.
  const worker = parseWorkflow(oneWorker("")).workflow.blocks;
  const withBlock = roundTrip({ ...workflow, blocks: worker }, emptyRoster, "blocks: [] + one block");
  assert.match(withBlock, /^blocks:$/m);
  assert.match(withBlock, /^ {2}- id: w$/m);

  const two = oneWorker("  - id: r\n    name: R\n    kind: reviewer\n    cli: claude\n\nedges: []");
  const parsedTwo = parseWorkflow(two);
  const withEdge = roundTrip(
    { ...parsedTwo.workflow, edges: [{ from: "w", to: "r" }] },
    two,
    "edges: [] + one edge"
  );
  assert.match(withEdge, /^edges:$/m);
  assert.match(withEdge, /^ {2}- \{ from: w, to: r \}$/m);

  const gated = two.replace("edges: []", "gates: {}");
  const parsedGated = parseWorkflow(gated);
  const targetGates = parseWorkflow(
    two.replace("edges: []", "gates:\n  merge:\n    require: all\n    reviewers: [r]")
  ).workflow.gates; // read, not hand-written, for the same reason as `worker` above
  const withGate = roundTrip(
    { ...parsedGated.workflow, gates: targetGates },
    gated,
    "gates: {} + a merge gate"
  );
  assert.match(withGate, /^gates:$/m);
  assert.match(withGate, /^ {2}merge:$/m);
});

test("a hand-written one-line section is rewritten, not written twice (#1090)", () => {
  // A flow mapping carries the section's WHOLE content on the key line, so reusing that line
  // under a regenerated body would emit `build:` twice — once inline, once as a child.
  const original = oneWorker("resources: { build: { slots: 2 } }");
  const { workflow } = parseWorkflow(original);
  const out = roundTrip(
    { ...workflow, resources: { build: { slots: 2 }, docs: {} } },
    original,
    "an inline flow mapping + one more resource"
  );
  assert.equal(out.match(/build/g)?.length, 1, "the inline copy must be gone, not duplicated");

  // …and the same line REPLACED by `{}` when the model empties out, rather than left standing
  // and silently undoing the deletion.
  const emptied = roundTrip({ ...workflow, resources: {} }, original, "an inline mapping emptied");
  assert.match(emptied, /^resources: \{\}$/m);
});

test("rewriting a section's key line keeps the comments around it (#1090)", () => {
  // #233's bar still holds through the rewrite: the comment ABOVE the section introduces the
  // section, and the one ON the key line came from the same human — neither is about the
  // empty-vs-block spelling that had to change.
  const original = oneWorker("# THE POOLS\nresources: {} # none yet");
  const { workflow } = parseWorkflow(original);
  const out = roundTrip(
    { ...workflow, resources: { catfish: {} } },
    original,
    "a commented empty section + one child"
  );
  assert.match(out, /^# THE POOLS\nresources: # none yet\n {2}catfish: \{\}$/m);
});

test("emptying the roster carries the `blocks:` line's own comment onto `blocks: []` (#1090)", () => {
  // `pushBlocks`'s EMPTY branch rewrites the key line too — it always did, since `blocks: []` is
  // the canonical empty roster — and it now goes through the same helper, so the comment on that
  // line survives the rewrite instead of being dropped with it. Pinned separately from the
  // `pushSection` case above because it is a different call site: deleting the last block is the
  // ordinary way a human reaches it.
  const original = `version: 1
name: t

# THE ROSTER
blocks: # the agents a run may use
  - id: w
    name: W
    kind: worker
    cli: claude
`;
  const { workflow } = parseWorkflow(original);
  const out = serializeWorkflowPreserving({ ...workflow, blocks: [] }, original);
  assert.deepEqual(codes(parseWorkflow(out).findings), [], out);
  assert.match(
    out,
    /^# THE ROSTER\nblocks: \[\] # the agents a run may use$/m,
    "both comments survive — the one introducing the section and the one on its key line"
  );
  // …and the roster refilled from there keeps them again, back in block form.
  const refilled = serializeWorkflowPreserving(workflow, out);
  assert.match(refilled, /^# THE ROSTER\nblocks: # the agents a run may use$/m);
  assert.deepEqual(parseWorkflow(refilled).workflow, workflow);
});

test("an untouched empty section is still reproduced byte for byte (#1090)", () => {
  // The rewrite is for a section whose body was REGENERATED. A file nobody touched — including
  // the `{}` sections this fix is about — must still come back exactly as it went in.
  const original = oneWorker("# THE POOLS\nresources: {} # none yet\n\nintake: {}");
  const { workflow } = parseWorkflow(original);
  assert.equal(serializeWorkflowPreserving(workflow, original), original);
  const renamed: Workflow = { ...workflow, name: "t2" };
  assert.equal(
    serializeWorkflowPreserving(renamed, original),
    original.replace("name: t", "name: t2"),
    "an edit ELSEWHERE must not reformat the empty sections either"
  );
});

test("the identifier rules accept exactly what the engine accepts (#1020)", () => {
  assert.equal(isValidIntakeLabel("agent-ready"), true);
  assert.equal(isValidIntakeLabel("Agent_Ready1"), true);
  assert.equal(isValidIntakeLabel(""), true, "empty = inherit this one");
  assert.equal(isValidIntakeLabel("-hold"), false, "a positional beginning with a dash reads as a flag");
  assert.equal(isValidIntakeLabel("needs triage"), false);
  assert.equal(isValidIntakeLabel("a".repeat(ID_MAX_CHARS + 1)), false, "rejected, never truncated");
  assert.equal(isValidResourceName("-build"), true, "no argv carries a resource name");
  assert.equal(isValidResourceName("heavy build"), false);
  assert.equal(isValidResourceName(""), false);
  assert.equal(isValidResourceName("a".repeat(ID_MAX_CHARS)), true);
});
