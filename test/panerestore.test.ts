// The per-pane restore policy + layout flattening (#194). Pure — panerestore.ts.
// Pins the adopted hybrid: agents auto-resume via a recorded session id, groups
// stay dormant, terminals re-spawn — and the ordered rebuild sequence for a
// nested layout.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planPaneRestore,
  planLayoutRestore,
  AUTO_RESUME_AGENTS,
  type RestoreStep,
} from "../src/panerestore.ts";
import type { PersistedPane, PersistedLayoutNode } from "../src/tabstore.ts";

const pane = (over: Partial<PersistedPane>): PersistedPane => ({
  paneKind: "terminal",
  name: "p",
  cwd: null,
  command: null,
  argv: null,
  shellKind: null,
  sessionId: null,
  ...over,
});

test("a terminal re-spawns a fresh shell in its recorded cwd + shell kind", () => {
  const action = planPaneRestore(pane({ paneKind: "terminal", name: "shell", cwd: "/repo", shellKind: "gitbash" }));
  assert.deepEqual(action, { type: "spawn-terminal", name: "shell", cwd: "/repo", shellKind: "gitbash" });
});

test("an agent WITH a session id auto-resumes (never replays a prompt)", () => {
  const action = planPaneRestore(
    pane({
      paneKind: "agent",
      name: "claude",
      cwd: "/repo",
      command: "claude",
      argv: ["claude"],
      sessionId: "abc-123",
    })
  );
  assert.deepEqual(action, {
    type: "resume-agent",
    name: "claude",
    cwd: "/repo",
    command: "claude",
    argv: ["claude"],
    sessionId: "abc-123",
  });
});

test("an agent WITHOUT a session id falls back to a dormant Start placeholder", () => {
  const action = planPaneRestore(
    pane({ paneKind: "agent", name: "copilot", cwd: "/repo", command: "copilot", argv: null, sessionId: null })
  );
  assert.deepEqual(action, {
    type: "dormant-agent",
    name: "copilot",
    cwd: "/repo",
    command: "copilot",
    argv: null,
  });
});

test("an orchestration pane ALWAYS restores dormant — never auto-resumed", () => {
  // The one credit/process-storm-sensitive case (#83/#78): a group is only ever
  // revived by the human via resumeOrchSession, so restore must not spawn it.
  const action = planPaneRestore(pane({ paneKind: "orch", name: "orchestrator", cwd: "/repo" }));
  assert.deepEqual(action, { type: "dormant-group", name: "orchestrator" });
});

test("even with a session id, a group stays dormant (the rule is keyed on kind, not id)", () => {
  // A worker pane could carry a resumable session id; auto-resuming it would be
  // exactly the process storm we refuse. Kind wins over the presence of an id.
  const action = planPaneRestore(pane({ paneKind: "orch", name: "worker-1", cwd: "/wt", sessionId: "xyz-1" }));
  assert.deepEqual(action, { type: "dormant-group", name: "worker-1" });
});

test("AUTO_RESUME_AGENTS is the adopted default (the one-line all-dormant flip)", () => {
  // Guards the promise that flipping this single constant makes agents dormant.
  assert.equal(AUTO_RESUME_AGENTS, true);
});

// ---------- layout flattening ----------

const LAYOUT: PersistedLayoutNode = {
  kind: "split",
  dir: "row",
  weight: 1,
  children: [
    {
      kind: "leaf",
      weight: 1,
      pane: pane({ paneKind: "terminal", name: "left", cwd: "/a", shellKind: "cmd" }),
    },
    {
      kind: "split",
      dir: "column",
      weight: 3,
      children: [
        {
          kind: "leaf",
          weight: 1,
          pane: pane({ paneKind: "agent", name: "top", command: "claude", sessionId: "s1" }),
        },
        {
          kind: "leaf",
          weight: 2,
          pane: pane({ paneKind: "orch", name: "bottom" }),
        },
      ],
    },
  ],
};

test("planLayoutRestore flattens a nested tree in pre-order with parent-split dir + weights", () => {
  const steps = planLayoutRestore(LAYOUT);
  const shape = steps.map((s: RestoreStep) => ({ type: s.action.type, dir: s.dir, weight: s.weight }));
  assert.deepEqual(shape, [
    // left leaf: parent is the root row-split
    { type: "spawn-terminal", dir: "row", weight: 1 },
    // the two panes under the nested column-split open in "column"
    { type: "resume-agent", dir: "column", weight: 1 },
    { type: "dormant-group", dir: "column", weight: 2 },
  ]);
});

test("planLayoutRestore on a single leaf yields one step with the default row dir", () => {
  const single: PersistedLayoutNode = {
    kind: "leaf",
    weight: 1,
    pane: pane({ paneKind: "terminal", name: "solo" }),
  };
  const steps = planLayoutRestore(single);
  assert.equal(steps.length, 1);
  assert.equal(steps[0].dir, "row");
  assert.equal(steps[0].action.type, "spawn-terminal");
});
