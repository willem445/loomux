// Unit tests for the pane-facts projection and the agent state ladder
// (src/agentrows.ts, #2122 slice A1/A3). Pure data in, one word out — no DOM,
// no clock, no `Pane`.
//
// The fixtures are built to DISCRIMINATE, not merely to pass (CLAUDE.md #1182):
// `the ladder's fixtures vary every field the predicate reads` below enumerates
// the ten inputs `deriveAgentState` actually consults and fails if the corpus
// happens to hold any one of them constant — a field every fixture shares is an
// unpinned axis, and the suite would stay green over a rung that stopped
// reading it.
//
// Red arm (mechanically): reorder any two rungs in `deriveAgentState` and
// `the ladder walks down in precedence order` reddens on that pair; drop the
// `!facts.welcome` term from the dead rung and `a welcome pane is not dead`
// reddens alone; make the orch idle rung ignore `bytesInWindow` and
// `an orch pane the roster calls idle is still working while it paints`
// reddens alone.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import {
  deriveAgentState,
  groupRows,
  agentRows,
  isAgentPane,
  toAgentRow,
  matchesFilter,
  sortRows,
  needsYouCount,
  type AgentOrder,
  type AgentRow,
  type AgentState,
  type PaneFacts,
  type TabRef,
} from "../src/agentrows.ts";
import { ACTIVITY_FLOOR_BYTES } from "../src/paneactivity.ts";
import { AGENTS, LAUNCHABLE_AGENT_PROGRAMS } from "../src/agents.ts";
import { agentMark, markProgram } from "../src/agenticons.ts";
import { sessionCliFromCommand } from "../src/panerestore.ts";
import { AGENT_STATE_LABEL, emptyMessage } from "../src/agentsviewmodel.ts";

const T0 = 1_000_000;

type FactsPatch = Partial<Omit<PaneFacts, "activity">> & {
  activity?: Partial<PaneFacts["activity"]>;
};

/** A live orchestration worker mid-turn: the `working` default, and the base
 *  every fixture below departs from in exactly the fields its own rung reads. */
function facts(patch: FactsPatch = {}): PaneFacts {
  const { activity, ...rest } = patch;
  return {
    key: "pane-1",
    name: "w-1",
    kind: "orch",
    tab: { id: "ws-1", title: "loomux", index: 0 },
    harness: "claude",
    // The launch line the header resolves its own mark from, carried on the
    // facts so the row and the header share one resolution (#2371 review round
    // 2, W1). Built from a COMMAND, the way `Pane.agentMarkInput` builds it.
    mark: { command: "claude", argv: null, knownCli: null, remote: false },
    orch: { group: "g", agentId: "w-1", role: "worker" },
    sessionId: "s-1",
    alive: true,
    dormant: false,
    welcome: false,
    attention: null,
    held: null,
    ...rest,
    activity: {
      lastOutputMs: T0,
      bytesInWindow: ACTIVITY_FLOOR_BYTES * 2,
      lastHumanInputMs: T0 - 5000,
      atPrompt: false,
      rosterIdle: false,
      ...activity,
    },
  };
}

/** The corpus the ladder is pinned on: one entry per rung, plus the cases that
 *  distinguish a rung from its neighbour. `axes` below reads this. */
const LADDER: { why: string; state: AgentState; facts: PaneFacts }[] = [
  {
    why: "a pane whose process exited, still wearing every stale signal",
    state: "dead",
    facts: facts({
      alive: false,
      held: "human-input",
      attention: { reason: "waiting", detail: null },
      activity: { atPrompt: true, rosterIdle: true, bytesInWindow: 0 },
    }),
  },
  {
    why: "a dormant restore placeholder",
    state: "dormant",
    facts: facts({ alive: false, dormant: true, held: "human-input" }),
  },
  {
    why: "loomux is withholding a delivery to this pane",
    state: "held",
    facts: facts({ held: "human-input", attention: { reason: "blocked", detail: "why" } }),
  },
  {
    why: "an urgent attention reason — wedged, will not un-wedge itself",
    state: "attention",
    facts: facts({ attention: { reason: "blocked", detail: "an error" } }),
  },
  {
    why: "a stranded prompt is urgent too",
    state: "attention",
    facts: facts({ attention: { reason: "stranded", detail: null } }),
  },
  {
    why: "a decision waiting on the human's own pace",
    state: "question",
    facts: facts({ attention: { reason: "gate", detail: "task is review" } }),
  },
  {
    // #2367: a `report` reason means the agent called `report(...)` and is
    // waiting on the ORCHESTRATOR — the header chip already words it
    // "✓ reported". It is not a human decision, so it gets its own rung
    // instead of reading as `question`.
    why: "a report waits on the orchestrator, not on a human decision",
    state: "reported",
    facts: facts({ attention: { reason: "report", detail: null } }),
  },
  {
    why: "the scan says waiting right now",
    state: "turn-done",
    facts: facts({ attention: { reason: "waiting", detail: null } }),
  },
  {
    why: "the scan said waiting and the focus ack cleared the chip — the latch survives",
    state: "turn-done",
    facts: facts({ attention: null, activity: { atPrompt: true } }),
  },
  {
    why: "an orch pane the roster calls idle, painting nothing",
    state: "idle",
    facts: facts({ activity: { rosterIdle: true, bytesInWindow: 0 } }),
  },
  {
    // #2195 review B1 / N1. This is the crossing nothing discriminated before:
    // a non-orchestration pane the human has never typed into, PAINTING. The
    // first draft read the floor on the orch arm only, so this fixture returned
    // `idle` — an autopilot-restored solo agent pane reported idle for its whole
    // working run. The ten-axis pin could not see it: `bytesInWindow` varied
    // across the corpus, but only on the branch that already read it.
    why: "a non-orch pane nobody has prompted is WORKING while it paints",
    state: "working",
    facts: facts({
      kind: "agent",
      orch: null,
      harness: "copilot",
      activity: {
        lastHumanInputMs: null,
        rosterIdle: null,
        bytesInWindow: ACTIVITY_FLOOR_BYTES,
      },
    }),
  },
  {
    why: "a basic pane nobody has ever prompted",
    state: "idle",
    facts: facts({
      kind: "agent",
      orch: null,
      harness: null,
      sessionId: null,
      activity: { lastHumanInputMs: null, rosterIdle: null, lastOutputMs: null, bytesInWindow: 0 },
    }),
  },
  {
    why: "a welcome form is not a dead pane",
    state: "idle",
    facts: facts({
      kind: "terminal",
      alive: false,
      welcome: true,
      orch: null,
      harness: null,
      sessionId: null,
      activity: { lastHumanInputMs: null, rosterIdle: null, lastOutputMs: null, bytesInWindow: 0 },
    }),
  },
  {
    // A content pane (files / editor / git / workflow) has no PTY BY DESIGN and
    // is live the moment it exists — `tabPaneInfo()` says so explicitly.
    //
    // Scope, stated because it is narrower than it looks: this pins what the
    // LADDER does with such a pane. It cannot pin how `Pane.facts()` DERIVES
    // `alive`, which is DOM code with no test file — the first draft derived it
    // as `ptyId !== null && !exited` and called every content pane dead, and no
    // literal-fed test here would have reddened. What keeps that out is
    // structural rather than a fixture: `facts()` now takes `alive` and `kind`
    // from ONE `tabPaneInfo()` reading, so there is no second rule to drift.
    why: "a content pane has no PTY and is not dead",
    state: "idle",
    facts: facts({
      kind: "files",
      alive: true,
      orch: null,
      harness: null,
      sessionId: null,
      activity: { lastHumanInputMs: null, rosterIdle: null, lastOutputMs: null, bytesInWindow: 0 },
    }),
  },
  {
    why: "no evidence of a prompt — the honest default",
    state: "working",
    facts: facts(),
  },
];

