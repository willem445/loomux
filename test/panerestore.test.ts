// The per-pane restore policy + layout flattening (#194). Pure — panerestore.ts.
// Pins the adopted hybrid: agents auto-resume via a recorded session id, groups
// stay dormant, terminals re-spawn — and the ordered rebuild sequence for a
// nested layout.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planPaneRestore,
  planLayoutRestore,
  agentResumeCommand,
  agentFreshCommand,
  shouldRespawnFresh,
  AUTO_RESUME_AGENTS,
  type RestoreAction,
  type RestoreOpenStep,
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

// ---------- BUG-1: resume vs fresh when the conversation is gone ----------

test("an agent whose session HAS a resumable conversation still resumes", () => {
  const action = planPaneRestore(
    pane({ paneKind: "agent", name: "claude", cwd: "/repo", command: "claude --session-id s1", sessionId: "s1" }),
    (id) => id === "s1" // predicate says the transcript exists
  );
  assert.equal(action.type, "resume-agent");
});

test("an agent whose session has NO conversation restores FRESH, keeping its identity", () => {
  // The BUG-1 crash: `claude --resume <id>` exits 1 ("No conversation found") when
  // the session was never prompted. With a predicate that says the id is gone, we
  // plan a fresh start in place — same name/cwd/CLI/id — instead of the doomed resume.
  const action = planPaneRestore(
    pane({ paneKind: "agent", name: "claude", cwd: "/repo", command: "claude --session-id s2", sessionId: "s2" }),
    () => false // no transcript for any id
  );
  assert.deepEqual(action, {
    type: "fresh-agent",
    name: "claude",
    cwd: "/repo",
    command: "claude --session-id s2",
    argv: null,
    sessionId: "s2",
  });
});

test("with NO predicate, an agent with a session id resumes (unchanged behavior)", () => {
  const action = planPaneRestore(
    pane({ paneKind: "agent", name: "claude", command: "claude", sessionId: "s3" })
  );
  assert.equal(action.type, "resume-agent");
});

test("planLayoutRestore threads the resumable predicate to every leaf", () => {
  const tree: PersistedLayoutNode = {
    kind: "split",
    dir: "row",
    weight: 1,
    children: [
      leaf(1, { paneKind: "agent", name: "live", command: "claude", sessionId: "here" }),
      leaf(1, { paneKind: "agent", name: "gone", command: "claude", sessionId: "missing" }),
    ],
  };
  const steps = planLayoutRestore(tree, (id) => id === "here");
  const types = steps.map((s) => s.action.type).sort();
  assert.deepEqual(types, ["fresh-agent", "resume-agent"], "one resumes, the missing one goes fresh");
});

test("agentFreshCommand pins the recorded id via --session-id (not --resume), stripping stale flags", () => {
  // From a resume line — becomes a fresh-start line with the same id, so the fresh
  // session is itself resumable next boot, and it never carries a prompt.
  assert.deepEqual(agentFreshCommand("claude --resume old --model opus", null, "s1"), {
    command: "claude --model opus --session-id s1",
  });
  // From the original launch line — the stale --session-id is replaced, not doubled.
  assert.deepEqual(agentFreshCommand("claude --session-id old", null, "s2"), {
    command: "claude --session-id s2",
  });
  // argv + bare fallbacks.
  assert.deepEqual(agentFreshCommand(null, ["claude", "--resume", "old"], "s3"), {
    argv: ["claude", "--session-id", "s3"],
  });
  assert.deepEqual(agentFreshCommand(null, null, "s4"), { command: "claude --session-id s4" });
});

test("shouldRespawnFresh: fresh-respawn only on an unexpected non-zero exit", () => {
  assert.equal(shouldRespawnFresh({ exit_code: 1, expected: false }), true, "resume-not-found (exit 1)");
  assert.equal(shouldRespawnFresh({ exit_code: 2, expected: false }), true, "any resume-time failure");
  assert.equal(shouldRespawnFresh({ exit_code: 0, expected: false }), false, "clean exit — the human quit");
  assert.equal(shouldRespawnFresh({ exit_code: 1, expected: true }), false, "loomux killed it (pane close)");
  assert.equal(shouldRespawnFresh({ exit_code: null, expected: false }), false, "no code — signal/kill");
});

// ---------- resume command building ----------

test("resume appends --resume to a plain claude command, keeping other flags", () => {
  assert.deepEqual(agentResumeCommand("claude --dangerously-skip-permissions", null, "s1"), {
    command: "claude --dangerously-skip-permissions --resume s1",
  });
});

test("resume replaces a recorded --session-id (space form) rather than doubling it", () => {
  assert.deepEqual(agentResumeCommand("claude --session-id old-id --model opus", null, "s2"), {
    command: "claude --model opus --resume s2",
  });
});

