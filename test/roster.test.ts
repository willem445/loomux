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
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import {
  MAX_AGENTS_CEILING,
  ORCH_ROLES,
  builtinRoster,
  capacityRaiseTarget,
  capacityWarning,
  describeBlock,
  describeRoster,
  joinWithAnd,
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
  min_agents: null,
  recommended_agents: null,
  reviewers_needed: null,
  extra_tiers: null,
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
  // 1 worker + 2 reviewers, all-pass over both: minimum = 2 + 1 = 3; recommended
  // = 1 worker + 2 reviewers (no planner block) = 3. minimum == recommended —
  // nothing declared beyond what the gate already needs.
  min_agents: 3,
  recommended_agents: 3,
  reviewers_needed: 2,
  extra_tiers: [],
});

// The #255 incident roster: orchestrator, planner, 2 worker tiers, 3 reviewers,
// all-pass over the 3. minimum = 3 + 1 = 4 (what one review round costs);
// recommended = 2 + 3 + 1 = 6 (every tier live at once) — the two diverge, which
// is exactly the case a soft vs hard warning has to tell apart.
const DECLARED_WITH_PLANNER = preview({
  gates: ["merge"],
  blocks: [
    block({ id: "orchestrator", kind: "orchestrator" }),
    block({ id: "planner", kind: "planner" }),
    block({ id: "worker-deep", kind: "worker" }),
    block({ id: "worker-quick", kind: "worker" }),
    block({ id: "rev-1", kind: "reviewer" }),
    block({ id: "rev-2", kind: "reviewer" }),
    block({ id: "rev-3", kind: "reviewer" }),
  ],
  min_agents: 4,
  recommended_agents: 6,
  reviewers_needed: 3,
  extra_tiers: ["1 more worker tier", "the planner"],
});

// #255 rev-1 B1's exact repro: a `threshold: 2` gate over a subset of 5
// declared reviewer blocks. minimum = 2 (reviewers_needed) + 1 worker = 3;
// recommended = 1 worker + 5 reviewers = 6 — the gate's requirement (2) and
// the reviewer BLOCK count (5) genuinely differ, which is exactly what a
// warning describing "5 reviewers (minimum 3)" got backwards.
const DECLARED_THRESHOLD = preview({
  gates: ["merge"],
  blocks: [
    block({ id: "worker", kind: "worker" }),
    block({ id: "rev-1", kind: "reviewer" }),
    block({ id: "rev-2", kind: "reviewer" }),
    block({ id: "rev-3", kind: "reviewer" }),
    block({ id: "rev-4", kind: "reviewer" }),
    block({ id: "rev-5", kind: "reviewer" }),
  ],
  min_agents: 3,
  recommended_agents: 6,
  reviewers_needed: 2,
  extra_tiers: ["3 more reviewers"],
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
  assert.deepEqual(
    r.capacity,
    { minimum: 3, recommended: 3, reviewersNeeded: 2, extraTiers: [] },
    "#255: mirrored straight off the preview, never recomputed from the block list"
  );
});

test("#255: only a declared roster carries a capacity recommendation", () => {
  // builtin / none / invalid all run the standard four with no merge gate to
  // derive a minimum from — there is nothing to warn about.
  assert.equal(resolveRoster(false, DECLARED, PICKS, "claude").capacity, null);
  assert.equal(resolveRoster(true, preview({ present: false, blocks: [] }), PICKS, "claude").capacity, null);
  assert.equal(
    resolveRoster(true, preview({ valid: false, errors: ["bad"], blocks: [] }), PICKS, "claude").capacity,
    null
  );
});

test("#255: capacityWarning is quiet at or above recommended, on either roster", () => {
  // DECLARED has minimum == recommended (3): nothing is left over to warn
  // about once the cap covers the gate + worker.
  const r = resolveRoster(true, DECLARED, PICKS, "claude");
  assert.equal(capacityWarning(r, 3), null, "at minimum == recommended — no thrash, nothing left over");
  assert.equal(capacityWarning(r, 4), null, "above it too");

  // DECLARED_WITH_PLANNER has minimum (4) < recommended (6): quiet only once
  // the cap reaches the FULL roster, not merely the minimum.
  const withPlanner = resolveRoster(true, DECLARED_WITH_PLANNER, PICKS, "claude");
  assert.equal(capacityWarning(withPlanner, 6), null, "at recommended — every declared tier fits");
  assert.equal(capacityWarning(withPlanner, 8), null, "comfortably above it too");

  assert.equal(
    capacityWarning(resolveRoster(false, DECLARED, PICKS, "claude"), 1),
    null,
    "a builtin roster has no gate to warn from, however low the cap"
  );
});