for (const row of LADDER) {
  test(`the ladder reads ${row.state}: ${row.why}`, () => {
    assert.equal(deriveAgentState(row.facts), row.state);
  });
}

test("the ladder's fixtures vary every field the predicate reads", () => {
  // The ten inputs `deriveAgentState` consults, each as a reading off one
  // fixture. A field with only one distinct value across the whole corpus is an
  // axis nothing pins: the ladder could stop reading it and every case above
  // would still pass. #1182 is that failure exactly, so it is asserted rather
  // than assumed — and this list is maintained BY HAND against the function,
  // which is the point: adding a rung that reads an eleventh field means adding
  // it here and finding out whether the corpus discriminates it.
  const AXES: { name: string; read: (f: PaneFacts) => unknown }[] = [
    { name: "alive", read: (f) => f.alive },
    { name: "dormant", read: (f) => f.dormant },
    { name: "welcome", read: (f) => f.welcome },
    { name: "held", read: (f) => f.held },
    { name: "attention.reason", read: (f) => f.attention?.reason ?? null },
    { name: "orch", read: (f) => f.orch === null },
    { name: "activity.atPrompt", read: (f) => f.activity.atPrompt },
    { name: "activity.rosterIdle", read: (f) => f.activity.rosterIdle },
    { name: "activity.bytesInWindow", read: (f) => f.activity.bytesInWindow },
    { name: "activity.lastHumanInputMs", read: (f) => f.activity.lastHumanInputMs },
  ];
  for (const axis of AXES) {
    const seen = new Set(LADDER.map((r) => JSON.stringify(axis.read(r.facts) ?? null)));
    assert.ok(
      seen.size > 1,
      `every ladder fixture shares one value for ${axis.name} (${[...seen].join(", ")}) — that axis is unpinned`,
    );
  }
  // And the corpus must actually reach every rung, or a state is untested while
  // the axis check above still passes.
  const reached = new Set(LADDER.map((r) => r.state));
  assert.deepEqual(
    [...reached].sort(),
    ["attention", "dead", "dormant", "held", "idle", "question", "reported", "turn-done", "working"],
    "the ladder corpus no longer covers every AgentState",
  );
});

test("the ladder walks down in precedence order", () => {
  // One fixture carrying EVERY signal at once, peeled one rung at a time. Each
  // step keeps every lower-precedence signal set, so a step passing is evidence
  // that the rung named really outranks the ones below it — not that the lower
  // ones happened to be absent.
  const every: FactsPatch = {
    held: "human-input",
    attention: { reason: "blocked", detail: "d" },
    activity: { atPrompt: true, rosterIdle: true, bytesInWindow: 0, lastHumanInputMs: null },
  };
  const walk: { patch: FactsPatch; state: AgentState }[] = [
    { patch: { ...every, alive: false }, state: "dead" },
    { patch: { ...every, alive: false, dormant: true }, state: "dormant" },
    { patch: { ...every }, state: "held" },
    { patch: { ...every, held: null }, state: "attention" },
    { patch: { ...every, held: null, attention: { reason: "gate", detail: null } }, state: "question" },
    // #2367: the reported rung sits between question and turn-done — the agent
    // has called in, but nothing is waiting on the human.
    { patch: { ...every, held: null, attention: { reason: "report", detail: null } }, state: "reported" },
    { patch: { ...every, held: null, attention: { reason: "waiting", detail: null } }, state: "turn-done" },
    { patch: { ...every, held: null, attention: null }, state: "turn-done" },
    {
      patch: { ...every, held: null, attention: null, activity: { ...every.activity, atPrompt: false } },
      state: "idle",
    },
    {
      patch: {
        ...every,
        held: null,
        attention: null,
        activity: { ...every.activity, atPrompt: false, rosterIdle: false },
      },
      state: "working",
    },
  ];
  for (const step of walk) {
    assert.equal(deriveAgentState(facts(step.patch)), step.state, JSON.stringify(step.patch));
  }
});

test("a welcome pane is not dead", () => {
  // Both are "no PTY". Only one of them is a failure.
  const welcome = facts({ alive: false, welcome: true });
  assert.notEqual(deriveAgentState(welcome), "dead");
  // Positive control: the SAME fixture without the welcome flag is dead, so the
  // assertion above is about `welcome` and not about some other field.
  assert.equal(deriveAgentState(facts({ alive: false })), "dead");
});

test("an orch pane the roster calls idle is still working while it paints", () => {
  // `idle_since_ms` means "the reaper would call this idle" (#2089), which is a
  // claim about assignments, not about the terminal. A pane genuinely painting
  // output is working whatever the roster thinks.
  const painting = facts({ activity: { rosterIdle: true, bytesInWindow: ACTIVITY_FLOOR_BYTES } });
  assert.equal(deriveAgentState(painting), "working");
  // The floor is the line, so one byte under it reads the other way.
  const quiet = facts({ activity: { rosterIdle: true, bytesInWindow: ACTIVITY_FLOOR_BYTES - 1 } });
  assert.equal(deriveAgentState(quiet), "idle");
});

test("the floor guard applies to BOTH idle branches, not just the orch one", () => {
  // #2195 review B1. The four crossings of {orch, non-orch} x {painting, quiet},
  // asserted together so neither arm can lose the floor on its own. Building
  // them from one base means the reading is about those two axes and nothing
  // else — the disjoint-literal failure #1300 names is what this avoids.
  const at = (orch: PaneFacts["orch"], bytes: number): AgentState =>
    deriveAgentState(
      facts({
        orch,
        activity: {
          bytesInWindow: bytes,
          // Both idleness signals say "idle" on both arms, so the ONLY thing
          // that can move the answer below is the floor.
          rosterIdle: true,
          lastHumanInputMs: null,
        },
      }),
    );
  const ORCH = { group: "g", agentId: "w-1", role: "worker" };
  const QUIET = ACTIVITY_FLOOR_BYTES - 1;
  const PAINTING = ACTIVITY_FLOOR_BYTES;
  assert.equal(at(ORCH, QUIET), "idle");
  assert.equal(at(null, QUIET), "idle");
  assert.equal(at(ORCH, PAINTING), "working");
  assert.equal(at(null, PAINTING), "working", "the non-orch arm must read the floor too (B1)");
});

