// Unit tests for the pure session-browser metadata formatting (#1). Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  taskSummary,
  repoBranchLine,
  paneNameLine,
  prLabel,
  restoredPaneName,
  sessionBadgeLabel,
} from "../src/sessionmeta.ts";
import type { SessionRoleInfo } from "../src/orchestration.ts";

const role = (over: Partial<SessionRoleInfo> = {}): SessionRoleInfo => ({
  session_id: "s-1",
  group_id: "g-1",
  role: "worker",
  agent_name: "builder",
  group_live: true,
  task: "",
  branch: null,
  repo: null,
  pr: null,
  ...over,
});

test("taskSummary returns the task text verbatim when short", () => {
  assert.equal(taskSummary(role({ task: "implement the thing" })), "implement the thing");
});

test("taskSummary truncates a long task with an ellipsis, not a hard cut", () => {
  const long = "x".repeat(200);
  const out = taskSummary(role({ task: long }));
  assert.ok(out && out.length <= 140, `expected <=140 chars, got ${out?.length}`);
  assert.ok(out?.endsWith("…"), "truncated text must end with an ellipsis marker");
});

test("taskSummary is null for an empty/whitespace task, not an empty string", () => {
  assert.equal(taskSummary(role({ task: "" })), null);
  assert.equal(taskSummary(role({ task: "   " })), null);
});

test("taskSummary is null when no role is recorded at all", () => {
  assert.equal(taskSummary(undefined), null);
});

test("repoBranchLine combines repo and branch when both are known", () => {
  assert.equal(
    repoBranchLine(role({ repo: "C:/Projects/loomux", branch: "feat/thing" })),
    "loomux @ feat/thing"
  );
});

test("repoBranchLine shows branch alone when repo is unknown", () => {
  assert.equal(repoBranchLine(role({ branch: "feat/thing" })), "feat/thing");
});

test("repoBranchLine shows repo alone when branch is unknown (the orchestrator's case)", () => {
  assert.equal(repoBranchLine(role({ repo: "C:/Projects/loomux" })), "loomux");
});

test("repoBranchLine is null when neither is known — never a fabricated placeholder", () => {
  assert.equal(repoBranchLine(role()), null);
  assert.equal(repoBranchLine(undefined), null);
});

test("prLabel normalizes a bare PR number to #N", () => {
  assert.equal(prLabel(role({ pr: "42" })), "#42");
});

test("prLabel passes through an already-shaped PR reference verbatim", () => {
  assert.equal(prLabel(role({ pr: "#42" })), "#42");
  assert.equal(prLabel(role({ pr: "https://github.com/o/r/pull/42" })), "https://github.com/o/r/pull/42");
});

test("prLabel is null when no PR is recorded yet", () => {
  assert.equal(prLabel(role({ pr: null })), null);
  assert.equal(prLabel(undefined), null);
});

// ---------- #722: the restored pane's name ----------

test("restoredPaneName names the CLI the row actually came from", () => {
  assert.equal(restoredPaneName("claude", "fix the login bug"), "claude · fix the login bug");
  assert.equal(restoredPaneName("copilot", "fix the login bug"), "copilot · fix the login bug");
  // The one this replaced a two-arm ternary for: with `source` read instead of
  // branched on, a third scanner's rows are named correctly the day they
  // arrive rather than inheriting whichever CLI sat in the else-branch — which
  // is how an opencode session would have opened a pane called "copilot · …".
  assert.equal(restoredPaneName("opencode", "fix the login bug"), "opencode · fix the login bug");
});

test("restoredPaneName cuts a long title with an ellipsis, keeping the CLI prefix intact", () => {
  const out = restoredPaneName("opencode", "y".repeat(200));
  assert.ok(out.startsWith("opencode · "), "the CLI must survive the truncation");
  assert.ok(out.endsWith("…"), "a cut title must say it was cut");
  assert.equal(out, `opencode · ${"y".repeat(34)}…`);
});

test("restoredPaneName leaves a title that fits exactly alone — no ellipsis on a non-cut", () => {
  const exact = "z".repeat(34);
  assert.equal(restoredPaneName("claude", exact), `claude · ${exact}`);
});

