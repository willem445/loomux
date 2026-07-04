// Unit tests for orchestration group-membership selection. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { panesInGroup } from "../src/group.ts";

// A minimal stand-in for a Pane: only the field the selector reads, plus a
// `minimized` marker used to assert visibility is irrelevant to selection.
const pane = (orchGroupId: string | null, minimized = false) => ({ orchGroupId, minimized });

test("selects only panes in the given group", () => {
  const panes = [pane("g1"), pane("g2"), pane("g1"), pane(null)];
  const picked = panesInGroup(panes, "g1");
  assert.equal(picked.length, 2);
  assert.ok(picked.every((p) => p.orchGroupId === "g1"));
});

test("a minimized group pane is still selected (the group-ended fix)", () => {
  const visible = pane("g1");
  const docked = pane("g1", true); // minimized — must not escape a group end
  const other = pane("g2");
  const picked = panesInGroup([visible, docked, other], "g1");
  assert.deepEqual(picked, [visible, docked]);
  assert.ok(picked.includes(docked), "minimized pane in the group is closed too");
});

test("panes with no group are never selected", () => {
  assert.deepEqual(panesInGroup([pane(null), pane(null)], "g1"), []);
});

test("no members yields an empty set, not a throw", () => {
  assert.deepEqual(panesInGroup([pane("g2")], "g1"), []);
  assert.deepEqual(panesInGroup([], "g1"), []);
});
