// The Agents tab's presentation decisions (#2122 slice B), separated from the
// DOM that applies them: which filter chips exist, what each says, which rows
// survive the current chip, and what the identity line under a row's name
// reads. `src/agentsview.ts` is the DOM and is hand-validated.

import { test } from "node:test";
import assert from "node:assert/strict";

import { agentMark } from "../src/agenticons.ts";
import { AGENTS } from "../src/agents.ts";
import type { AgentRow, AgentState, TabRef } from "../src/agentrows.ts";
import { sessionCliFromCommand } from "../src/panerestore.ts";
import {
  AGENT_ORDER_LABEL,
  AGENT_STATE_LABEL,
  ORDER_CHOICES,
  agentIdentityLine,
  agentRowMark,
  filterChips,
  listSlots,
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
  mark: LOCAL_SHELL,
  ...over,
});

/** `Pane.agentMarkInput` for a pane launched LOCALLY with `command`, built the
 *  way `Pane` builds it — a local pane has no `knownCli` (that field is the SSH
 *  profile's declared far-end CLI) and is not remote.
 *
 *  THE FIXTURE GOES THROUGH THE PRODUCTION DERIVATION ON PURPOSE (#2371 review
 *  round 2, W2). The first version of the icon tests handed `agentRowMark` a
 *  `harness` naming each CLI directly — a value `Pane.facts()` cannot produce
 *  for four of them — so the specimen had left the class it witnesses and the
 *  population control certified coverage production did not deliver. Building
 *  the fixture from a launch COMMAND is what makes these assertions about panes
 *  that can exist. */
const localPane = (command: string | null): AgentRow["mark"] => ({
  command,
  argv: null,
  knownCli: null,
  remote: false,
});

/** A plain shell: a launch line naming no agent. Draws no mark. */
const LOCAL_SHELL = localPane("bash");

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

// --- #2371 review round 2: the render sequence, as data ----------------------

/** `listSlots` read as `["#tab-title", "row-name", …]` — headers marked with a
 *  leading `#` so the interleaving is legible in one line. */
const slotShape = (rows: readonly AgentRow[], order: Parameters<typeof visibleGroups>[2]): string[] =>
  listSlots(visibleGroups(rows, "all", order)).map((s) => (s.kind === "header" ? `#${s.title}` : s.row.name));

const DOCS: TabRef = { id: "ws-2", title: "docs", index: 1 };

test("the render sequence interleaves each header with its own rows", () => {
  const rows = [
    row("working", { name: "a", tab: WS }),
    row("attention", { name: "b", tab: DOCS }),
    row("idle", { name: "c", tab: WS }),
  ];
  // `state` order puts docs first (its worst row is `attention`); `tab` order
  // puts loomux first (strip index 0). The two DIVERGE, so neither assertion
  // could pass under the other's implementation.
  assert.deepEqual(slotShape(rows, "state"), ["#docs", "b", "#loomux", "a", "c"]);
  assert.deepEqual(slotShape(rows, "tab"), ["#loomux", "a", "c", "#docs", "b"]);
});

test("a header's key is its TAB id and a row's key is its PANE key, in that shape", () => {
  // The two kinds live in separate maps in the view, so this pins that the
  // projection hands each the key its own map is keyed by. A header keyed on a
  // title, or a row keyed on anything but `PaneFacts.key`, is the reconcile bug
  // the view cannot be unit-tested for directly.
  const r = row("working", { name: "a", tab: WS, key: "pane-77" });
  assert.deepEqual(listSlots(visibleGroups([r], "all", "tab")), [
    { kind: "header", key: "ws-1", title: "loomux" },
    { kind: "row", key: "pane-77", row: r },
  ]);
});

