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
import {
  deriveAgentState,
  groupRows,
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
  });
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
  // The mid-rename tick: one row still carries the old title. Still ONE group —
  // and it wears the fresher label, not the stale one.
  const midway = [row("m1", "working", WS.mike), row("m2", "working", renamed)];
  assert.deepEqual(shape(midway, "tab"), [["MIKE renamed", "m1", "m2"]]);
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