test("#255 rev-1 B2: the incident's own numbers (cap == minimum < recommended) get the SOFT warning", () => {
  // This is the exact run that thrashed for two hours: max_agents (4) equals
  // the roster's minimum, so no review round ever gets evicted mid-flight —
  // but it is still 2 short of the 6 the full two-tier roster needs, and a
  // hard-only (below-minimum) check was silent on precisely this boundary.
  const r = resolveRoster(true, DECLARED_WITH_PLANNER, PICKS, "claude");
  const msg = capacityWarning(r, 4);
  assert.ok(msg, "cap (4) covers one review round but not the full roster (6) — must warn");
  assert.match(msg!, /full roster needs 6 live agents/, "names the full-roster number");
  assert.match(msg!, /max_agents is 4/);
  assert.match(msg!, /1 more worker tier and the planner/, "names exactly the tiers that can never be live at once");
  assert.match(msg!, /Raise it to 6/, "the fix is the recommended count, not the minimum");
  assert.doesNotMatch(msg!, /evicting a live agent/i, "soft tier: nothing is being evicted, unlike the hard tier");
});

test("#255 rev-1 B2: below the minimum gets the HARD warning, distinct from the soft one", () => {
  const r = resolveRoster(true, DECLARED_WITH_PLANNER, PICKS, "claude");
  const msg = capacityWarning(r, 3);
  assert.ok(msg, "max_agents (3) is below this roster's minimum (4)");
  assert.match(msg!, /3 reviewers/, "names the gate's reviewer requirement");
  assert.match(msg!, /\+ a worker/, "and the worker slot a review round needs");
  assert.match(msg!, /minimum 4/);
  assert.match(msg!, /max_agents is 3/);
  assert.match(msg!, /at least 4/, "raise-to-minimum, the cheapest fix");
  assert.match(msg!, /6 to run every declared tier at once/, "raise-to-recommended, the full fix");
  assert.match(msg!, /evicting a live agent/i, "hard tier: this is the one that actively thrashes");
});

test("#255 rev-1 B1: the warning names the GATE's reviewer requirement, never a recount of reviewer blocks", () => {
  // The exact bug the review caught: 1 worker + 5 reviewer blocks under
  // `threshold: 2` used to print "needs 5 reviewers + a worker (minimum 3
  // live agents)" — 5 + 1 != 3. The number in the sentence must be the same
  // one `minimum` was built from.
  const r = resolveRoster(true, DECLARED_THRESHOLD, PICKS, "claude");
  const msg = capacityWarning(r, 2);
  assert.ok(msg, "max_agents (2) is below this roster's minimum (3)");
  assert.match(msg!, /2 reviewers \+ a worker/, "2 (the gate's need), not 5 (the declared reviewer blocks)");
  assert.doesNotMatch(msg!, /5 reviewers/, "must never recount reviewer BLOCKS to describe a GATE-derived minimum");
  assert.match(msg!, /minimum 3/);

  // And the soft tier, once the hard shortfall is fixed: 3 more reviewer
  // blocks are declared than the gate needs, so THOSE are what's left over.
  const soft = capacityWarning(r, 3);
  assert.ok(soft, "cap (3) meets the minimum but not the full roster (6)");
  assert.match(soft!, /3 more reviewers/);
});

