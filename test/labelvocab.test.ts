import { test } from "node:test";
import assert from "node:assert/strict";
import { scopeChanged, shouldReresolve, resolvedHold } from "../src/labelvocab.ts";

const S = (repo: string | null, group: string | null) => ({ repo, group });

test("scopeChanged sees a repo move and a group move independently", () => {
  assert.equal(scopeChanged(S("/r", "g1"), S("/r", "g1")), false);
  assert.equal(scopeChanged(S("/r", "g1"), S("/other", "g1")), true, "repo moved");
  assert.equal(scopeChanged(S("/r", "g1"), S("/r", "g2")), true, "group moved");
  // The two shapes #407's in-place promotion actually produces: a pane with no
  // group gains one, and (on a fresh respawn into a different orchestration) a
  // pane's group is replaced. Both must read as a change, because the cached
  // spelling is now about a scope that is not this one.
  assert.equal(scopeChanged(S("/r", null), S("/r", "g1")), true, "plain pane promoted");
  assert.equal(scopeChanged(S(null, null), S("/r", null)), true, "first resolve");
});

test("a group's vocabulary is re-resolved on every refresh, because apply_workflow moves it", () => {
  // The defect this closes: scope equality CANNOT decide it. `apply_workflow`
  // rewrites `guardrails.intake` under an open view, and the scope is identical
  // across that apply — so an unchanged scope must still re-resolve while a
  // group is in play.
  const same = S("/r", "g1");
  assert.equal(scopeChanged(same, same), false, "the premise: the scope really is unchanged");
  assert.equal(shouldReresolve(same, same), true, "...and it re-resolves anyway");
});

test("with no group the pre-#2663 cache is kept exactly: resolve on a scope change only", () => {
  assert.equal(shouldReresolve(S("/r", null), S("/r", null)), false, "no group, no change");
  assert.equal(shouldReresolve(S("/r", null), S("/other", null)), true, "no group, repo moved");
  // The discriminating pair: identical inputs except for the group, opposite
  // answers. Without this, "always re-resolve" would pass every case above.
  assert.notEqual(
    shouldReresolve(S("/r", null), S("/r", null)),
    shouldReresolve(S("/r", "g1"), S("/r", "g1"))
  );
});

test("a resolved spelling wins, and is trimmed", () => {
  assert.equal(resolvedHold("do-not-touch", null, "agent-hold"), "do-not-touch");
  assert.equal(resolvedHold("  do-not-touch  ", "old", "agent-hold"), "do-not-touch");
});

test("a transient failure keeps the last good spelling within one scope", () => {
  // Both no-answer shapes: the call REJECTED (null) and it resolved to nothing
  // (""). Resetting to the built-in here would make a blipped IPC call
  // indistinguishable from "this group stopped renaming the veto", and the
  // button would start writing a label the poller ignores.
  assert.equal(resolvedHold(null, "do-not-touch", "agent-hold"), "do-not-touch");
  assert.equal(resolvedHold("", "do-not-touch", "agent-hold"), "do-not-touch");
  assert.equal(resolvedHold("   ", "do-not-touch", "agent-hold"), "do-not-touch");
});

test("a scope change discards the previous answer, so no answer means the built-in", () => {
  // The caller passes `previous: null` across a scope change — the old spelling
  // describes a different repo or group. The built-in is what the backend's own
  // allow-list falls back to for a repo whose file it could not read either.
  assert.equal(resolvedHold(null, null, "agent-hold"), "agent-hold");
  assert.equal(resolvedHold("", null, "agent-hold"), "agent-hold");
});

test("the two halves compose over the sequence that broke: open, then apply", () => {
  // Drives the real order rather than asserting the parts in isolation.
  let scope = S(null, null);
  let hold: string | null = null;

  // 1. First refresh in a group pane: scope changed, backend says the built-in.
  let next = S("/r", "g1");
  assert.equal(shouldReresolve(scope, next), true);
  hold = resolvedHold("agent-hold", scopeChanged(scope, next) ? null : hold, "agent-hold");
  scope = next;
  assert.equal(hold, "agent-hold");

  // 2. The group applies a workflow renaming the veto. Same repo, same group.
  assert.equal(scopeChanged(scope, scope), false, "nothing about the scope moved");
  assert.equal(shouldReresolve(scope, scope), true, "and the view asks again anyway");
  hold = resolvedHold("do-not-touch", hold, "agent-hold");
  assert.equal(hold, "do-not-touch", "the button now offers what the poller watches");

  // 3. A blipped call must not retract it.
  hold = resolvedHold(null, hold, "agent-hold");
  assert.equal(hold, "do-not-touch");

  // 4. The pane moves to a different repo with no group: previous is discarded.
  next = S("/other", null);
  assert.equal(scopeChanged(scope, next), true);
  hold = resolvedHold(null, scopeChanged(scope, next) ? null : hold, "agent-hold");
  assert.equal(hold, "agent-hold", "a stale rename must not follow the pane to another repo");
});
