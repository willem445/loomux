// The workflow pane's pure DECISIONS (#222 v2, rev-15): which surface it shows, how a save is
// allowed to write, and what the layout file is allowed to forget.
//
// Every test here is a bug that shipped. The view used to hold these three answers itself, and
// got all three wrong in ways no amount of DOM-wiring care would have caught — they are rules,
// not rendering, and rules belong somewhere they can be stated once and tested.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  paneSurface,
  createAllowed,
  savePlan,
  layoutPruneIds,
  rewriteImpact,
  rewriteImpactMessage,
} from "../src/workflowpane.ts";
import {
  parseWorkflow,
  starterWorkflow,
  removeBlockAt,
  addBlock,
  newBlock,
  serializeWorkflow,
  formatWorkflowText,
  connectBlocks,
} from "../src/workflowmodel.ts";

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

// ---------- the live bug: a create that was reachable over a workflow that was RIGHT THERE ----------

test("CREATE is refused while a workflow is loaded — the one the human watched get overwritten", () => {
  // WHAT ACTUALLY HAPPENED (the demo, in loomux-testbed). The pane read `.loomux/workflow.yml`,
  // validated it, and showed it — and showed the "Create workflow" button on top of it, because
  // `hidden` doesn't hide anything the stylesheet gave a `display` to (test/hiddenrule.test.ts).
  // The human pressed it and the scaffold replaced their workflow.
  //
  // Every guard BELOW the button did its job. That is the part worth staring at: from the save's
  // point of view this was an ordinary edit — the pane had read the file and held its hash, so
  // there was no conflict to detect and nothing to refuse. The rule below is the only place the
  // answer could have come from.
  const loaded = { loadError: null, exists: true, text: "version: 1\nname: default\n" };
  assert.equal(paneSurface(loaded), "body");
  assert.equal(createAllowed(loaded), false, "there is a workflow here — creating means destroying it");
});

test("CREATE is refused on the ERROR surface — you cannot scaffold over what you refused to show", () => {
  // The older, subtler shape of the same thing: a file that is THERE and unreadable (UTF-16 from
  // PowerShell) must not be offered a "Create workflow" button either, and the reason is exactly
  // that the pane cannot see what it would be destroying.
  assert.equal(createAllowed({ loadError: "not valid UTF-8", exists: false, text: "" }), false);
});

test("CREATE is allowed only where there is nothing to lose", () => {
  assert.equal(createAllowed({ loadError: null, exists: false, text: "" }), true);
  // …and not once a scaffold is sitting unsaved in the buffer: that is content, the pane is on its
  // body surface, and a second create would throw away the first one's edits.
  assert.equal(createAllowed({ loadError: null, exists: false, text: "version: 1\n" }), false);
});

test("every state that permits a CREATE plans a claim — a create can never be ASKED to clobber", () => {
  // The property that makes the gate structural rather than a second opinion. `createAllowed` is
  // true only on the start surface, and the start surface is BY DEFINITION "no file, empty buffer"
  // — which is exactly the state in which `savePlan` claims the path atomically instead of writing
  // against a hash. So the two rules cannot drift apart into a create that overwrites: the write
  // plan for a permitted create is a `claim-then-write`, always, and `fm_new_file` refuses without
  // truncating if anything got there first (src-tauri/tests/workflowfile.rs).
  const states = [
    { loadError: null, exists: false, text: "" },
    { loadError: null, exists: false, text: "   \n\t " },
    { loadError: null, exists: true, text: "version: 1\n" },
    { loadError: "unreadable", exists: false, text: "" },
    { loadError: null, exists: false, text: "version: 1\n" },
  ];
  for (const s of states) {
    if (!createAllowed(s)) continue;
    // A permitted create never believes a file is there, so it never has a hash to write against.
    assert.deepEqual(savePlan({ exists: s.exists, savedHash: "" }), { kind: "claim-then-write" });
  }
});

