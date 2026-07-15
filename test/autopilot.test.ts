// Unit tests for the single-pane autopilot toggle's persisted default-ON
// semantics (#101), plus the standalone channel-tools toggle (#271 W3
// addendum / PR #289 review round 2, N1) which shares the identical
// default-ON/explicit-"0"-off shape. Run with `npm test`. The pure
// `*FromStored` functions are tested directly so the default-ON rule needs
// no localStorage shim.
import { test } from "node:test";
import assert from "node:assert/strict";
import { autopilotFromStored, channelToolsFromStored } from "../src/agents.ts";

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

test("channel tools default ON when nothing is stored", () => {
  // A brand-new user (no key yet) launches claude/copilot agent panes with
  // eager solo-prepare on — the addendum's stated "full membership at spawn"
  // default (#271 W3, N1 fix).
  assert.equal(channelToolsFromStored(null), true);
});

test('only an explicit "0" turns channel tools off', () => {
  assert.equal(channelToolsFromStored("0"), false);
});

test('a stored "1" keeps channel tools on', () => {
  assert.equal(channelToolsFromStored("1"), true);
});

test("an empty or unrecognized channel-tools value stays ON (fail-safe to the default)", () => {
  assert.equal(channelToolsFromStored(""), true);
  assert.equal(channelToolsFromStored("yes"), true);
  assert.equal(channelToolsFromStored("false"), true);
});
