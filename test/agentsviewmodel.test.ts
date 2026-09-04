// The Agents tab's presentation decisions (#2122 slice B), separated from the
// DOM that applies them: which filter chips exist, what each says, which rows
// survive the current chip, and what the identity line under a row's name
// reads. `src/agentsview.ts` is the DOM and is hand-validated.

import { test } from "node:test";
import assert from "node:assert/strict";

import type { AgentRow, AgentState } from "../src/agentrows.ts";
import {
  AGENT_STATE_LABEL,
  agentIdentityLine,
  filterChips,
  visibleRows,
} from "../src/agentsviewmodel.ts";

let seq = 0;
const row = (state: AgentState, over: Partial<AgentRow> = {}): AgentRow => ({
  key: `pane-${++seq}`,
  name: `pane ${seq}`,
  harness: null,
  group: null,
  agentId: null,
  role: null,
  state,
  notes: null,
  ...over,
});

test("every state a row can carry has a word for it", () => {
  // `Record<AgentState, string>` is total, so a state added to the ladder
  // without a label fails to compile rather than rendering blank. This asserts
  // the other half — that no label is empty, which the type cannot say.
  for (const [state, label] of Object.entries(AGENT_STATE_LABEL)) {
    assert.ok(label.length > 0, `${state} has no label`);
  }
});

test("the reported state uses the header chip's own word (#2367)", () => {
  // The pane header renders the `report` reason as "✓ reported" — the Agents
  // tab used to call the same pane "question", which is the divergence #2367
  // exists to fix. Pin the word, so the two surfaces cannot drift apart again.
  assert.equal(AGENT_STATE_LABEL.reported, "reported");
});

test("a chip is offered for every state present, and never for one that is not", () => {
  const rows = [row("working"), row("working"), row("idle")];
  const chips = filterChips(rows, "all");
  assert.deepEqual(
    chips.map((c) => c.filter),
    ["all", "working", "idle"],
    "chips follow the ladder's own precedence order, after `all`"
  );
  assert.deepEqual(
    chips.map((c) => c.count),
    [3, 2, 1]
  );
  assert.ok(chips.every((c) => (c.filter === "all") === c.selected));
});

// The chip you are STANDING ON must not vanish. If it did, selecting `held`
// and watching the last held pane resolve would leave an empty list with no
// control to get back out of it — a filter you cannot clear.
test("the selected chip survives its own rows going away", () => {
  const chips = filterChips([row("working")], "held");
  const held = chips.find((c) => c.filter === "held");
  assert.ok(held, "the selected chip disappeared when its rows did");
  assert.equal(held.count, 0);
  assert.equal(held.selected, true);
});

test("the filter decides which rows render, and the order never depends on it", () => {
  const rows = [row("idle", { name: "zeta" }), row("attention", { name: "alpha" }), row("idle", { name: "beta" })];
  assert.deepEqual(
    visibleRows(rows, "all").map((r) => r.name),
    ["alpha", "beta", "zeta"],
    "most-wants-you first, then by name"
  );
  assert.deepEqual(
    visibleRows(rows, "idle").map((r) => r.name),
    ["beta", "zeta"]
  );
  assert.deepEqual(visibleRows(rows, "held"), []);
});

test("the identity line names what a pane actually has, and nothing it does not", () => {
  assert.equal(agentIdentityLine(row("working")), "");
  assert.equal(agentIdentityLine(row("working", { harness: "claude" })), "claude");
  assert.equal(
    agentIdentityLine(row("working", { harness: "copilot", role: "worker", group: "g-1" })),
    "copilot · worker · g-1"
  );
  // A group with no role, and a role with no group, both read honestly rather
  // than emitting a stray separator.
  assert.equal(agentIdentityLine(row("working", { group: "g-1" })), "g-1");
  assert.equal(agentIdentityLine(row("working", { role: "reviewer" })), "reviewer");
});

test("a workflow block shows as itself, not as the built-in role it resembles", () => {
  // `orchRole` carries the declared block id for a workflow group (#222), so a
  // `rev-security` agent must read `rev-security` here. Branching on a known
  // role name to produce a label is the #722/#841 defect one channel over.
  assert.equal(agentIdentityLine(row("working", { role: "rev-security" })), "rev-security");
});
