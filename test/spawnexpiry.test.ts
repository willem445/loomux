// Unit tests for the spawn-request expiry decision (issue #106). The bug: a
// frontend stalled past the backend's 20s bind timeout would, on recovery, still
// service a queued orch-spawn-request — opening a zombie pane whose CLI booted
// against a config the bind-timeout had already deleted. The backend now stamps
// each request with the deadline of its own bind wait; the frontend drops any
// request already past it. This pins the drop rule both sides agree on. Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { isSpawnRequestExpired } from "../src/spawnexpiry.ts";

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
