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
  hasErrors,
  BLOCK_KINDS,
  WORKFLOW_VERSION,
  roleHintRequires,
  type Workflow,
  type Finding,
  type FindingCode,
} from "../src/workflowmodel.ts";

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
    allow: ["Bash(gh pr view --json title,body)", "Bash(git status)"],
    limits: { cpu: 2, note: "a,b" },
  });
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
  assert.equal(workflow.extra?.authored_with, "0.9.0");
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
  assert.deepEqual(older.extra, { authored_with: "0.6.1" });
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