test("resume replaces a recorded --resume/--session-id in the `=` form too", () => {
  assert.deepEqual(agentResumeCommand("claude --session-id=old --resume=stale", null, "s3"), {
    command: "claude --resume s3",
  });
});

test("resume never carries a prompt — only the launch flags plus --resume", () => {
  // Guards the no-replay rule: whatever was recorded, the output is just the
  // program + surviving flags + the resume id, never a queued prompt.
  const out = agentResumeCommand("claude", null, "abc");
  assert.equal(out.command, "claude --resume abc");
});

test("resume falls back to argv when there is no string command", () => {
  assert.deepEqual(agentResumeCommand(null, ["claude", "--session-id", "old"], "s4"), {
    argv: ["claude", "--resume", "s4"],
  });
});

test("resume with neither command nor argv best-efforts a bare claude --resume", () => {
  assert.deepEqual(agentResumeCommand(null, null, "s5"), { command: "claude --resume s5" });
});

// ---------- layout plan: reconstructible round-trip ----------
//
// The plan must be replayable into the EXACT tree — structure and weights.
// `rebuild` below is a pure model of grid.ts's `insertBeside` + the weight rule
// panerestore documents, so a serialize → plan → replay round-trip can be
// asserted here without a DOM. If P4's real grid wiring matches this model,
// restore is faithful; the model IS the contract.

type SimNode =
  | { kind: "leaf"; weight: number; pending: number[]; action: RestoreAction; parent: SimSplit | null }
  | SimSplit;
interface SimSplit {
  kind: "split";
  dir: "row" | "column";
  weight: number;
  children: SimNode[];
  parent: SimSplit | null;
}

/** Replay an open-plan through a model of grid.insertBeside, returning the rebuilt
 *  tree. Mirrors: same-direction parent → add a sibling; else wrap the anchor in a
 *  new 2-way split that inherits the anchor's slot weight. Applies the weight
 *  chain the same way the real restore must (outer slot weight to the new split,
 *  next weight to the anchor inside it). */
function rebuild(steps: RestoreOpenStep[]): SimNode {
  const leaves: SimNode[] = [];
  let root: SimNode = {
    kind: "leaf",
    weight: steps[0].weights[0],
    pending: steps[0].weights.slice(1),
    action: steps[0].action,
    parent: null,
  };
  leaves.push(root);
  for (let i = 1; i < steps.length; i++) {
    const s = steps[i];
    const anchor = leaves[s.relativeTo!];
    const leaf: SimNode = {
      kind: "leaf",
      weight: s.weights[0],
      pending: s.weights.slice(1),
      action: s.action,
      parent: null,
    };
    leaves.push(leaf);
    const parent = anchor.parent;
    if (parent && parent.dir === s.dir) {
      // Mirror grid.insertBeside: a same-direction sibling splices in AFTER the
      // anchor (idx+1), NOT appended at the end — this is what makes a
      // wrong-anchor plan reorder middle siblings and get caught here.
      leaf.parent = parent;
      parent.children.splice(parent.children.indexOf(anchor) + 1, 0, leaf);
    } else {
      const split: SimSplit = {
        kind: "split",
        dir: s.dir,
        weight: anchor.weight, // new split takes the anchor's outer slot
        children: [anchor, leaf],
        parent,
      };
      anchor.weight = anchor.pending.shift()!; // anchor's weight one level in
      anchor.parent = split;
      leaf.parent = split;
      if (parent) parent.children[parent.children.indexOf(anchor)] = split;
      else root = split;
    }
  }
  return root;
}

/** Strip a persisted tree to the comparable shape `rebuild` produces (panes →
 *  their restore actions, no parent pointers). */
function actionTree(node: PersistedLayoutNode): unknown {
  return node.kind === "leaf"
    ? { kind: "leaf", weight: node.weight, action: planPaneRestore(node.pane) }
    : { kind: "split", dir: node.dir, weight: node.weight, children: node.children.map(actionTree) };
}

/** Strip a rebuilt SimNode to the same comparable shape. */
function simShape(node: SimNode): unknown {
  return node.kind === "leaf"
    ? { kind: "leaf", weight: node.weight, action: node.action }
    : { kind: "split", dir: node.dir, weight: node.weight, children: node.children.map(simShape) };
}

const leaf = (weight: number, over: Partial<PersistedPane>): PersistedLayoutNode => ({
  kind: "leaf",
  weight,
  pane: pane(over),
});

// A 2×2 grid: row of two column-splits. Distinct weights everywhere so a weight
// drop or a mis-nesting is caught.
const GRID_2x2: PersistedLayoutNode = {
  kind: "split",
  dir: "row",
  weight: 1,
  children: [
    { kind: "split", dir: "column", weight: 3, children: [leaf(1, { name: "A" }), leaf(2, { name: "B" })] },
    { kind: "split", dir: "column", weight: 6, children: [leaf(4, { name: "C" }), leaf(5, { name: "D" })] },
  ],
};

