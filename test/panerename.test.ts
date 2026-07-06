// Regression tests for the pane inline-rename double-commit crash (#75, the
// twin of the #77 tasksview fix). commit() is wired to Enter/Escape AND blur;
// the first commit detaches the focused input, firing blur → a second commit
// that used to call replaceWith on a detached node and throw NotFoundError out
// into the app-wide error banner. makeRenameCommit must run its body exactly
// once. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeRenameCommit } from "../src/panerename.ts";

// A recorder that mimics the DOM ops, with `restore` modelling the real crash:
// the browser's replaceWith throws if invoked a second time on the detached
// input. If the guard leaks, this reproduces the original NotFoundError.
function harness(initial: string) {
  const state = { name: initial, restores: 0, saves: [] as string[], value: initial };
  const commit = makeRenameCommit({
    value: () => state.value,
    save: (n) => {
      state.name = n;
      state.saves.push(n);
    },
    restore: () => {
      if (state.restores > 0) {
        throw new Error("NotFoundError: replaceWith on a detached node");
      }
      state.restores++;
    },
  });
  return { state, commit };
}

test("Enter then the blur it triggers commits exactly once and never throws", () => {
  const { state, commit } = harness("old");
  state.value = "new-name";
  assert.doesNotThrow(() => {
    commit(true); // Enter
    commit(true); // blur fired by detaching the focused input
  });
  assert.equal(state.name, "new-name");
  assert.equal(state.saves.length, 1, "saved once, not twice");
  assert.equal(state.restores, 1, "swapped back once");
});

test("Escape cancels and wins over the trailing blur-save", () => {
  const { state, commit } = harness("old");
  state.value = "typed-but-discarded";
  commit(false); // Escape — do not save
  commit(true); // blur from the detach must NOT resurrect the edit
  assert.equal(state.name, "old", "cancel truly cancels");
  assert.deepEqual(state.saves, []);
  assert.equal(state.restores, 1);
});

test("a lone blur (click away) saves once", () => {
  const { state, commit } = harness("old");
  state.value = "clicked-away";
  commit(true);
  assert.equal(state.name, "clicked-away");
  assert.equal(state.saves.length, 1);
  assert.equal(state.restores, 1);
});

test("committing an empty/whitespace value keeps the old name but still restores", () => {
  const { state, commit } = harness("keep-me");
  state.value = "   ";
  commit(true);
  commit(true); // trailing blur
  assert.equal(state.name, "keep-me", "blank rename is rejected");
  assert.deepEqual(state.saves, []);
  assert.equal(state.restores, 1, "the input is still swapped back exactly once");
});

test("the value is trimmed before saving", () => {
  const { state, commit } = harness("old");
  state.value = "  spaced  ";
  commit(true);
  assert.equal(state.name, "spaced");
});