test("the save layer could not have saved us — which is why the gate is above it", () => {
  // Not a guard, a POST-MORTEM, pinned so nobody re-argues that F2's claim-then-write already
  // covered this. It didn't, and it couldn't: claim-then-write arms when the pane believes there
  // is NO file. Pressing Create over a LOADED workflow is the opposite state — `exists` is true
  // and the hash is the real file's — so the plan is a guarded write, the hash matches (nothing
  // else touched the file), and the backend overwrites, correctly, as instructed.
  assert.deepEqual(savePlan({ exists: true, savedHash: "the-real-file's-hash" }), {
    kind: "guarded-write",
    expectedHash: "the-real-file's-hash",
  });
  // No conflict. No refusal. No dialog. The write is only wrong because the BUTTON was wrong.
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

// ---------- F6: a rewrite the human didn't ask for is announced before it happens ----------

const canon = (t: string): boolean => formatWorkflowText(t) === t;

test("saving canonical text over a COMMENTED file warns, and says what it costs", () => {
  // The trade: a form or canvas edit re-serializes the whole workflow from the model, and the
  // model does not carry comments. For a file loomux wrote that costs nothing. For a file a human
  // wrote, the comments are frequently the most valuable lines in it — and until this guard, one
  // dragged edge took all of them without a word.
  const commented = `# who runs, and why
version: 1
name: x

blocks:
  - id: worker          # the one that opens the PR
    name: Worker
    kind: worker
    cli: claude
`;
  const impact = rewriteImpact(commented, formatWorkflowText(commented), canon);
  assert.ok(impact, "this save must not be silent");
  // TWO: the header comment, and the TRAILING one on the `id:` line. Counting only whole-line
  // comments would under-report what the human loses — the trailing ones go too.
  assert.equal(impact.droppedComments, 2);
  assert.equal(impact.reformats, true);
  assert.match(rewriteImpactMessage(impact, ".loomux/workflow.yml"), /comments on 2 lines will be dropped/);
  assert.match(rewriteImpactMessage(impact, ".loomux/workflow.yml"), /canonical form/);
});

test("a file already in canonical form saves SILENTLY — there is nothing to lose", () => {
  // A confirm that fires when nothing is at stake is a confirm people learn to click through, and
  // then they click through the one that mattered. Anything loomux wrote is already canonical.
  const canonical = serializeWorkflow(starterWorkflow());
  const edited = serializeWorkflow(connectBlocks(starterWorkflow(), "planner", "reviewer"));
  assert.equal(rewriteImpact(canonical, edited, canon), null);
  assert.equal(rewriteImpact(canonical, canonical, canon), null, "and writing identical bytes is not a rewrite");
});

test("creating a file destroys nothing, so it never asks", () => {
  assert.equal(rewriteImpact("", serializeWorkflow(starterWorkflow()), canon), null);
  assert.equal(rewriteImpact("   \n", serializeWorkflow(starterWorkflow()), canon), null);
});

test("editing the YAML tab by hand is never a 'rewrite' — you can see what you're saving", () => {
  // The human typed it. Warning them that their own keystrokes will change the file would be
  // absurd — and DELETING A COMMENT is the sharpest version of that, because it is exactly what
  // the `droppedComments` signal would otherwise trip on. They selected the line and pressed
  // Delete; a dialog explaining that the line is about to be deleted is not a guard, it is a
  // dialog explaining your own keystroke back to you.
  const commented =
    "# the roster, and why\nversion: 1\nblocks:\n  - id: w\n    kind: worker\n    cli: claude\n";
  const commentDeleted = commented.replace("# the roster, and why\n", "");
  assert.equal(
    rewriteImpact(commented, commentDeleted, canon),
    null,
    "they deleted the comment themselves — there is nothing to tell them"
  );

  // …and the same for a hand edit that keeps the comments.
  const renamed = commented.replace("id: w", "id: worker");
  assert.equal(rewriteImpact(commented, renamed, canon), null, "their comments survive; nothing is lost");
});

test("the guard fires on the REWRITE, not on the comment count (rev-15 F7)", () => {
  // `droppedComments` describes the loss; it does not decide whether to warn. A form or canvas
  // save ALWAYS reformats — a commented file is never canonical, and the model always emits
  // canonical text — so a comments-only impact can only ever come from text the human typed
  // themselves, which is precisely the case that must stay silent. Warning on it looked like
  // belt-and-braces and was in fact noise, and noise is how the guard that MATTERS gets clicked
  // through.
  const withComments = "version: 1\n# a note the human wrote\nblocks: []\n";
  const stripped = "version: 1\nblocks: []\n";
  assert.equal(
    rewriteImpact(withComments, stripped, () => false), // neither side canonical → a hand edit
    null,
    "comments dropped, nothing reformatted — so this is the human's own edit, and it is silent"
  );

  // The case that DOES warn is unchanged: canonical text written over a non-canonical file.
  const impact = rewriteImpact(withComments, stripped, (t) => t === stripped);
  assert.ok(impact, "a canonical write over a commented file still warns");
  assert.equal(impact.reformats, true);
  assert.equal(impact.droppedComments, 1, "…and still says what it costs");
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