// Four stacked panes: a single column split of four leaves.
const STACK_4: PersistedLayoutNode = {
  kind: "split",
  dir: "column",
  weight: 1,
  children: [leaf(1, { name: "A" }), leaf(2, { name: "B" }), leaf(3, { name: "C" }), leaf(4, { name: "D" })],
};

// Asymmetric nesting with a weighted subtree (the divided divider case).
const ASYMMETRIC: PersistedLayoutNode = {
  kind: "split",
  dir: "row",
  weight: 1,
  children: [
    leaf(1, { paneKind: "terminal", name: "left", cwd: "/a", shellKind: "cmd" }),
    {
      kind: "split",
      dir: "column",
      weight: 3, // outer divider dragged to 25/75 — must survive
      children: [
        leaf(1, { paneKind: "agent", name: "top", command: "claude", sessionId: "s1" }),
        leaf(2, { paneKind: "orch", name: "bottom" }),
      ],
    },
  ],
};

for (const [label, tree] of [
  ["2×2 grid", GRID_2x2],
  ["4-pane stack", STACK_4],
  ["asymmetric weighted nesting", ASYMMETRIC],
] as const) {
  test(`round-trip is structure- AND weight-identical: ${label}`, () => {
    const rebuilt = rebuild(planLayoutRestore(tree));
    assert.deepEqual(simShape(rebuilt), actionTree(tree));
  });
}

/** In-order leaf names of a rebuilt tree — to pin sibling ORDER, not just shape. */
function leafOrder(node: SimNode): string[] {
  if (node.kind === "leaf") {
    const a = node.action;
    return [a.name];
  }
  return node.children.flatMap(leafOrder);
}

test("≥3 siblings replay in insertion order (grid splices after the anchor, so anchoring must walk forward)", () => {
  // The exact regression: col[A,B,C,D] must NOT come back as col[A,D,C,B].
  assert.deepEqual(leafOrder(rebuild(planLayoutRestore(STACK_4))), ["A", "B", "C", "D"]);
  // And a 3-wide row, for good measure.
  const ROW_3: PersistedLayoutNode = {
    kind: "split",
    dir: "row",
    weight: 1,
    children: [leaf(1, { name: "X" }), leaf(1, { name: "Y" }), leaf(1, { name: "Z" })],
  };
  assert.deepEqual(leafOrder(rebuild(planLayoutRestore(ROW_3))), ["X", "Y", "Z"]);
});

test("a 2×2 grid and 4 stacked panes produce DIFFERENT plans (the ambiguity the flat list lost)", () => {
  // The whole point: distinct nestings must not flatten to the same sequence, or
  // no P4 wiring could tell them apart.
  assert.notDeepEqual(planLayoutRestore(GRID_2x2), planLayoutRestore(STACK_4));
  // And each rebuilds only to itself.
  assert.deepEqual(simShape(rebuild(planLayoutRestore(GRID_2x2))), actionTree(GRID_2x2));
  assert.deepEqual(simShape(rebuild(planLayoutRestore(STACK_4))), actionTree(STACK_4));
  assert.notDeepEqual(simShape(rebuild(planLayoutRestore(GRID_2x2))), actionTree(STACK_4));
});

test("the second+ child of a split carries the dir; the first child never re-opens", () => {
  // left is the row's anchor (relativeTo null, the grid-filling root); the column
  // subtree's ENTRY (top) opens with the ROW dir relative to left, and only its
  // sibling (bottom) opens with the column dir — the reviewer's "dir belongs to
  // the second leaf" rule.
  const steps = planLayoutRestore(ASYMMETRIC);
  assert.deepEqual(
    steps.map((s) => ({ type: s.action.type, rel: s.relativeTo, dir: s.dir })),
    [
      { type: "spawn-terminal", rel: null, dir: "row" }, // left (root fill)
      { type: "resume-agent", rel: 0, dir: "row" }, // column subtree entry, opened in ROW beside left
      { type: "dormant-group", rel: 1, dir: "column" }, // bottom, opened in COLUMN beside top
    ]
  );
  // The weighted subtree's weight (3) is carried, not dropped.
  assert.deepEqual(steps[1].weights, [3, 1], "column-subtree slot weight 3, then top's own weight 1");
});

test("planLayoutRestore on a single leaf yields one root-fill step", () => {
  const steps = planLayoutRestore(leaf(1, { paneKind: "terminal", name: "solo" }));
  assert.equal(steps.length, 1);
  assert.equal(steps[0].relativeTo, null);
  assert.equal(steps[0].action.type, "spawn-terminal");
  assert.deepEqual(simShape(rebuild(steps)), actionTree(leaf(1, { paneKind: "terminal", name: "solo" })));
});
