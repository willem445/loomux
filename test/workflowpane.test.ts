// The workflow pane's pure DECISIONS (#222 v2, rev-15): which surface it shows, how a save is
// allowed to write, and what the layout file is allowed to forget.
//
// Every test here is a bug that shipped. The view used to hold these three answers itself, and
// got all three wrong in ways no amount of DOM-wiring care would have caught — they are rules,
// not rendering, and rules belong somewhere they can be stated once and tested.
import { test } from "node:test";
import assert from "node:assert/strict";
import { paneSurface, savePlan, layoutPruneIds } from "../src/workflowpane.ts";
import { parseWorkflow, starterWorkflow, removeBlockAt, addBlock, newBlock } from "../src/workflowmodel.ts";

// ---------- F1: a file that is THERE is never reported as absent ----------

test("an unreadable workflow shows the ERROR surface, never the start surface", () => {
  // The bug this pane's v2 exists to fix, and then the bug the fix itself shipped with: a
  // UTF-16 workflow (what PowerShell's `>` writes) is a file that EXISTS and cannot be decoded.
  // Reporting "no workflow in this repo yet" for it is not just wrong, it is dangerous — the
  // start surface offers to CREATE one, i.e. to overwrite the file we just refused to show.
  assert.equal(
    paneSurface({ loadError: "the file isn't valid UTF-8", exists: false, text: "" }),
    "error"
  );
  // …and it stays the error surface even though `exists` is false and the buffer is empty,
  // which is exactly the state the start surface otherwise matches. THAT is the whole trap.
});

test("no workflow file at all is the START surface — the normal beginning of every repo", () => {
  assert.equal(paneSurface({ loadError: null, exists: false, text: "" }), "start");
  assert.equal(paneSurface({ loadError: null, exists: false, text: "   \n " }), "start");
});

test("a workflow — saved, or scaffolded and not yet saved — is the BODY", () => {
  assert.equal(paneSurface({ loadError: null, exists: true, text: "version: 1\n" }), "body");
  // A scaffold the human hasn't saved yet is content: dropping them back to the start surface
  // ("create a workflow") while one is sitting unsaved in the buffer would be absurd.
  assert.equal(paneSurface({ loadError: null, exists: false, text: "version: 1\n" }), "body");
});

// ---------- F2: a create can never overwrite ----------

test("a CREATE claims the path first — it never writes unconditionally", () => {
  // THE DATA-LOSS BUG. A null expected hash is "write unconditionally" to the backend. The pane
  // can sit on its start surface for minutes; if a workflow arrives in that window (an agent
  // writes one, a `git pull` brings one in) the scaffold overwrote it — and said "Saved".
  assert.deepEqual(savePlan({ exists: false, savedHash: "" }), { kind: "claim-then-write" });

  // The plan type has no "write unconditionally" member at all, which is the point: the only
  // path allowed to clobber is the human answering "Overwrite" in the conflict dialog, and that
  // is an answer to a question, not a save plan.
  const plans = [
    savePlan({ exists: false, savedHash: "" }),
    savePlan({ exists: true, savedHash: "abc" }),
  ];
  assert.ok(plans.every((p) => p.kind === "claim-then-write" || p.expectedHash !== ""));
});

test("an ordinary save writes against the hash it read — so a file that moved is a CONFLICT", () => {
  assert.deepEqual(savePlan({ exists: true, savedHash: "abc123" }), {
    kind: "guarded-write",
    expectedHash: "abc123",
  });
});

test("believing a file exists without holding its hash still claims rather than clobbers", () => {
  // Belt and braces: `exists` is the pane's BELIEF, and a belief with no hash behind it cannot
  // be used to authorize an unguarded write. (Reachable if a read half-failed.)
  assert.deepEqual(savePlan({ exists: true, savedHash: "" }), { kind: "claim-then-write" });
});

// ---------- F5: a drag cannot write a deletion the human hasn't made ----------

test("dragging a node never prunes a block the human has deleted but not SAVED", () => {
  // Repro from the review: open a workflow, delete `reviewer` in the form (don't save), drag any
  // other node. The layout write used to prune against the BUFFER, so `reviewer`'s coordinate
  // was removed from workflow.layout.json ON DISK — and discarding the edit brought the block
  // back with its position gone. A position is disposable, so this cost a drag; but it is a
  // write to disk on the strength of an edit the human had not made.
  const saved = starterWorkflow(); // planner, worker, reviewer — on disk
  const buffer = removeBlockAt(saved, 2); // reviewer deleted in the buffer, NOT saved

  const onDrag = layoutPruneIds(saved, buffer, "drag");
  assert.ok(onDrag.includes("reviewer"), "its position survives a drag — the deletion isn't real yet");
  assert.ok(onDrag.includes("planner") && onDrag.includes("worker"));

  // And once the human actually SAVES that deletion, the position goes: pruning is still doing
  // its job, at the one moment the roster on disk and the roster in memory are the same roster.
  const onSave = layoutPruneIds(buffer, buffer, "save");
  assert.ok(!onSave.includes("reviewer"));
});

test("a block created but not yet saved keeps the position it was dropped at", () => {
  // The other half of the same rule: a drag must not forget a block that exists only in the
  // buffer either. (Add a block on the canvas, drag it, and it must not spring back.)
  const saved = starterWorkflow();
  const buffer = addBlock(saved, newBlock("rev-perf", "Perf"));
  assert.ok(layoutPruneIds(saved, buffer, "drag").includes("rev-perf"));
});

test("a block that exists in neither is still forgotten — pruning still prunes", () => {
  // Without this, the layout of a workflow you've edited for a year is mostly ghosts.
  const saved = starterWorkflow();
  const ids = layoutPruneIds(saved, saved, "drag");
  assert.ok(!ids.includes("deleted-last-year"));
  assert.deepEqual([...ids].sort(), ["planner", "reviewer", "worker"]);
});

test("with no saved workflow at all, a drag prunes against the buffer alone", () => {
  // The scaffold-then-drag-then-save path: there is nothing on disk to protect yet.
  const buffer = parseWorkflow("version: 1\nblocks:\n  - id: a\n    kind: worker\n    cli: claude\n").workflow;
  assert.deepEqual(layoutPruneIds(null, buffer, "drag"), ["a"]);
});
