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
// `connected` models input.isConnected — `true` for the ordinary Enter/click-away
// path, flipped to `false` to model an involuntary grid/dock detach mid-edit.
function harness(initial: string, connected = true) {
  const state = { name: initial, restores: 0, saves: [] as string[], value: initial, connected };
  const commit = makeRenameCommit({
    value: () => state.value,
    isConnected: () => state.connected,
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

// ---- #113: an orchestrator-driven grid move detaches the input mid-edit ----
//
// The grid reuses the pane element and moves its subtree on spawn/kill/reorder,
// which blurs the open rename input WHILE it is off the document. That blur fires
// commit(true), but because the end was involuntary (isConnected() === false) it
// must CANCEL — never persist a half-typed name and never sync it to the roster.
// Mirror the pane's real save(), including the backend-sync counter.
function paneHarness(initial: string, connected: boolean, hasOrchAgent = true) {
  const state = { name: initial, value: initial, restores: 0, backendRenames: 0, connected };
  const commit = makeRenameCommit({
    value: () => state.value,
    isConnected: () => state.connected,
    save: (name) => {
      const changed = name !== state.name;
      state.name = name;
      if (hasOrchAgent && changed) state.backendRenames++;
    },
    restore: () => {
      state.restores++;
    },
  });
  return { state, commit };
}

test("a mid-edit grid move (detached blur) cancels — no half-typed save, no sync", () => {
  const { state, commit } = paneHarness("agent-1", /*connected*/ false);
  state.value = "half-typ"; // human was mid-typing when the grid moved the pane
  commit(true); // blur fires with save=true, but the input is off the document
  assert.equal(state.name, "agent-1", "the pre-edit name is kept, not the half-typed one");
  assert.equal(state.backendRenames, 0, "an unconfirmed name never reaches the roster");
  assert.equal(state.restores, 1, "the header is still reconciled exactly once");
});

test("an explicit commit while connected still saves once and syncs once", () => {
  const { state, commit } = paneHarness("agent-1", /*connected*/ true);
  state.value = "renamed"; // human typed a name and pressed Enter (still on document)
  commit(true); // Enter
  commit(true); // the trailing blur it triggers must not double-apply
  assert.equal(state.name, "renamed");
  assert.equal(state.backendRenames, 1, "synced to the backend exactly once");
  assert.equal(state.restores, 1);
});

test("a connected commit with an unchanged name does not re-broadcast a rename", () => {
  const { state, commit } = paneHarness("agent-1", /*connected*/ true);
  commit(true); // Enter/blur without changing anything
  assert.equal(state.name, "agent-1");
  assert.equal(state.backendRenames, 0, "no-op rename is not sent to the backend");
  assert.equal(state.restores, 1);
});

test("Escape cancels regardless of connection state", () => {
  for (const connected of [true, false]) {
    const { state, commit } = paneHarness("agent-1", connected);
    state.value = "discard-me";
    commit(false); // Escape
    commit(true); // trailing blur must not resurrect the edit
    assert.equal(state.name, "agent-1", `Escape wins (connected=${connected})`);
    assert.equal(state.backendRenames, 0);
  }
});
