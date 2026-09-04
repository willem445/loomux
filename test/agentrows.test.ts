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
  toAgentRow,
  matchesFilter,
  sortRows,
  needsYouCount,
  type AgentRow,
  type AgentState,
  type PaneFacts,
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
    why: "a report is a question-class reason, not an urgent one",
    state: "question",
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
    ["attention", "dead", "dormant", "held", "idle", "question", "turn-done", "working"],
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
  });
});

test("a pane with no orchestration identity flattens to nulls, not to undefined", () => {
  const row = toAgentRow(facts({ orch: null, harness: null }));
  assert.equal(row.group, null);
  assert.equal(row.agentId, null);
  assert.equal(row.role, null);
  assert.equal(row.harness, null);
  assert.equal(row.notes, null, "notes default to 'not loaded', which is not 0");
});

function row(name: string, state: AgentState): AgentRow {
  return { key: `k-${name}`, name, harness: "claude", group: "g", agentId: name, role: "worker", state, notes: null };
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
  ];
  const before = input.map((r) => r.name);
  const out = sortRows(input);
  assert.deepEqual(
    out.map((r) => r.name),
    ["beta", "epsilon", "delta", "alpha", "zeta", "gamma"],
  );
  assert.deepEqual(input.map((r) => r.name), before, "sortRows must not reorder the caller's array");
});

test("needsYouCount counts the two states a person must act on", () => {
  const rows = [
    row("a", "attention"),
    row("b", "question"),
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
  assert.equal(rows.length, 8);
  assert.equal(needsYouCount(rows), 2);
  assert.equal(needsYouCount([]), 0);
  assert.equal(needsYouCount([row("a", "working")]), 0);
});