test("#255 rev-1 NB2: the raise target never exceeds MAX_AGENTS_CEILING", () => {
  // A workflow's structural need isn't bounded by the ceiling — only
  // `max_agents` is. 1 worker + 12 reviewer blocks, `threshold: 2`: minimum =
  // 2 + 1 = 3; recommended = 1 + 12 = 13, past the 12-agent ceiling.
  const overCeiling = preview({
    gates: ["merge"],
    blocks: [
      block({ id: "worker", kind: "worker" }),
      ...Array.from({ length: 12 }, (_, i) => block({ id: `rev-${i}`, kind: "reviewer" })),
    ],
    min_agents: 3,
    recommended_agents: 13,
    reviewers_needed: 2,
    extra_tiers: ["10 more reviewers"],
  });
  const r = resolveRoster(true, overCeiling, PICKS, "claude");
  assert.equal(r.capacity!.recommended, 13, "the TRUE structural number is reported honestly");
  assert.equal(
    capacityRaiseTarget(r),
    MAX_AGENTS_CEILING,
    "but the offered fix is clamped to what the cap can actually reach"
  );

  // Hard tier (cap below minimum): the "raise to minimum" clause is unaffected
  // (3 is well under the ceiling) — only the recommended-count offer is capped.
  const hard = capacityWarning(r, 2);
  assert.ok(hard);
  assert.match(hard!, /at least 3/);
  assert.match(hard!, /or 12 to run every declared tier at once/, "offers the CLAMPED number, not 13");
  assert.match(hard!, /above loomux's 12-agent limit/, "says WHY 12 is offered instead of 13");

  // Soft tier (cap at the minimum, short of the full roster): same clamp.
  const soft = capacityWarning(r, 5);
  assert.ok(soft);
  assert.match(soft!, /Raise it to 12/);
  assert.match(soft!, /above loomux's 12-agent limit/);

  // A roster whose recommendation fits comfortably under the ceiling must not
  // get the ceiling caveat at all — it would be noise.
  const underCeiling = resolveRoster(true, DECLARED_WITH_PLANNER, PICKS, "claude");
  assert.equal(capacityRaiseTarget(underCeiling), 6);
  assert.doesNotMatch(capacityWarning(underCeiling, 3)!, /ceiling|as high as this cap can go/);
});

test("#255 rev-2 non-blocking #2: when even the ceiling can't reach the minimum, no false clause", () => {
  // Exotic (a gate needing 14 reviewers), but real: minimum (15) itself is
  // above MAX_AGENTS_CEILING (12), so `capacityRaiseTarget` clamps to 12 —
  // which doesn't even cover ONE review round, let alone "every declared tier
  // at once". The old wording offered exactly that false clause, so raising
  // to the button's own number left the hard warning lit.
  const wayOverCeiling = preview({
    gates: ["merge"],
    blocks: [
      block({ id: "worker", kind: "worker" }),
      ...Array.from({ length: 14 }, (_, i) => block({ id: `rev-${i}`, kind: "reviewer" })),
    ],
    min_agents: 15,
    recommended_agents: 15,
    reviewers_needed: 14,
    extra_tiers: [],
  });
  const r = resolveRoster(true, wayOverCeiling, PICKS, "claude");
  assert.equal(capacityRaiseTarget(r), MAX_AGENTS_CEILING, "clamped, same as any over-ceiling roster");

  const msg = capacityWarning(r, 4);
  assert.ok(msg, "max_agents (4) is nowhere near this roster's minimum (15)");
  assert.match(msg!, /minimum itself \(15\) is above loomux's 12-agent limit/);
  assert.doesNotMatch(
    msg!,
    /or 12 to run every declared tier at once/,
    "12 doesn't reach the minimum, let alone the full roster — must never claim it does"
  );
  assert.doesNotMatch(
    msg!,
    /Raise it to at least 15/,
    "no actionable 'raise to X' framing when nothing raisable clears the warning"
  );

  // Raising max_agents to the offered ceiling must not read as a fix here —
  // capacityWarning(r, 12) still returns the same "wall" message, not null and
  // not the ordinary hard-tier phrasing.
  const atCeiling = capacityWarning(r, MAX_AGENTS_CEILING);
  assert.ok(atCeiling, "12 < minimum (15) — still below it, the warning must not go quiet");
  assert.match(atCeiling!, /minimum itself \(15\) is above loomux's 12-agent limit/);
});

test("#255 rev-2 non-blocking #3: MAX_AGENTS_CEILING mirrors the Rust source it's duplicated from", () => {
  // roster.ts's copy exists only because the launcher needs to reason about
  // the ceiling before Create ever calls the backend — nothing at the type
  // level ties the two together, so read the Rust literal back and fail
  // loudly the day someone changes one without the other, rather than
  // trusting a comment to catch the drift.
  const here = dirname(fileURLToPath(import.meta.url));
  const rustSrc = readFileSync(
    join(here, "..", "src-tauri", "src", "orchestration", "mod.rs"),
    "utf8"
  );
  const declared = rustSrc.match(/const MAX_AGENTS_CEILING: u32 = (\d+);/);
  assert.ok(declared, "mod.rs's MAX_AGENTS_CEILING declaration must match this exact pattern — update it here too if that line's wording changes");
  assert.equal(MAX_AGENTS_CEILING, Number(declared![1]), "roster.ts's copy has drifted from the Rust source");
});

test("joinWithAnd reads like English at every list length", () => {
  assert.equal(joinWithAnd([]), "");
  assert.equal(joinWithAnd(["the planner"]), "the planner");
  assert.equal(joinWithAnd(["the planner", "1 more worker tier"]), "the planner and 1 more worker tier");
  assert.equal(
    joinWithAnd(["the planner", "1 more worker tier", "2 more reviewers"]),
    "the planner, 1 more worker tier, and 2 more reviewers"
  );
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

test("a role_hint block gets an ADVISOR/PROCESS chip (#250/#324)", () => {
  assert.equal(
    describeBlock(block({ id: "advisor", kind: "planner", role_hint: "advisor" })),
    "planner · claude · sonnet · ADVISOR"
  );
  assert.equal(
    describeBlock(block({ id: "proc", kind: "worker", role_hint: "process" })),
    "worker · claude · sonnet · PROCESS"
  );
  // The chip stacks after the persona note, never replaces it — both are things
  // the human is consenting to, independently.
  assert.equal(
    describeBlock(
      block({ id: "advisor", kind: "planner", persona: "prompt", role_hint: "advisor" })
    ),
    "planner · claude · sonnet · repo persona · ADVISOR"
  );
  // No hint, no chip — today's behavior, byte for byte.
  assert.equal(describeBlock(block({ id: "w", kind: "worker" })), "worker · claude · sonnet");
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