// ---- paneNameLine (#2116) ----

test("a name the human chose is shown", () => {
  assert.equal(paneNameLine("w: #2116 notes", "Fix the login bug", "claude"), "w: #2116 notes");
});

test("nothing recorded shows no line — the row's own title IS the fallback", () => {
  // Never a placeholder and never an empty line: the caller renders nothing at
  // all, and the title the row already shows is what the human reads.
  assert.equal(paneNameLine(undefined, "Fix the login bug", "claude"), null);
  assert.equal(paneNameLine("", "Fix the login bug", "claude"), null);
  assert.equal(paneNameLine("   ", "Fix the login bug", "claude"), null);
});

test("a name equal to the title is not printed twice", () => {
  assert.equal(paneNameLine("Fix the login bug", "Fix the login bug", "claude"), null);
  assert.equal(paneNameLine("  Fix the login bug  ", "Fix the login bug", "claude"), null);
});

test("the auto-name a restore MINTS is not a name the human chose", () => {
  // The row this matters most for is the commonest one on the page: a pane
  // opened by clicking this very list is auto-named `<cli> · <title>`, so
  // without this clause every such row grows a second line restating its own
  // title with a CLI prefix.
  //
  // Built by CALLING restoredPaneName rather than by spelling the format out,
  // so a change to the auto-name cannot leave this test asserting the old one
  // while the rows go noisy.
  const title = "Fix the login bug";
  assert.equal(paneNameLine(restoredPaneName("claude", title), title, "claude"), null);
  // Including the truncating case, where the auto-name is not a prefix-plus-
  // title at all.
  const long = "y".repeat(200);
  assert.equal(paneNameLine(restoredPaneName("opencode", long), long, "opencode"), null);
});

test("the auto-name of a DIFFERENT cli is still a name worth showing", () => {
  // The negative control for the clause above: it must suppress THIS row's
  // auto-name, not every string that happens to look like one. A pane named
  // after another CLI is a fact about the pane, not noise.
  const title = "Fix the login bug";
  assert.equal(
    paneNameLine(restoredPaneName("copilot", title), title, "claude"),
    `copilot · ${title}`
  );
});

test("a rename that only changes case is a rename", () => {
  // This is a report of what the human wrote, not a guess at what they meant.
  assert.equal(paneNameLine("WORKER", "worker", "claude"), "WORKER");
});

test("sessionBadgeLabel names the row's own CLI, whichever it is", () => {
  assert.equal(sessionBadgeLabel("claude"), "CLAUDE");
  assert.equal(sessionBadgeLabel("copilot"), "COPILOT");
  // The sidebar's own version of restoredPaneName's bug: a two-arm ternary
  // labelled an opencode row COPILOT — the badge asserting one CLI while the
  // resume command underneath it named another.
  assert.equal(sessionBadgeLabel("opencode"), "OPENCODE");
});
test("a fourth source names itself, on the badge and on the restored pane (#2126)", () => {
  // Both functions read the row's `source` rather than branching on it, so a new
  // scanner is named correctly on ARRIVAL. This test exists because the shape
  // they replaced — `s.source === "claude" ? … : "COPILOT"` — was correct only
  // while there were exactly two, and #2126 P2 found the LAST copy of it still
  // live in main.ts's dormant card.
  assert.equal(sessionBadgeLabel("pi"), "PI");
  assert.equal(restoredPaneName("pi", "fix the login bug"), "pi · fix the login bug");

  // The property, not the row: every source the wire can carry labels itself,
  // and no two collapse onto one label. Written as a loop over the union's own
  // members so a fifth source has to be added HERE, not discovered in the UI.
  const sources = ["claude", "copilot", "opencode", "pi"];
  const labels = sources.map(sessionBadgeLabel);
  assert.deepEqual(labels, ["CLAUDE", "COPILOT", "OPENCODE", "PI"]);
  assert.equal(new Set(labels).size, sources.length, "two sources share one badge label");
});