test("a basic pane that HAS been prompted is working, not idle", () => {
  // The roster covers no basic pane, so "never prompted" is the only idleness
  // evidence available for one — and it is a one-way door.
  const prompted = facts({ orch: null, activity: { lastHumanInputMs: T0, rosterIdle: null } });
  assert.equal(deriveAgentState(prompted), "working");
});

test("an orch pane is never judged idle by the basic pane's rule", () => {
  // An orchestration pane nobody has typed into is the NORMAL case — the
  // orchestrator spawns it and the agent works unattended. Reading the basic
  // rule on it would report every unattended worker as idle.
  const unattended = facts({ activity: { lastHumanInputMs: null, rosterIdle: false } });
  assert.equal(deriveAgentState(unattended), "working");
});

test("an unknown attention reason does not fall into the urgent or question rungs", () => {
  // A reason the backend adds tomorrow must not silently read as urgent (it
  // would jump the badge) nor be swallowed — it falls through to whatever the
  // pane's own activity says, which here is working.
  assert.equal(deriveAgentState(facts({ attention: { reason: "brand-new-reason", detail: null } })), "working");
});

// --- the row projection -----------------------------------------------------

test("toAgentRow carries the identity fields through and derives the state", () => {
  const row = toAgentRow(facts({ name: "reviewer", attention: { reason: "gate", detail: "d" } }), 3);
  assert.deepEqual(row, {
    key: "pane-1",
    name: "reviewer",
    harness: "claude",
    group: "g",
    agentId: "w-1",
    role: "worker",
    state: "question",
    notes: 3,
    tab: { id: "ws-1", title: "loomux", index: 0 },
    mark: { command: "claude", argv: null, knownCli: null, remote: false },
  });
});

test("the mark input is carried onto the row untouched (#2371 review W1)", () => {
  // The row must hand the resolver exactly what the pane header hands it —
  // untouched, not re-derived — or the two surfaces can answer differently
  // about one pane. `harness` is deliberately NOT it: a `codex` pane is a real
  // agent pane that no session store covers, so `harness` is null while its
  // launch line names the program perfectly well.
  const codex = facts({
    harness: null,
    mark: { command: "codex --resume", argv: null, knownCli: null, remote: false },
  });
  const projected = toAgentRow(codex);
  assert.deepEqual(projected.mark, codex.mark);
  assert.equal(projected.harness, null, "a codex pane is outside the session-store set, and that is correct");
  assert.notEqual(projected.mark.command, null, "…while its launch line still names the program");
});

test("the tab a reading named is carried onto the row, and so is its absence", () => {
  // #2371. `PaneFacts.tab` is supplied by the caller (a `Pane` does not know
  // its workspace), so both halves are real cases: the Agents view names one,
  // and the notes rows / focus walk name none.
  const named = toAgentRow(facts({ tab: { id: "ws-9", title: "docs", index: 4 } }));
  assert.deepEqual(named.tab, { id: "ws-9", title: "docs", index: 4 });
  assert.equal(toAgentRow(facts({ tab: null })).tab, null);
});

test("a pane with no orchestration identity flattens to nulls, not to undefined", () => {
  const row = toAgentRow(facts({ orch: null, harness: null }));
  assert.equal(row.group, null);
  assert.equal(row.agentId, null);
  assert.equal(row.role, null);
  assert.equal(row.harness, null);
  assert.equal(row.notes, null, "notes default to 'not loaded', which is not 0");
});

function row(name: string, state: AgentState, tab: TabRef | null = null): AgentRow {
  return {
    key: `k-${name}`,
    name,
    harness: "claude",
    group: "g",
    agentId: name,
    role: "worker",
    state,
    notes: null,
    tab,
    mark: { command: "claude", argv: null, knownCli: null, remote: false },
  };
}

test("matchesFilter passes everything on 'all' and exactly one state otherwise", () => {
  const working = row("a", "working");
  const idle = row("b", "idle");
  assert.equal(matchesFilter(working, "all"), true);
  assert.equal(matchesFilter(idle, "all"), true);
  assert.equal(matchesFilter(working, "working"), true);
  assert.equal(matchesFilter(idle, "working"), false);
});

test("sortRows orders by state urgency, then by name, without mutating its input", () => {
  const input: AgentRow[] = [
    row("zeta", "working"),
    row("alpha", "working"),
    row("beta", "attention"),
    row("gamma", "dead"),
    row("delta", "turn-done"),
    row("epsilon", "question"),
    // #2367: pins the reported rung's position in STATE_ORDER — after
    // question, before turn-done.
    row("kappa", "reported"),
  ];
  const before = input.map((r) => r.name);
  const out = sortRows(input);
  assert.deepEqual(
    out.map((r) => r.name),
    ["beta", "epsilon", "kappa", "delta", "alpha", "zeta", "gamma"],
  );
  assert.deepEqual(input.map((r) => r.name), before, "sortRows must not reorder the caller's array");
});

test("needsYouCount counts the two states a person must act on", () => {
  const rows = [
    row("a", "attention"),
    row("b", "question"),
    // #2367: a reported pane waits on the ORCHESTRATOR, not on the human —
    // it must not raise the badge.
    row("i", "reported"),
    row("c", "held"),
    row("d", "turn-done"),
    row("e", "working"),
    row("f", "idle"),
    row("g", "dormant"),
    row("h", "dead"),
  ];
  // Non-vacuous by construction: the corpus holds one row per state, so a
  // predicate that counted nothing (or everything) fails here rather than
  // passing on an empty list.
  assert.equal(rows.length, 9);
  assert.equal(needsYouCount(rows), 2);
  // The positive control for the exclusion above: a question row DOES count,
  // so the assertion is about `reported` and not about some other field.
  assert.equal(needsYouCount([row("i", "reported"), row("b", "question")]), 1);
  assert.equal(needsYouCount([]), 0);
  assert.equal(needsYouCount([row("a", "working")]), 0);
});

// --- #2371: grouping by tab ---------------------------------------------------

// THE STRIP ORDER IS DELIBERATELY NOT THE ALPHABETICAL ONE. Every fixture below
// draws from this table, whose titles sort into the exact REVERSE of the strip
// positions the human dragged them into — so a `groupRows` that sorted by title
// (or that fell back to arrival order after a shuffled input) produces a
// different answer from the correct one on every one of these tests, rather than
// only on the one written to catch it.
const WS: Record<string, TabRef> = {
  // strip order: zulu, mike, alpha.  alphabetical: alpha, mike, zulu.
  zulu: { id: "ws-z", title: "zulu", index: 0 },
  mike: { id: "ws-m", title: "mike", index: 1 },
  alpha: { id: "ws-a", title: "alpha", index: 2 },
};

