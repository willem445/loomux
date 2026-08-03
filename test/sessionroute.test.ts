import { test } from "node:test";
import assert from "node:assert/strict";
import { sessionRestoreRoute } from "../src/sessionroute.ts";

const ORCH = { group_id: "loomux-abc123", role: "orchestrator" };

// #781, the regression this module exists for. A copilot orchestrator session
// with a recorded roster row must rejoin its group exactly like a claude one:
// the pre-fix route gated on the CLI, so this row fell through to a bare
// `copilot --resume=<id>` with no MCP config, no --add-dir, no allow-all
// posture and no group binding — while its ORCH chip told the human clicking it
// would "restore the whole orchestration".
test("a recorded orchestration session rejoins its group on EVERY cli", () => {
  for (const source of ["copilot", "claude"]) {
    assert.deepEqual(
      sessionRestoreRoute({ source, title: "review the spawn path" }, ORCH),
      { kind: "orchestration", groupId: "loomux-abc123", role: "orchestrator" },
      `${source} must route by recorded membership, not by which CLI wrote the session`
    );
  }
});

// Every recorded role rejoins, not just the orchestrator — a worker/reviewer
// row is the same membership fact.
test("worker and reviewer rows rejoin too", () => {
  for (const role of ["worker", "reviewer", "planner"]) {
    assert.deepEqual(sessionRestoreRoute({ source: "copilot", title: "t" }, { group_id: "g", role }), {
      kind: "orchestration",
      groupId: "g",
      role,
    });
  }
});

// The other half of the rule: no record, no group. A session loomux has no
// membership evidence for is a plain pane and says so — never a group rejoin on
// nothing but the CLI it happens to have been written by.
test("a session with no recorded membership restores plain", () => {
  assert.deepEqual(sessionRestoreRoute({ source: "copilot", title: "scratch" }, undefined), {
    kind: "plain",
    paneName: "copilot · scratch",
  });
  assert.deepEqual(sessionRestoreRoute({ source: "claude", title: "scratch" }, undefined), {
    kind: "plain",
    paneName: "claude · scratch",
  });
});

// Edge case: a long title is truncated so the pane tab stays readable, and the
// boundary is inclusive — exactly 34 characters is kept whole.
test("plain pane names truncate a long title at 34 chars", () => {
  const exactly34 = "x".repeat(34);
  assert.deepEqual(sessionRestoreRoute({ source: "claude", title: exactly34 }, undefined), {
    kind: "plain",
    paneName: `claude · ${exactly34}`,
  });
  assert.deepEqual(sessionRestoreRoute({ source: "copilot", title: "y".repeat(80) }, undefined), {
    kind: "plain",
    paneName: `copilot · ${"y".repeat(34)}…`,
  });
});
