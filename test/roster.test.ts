// Roster resolution (#222) — what the launcher tells the human a group will run,
// BEFORE it runs it. The consent surface for the advanced-orchestrator toggle, so
// what these tests defend is not "the code does what it does" but "the launcher
// cannot promise a roster the backend won't deliver".
//
// The four outcomes it has to get right, in rising order of how badly a wrong
// answer would burn someone:
//   toggle off        → the standard four roles; a workflow file, if any, is not read
//   on, no file       → a NO-OP, not an error (it is how you launch before writing one)
//   on, broken file   → a WARNING, not a blocker (the group still launches, standard)
//   on, valid file    → the declared blocks, personas and all

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ORCH_ROLES,
  builtinRoster,
  describeBlock,
  describeRoster,
  resolveRoster,
  rosterNeedsReview,
  type RolePick,
  type RosterBlock,
  type WorkflowPreview,
} from "../src/roster.ts";

const PICKS: RolePick[] = [
  { key: "orchestrator", cli: "claude", model: "opus" },
  { key: "worker", cli: "copilot", model: "auto" },
  { key: "reviewer", cli: "claude", model: "sonnet" },
  { key: "planner", cli: "claude", model: "opus" },
];

const block = (o: Partial<RosterBlock> & Pick<RosterBlock, "id" | "kind">): RosterBlock => ({
  name: o.id,
  cli: "claude",
  model: "sonnet",
  persona: "none",
  ...o,
});

const preview = (o: Partial<WorkflowPreview>): WorkflowPreview => ({
  path: ".loomux/workflow.yml",
  present: true,
  valid: true,
  name: "focused-review",
  errors: [],
  gates: [],
  blocks: [],
  ...o,
});

const DECLARED = preview({
  gates: ["merge"],
  blocks: [
    block({ id: "orchestrator", kind: "orchestrator", model: "opus" }),
    block({ id: "worker", kind: "worker", cli: "copilot", model: "auto", persona: "profile" }),
    block({ id: "rev-security", name: "Security review", kind: "reviewer", model: "opus", persona: "prompt" }),
    block({ id: "rev-tests", name: "Test-quality review", kind: "reviewer", persona: "prompt" }),
  ],
});

test("the role table is the full closed set, planner included", () => {
  // The bug this replaces: groupview.ts kept its own copy of this table and never
  // gained `planner`, so a planner pane showed a generic chip. One table, four
  // classes, everybody reads it.
  assert.deepEqual(
    ORCH_ROLES.map((r) => r.key),
    ["orchestrator", "worker", "reviewer", "planner"]
  );
});

test("the toggle off runs the standard roster and never reads the file", () => {
  const r = resolveRoster(false, DECLARED, PICKS, "claude");
  assert.equal(r.status, "builtin");
  assert.equal(r.errors.length, 0);
  assert.deepEqual(
    r.blocks.map((b) => b.id),
    ["orchestrator", "worker", "reviewer", "planner"],
    "the declared blocks must not leak into an opted-out launch"
  );
  assert.ok(!r.blocks.some((b) => b.persona !== "none"), "nor may any repo-authored persona");
  // A form in its default state must not need reviewing — that is what "default"
  // means, and a nag on every launch is how a consent surface stops being read.
  assert.equal(rosterNeedsReview(r), false);
});

test("the toggle off SAYS the repo's workflow file is being ignored — but only if there is one", () => {
  // Silence here is the confusing case: you wrote a workflow, you launched, and
  // nothing happened. Say it plainly.
  const withFile = resolveRoster(false, DECLARED, PICKS, "claude");
  assert.match(withFile.summary, /will not be used/);
  assert.match(withFile.summary, /\.loomux\/workflow\.yml/);

  // ...and a repo with no workflow must not have one advertised at it.
  const without = resolveRoster(false, preview({ present: false, blocks: [] }), PICKS, "claude");
  assert.doesNotMatch(without.summary, /will not be used/);
  assert.match(without.summary, /Standard roster/);
});

test("the toggle on with a valid file resolves to the declared blocks", () => {
  const r = resolveRoster(true, DECLARED, PICKS, "claude");
  assert.equal(r.status, "declared");
  assert.deepEqual(
    r.blocks.map((b) => b.id),
    ["orchestrator", "worker", "rev-security", "rev-tests"],
    "the file's blocks, in the file's order — there is no `reviewer` block, and none is invented"
  );
  assert.equal(r.errors.length, 0);
  assert.equal(rosterNeedsReview(r), true, "a declared roster is exactly what needs a human look");
  assert.match(r.summary, /focused-review/);
  assert.match(r.summary, /1 worker, 2 reviewers/, "the delegate counts are the headline");
  assert.match(r.summary, /gated on merge/, "and a declared gate is not a detail");
});

