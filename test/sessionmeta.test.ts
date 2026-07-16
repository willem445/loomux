// Unit tests for the pure session-browser metadata formatting (#1). Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { taskSummary, repoBranchLine, prLabel } from "../src/sessionmeta.ts";
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