test("a tab and a pane may share a string without colliding", () => {
  // Two maps, so a workspace named `x` and a pane keyed `x` are different
  // entries. Pinned because a single-map implementation would pass every other
  // assertion in this file.
  const twin: TabRef = { id: "x", title: "x", index: 0 };
  const slots = listSlots(visibleGroups([row("working", { name: "a", key: "x", tab: twin })], "all", "tab"));
  assert.deepEqual(slots.map((s) => [s.kind, s.key]), [["header", "x"], ["row", "x"]]);
});

test("the headerless group contributes rows and no header", () => {
  const rows = [row("working", { name: "u", tab: null }), row("working", { name: "a", tab: WS })];
  assert.deepEqual(slotShape(rows, "tab"), ["#loomux", "a", "u"]);
  // Positive control: give that row a tab and a header DOES appear, so the
  // absence above is about `tab: null` and not about the projection dropping
  // headers generally.
  const withTab = [row("working", { name: "u", tab: DOCS }), row("working", { name: "a", tab: WS })];
  assert.deepEqual(slotShape(withTab, "tab"), ["#loomux", "a", "#docs", "u"]);
});

test("a pane moving between tabs on the same tick a tab is renamed lands in one sequence", () => {
  // The premortem case from #2371 review round 2: header and row lifetimes skew
  // on one tick — a pane moves tabs, its old tab is renamed, and a third tab
  // loses its last row. Every unit test in this file could pass while the view
  // rendered this wrong, which is why the SEQUENCE is now data.
  const before = [
    row("working", { name: "mover", key: "p1", tab: WS }),
    row("working", { name: "stayer", key: "p2", tab: WS }),
    row("working", { name: "lonely", key: "p3", tab: DOCS }),
  ];
  assert.deepEqual(slotShape(before, "tab"), ["#loomux", "mover", "stayer", "#docs", "lonely"]);
  // Next tick: `mover` moved to docs, loomux was renamed, and `lonely` closed.
  const renamed: TabRef = { ...WS, title: "loomux (renamed)" };
  const after = [
    row("working", { name: "mover", key: "p1", tab: DOCS }),
    row("working", { name: "stayer", key: "p2", tab: renamed }),
  ];
  assert.deepEqual(slotShape(after, "tab"), ["#loomux (renamed)", "stayer", "#docs", "mover"]);
  // The header for loomux keeps its KEY across the rename — that is what lets
  // the view re-label the element instead of destroying and rebuilding it, and
  // it is the half a title-keyed map would get wrong while still rendering the
  // right words.
  const keys = listSlots(visibleGroups(after, "all", "tab"))
    .filter((s) => s.kind === "header")
    .map((s) => s.key);
  assert.deepEqual(keys, ["ws-1", "ws-2"]);
});

// --- #2371: the agent-type mark, and the group-order control -----------------

