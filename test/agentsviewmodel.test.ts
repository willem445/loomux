// The Agents tab's presentation decisions (#2122 slice B), separated from the
// DOM that applies them: which filter chips exist, what each says, which rows
// survive the current chip, and what the identity line under a row's name
// reads. `src/agentsview.ts` is the DOM and is hand-validated.

import { test } from "node:test";
import assert from "node:assert/strict";

import { AGENTS } from "../src/agents.ts";
import type { AgentRow, AgentState, TabRef } from "../src/agentrows.ts";
import {
  AGENT_ORDER_LABEL,
  AGENT_STATE_LABEL,
  ORDER_CHOICES,
  agentIdentityLine,
  agentRowMark,
  filterChips,
  visibleGroups,
} from "../src/agentsviewmodel.ts";

/** One tab, so every fixture below lands in one group and the assertions stay
 *  about the thing under test. `groupRows`' own ordering is pinned in
 *  `test/agentrows.test.ts`. */
const WS: TabRef = { id: "ws-1", title: "loomux", index: 0 };

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
  tab: WS,
  ...over,
});

/** The rows `visibleGroups` renders, flattened — every assertion below that was
 *  written against the pre-#2371 flat list reads through this, so grouping is
 *  not silently re-asserted where the subject is the filter. */
const visibleNames = (rows: readonly AgentRow[], filter: Parameters<typeof visibleGroups>[1]): string[] =>
  visibleGroups(rows, filter, "state").flatMap((g) => g.rows.map((r) => r.name));

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
  assert.deepEqual(visibleNames(rows, "all"), ["alpha", "beta", "zeta"], "most-wants-you first, then by name");
  assert.deepEqual(visibleNames(rows, "idle"), ["beta", "zeta"]);
  assert.deepEqual(visibleNames(rows, "held"), []);
});

test("a tab whose every row is filtered out loses its header too (#2371)", () => {
  // The filter runs BEFORE the grouping, so "a tab with no agent rows shows no
  // header" holds for a filtered list by the same rule it holds for a tab with
  // no panes — there is no second mechanism, and no empty header left behind.
  const other: TabRef = { id: "ws-2", title: "docs", index: 1 };
  const rows = [row("idle", { name: "a", tab: WS }), row("working", { name: "b", tab: other })];
  assert.deepEqual(
    visibleGroups(rows, "all", "tab").map((g) => g.tab?.title),
    ["loomux", "docs"],
  );
  // Positive control on the same corpus: unfiltered it IS two groups, so the
  // single group below is the filter's doing and not an empty list.
  assert.deepEqual(
    visibleGroups(rows, "idle", "tab").map((g) => g.tab?.title),
    ["loomux"],
  );
  assert.deepEqual(visibleGroups(rows, "held", "tab"), []);
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

// --- #2371: the agent-type mark, and the group-order control -----------------

test("every launchable CLI in AGENTS gets a mark naming ITSELF", () => {
  // THE POPULATION SHAPE (#1327/#1344): iterate the catalog, never name the
  // CLIs. Naming three of them would pin the icon for the three someone
  // remembered and leave the fourth free to inherit a `harness === "claude" ? …`
  // else-branch — the #722/#841 defect this row exists to avoid.
  //
  // `custom` is excluded because it is the launcher's "type your own" entry and
  // its `command` is the empty string: it names no program, so there is no CLI
  // for a mark to be about.
  const launchable = AGENTS.filter((a) => a.id !== "custom");
  let verified = 0;
  for (const agent of launchable) {
    const view = agentRowMark(row("working", { harness: agent.id }));
    assert.ok(view, `${agent.id} got no mark at all`);
    // The mark NAMES this CLI. Not "some mark came back" — `program` is the
    // resolver's own answer to "which CLI is this", and the failure being
    // guarded is precisely a CLI wearing another's identity.
    assert.equal(view.program, agent.id, `${agent.id}'s mark names ${view.program}`);
    assert.ok(view.svg.length > 0, `${agent.id}'s mark is empty markup`);
    assert.ok(view.label.includes(agent.id), `${agent.id}'s tooltip does not name it`);
    verified += 1;
  }
  // COUNT AT THE VERIFIED SITE, not at the match site (#1327): a loop that
  // `continue`d past a CLI it could not judge would still have walked the
  // catalog. And the floor is a raw read of the container, so a catalog that
  // shrank to one entry cannot make this pass vacuously.
  assert.equal(verified, launchable.length);
  assert.ok(launchable.length >= 4, `AGENTS lists only ${launchable.length} launchable CLIs`);
});

test("a row with no CLI draws nothing, rather than a badge that guesses", () => {
  // `harness: null` is a plain shell, or a pane whose launch line names no
  // program loomux recognises. The resolver's own rule answers `null` — "a row
  // of `?` badges over every terminal is noise dressed as information".
  assert.equal(agentRowMark(row("working", { harness: null })), null);
  // Positive control: the SAME row with a harness DOES get a mark, so the null
  // above is about `harness` and not about `agentRowMark` returning null always.
  assert.ok(agentRowMark(row("working", { harness: "claude" })));
});

test("a transport or shell in the harness field is refused a CLI caption", () => {
  // An SSH profile's `defaultCli` is a declared string, so it can name a shell.
  // The resolver's denylist catches that on every route into it — the row must
  // not caption a pane "Agent CLI: bash".
  for (const notAnAgent of ["bash", "ssh", "pwsh"]) {
    const view = agentRowMark(row("working", { harness: notAnAgent }));
    assert.ok(view, `${notAnAgent} got no view`);
    assert.equal(view.kind, "unknown");
    assert.equal(view.program, null);
    assert.doesNotMatch(view.label, /Agent CLI:/, `${notAnAgent} was captioned as an agent CLI`);
  }
});

test("every group order has a word for it, and the control offers each exactly once", () => {
  // `Record<AgentOrder, string>` is total, so an order added without a label
  // fails to compile. This asserts what the type cannot: no empty label, and a
  // choice list that is the label table's own keys rather than a second list.
  for (const [order, label] of Object.entries(AGENT_ORDER_LABEL)) {
    assert.ok(label.length > 0, `${order} has no label`);
  }
  assert.deepEqual(ORDER_CHOICES, ["state", "tab"]);
  assert.equal(new Set(ORDER_CHOICES).size, ORDER_CHOICES.length, "an order is offered twice");
});

test("a workflow block shows as itself, not as the built-in role it resembles", () => {
  // `orchRole` carries the declared block id for a workflow group (#222), so a
  // `rev-security` agent must read `rev-security` here. Branching on a known
  // role name to produce a label is the #722/#841 defect one channel over.
  assert.equal(agentIdentityLine(row("working", { role: "rev-security" })), "rev-security");
});