/** Read a grouping as `[[tab title | null, ...row names]]` — the shape every
 *  assertion below is written against. */
const shape = (rows: readonly AgentRow[], order: AgentOrder): (string | null)[][] =>
  groupRows(rows, order).map((g) => [g.tab?.title ?? null, ...g.rows.map((r) => r.name)]);

test("the group order follows the tab strip, not the alphabet", () => {
  // The negative control is the whole test: `WS` is built so the two orders
  // DISAGREE, and the wrong answer is spelled out so the assertion cannot be
  // read as "some order came back".
  const rows = [
    row("a1", "working", WS.alpha),
    row("m1", "working", WS.mike),
    row("z1", "working", WS.zulu),
  ];
  const byStrip = [["zulu", "z1"], ["mike", "m1"], ["alpha", "a1"]];
  const byAlphabet = [["alpha", "a1"], ["mike", "m1"], ["zulu", "z1"]];
  assert.deepEqual(shape(rows, "tab"), byStrip);
  assert.notDeepEqual(
    shape(rows, "tab"),
    byAlphabet,
    "sorting groups by title would pass every other assertion here",
  );
  // And it is the STRIP position that decides, not the order the rows arrived
  // in: the same three rows fed in a third arrangement land the same way.
  assert.deepEqual(shape([rows[2], rows[0], rows[1]], "tab"), byStrip);
});

test("a tab with no agent rows produces no group", () => {
  // Two of the three tabs in `WS` have no rows here. They are absent from the
  // result rather than present-and-empty, which is what makes "a tab with no
  // agent rows shows no header" true of the view without the view filtering
  // anything.
  const groups = groupRows([row("m1", "working", WS.mike)], "tab");
  assert.deepEqual(groups.map((g) => g.tab?.id), ["ws-m"]);
  // Non-vacuous: the same call over rows from all three tabs DOES return three,
  // so the assertion above is about the missing rows and not about `groupRows`
  // returning a short list whatever it is fed.
  const all = groupRows(
    [row("m1", "working", WS.mike), row("z1", "working", WS.zulu), row("a1", "working", WS.alpha)],
    "tab",
  );
  assert.equal(all.length, 3);
});

test("the state sort still applies inside a group, in both orders", () => {
  // Rows are handed in deliberately mis-ordered within each tab. `sortRows`'
  // own rule — state urgency, then name — must survive grouping unchanged,
  // whichever GROUP order is selected: `order` decides which tab you read
  // first and nothing else.
  const rows = [
    row("zoe", "working", WS.mike),
    row("amy", "attention", WS.mike),
    row("bob", "working", WS.mike),
    row("carl", "question", WS.zulu),
    row("dan", "dead", WS.zulu),
  ];
  const inMike = ["amy", "bob", "zoe"];
  const inZulu = ["carl", "dan"];
  assert.deepEqual(shape(rows, "tab"), [["zulu", ...inZulu], ["mike", ...inMike]]);
  // `state` order puts mike first (its worst row is `attention`, zulu's is
  // `question`) — and the rows INSIDE each group are identical to the line
  // above, which is the property this test exists for.
  assert.deepEqual(shape(rows, "state"), [["mike", ...inMike], ["zulu", ...inZulu]]);
});

test("under 'most wants you' the group holding the most urgent row comes first", () => {
  // The reading that makes `state` a group order rather than an accident. Strip
  // order here is zulu, mike, alpha — and the answer inverts it completely, so
  // a `groupRows` that ignored `order` and always sorted by strip fails.
  const rows = [
    row("z1", "idle", WS.zulu),
    row("m1", "working", WS.mike),
    row("a1", "attention", WS.alpha),
  ];
  assert.deepEqual(shape(rows, "state"), [["alpha", "a1"], ["mike", "m1"], ["zulu", "z1"]]);
  assert.deepEqual(shape(rows, "tab"), [["zulu", "z1"], ["mike", "m1"], ["alpha", "a1"]]);
});

test("groups whose worst row ties fall back to strip order, never to arrival order", () => {
  // Two tabs whose most urgent row is the same state. Fed alpha-first, so a
  // tie-break that kept arrival order would put alpha first — and the human's
  // own arrangement says zulu.
  const rows = [row("a1", "working", WS.alpha), row("z1", "working", WS.zulu)];
  assert.deepEqual(shape(rows, "state"), [["zulu", "z1"], ["alpha", "a1"]]);
});

test("rows naming no tab share one headerless group, which sorts last only where it has no position", () => {
  // `PaneFacts.tab` is null for any reading that did not name one. Such rows are
  // still listed — dropping them would hide panes — but they carry no title, so
  // the view renders no header.
  //
  // The two orders answer differently ON PURPOSE, and the fixture is built so
  // they DIVERGE rather than agreeing under both readings:
  //  - `tab` is strip order, and a group with no tab has no strip position, so
  //    it goes last;
  //  - `state` ranks by the worst row, and a headerless group holding a wedged
  //    pane is still a wedged pane — burying it under a tab whose worst row is
  //    `idle` would hide urgency for tidiness.
  const rows = [row("untabbed", "attention", null), row("m1", "idle", WS.mike)];
  assert.deepEqual(shape(rows, "state"), [[null, "untabbed"], ["mike", "m1"]]);
  assert.deepEqual(shape(rows, "tab"), [["mike", "m1"], [null, "untabbed"]]);
  assert.notDeepEqual(
    shape(rows, "state"),
    shape(rows, "tab"),
    "the fixture must distinguish the two orders, or neither is pinned",
  );
  // The tie-break half: with the states equal, `state` has nothing to rank on
  // and falls through to the same answer `tab` gives.
  const tied = [row("untabbed", "idle", null), row("m1", "idle", WS.mike)];
  assert.deepEqual(shape(tied, "state"), [["mike", "m1"], [null, "untabbed"]]);
});

test("renaming a tab re-labels its group without splitting it", () => {
  // A rename changes `title` and not `id`, and the group is keyed on `id`. So
  // the two rows stay in ONE group wearing the new name — the failure this
  // guards is a title-keyed grouping, which would show two headers for one tab
  // during the tick where a rename has reached one pane's reading and not the
  // other's.
  const before = [row("m1", "working", WS.mike), row("m2", "working", WS.mike)];
  assert.deepEqual(shape(before, "tab"), [["mike", "m1", "m2"]]);
  const renamed: TabRef = { ...WS.mike, title: "MIKE renamed" };
  const after = [row("m1", "working", renamed), row("m2", "working", renamed)];
  assert.deepEqual(shape(after, "tab"), [["MIKE renamed", "m1", "m2"]]);
  // A hypothetical tick where two rows of one tab carry DIFFERENT titles. Still
  // ONE group — that is the property this test is for, and it holds whichever
  // label wins.
  //
  // WHICH label wins is LAST IN INPUT ORDER, and nothing here is "fresher"
  // (#2371 review round 2, R2 — an earlier version of this comment claimed the
  // group "wears the fresher label", which the code cannot do: there is no
  // timestamp on a `TabRef`, so the tie is decided by argument order and this
  // fixture merely happens to put the renamed reading last). Both orders are
  // asserted, so the rule is pinned rather than illustrated by one lucky
  // arrangement.
  const midway = [row("m1", "working", WS.mike), row("m2", "working", renamed)];
  assert.deepEqual(shape(midway, "tab"), [["MIKE renamed", "m1", "m2"]]);
  const reversed = [row("m1", "working", renamed), row("m2", "working", WS.mike)];
  assert.deepEqual(
    shape(reversed, "tab"),
    [["mike", "m1", "m2"]],
    "the label is the LAST reading in input order — reverse the input and the other title wins",
  );
  // In production this case cannot arise at all: `main.ts`'s walk reads one
  // title per tab and hands the same `TabRef` to every pane in it. The rule is
  // stated so the tie is decided somewhere rather than by bucket-insertion
  // luck, not because a caller is expected to produce a split reading.
});

