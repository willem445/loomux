// Unit tests for the spawn-request expiry decision (issue #106). The bug: a
// frontend stalled past the backend's 20s bind timeout would, on recovery, still
// service a queued orch-spawn-request — opening a zombie pane whose CLI booted
// against a config the bind-timeout had already deleted. The backend now stamps
// each request with the deadline of its own bind wait; the frontend drops any
// request already past it. This pins the drop rule both sides agree on. Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { isSpawnRequestExpired, spawnsForGroup } from "../src/spawnexpiry.ts";

test("a request whose deadline is in the future is NOT expired", () => {
  const now = 1_000_000;
  assert.equal(isSpawnRequestExpired(now + 20_000, now), false);
});

test("a request whose deadline has passed IS expired (the zombie-pane case)", () => {
  const now = 1_000_000;
  // Frontend recovered 5s after the deadline — the classic stalled-then-recovered
  // scenario from the incident. Must drop.
  assert.equal(isSpawnRequestExpired(now - 5_000, now), true);
});

test("exactly at the deadline is not yet expired (boundary is strict `>`)", () => {
  const t = 1_000_000;
  assert.equal(isSpawnRequestExpired(t, t), false);
  assert.equal(isSpawnRequestExpired(t, t + 1), true);
});

test("deadline 0 means unstamped (legacy backend) and never expires", () => {
  // An older backend that doesn't stamp the field must degrade to the previous
  // always-service behaviour rather than dropping every spawn.
  assert.equal(isSpawnRequestExpired(0, 5_000_000), false);
});

// --- spawnsForGroup: the orch-group-ended backstop for cancelledSpawns (#1316) ---
//
// The bug: a cancel for a spawn already dropped by isSpawnRequestExpired above
// never has a matching openAgentPane, so cancelledSpawns.delete() (its only
// production caller) never runs for that id — it's stranded for the life of
// the window. orch-group-ended sweeps every entry for its group as a backstop.

test("spawnsForGroup picks out only the entries for that group", () => {
  const entries: [string, string][] = [
    ["a-1", "g1"],
    ["a-2", "g2"],
    ["a-3", "g1"],
  ];
  assert.deepEqual(spawnsForGroup(entries, "g1").sort(), ["a-1", "a-3"]);
  assert.deepEqual(spawnsForGroup(entries, "g2"), ["a-2"]);
});

test("spawnsForGroup on a group with no stranded entries returns nothing", () => {
  const entries: [string, string][] = [["a-1", "g1"]];
  assert.deepEqual(spawnsForGroup(entries, "g-never-seen"), []);
  assert.deepEqual(spawnsForGroup([], "g1"), []);
});
