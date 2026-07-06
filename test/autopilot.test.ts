// Unit tests for the single-pane autopilot toggle's persisted default-ON
// semantics (#101). Run with `npm test`. The pure `autopilotFromStored` is
// tested directly so the default-ON rule needs no localStorage shim.
import { test } from "node:test";
import assert from "node:assert/strict";
import { autopilotFromStored } from "../src/agents.ts";

test("autopilot defaults ON when nothing is stored", () => {
  // A brand-new user (no key yet) launches with autopilot on.
  assert.equal(autopilotFromStored(null), true);
});

test('only an explicit "0" turns autopilot off', () => {
  assert.equal(autopilotFromStored("0"), false);
});

test('a stored "1" keeps autopilot on', () => {
  assert.equal(autopilotFromStored("1"), true);
});

test("an empty or unrecognized value stays ON (fail-safe to the default)", () => {
  // A corrupted value must not silently disable autopilot — default wins.
  assert.equal(autopilotFromStored(""), true);
  assert.equal(autopilotFromStored("yes"), true);
  assert.equal(autopilotFromStored("false"), true);
});