test("two tabs sharing a name stay two groups", () => {
  // The other half of keying on `id`: the human may legally name two tabs the
  // same thing, and merging them would hide a pane's real home.
  const twin: TabRef = { id: "ws-a", title: "mike", index: 2 };
  const rows = [row("m1", "working", WS.mike), row("a1", "working", twin)];
  const groups = groupRows(rows, "tab");
  assert.deepEqual(groups.map((g) => g.tab?.id), ["ws-m", "ws-a"]);
  assert.deepEqual(groups.map((g) => g.tab?.title), ["mike", "mike"]);
});

test("groupRows returns fresh arrays and does not reorder its input", () => {
  const rows = [row("zoe", "working", WS.mike), row("amy", "attention", WS.mike)];
  const before = rows.map((r) => r.name);
  const groups = groupRows(rows, "tab");
  assert.deepEqual(groups[0].rows.map((r) => r.name), ["amy", "zoe"]);
  assert.deepEqual(rows.map((r) => r.name), before, "groupRows must not reorder the caller's array");
});

test("the needs-you badge is unchanged by grouping, in either order", () => {
  // The badge counts LADDER states over the whole window (`agentsview.ts`
  // derives it from the ungrouped rows, before any grouping runs). This pins
  // that grouping is a pure re-arrangement: the same rows, redistributed.
  const rows = [
    row("a1", "attention", WS.alpha),
    row("a2", "question", WS.alpha),
    row("m1", "working", WS.mike),
    row("m2", "question", WS.mike),
    row("z1", "held", WS.zulu),
    row("u1", "reported", null),
  ];
  const flat = needsYouCount(rows);
  // Positive control for the count itself: it is 3 and not 0, so the equalities
  // below are comparing a real number rather than agreeing on nothing. A
  // `question` row DOES count — the corpus holds two, plus one `attention`.
  assert.equal(flat, 3);
  assert.equal(needsYouCount([row("q", "question", WS.mike)]), 1, "a question row counts");
  for (const order of ["state", "tab"] as AgentOrder[]) {
    const groups = groupRows(rows, order);
    const regrouped = groups.reduce((n, g) => n + needsYouCount(g.rows), 0);
    assert.equal(regrouped, flat, `grouping in ${order} order changed the badge`);
    // And no row was dropped or duplicated on the way, which is the property
    // the count alone cannot see: a lost `working` row would leave it at 3.
    assert.deepEqual(
      groups.flatMap((g) => g.rows.map((r) => r.key)).sort(),
      rows.map((r) => r.key).sort(),
      `grouping in ${order} order did not preserve the row set`,
    );
  }
});

// --- membership: which panes are agent panes at all (#2514) ------------------
//
// Red arm (mechanically): make `isAgentPane` return true unconditionally and
// the three "is not an agent row" tests redden together; drop its catalog arm
// and `the three launchable CLIs no session store covers` reddens alone; drop
// its `harness` arm and the issue's own positive control reddens alone; make
// `agentRows` skip the filter and the badge test reddens.

/** A plain terminal the human opened and typed into: no launch line, no
 *  orchestration identity, no session-store CLI — and painting above the floor,
 *  which is exactly what made the ladder call it `working` (#2514). */
function shell(patch: FactsPatch = {}): PaneFacts {
  return facts({
    key: "pane-shell",
    name: "bash",
    kind: "terminal",
    harness: null,
    orch: null,
    sessionId: null,
    mark: { command: null, argv: null, knownCli: null, remote: false },
    ...patch,
  });
}

test("a plain shell the human has typed into is not an agent row (#2514)", () => {
  const pane = shell();
  // The reported bug, stated as the two facts that make it: the ladder is asked
  // and answers `working` — correctly, about a question it was never the right
  // one to ask. Pinning the ladder's answer here is the POSITIVE CONTROL for
  // the exclusion below: without it, "not a row" would pass just as well on a
  // fixture the ladder had stopped reading at all.
  assert.equal(pane.activity.lastHumanInputMs !== null, true, "the human has typed into it");
  assert.ok(pane.activity.bytesInWindow >= ACTIVITY_FLOOR_BYTES, "and it is painting");
  assert.equal(deriveAgentState(pane), "working", "the ladder still calls it working, and always did");
  assert.equal(isAgentPane(pane), false, "…but it is not an agent pane, so nothing asks the ladder");
  assert.deepEqual(agentRows([pane]), [], "and it never becomes a row");
});

test("the same facts with a harness ARE an agent row, still working (#2514)", () => {
  // The issue's own positive control, and it changes exactly ONE field: if the
  // exclusion above were coming from something else in the fixture — the kind,
  // the null session id, the absent launch line — this would still be empty.
  const pane = shell({ harness: "claude" });
  assert.equal(isAgentPane(pane), true);
  const rows = agentRows([pane]);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].state, "working");
});

test("a content pane is not an agent row (#2514)", () => {
  // Files, editor, git and workflow panes are `alive` BY DESIGN — they have no
  // PTY at all — so every rung above `working` declines and the default rung
  // caught them too. `kind` is not what excludes them: the three membership
  // arms are, and a content pane satisfies none. That is why the loop varies
  // the kind and asserts the same answer rather than pretending it is read.
  for (const kind of ["files", "editor", "git", "workflow"]) {
    assert.equal(isAgentPane(shell({ kind, name: kind })), false, `a ${kind} pane is not an agent`);
  }
});

test("an orchestration pane with no harness is still an agent row (#2514)", () => {
  // A manager arrives through the repo's workflow file and may carry no CLI
  // loomux launched it with; the group is the evidence.
  const pane = shell({ name: "mgr", orch: { group: "g", agentId: null, role: "manager" } });
  assert.equal(pane.harness, null);
  assert.equal(isAgentPane(pane), true);
});

