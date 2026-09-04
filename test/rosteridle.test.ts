// The strip snapshot -> `Pane.noteRosterIdle` mapping (#2122 slice B). The
// reading feeds the `idle` rung of `deriveAgentState` and NOTHING else, so what
// this module gets wrong shows up as a working agent reported idle — which is
// exactly the claim the whole tab exists to make.

import { test } from "node:test";
import assert from "node:assert/strict";

import { rosterIdleFor, type RosterReading } from "../src/rosteridle.ts";

const reading = (agents: { id: string; idle_since_ms: number | null }[] | null): RosterReading => ({
  groups: { g1: { summary: agents === null ? null : { agents } } },
});

test("an agent the roster has parked reads idle, one with work does not", () => {
  const strip = reading([
    { id: "a-idle", idle_since_ms: 1_700_000_000_000 },
    { id: "a-busy", idle_since_ms: null },
  ]);
  assert.equal(rosterIdleFor(strip, "g1", "a-idle"), true);
  assert.equal(rosterIdleFor(strip, "g1", "a-busy"), false);
});

// The three ways the roster can fail to answer, and they must ALL read `null`
// rather than `false`. `null` means "the roster does not cover this pane", which
// the ladder reads as "no evidence" and resolves to `working`; `false` would be
// a positive claim that the agent HAS work, made from a lookup that found
// nothing. Both land the pane on `working` today — but only one of them is
// still true if the ladder's `idle` rung is ever inverted.
test("a lookup that finds nothing says so, rather than claiming the agent is busy", () => {
  const strip = reading([{ id: "a-idle", idle_since_ms: 5 }]);
  assert.equal(rosterIdleFor(strip, "g1", "nobody"), null, "an agent absent from the roster");
  assert.equal(rosterIdleFor(strip, "other-group", "a-idle"), null, "a group absent from the strip");
  assert.equal(rosterIdleFor(reading(null), "g1", "a-idle"), null, "a group the backend refused");
});

test("a pane with no orchestration identity is never given a roster reading", () => {
  const strip = reading([{ id: "a-idle", idle_since_ms: 5 }]);
  assert.equal(rosterIdleFor(strip, null, "a-idle"), null, "no group");
  assert.equal(rosterIdleFor(strip, "g1", null), null, "no agent id");
});

test("an idle stamp of 0 is still an idle stamp", () => {
  // `idle_since_ms` is a unix-ms timestamp and 0 is a legal one. Reading it as
  // falsy — the obvious `!!a.idle_since_ms` — reports a parked agent as busy,
  // and no fixture built from `Date.now()` would ever catch it.
  assert.equal(rosterIdleFor(reading([{ id: "a", idle_since_ms: 0 }]), "g1", "a"), true);
});