test("every launchable CLI in AGENTS gets a mark naming ITSELF, launched the way a pane launches it", () => {
  // THE POPULATION SHAPE (#1327/#1344): iterate the catalog, never name the
  // CLIs. Naming three of them would pin the icon for the three someone
  // remembered and leave the fourth free to inherit a `=== "claude" ? …`
  // else-branch — the #722/#841 defect this row exists to avoid.
  //
  // The fixture is built from each entry's own `command`, through `localPane`,
  // which is `Pane.agentMarkInput` for a local pane. That is the load-bearing
  // half (#2371 review round 2, W2): the previous version handed the resolver a
  // `harness` naming each CLI, and for `codex`, `gemini`, `hermes` and `ante`
  // that is a value `Pane.facts()` cannot produce — so it passed while those
  // four drew nothing in the real app.
  //
  // `custom` is excluded because it is the launcher's "type your own" entry and
  // its `command` is the empty string: it names no program, so there is no CLI
  // for a mark to be about.
  const launchable = AGENTS.filter((a) => a.id !== "custom");
  let verified = 0;
  for (const agent of launchable) {
    const view = agentRowMark(row("working", { mark: localPane(agent.command) }));
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
  // The catalog is WIDER than the session-store CLI set, which is the fact the
  // old fixture hid. Asserted rather than assumed, so this test keeps meaning
  // its point if `sessionCliFromCommand` ever widens to cover the catalog.
  const storeCovered = launchable.filter((a) => sessionCliFromCommand(a.command) !== null);
  assert.ok(
    storeCovered.length < launchable.length,
    "AGENTS is no longer wider than the session-store CLI set — this test's whole point was that reading `harness` " +
      "for the icon silently dropped the difference; re-check that before relaxing it",
  );
});

test("the row's mark and the pane header's mark are one resolution, not two that agree", () => {
  // #2371 review round 2, W1. The header calls `agentMark(Pane.agentMarkInput)`
  // and the row calls `agentMark(row.mark)`, where `row.mark` IS that same
  // object — so the check here is that the row's answer equals the answer
  // `agentMark` gives the header's input, for every launchable CLI.
  //
  // The specimen that made this necessary is `codex`: covered by no session
  // store, so the old `harness` route answered null for it while the header
  // drew a letter badge. It is asserted BY NAME here, and only here, because a
  // regression test for a specific divergence should name the specimen that
  // diverged; the population sweep above is the general guard.
  for (const agent of AGENTS.filter((a) => a.id !== "custom")) {
    const input = localPane(agent.command);
    assert.deepEqual(agentRowMark(row("working", { mark: input })), agentMark(input), agent.id);
  }
  const codex = agentRowMark(row("working", { mark: localPane("codex") }));
  assert.equal(codex?.program, "codex", "the specimen W1 was found on draws nothing again");
  assert.equal(sessionCliFromCommand("codex"), null, "codex is still outside the session-store set");
});

test("a row with no launch line draws nothing, rather than a badge that guesses", () => {
  // No command and no argv is a pane loomux cannot name at all. The resolver's
  // own rule answers `null` — "a row of `?` badges over every terminal is noise
  // dressed as information".
  assert.equal(agentRowMark(row("working", { mark: localPane(null) })), null);
  // Positive control: the SAME row with a launch line DOES get a mark, so the
  // null above is about the absent command and not about `agentRowMark`
  // returning null always.
  assert.ok(agentRowMark(row("working", { mark: localPane("claude") })));
});

test("a transport or shell is refused a CLI caption", () => {
  // A pane launched with a shell, and an #887 SSH pane whose launch line is the
  // local ssh client. Neither may be captioned "Agent CLI: bash".
  for (const notAnAgent of ["bash", "ssh", "pwsh"]) {
    const view = agentRowMark(row("working", { mark: localPane(notAnAgent) }));
    assert.ok(view, `${notAnAgent} got no view`);
    assert.equal(view.kind, "unknown");
    assert.equal(view.program, null);
    assert.doesNotMatch(view.label, /Agent CLI:/, `${notAnAgent} was captioned as an agent CLI`);
  }
});

test("a remote pane and its local twin draw the same mark for the same CLI", () => {
  // The premortem case from #2371 review round 2: an SSH profile declaring
  // `defaultCli: "codex"` set `harness: "codex"`, so the REMOTE pane drew a
  // mark while its local twin drew none — the same CLI answering two ways
  // depending on where it ran. Sharing `Pane.agentMarkInput` closes it.
  const remote = { command: "ssh host", argv: null, knownCli: "codex", remote: true };
  assert.equal(agentRowMark(row("working", { mark: remote }))?.program, "codex");
  assert.equal(agentRowMark(row("working", { mark: localPane("codex") }))?.program, "codex");
  // And a remote pane whose profile declares NOTHING gets the neutral badge —
  // not null, and not the transport's name, which is what `remote` is for.
  const unknown = agentRowMark(row("working", { mark: { command: "ssh host", argv: null, knownCli: null, remote: true } }));
  assert.equal(unknown?.kind, "unknown");
  assert.equal(unknown?.program, null);
  assert.doesNotMatch(unknown?.label ?? "", /ssh/, "the transport's own name was read as the agent");
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