test("the toggle on with NO file is a no-op, not an error", () => {
  // Turning the toggle on before you have written a workflow is the normal first
  // step, not a mistake. It must launch, on the standard roster, and say so.
  const r = resolveRoster(true, preview({ present: false, name: "", blocks: [] }), PICKS, "claude");
  assert.equal(r.status, "none");
  assert.equal(r.errors.length, 0);
  assert.deepEqual(r.blocks, builtinRoster(PICKS, "claude"), "the standard roster still runs");
  assert.match(r.summary, /standard roster will run/);
  assert.doesNotMatch(r.summary, /error/i, "absence is not invalidity");
});

test("a broken workflow file is a warning that still launches, not a blocker", () => {
  // The backend audits a broken file and falls back — a repo file may never stop a
  // group from starting. The launcher has to say the SAME thing, or the human
  // reads a red box and assumes Create is dead.
  const r = resolveRoster(
    true,
    preview({
      valid: false,
      name: "",
      errors: ["block 1: unknown kind 'not-a-kind'", "block 2: unknown cli 'emacs'"],
      blocks: [],
    }),
    PICKS,
    "claude"
  );
  assert.equal(r.status, "invalid");
  assert.deepEqual(r.blocks, builtinRoster(PICKS, "claude"), "the fallback roster is what runs");
  assert.equal(r.errors.length, 2, "every finding, not just the first");
  assert.match(r.summary, /2 errors/);
  assert.match(r.summary, /skipped/);
  assert.match(r.summary, /standard roster will run instead/, "the launch is NOT blocked");
});

test("a preview that never arrived degrades to 'no workflow', never to a throw", () => {
  // The fetch failed, or hasn't resolved. The form must still render, and must not
  // claim a roster it has not seen.
  const r = resolveRoster(true, null, PICKS, "claude");
  assert.equal(r.status, "none");
  assert.deepEqual(r.blocks, builtinRoster(PICKS, "claude"));
});

test("the built-in roster is the launcher's own picks, with no invention", () => {
  const blocks = builtinRoster(PICKS, "claude");
  assert.deepEqual(
    blocks.map((b) => [b.id, b.cli, b.model]),
    [
      ["orchestrator", "claude", "opus"],
      ["worker", "copilot", "auto"],
      ["reviewer", "claude", "sonnet"],
      ["planner", "claude", "opus"],
    ]
  );
  // A block id equals its class name, which is precisely why the built-in roster
  // keeps the historic instruction-file names and agent-id prefixes backend-side.
  assert.ok(blocks.every((b) => b.id === b.kind));
  assert.ok(blocks.every((b) => b.persona === "none"), "the standard roles have no persona");

  // A pick with no CLI of its own inherits the group default rather than emitting
  // an empty CLI the backend would have to guess at.
  const sparse = builtinRoster([{ key: "worker", cli: "", model: "" }], "copilot");
  assert.equal(sparse.find((b) => b.kind === "worker")!.cli, "copilot");
  assert.equal(sparse.find((b) => b.kind === "planner")!.cli, "copilot", "an absent pick too");
});

test("a block's one-line description leads with what the human is consenting to", () => {
  assert.equal(
    describeBlock(block({ id: "rev-security", kind: "reviewer", model: "opus", persona: "prompt" })),
    "reviewer · claude · opus · repo persona"
  );
  assert.equal(
    describeBlock(block({ id: "worker", kind: "worker", cli: "copilot", model: "auto", persona: "profile" })),
    "worker · copilot · auto · repo persona (file)"
  );
  // No persona = nothing repo-authored reaches the agent's instructions, and the
  // line must not imply otherwise.
  assert.equal(describeBlock(block({ id: "w", kind: "worker" })), "worker · claude · sonnet");
  // An inherited model resolves backend-side; if it somehow didn't, say so rather
  // than rendering a blank.
  assert.equal(describeBlock(block({ id: "w", kind: "worker", model: "" })), "worker · claude · default model");
});

test("the roster description counts delegates, not the orchestrator", () => {
  // Every group has exactly one orchestrator and it is not a choice the roster
  // makes, so counting it would just pad every line with the same "1 orchestrator".
  assert.equal(describeRoster(DECLARED.blocks), "1 worker, 2 reviewers");
  assert.equal(
    describeRoster([block({ id: "orchestrator", kind: "orchestrator" })]),
    "no delegates",
    "a roster with nothing to delegate to says so"
  );
  assert.equal(
    describeRoster([
      block({ id: "orchestrator", kind: "orchestrator" }),
      block({ id: "p", kind: "planner" }),
      block({ id: "w1", kind: "worker" }),
      block({ id: "w2", kind: "worker" }),
    ]),
    "2 workers, 1 planner",
    "class order follows the role table, not the file's order"
  );
});