test("an SSH pane whose profile declares the far-end CLI is an agent row (#2514)", () => {
  // Production shape: `Pane.facts()` sets `harness` to `agentCli ??
  // sshDefaultCli`, so a declared far-end CLI lands on BOTH this and
  // `mark.knownCli` — two arms carry it, and writing the fixture with only one
  // of them would be pinning a pane that cannot exist. The catalog arm is
  // pinned ALONE on the local `codex` pane below, which `harness` cannot carry.
  const pane = shell({
    name: "prod",
    harness: "claude",
    mark: { command: "ssh prod", argv: null, knownCli: "claude", remote: true },
  });
  assert.equal(isAgentPane(pane), true);
});

test("an SSH pane that declares no far-end CLI is not an agent row (#2514)", () => {
  // Deliberate, and the honest answer: nothing here says an agent is running.
  // The launch line is the TRANSPORT — reading `ssh` as the pane's CLI is the
  // confident-wrong-answer `agenticons.ts` exists to refuse — and the profile
  // named nothing. RESIDUAL, stated in `doc/design/agents-tab.md`: a human who
  // SSHes out and starts an agent BY HAND gets no row until the profile
  // declares one. Declaring it is the fix; guessing is not.
  const pane = shell({ name: "box", mark: { command: "ssh box", argv: null, knownCli: null, remote: true } });
  assert.equal(isAgentPane(pane), false);
});

test("the three launchable CLIs no session store covers are still agent rows (#2514)", () => {
  // THE ARM `harness` CANNOT CARRY, and the reason the predicate is not just
  // `harness !== null || orch !== null`. `sessionCliFromCommand` is a closed
  // FIVE-name membership test — it is matched against `listSessions()` rows —
  // while the launcher starts panes on eight CLIs. Resting membership on it
  // would drop these three out of the tab AND out of the badge: an agent asking
  // the human a question, invisible.
  //
  // WAS FOUR UNTIL #2515 C2, and codex is the one that left: it gained a
  // session store, so it is now inside the membership test and can no longer
  // witness what happens to a CLI outside it. Relocated rather than relaxed
  // (#1225) — the three below are still genuinely outside, which the negative
  // control on each one re-proves rather than assumes, and the class is
  // non-empty so the property still has a witness. codex's own row is covered
  // by the harness arm now, like every other session-store CLI's.
  for (const program of ["gemini", "hermes", "ante"]) {
    const pane = shell({
      name: program,
      mark: { command: `${program} --resume`, argv: null, knownCli: null, remote: false },
    });
    // The negative control that makes this test discriminate at all: these
    // really are outside the session-store set, so `harness` is genuinely null
    // in production and only the catalog arm can be answering.
    assert.equal(sessionCliFromCommand(program), null, `${program} is outside the session-store set`);
    assert.equal(pane.harness, null);
    assert.equal(isAgentPane(pane), true, `${program} is a launchable agent CLI and must be a row`);
  }
});

test("a custom-command pane naming an unrecognised program is not an agent row (#2514)", () => {
  const pane = shell({ name: "build", mark: { command: "make -j8", argv: null, knownCli: null, remote: false } });
  assert.equal(isAgentPane(pane), false);
  // NEGATIVE CONTROL for the arm's SHAPE. `agentMarkFor` is total: it gives
  // this pane a lettered badge, so a predicate reading "does the launch line
  // resolve to any program at all" would have let it straight in. Membership is
  // the launcher's catalog, which is the stricter question — and this assertion
  // is what fails if someone ever relaxes it to the resolver's.
  const view = agentMark(pane.mark);
  assert.equal(view?.kind, "letter", "the resolver still badges it; membership is stricter than the badge");
});

test("the launchable set is the launcher's own catalog (#2514)", () => {
  // Spelled out ONCE, here, on purpose: production derives it from `AGENTS`, so
  // this is the place a ninth CLI becomes a visible decision about the Agents
  // tab's membership rather than a silent widening.
  assert.deepEqual(
    [...LAUNCHABLE_AGENT_PROGRAMS].sort(),
    ["ante", "claude", "codex", "copilot", "gemini", "hermes", "opencode", "pi"],
  );
  // And that it is DERIVED, not a copy that can drift: exactly the catalog
  // minus the `custom` row, whose command names no program.
  assert.equal(LAUNCHABLE_AGENT_PROGRAMS.size, AGENTS.length - 1);
  assert.equal(LAUNCHABLE_AGENT_PROGRAMS.has(""), false);
  for (const notAnAgent of ["bash", "pwsh", "cmd", "ssh", "make"]) {
    assert.equal(LAUNCHABLE_AGENT_PROGRAMS.has(notAnAgent), false, `${notAnAgent} is not an agent CLI`);
  }
});

test("the badge and the list are read off one filtered array (#2514)", () => {
  // "One rule, not two": the count and the rendered list both come from
  // `agentRows`, so a pane cannot be excluded from the list and still counted.
  const panes = [
    shell({ key: "p-shell" }),
    shell({ key: "p-question", harness: "claude", attention: { reason: "gate", detail: null } }),
  ];
  const rows = agentRows(panes);
  assert.deepEqual(rows.map((r) => r.key), ["p-question"]);
  assert.equal(needsYouCount(rows), 1);
  // The pre-fix reading, for contrast — and the control that the fixture really
  // does hold a pane the old path counted: mapping without the rule gives two.
  assert.equal(panes.map((f) => toAgentRow(f)).length, 2);
});

test("nothing in src/ projects an agent row outside the membership rule (#2514)", () => {
  // DEFAULT-DENY. `agentRows` carries the rule; a caller reaching `toAgentRow`
  // directly is a second place the filter can be forgotten, which is exactly
  // the "one rule, not two" the issue asks for. Decided on the SYMBOL — the
  // module's own API, which cannot be renamed away without renaming the export
  // — not on any binding's name (CLAUDE.md's source-scanning-guard rule).
  // RECURSIVE, and that is finding 2 of review round 1: an earlier draft read
  // only `readdirSync("../src/")` filtered on `.ts`, so a future `src/sub/`
  // module could call `toAgentRow` and escape a guard whose own name claims
  // "nothing in src/". `vendor/` is excluded BY NAME with a reason: it is
  // third-party source this repo may not edit in place (`THIRD_PARTY_NOTICES`),
  // so a hit there would be unfixable rather than a finding — and the walk
  // asserts below that it really did descend, so the exclusion cannot quietly
  // become the whole answer.
  const walk = (rel: string): string[] =>
    readdirSync(new URL(`../src/${rel}`, import.meta.url), { withFileTypes: true }).flatMap((e) =>
      e.isDirectory()
        ? e.name === "vendor"
          ? []
          : walk(`${rel}${e.name}/`)
        : e.name.endsWith(".ts")
          ? [`${rel}${e.name}`]
          : [],
    );
  const all = walk("");
  const hits = (files: string[]) =>
    files.flatMap((f) =>
      readFileSync(new URL(`../src/${f}`, import.meta.url), "utf8")
        .split(/\r?\n/)
        .filter((l) => l.includes("toAgentRow("))
        .map((l) => `${f}: ${l.trim()}`),
    );
  assert.ok(all.length > 10, `only ${all.length} source files scanned — the walk is broken`);
  assert.deepEqual(
    hits(all.filter((f) => f !== "agentrows.ts")),
    [],
    "a module projects agent rows without the membership rule (#2514). Call agentRows() instead.",
  );
  // POSITIVE CONTROL, in the SAME shape as the scan above: a walk that matched
  // nothing at all would report zero denials and pass. `agentrows.ts` really
  // does carry the symbol — its declaration and `agentRows`' own call.
  assert.equal(hits(["agentrows.ts"]).length, 2, "the scan cannot see toAgentRow where it is defined and used");
  // ...and that the walk really DESCENDS, so "recursive" is not a claim about
  // a loop that only ever saw the top level. `src/` has exactly one
  // subdirectory today and it is the excluded one, so the subject is a
  // directory the walk is asked to skip: assert it was SEEN and skipped,
  // which is the only observable a correct walk and a top-level-only loop
  // differ on here.
  const dirs = readdirSync(new URL("../src/", import.meta.url), { withFileTypes: true })
    .filter((e) => e.isDirectory())
    .map((e) => e.name);
  assert.deepEqual(dirs, ["vendor"], "src/ gained a subdirectory — check the scan still reaches it");
  assert.equal(
    all.some((f) => f.startsWith("vendor/")),
    false,
    "vendor/ is third-party and deliberately outside this guard",
  );
});

