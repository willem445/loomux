// Pure promote-to-orchestrator model (#407 slice B) — promote.ts. Pins the three
// decisions the gesture cannot get wrong: what the human consents to, what the
// relaunched pane is actually spawned with (the env/argv trap), and what they are
// told when it fails AFTER the old process was killed.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  promoteConfirmLines,
  promoteFailureText,
  promoteOffersRoster,
  promotePaneOptions,
  promoteRecoveryNote,
} from "../src/promote.ts";
import type { OrchSpawnRequest } from "../src/orchestration.ts";

const spawnRequest = (overrides: Partial<OrchSpawnRequest> = {}): OrchSpawnRequest => ({
  group_id: "loomux-1a2b",
  agent_id: "orch-4",
  role: "orchestrator",
  name: "orchestrator",
  cwd: "/repo/poc",
  command: "claude --resume 11111111-2222-3333-4444-555555555555 --mcp-config … --strict-mcp-config",
  deadline_ms: 0,
  argv: ["claude", "--resume", "11111111-2222-3333-4444-555555555555", "--strict-mcp-config"],
  env: [
    ["PATH", "C:\\shim;C:\\Windows"],
    ["ORRERIX_GROUP_DIR", "C:\\state\\loomux-1a2b"],
    ["ORRERIX_AGENT_ID", "orch-4"],
    ["LOOMUX_GROUP_DIR", "C:\\state\\loomux-1a2b"],
    ["LOOMUX_AGENT_ID", "orch-4"],
  ],
  ...overrides,
});

// ---------- the env/argv trap ----------

test("a promoted pane is spawned WITH the request's env — the gh shim and BOTH group-dir spellings reach the pty", () => {
  // A normal launcher pane carries no env at all, so dropping this is invisible:
  // the pane looks right and simply has no merge gate. This is the assertion that
  // notices.
  const opts = promotePaneOptions(spawnRequest(), "11111111-2222-3333-4444-555555555555");
  assert.deepEqual(opts.env, [
    ["PATH", "C:\\shim;C:\\Windows"],
    ["ORRERIX_GROUP_DIR", "C:\\state\\loomux-1a2b"],
    ["ORRERIX_AGENT_ID", "orch-4"],
    ["LOOMUX_GROUP_DIR", "C:\\state\\loomux-1a2b"],
    ["LOOMUX_AGENT_ID", "orch-4"],
  ]);
});

test("a promoted pane carries the request's argv (direct-CLI spawn) and its command, unmodified", () => {
  const req = spawnRequest();
  const opts = promotePaneOptions(req, "11111111-2222-3333-4444-555555555555");
  assert.deepEqual(opts.argv, req.argv);
  assert.equal(opts.command, req.command);
  assert.equal(opts.cwd, "/repo/poc");
});

test("a promoted pane takes the group identity from the request, and records the session it resumed", () => {
  const opts = promotePaneOptions(spawnRequest(), "11111111-2222-3333-4444-555555555555");
  assert.equal(opts.orchGroup, "loomux-1a2b");
  assert.equal(opts.orchRole, "orchestrator");
  assert.equal(opts.orchAgent, "orch-4");
  assert.equal(opts.sessionId, "11111111-2222-3333-4444-555555555555");
  assert.ok(opts.badge, "a promoted pane gets the group/role chip every orchestration pane has");
});

test("a spawn request from a backend with no env/argv yields a pane with none — never an empty-array stand-in", () => {
  const opts = promotePaneOptions(spawnRequest({ env: undefined, argv: undefined }), "s-1");
  assert.equal(opts.env, undefined);
  assert.equal(opts.argv, undefined);
});

// ---------- refusals: the tag is for code, the sentence is for the human ----------

test("a tagged backend refusal is shown as its sentence, without the machine tag", () => {
  const raw =
    "promote-unsupported-cli: promoting a copilot pane is not supported yet — v1 covers Claude panes, whose session id loomux knows and can resume";
  assert.equal(
    promoteFailureText(raw),
    "promoting a copilot pane is not supported yet — v1 covers Claude panes, whose session id loomux knows and can resume"
  );
});

test("every promote tag the backend can emit is stripped, not just the first one anybody tested", () => {
  for (const tag of [
    "promote-unsupported-cli",
    "promote-bad-session",
    "promote-bad-repo",
    "promote-already-managed",
    "promote-not-found",
    "promote-store-unreadable",
    "promote-cli-mismatch",
  ]) {
    assert.equal(promoteFailureText(`${tag}: the reason`), "the reason", tag);
  }
});

