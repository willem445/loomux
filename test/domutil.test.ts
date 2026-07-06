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
import {
  swapIfConnected,
  swapEditor,
  type Swappable,
  type Reparentable,
} from "../src/domutil.ts";

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

// ---- swapEditor: the pane-rename detach/reparent hardening (#113) ----
//
// A pane reuses its element; the grid moves its whole subtree with
// replaceWith/replaceChildren on spawn/kill/reorder. That detaches the focused
// rename input mid-edit and fires blur. Model a header whose child is the input,
// mirroring the browser: replaceWith is a no-op when the node is unparented (so
// it never throws) and otherwise swaps the label into the input's slot.
class FakeHeader {
  children: FakeInput[] = [];
}
class FakeInput implements Reparentable {
  isConnected = true;
  parentNode: FakeHeader | null;
  constructor(header: FakeHeader) {
    this.parentNode = header;
    header.children.push(this);
  }
  /** Detach this input from the document (grid moved the subtree off-screen)
   *  while it stays parented to its (now off-document) header. */
  detachFromDocument(): void {
    this.isConnected = false;
  }
  /** Fully remove this input from its header (nothing left to swap). */
  orphan(): void {
    this.isConnected = false;
    if (this.parentNode) this.parentNode.children = [];
    this.parentNode = null;
  }
  replaceWith(to: unknown): void {
    // WHATWG DOM: replaceWith on a parentless node returns early (no throw).
    if (this.parentNode == null) return;
    const idx = this.parentNode.children.indexOf(this);
    this.parentNode.children[idx] = to as FakeInput;
    this.parentNode = null;
  }
}

test("connected input: swaps the label in and reports live (refocus)", () => {
  const header = new FakeHeader();
  const input = new FakeInput(header);
  const label = { label: true } as unknown as FakeInput;
  const r = swapEditor(input, label);
  assert.deepEqual(r, { swapped: true, live: true });
  assert.deepEqual(header.children, [label], "label sits where the input was");
});

test("detached-but-parented input (grid moved the subtree): swaps, but NOT live", () => {
  const header = new FakeHeader();
  const input = new FakeInput(header);
  input.detachFromDocument(); // grid.ts swap/renderSplit moved the pane off-document
  const label = { label: true } as unknown as FakeInput;
  let refocused = false;
  const r = swapEditor(input, label);
  if (r.live) refocused = true; // the pane.ts caller only focuses on live
  assert.deepEqual(r, { swapped: true, live: false });
  assert.equal(refocused, false, "must not steal focus mid-restructure (#113 crash)");
  assert.deepEqual(
    header.children,
    [label],
    "header left consistent: label restored, no orphaned input"
  );
});

test("detached and unparented input (already gone): no-op, no throw", () => {
  const header = new FakeHeader();
  const input = new FakeInput(header);
  input.orphan(); // the editor was fully removed before the commit ran
  const r = swapEditor(input, { label: true });
  assert.deepEqual(r, { swapped: false, live: false });
});

test("swapEditor never throws across the full mid-edit-move commit sequence", () => {
  const header = new FakeHeader();
  const input = new FakeInput(header);
  const label = { label: true } as unknown as FakeInput;
  // blur from the grid move commits first while detached-but-parented...
  input.detachFromDocument();
  assert.doesNotThrow(() => {
    const first = swapEditor(input, label);
    assert.equal(first.swapped, true);
    // ...and any trailing blur/keydown that also reaches restore is now a no-op
    // (input is unparented after the swap) and still never throws.
    const second = swapEditor(input, label);
    assert.equal(second.swapped, false);
  });
  assert.deepEqual(header.children, [label]);
});
