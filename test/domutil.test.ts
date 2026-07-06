// Regression tests for the inline-rename crash reported in issue #77: a red
// "Uncaught NotFoundError: Failed to execute 'replaceWith' ... Perhaps it was
// moved in a 'blur' event handler?" banner appeared mid orchestration session.
//
// Root cause: an inline editor's commit() is bound to both a key handler and
// blur. Committing detaches the focused input, which fires blur → commit()
// runs a second time and calls replaceWith on the already-detached node, which
// throws. swapIfConnected() makes commit() idempotent.
import { test } from "node:test";
import assert from "node:assert/strict";
import { swapIfConnected, type Swappable } from "../src/domutil.ts";

/** Minimal stand-in for a DOM element mirroring the browser's own semantics:
 *  replaceWith on a node that is no longer attached throws NotFoundError. */
class FakeNode implements Swappable {
  isConnected = true;
  replaceWith(): void {
    if (!this.isConnected) {
      throw new Error(
        "NotFoundError: Failed to execute 'replaceWith' on 'Element': " +
          "The node to be removed is no longer a child of this node."
      );
    }
    this.isConnected = false; // the swap detaches this node from the tree
  }
}

test("the raw double-swap reproduces the #77 crash (documents the bug)", () => {
  const input = new FakeNode();
  input.replaceWith(); // first commit succeeds, detaches input
  assert.throws(() => input.replaceWith(), /NotFoundError/); // blur's second commit throws
});

test("swapIfConnected swaps once, then no-ops instead of throwing", () => {
  const input = new FakeNode();
  assert.equal(swapIfConnected(input, {}), true); // first commit performs the swap
  assert.equal(swapIfConnected(input, {}), false); // blur's redundant commit is a safe no-op
});

test("the Enter-then-blur commit sequence never throws or double-saves", () => {
  const input = new FakeNode();
  let saves = 0;
  // Mirror tasksview.ts commit(): guard the swap, only act on the real commit.
  const commit = (save: boolean) => {
    if (!swapIfConnected(input, {})) return;
    if (save) saves++;
  };
  assert.doesNotThrow(() => {
    commit(true); // Enter
    commit(true); // blur fired by detaching the focused input
  });
  assert.equal(saves, 1, "the edit is saved exactly once, not twice");
});

test("Escape then blur discards the edit and never saves", () => {
  const input = new FakeNode();
  let saves = 0;
  const commit = (save: boolean) => {
    if (!swapIfConnected(input, {})) return;
    if (save) saves++;
  };
  assert.doesNotThrow(() => {
    commit(false); // Escape restores the label without saving
    commit(true); // blur's redundant commit is a no-op
  });
  assert.equal(saves, 0, "Escape must not save");
});

test("a background re-render that already removed the editor is a safe no-op", () => {
  const input = new FakeNode();
  input.isConnected = false; // e.g. a live task refresh replaced the row
  let saves = 0;
  const commit = (save: boolean) => {
    if (!swapIfConnected(input, {})) return;
    if (save) saves++;
  };
  assert.doesNotThrow(() => commit(true));
  assert.equal(saves, 0);
});