test("an untagged failure is passed through whole — a raw message beats a swallowed one", () => {
  assert.equal(promoteFailureText("  invoke failed: window not found  "), "invoke failed: window not found");
  // A `resume-`tagged error is not this gesture's; it must not be half-eaten.
  assert.equal(promoteFailureText("resume-not-found: nope"), "resume-not-found: nope");
});

// ---------- the post-kill failure: never a silent fresh session ----------

test("a post-kill failure names the group and routes to the Resume card — never to a fresh session", () => {
  for (const stage of ["spawn", "bind"] as const) {
    const note = promoteRecoveryNote("loomux-1a2b", stage);
    assert.match(note, /loomux-1a2b/, `${stage}: the human needs the group id to find the card`);
    assert.match(note, /resume/i, `${stage}: the recovery route is the dormant-group Resume card`);
    assert.doesNotMatch(
      note,
      /start(ed|ing)? (a )?fresh/i,
      `${stage}: a fresh session discards the conversation this feature exists to keep`
    );
  }
});

test("the two post-kill stages are distinguishable — 'never started' and 'started but unbound' are different situations", () => {
  assert.notEqual(promoteRecoveryNote("g1", "spawn"), promoteRecoveryNote("g1", "bind"));
});

// ---------- consent: what the modal has to say before one click interrupts a turn ----------

const validWorkflow = (name = "loomux dev") => ({ name, valid: true });

test("the confirm names the repo, warns the live turn is interrupted, and states all three group cases", () => {
  const lines = promoteConfirmLines("/repo/poc", null).join("\n");
  assert.match(lines, /\/repo\/poc/);
  assert.match(lines, /interrupt/i, "promotion kills a live CLI mid-turn — that is the consent");
  assert.match(lines, /new group/i);
  assert.match(lines, /dormant/i);
  assert.match(lines, /sibling/i);
});

test("the workflow line appears only when the repo actually declares a workflow", () => {
  assert.ok(
    !promoteConfirmLines("/repo/poc", null).some((l) => /workflow/i.test(l)),
    "no file → no roster line, and no checkbox to explain"
  );
  const declared = promoteConfirmLines("/repo/poc", validWorkflow()).join("\n");
  assert.match(declared, /workflow/i);
  assert.match(declared, /loomux dev/, "the human is told WHICH workflow they'd be running");
  // A present-but-nameless file still gets a line, naming the file instead.
  assert.match(promoteConfirmLines("/repo/poc", validWorkflow("")).join("\n"), /workflow\.yml/);
});

test("the confirm says a reattached dormant group keeps its own roster — on BOTH workflow arms (rev-2 N9)", () => {
  // The clause answers "which roster will this promotion actually run", and the
  // answer turns on the group case whether or not the file validates. A human
  // who happens to get the invalid line must not be left with a different
  // understanding of the same promotion.
  for (const workflow of [validWorkflow("wf"), { name: "wf", valid: false }]) {
    assert.match(
      promoteConfirmLines("/repo/poc", workflow).join("\n"),
      /dormant group keeps the roster/i,
      `valid=${workflow.valid}`
    );
  }
});

// rev-1 N2: a workflow file that does not validate.

test("#407 rev-1 N2: an INVALID workflow file offers no roster checkbox — the group would run the built-in roles regardless", () => {
  assert.equal(promoteOffersRoster({ name: "broken", valid: false }), false);
  assert.equal(promoteOffersRoster(null), false);
  assert.equal(promoteOffersRoster({ name: "loomux dev", valid: true }), true);
});

test("#407 rev-1 N2: an INVALID workflow file is still NAMED, and says what will actually run instead", () => {
  // Silence would be safe but misleading: this is the same consent moment the
  // launcher warns inline at, for the same file.
  const lines = promoteConfirmLines("/repo/poc", { name: "loomux dev", valid: false }).join("\n");
  assert.match(lines, /loomux dev/, "the human is told which file is broken");
  assert.match(lines, /doesn't validate|does not validate/i);
  assert.match(lines, /built-in four roles/i, "…and what the group runs instead");
  assert.doesNotMatch(
    lines,
    /Tick the box/i,
    "a broken file must not be described as something a checkbox can run"
  );
});