test("one catalog rule over BOTH names the facts carry (#2514 review round 2, W2)", () => {
  // `harness` is `agentCli ?? sshDefaultCli`, and `sshDefaultCli` is FREE TEXT:
  // `normalizeSshProfile` only trims it, and the launcher appends a select option
  // for a value its catalog does not offer. An earlier draft tested `mark`
  // against the catalog and accepted `harness` on sight, so this profile walked
  // in through the door the other arm exists to close.
  const declaredShell = shell({
    name: "prod",
    harness: "bash",
    mark: { command: "ssh prod", argv: null, knownCli: "bash", remote: true },
  });
  assert.equal(isAgentPane(declaredShell), false);
  // The divergence that made it a defect rather than a preference: the very same
  // pane's header says, in so many words, that it is not an agent. One pane must
  // not get two answers.
  assert.equal(agentMark(declaredShell.mark)?.kind, "unknown");
  assert.match(agentMark(declaredShell.mark)?.label ?? "", /not an agent/);

  // POSITIVE CONTROL on the same door: a profile declaring a real CLI still
  // opens it, so the fix is a catalog test and not a blanket refusal of
  // `harness`.
  const declaredAgent = shell({
    name: "prod",
    harness: "claude",
    mark: { command: "ssh prod", argv: null, knownCli: "claude", remote: true },
  });
  assert.equal(isAgentPane(declaredAgent), true);

  // And the rule is applied to `harness` in the same SHAPE as to the launch
  // line — normalized. A profile declaring `Claude.exe` is the same claim as one
  // declaring `claude`, and only `markProgram`'s answer arrives pre-normalized.
  assert.equal(isAgentPane(shell({ harness: "C:\\tools\\Claude.exe" })), true);
  assert.equal(isAgentPane(shell({ harness: "C:\\tools\\Bash.exe" })), false);
});

test("a knownCli that normalizes to nothing names no program (#2514 review round 2, R1)", () => {
  // `normalizeAgentProgram` strips a path prefix and an `.exe`/`.cmd`/`.bat`
  // suffix, so these normalize to the EMPTY string. `markProgram` returns null
  // rather than `""`, so no caller depends on `""` being falsy and the catalog is
  // never asked about a name nobody wrote.
  for (const knownCli of [".exe", ".cmd", "C:/bin/"]) {
    assert.equal(markProgram({ knownCli, remote: true }), null, `${knownCli} names no program`);
    assert.equal(isAgentPane(shell({ harness: knownCli, mark: { knownCli, remote: true } })), false);
    // The mark still DRAWS, on the unknown tier — the pane is remote and orrerix
    // says so rather than saying nothing.
    assert.equal(agentMark({ knownCli, remote: true })?.kind, "unknown");
  }
  // POSITIVE CONTROL: the same function on a name that survives normalization.
  assert.equal(markProgram({ knownCli: "C:/bin/Claude.exe", remote: true }), "claude");
});

test("the empty-state line does not claim the window is empty (#2514 review round 2, W1)", () => {
  // It used to read "No panes open in this window.", true by construction while
  // the view projected every pane and FALSE the moment membership arrived: a
  // window of shells and a git view would have said that to a human looking at
  // them. No test pinned the string, which is why a green suite said nothing.
  assert.equal(emptyMessage("all"), "No agent panes in this window.");
  assert.doesNotMatch(emptyMessage("all"), /panes open/);
  // The filtered branch was correct and stays correct — it is a claim about one
  // state, not about the window.
  assert.equal(emptyMessage("turn-done"), "No panes are turn done.", "the LABEL, not the state key");
  for (const state of Object.keys(AGENT_STATE_LABEL) as AgentState[]) {
    assert.match(emptyMessage(state), /^No panes are .+\.$/, `the ${state} chip's empty line`);
  }
});

test("a wrapper launch line is not an agent row, and that is the stated residual (#2514)", () => {
  // RESIDUAL, pinned rather than described (CLAUDE.md: a documented blind spot
  // is a counterfactual, and only a test that performs it pins it). Membership
  // reads the FIRST token, so a shell wrapper around a real agent names the
  // wrapper. Both docs say so.
  for (const command of ['bash -lc "claude"', "npx claude", "my-claude-shim"]) {
    assert.equal(isAgentPane(shell({ mark: { command, argv: null, knownCli: null, remote: false } })), false, command);
  }
  // The cost is not only the row: the pane is outside `needsYouCount` too, and
  // on a closed panel that badge is the only signal that an agent asked
  // something. This asserts the residual's real size rather than its comfortable
  // half.
  const wrapped = shell({
    mark: { command: 'bash -lc "claude"', argv: null, knownCli: null, remote: false },
    attention: { reason: "gate", detail: null },
  });
  assert.equal(deriveAgentState(wrapped), "question", "positive control: it IS a pane wanting the human");
  assert.equal(needsYouCount(agentRows([wrapped])), 0, "…and membership costs the badge, not just the row");
  // The way out is the one the docs name — declare it, or launch it unwrapped.
  assert.equal(isAgentPane(shell({ mark: { command: "claude", argv: null, knownCli: null, remote: false } })), true);
});

