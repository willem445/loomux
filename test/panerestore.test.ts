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
      leaf.parent = parent;
      parent.children.push(leaf);
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