test("the catalog cannot be widened at runtime behind the tab (#2514 review round 2, premortem 2)", () => {
  // `LAUNCHABLE_AGENT_PROGRAMS` is a snapshot taken at import, and the catalog
  // test above asserts the eight names — so a runtime `AGENTS.push` would widen
  // the launcher, not the tab, with a green suite. `AGENTS` is `readonly`, which
  // makes that a compile error; this pins the runtime half of the same claim,
  // since `tsc` does not run over `test/`.
  assert.equal(Array.isArray(AGENTS), true);
  const snapshot = AGENTS.map((a) => a.id);
  assert.deepEqual(
    snapshot,
    ["claude", "copilot", "codex", "opencode", "pi", "gemini", "hermes", "ante", "custom"],
    "the catalog changed — widen LAUNCHABLE_AGENT_PROGRAMS' pin above deliberately",
  );
  assert.equal(LAUNCHABLE_AGENT_PROGRAMS.size, snapshot.length - 1);
});

test("a declared far-end CLI the catalog does not name is still an agent row (#2514 review round 3, B1)", () => {
  // Round 2 fixed a declared `bash` by holding `harness` to the LAUNCHER'S
  // CATALOG, and that over-corrected: it also refused a declared CLI the badge
  // positively identifies. `setSshCli` round-trips such a value on purpose,
  // renders it as "<cli> — not a CLI orrerix knows", and WARNS rather than
  // refusing — so it is a state the product supports, and the human who set it
  // has asserted an agent runs there.
  for (const declared of ["aider", "crush", "some-inhouse-cli"]) {
    const pane = shell({
      name: declared,
      harness: declared,
      mark: { command: "ssh box", argv: null, knownCli: declared, remote: true },
    });
    assert.equal(isAgentPane(pane), true, `a profile declaring ${declared}`);
    // The negative control that makes this test discriminate: these really are
    // outside the catalog, so only the DECLARED arm can be answering.
    assert.equal(LAUNCHABLE_AGENT_PROGRAMS.has(declared), false, `${declared} is off-catalog`);
  }
});

test("the row and the header never disagree about a declared far-end CLI (#2514 review round 3, B1)", () => {
  // THE INVARIANT, rather than the two cases above and below it. Membership on
  // the declared arm IS the badge's own unknown-tier decision, so this
  // biconditional is the thing to pin: a corpus in which each side answers both
  // ways, and no member on which they differ. Round 2's fix broke it in one
  // direction (`aider`: header "Agent CLI: aider", no row) exactly as the bug
  // it fixed broke it in the other (`bash`: header "not an agent", a row).
  const CORPUS = ["claude", "copilot", "codex", "aider", "crush", "make", "bash", "pwsh", "fish", "ssh", "wsl", "1pass"];
  let listed = 0;
  for (const declared of CORPUS) {
    const pane = shell({
      harness: declared,
      mark: { command: "ssh box", argv: null, knownCli: declared, remote: true },
    });
    const badgeSaysAgent = agentMark(pane.mark)?.kind !== "unknown";
    assert.equal(isAgentPane(pane), badgeSaysAgent, `row and header disagree about a declared ${declared}`);
    if (badgeSaysAgent) listed += 1;
  }
  // POSITIVE CONTROLS on the corpus itself: an agreement assertion passes
  // vacuously on a corpus where one side never varies, so pin that BOTH answers
  // are represented and by how much.
  assert.equal(listed, 7, "claude, copilot, codex, aider, crush, make and 1pass are agents to the badge");
  assert.equal(CORPUS.length - listed, 5, "bash, pwsh, fish, ssh and wsl are not — every one of them a shell or transport");
});

test("a declared shell or transport is still refused (#2514 review round 2, W2 — unchanged by round 3)", () => {
  // The round-2 defect, re-pinned after round 3 widened the arm: widening it to
  // the badge's answer must not let a declared `bash` back in. It does not,
  // because the badge refuses it too.
  for (const declared of ["bash", "pwsh", "fish", "ssh", "wsl", "cmd"]) {
    const pane = shell({
      harness: declared,
      mark: { command: "ssh box", argv: null, knownCli: declared, remote: true },
    });
    assert.equal(isAgentPane(pane), false, `a profile declaring ${declared}`);
    assert.match(agentMark(pane.mark)?.label ?? "", /not an agent/, "…and the header says why");
  }
});

test("the two arms differ, and each is held to its own standard (#2514 review round 3, B1)", () => {
  // The asymmetry, asserted rather than described: the SAME name is refused as
  // an inferred launch line and accepted as a declared far-end CLI, because a
  // launch line is loomux's guess and a declaration is the human's assertion.
  const inferred = shell({ mark: { command: "aider --model x", argv: null, knownCli: null, remote: false } });
  assert.equal(isAgentPane(inferred), false, "an off-catalog LOCAL launch line is loomux guessing");
  const declared = shell({ harness: "aider", mark: { command: "ssh box", argv: null, knownCli: "aider", remote: true } });
  assert.equal(isAgentPane(declared), true, "…the same name DECLARED is the human asserting");
});

test("the catalog is joined on the field the launch line carries (#2514 review round 3, premortem 1)", () => {
  // The Remote CLI select stores an agent ID, while LAUNCHABLE_AGENT_PROGRAMS is
  // derived from a.command — two fields that are equal on all nine rows today,
  // so a membership rule joining them would work BY COINCIDENCE. Round 3 removed
  // that join: the declared arm no longer consults the catalog at all, and the
  // catalog is only ever asked about a program name taken from a launch line.
  // This pins the coincidence so a future `{ id: "claude-code", command: "claude" }`
  // reddens here rather than silently emptying the tab.
  const divergent = AGENTS.filter((a) => a.command !== "" && a.id !== a.command).map((a) => a.id);
  assert.deepEqual(divergent, [], "an AgentDef's id has diverged from its command — check every catalog join");
  // POSITIVE CONTROL: the comparison is over a non-empty set, and the one row
  // whose command IS empty is the `custom` row, which names no program.
  assert.equal(AGENTS.filter((a) => a.command !== "").length, LAUNCHABLE_AGENT_PROGRAMS.size);
  assert.deepEqual(AGENTS.filter((a) => a.command === "").map((a) => a.id), ["custom"]);
});

test("a catalog row cannot be rewritten in place either (#2514 review round 3, premortem 2)", () => {
  // `readonly AgentDef[]` refuses a push; that is one level shallower than the
  // claim, because `AGENTS[0].command = "…"` still compiled and the launchable
  // set is a snapshot taken before it. `AgentDef`'s fields are `readonly` now
  // too. `tsc` does not run over `test/`, so this pins the runtime half —
  // it is the assignment itself that must not be expressible, and a
  // `@ts-expect-error` here would assert the compiler's opinion in a file the
  // compiler never reads.
  const row = AGENTS[0];
  const descriptors = Object.keys(row).map((k) => [k, typeof (row as unknown as Record<string, unknown>)[k]]);
  assert.deepEqual(descriptors, [["id", "string"], ["label", "string"], ["command", "string"]]);
  // And the derived set really is downstream of `command`, which is what makes
  // an in-place rewrite a widening: every launchable program is some row's
  // command, and every non-empty command is in the set.
  for (const a of AGENTS) {
    assert.equal(LAUNCHABLE_AGENT_PROGRAMS.has(a.command), a.command !== "", `${a.id}`);
  }
});
