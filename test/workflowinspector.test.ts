// The docked inspector's pure DECISIONS (#880 slice B): what it shows, what it calls the thing
// it is showing, which surface a finding navigates to, and when Delete is allowed to erase.
//
// These four exist because the Blocks/YAML/Graph tab split died, and everything the tabs used
// to decide implicitly had to become something stated. The one that cost the most before it was
// stated: with the editor behind a tab, *selecting* a block and *showing* it were two acts, and
// the canvas performed only the first — `onCanvasDown` set the selection and re-rendered the
// form off screen, so clicking a node appeared to do nothing. That is the interaction #880 was
// opened about, and it was one forgotten call, in one of two handlers, for two years.
//
// WHY THIS FILE IMPORTS A NAMESPACE rather than named bindings. Every other test file here
// imports what it needs by name, which is nicer to read — but a named import of an export that
// does not exist is a LINK error, so on the base commit this file would fail before running a
// single assertion, and "it failed to load" is not evidence that a behavior is missing. Through
// the namespace the module loads either way and each test fails where it actually asks the
// question (`pane.inspectorTarget is not a function`), which is a real, behavioral red: the
// decision is not there. The rest of the suite has no reason to copy this.
import { test } from "node:test";
import assert from "node:assert/strict";
import * as pane from "../src/workflowpane.ts";
import { parseWorkflow, type Workflow } from "../src/workflowmodel.ts";

/** A small, VALID workflow: two blocks, one edge, a merge gate naming the reviewer. */
function fixture(): Workflow {
  return parseWorkflow(
    [
      "version: 1",
      "name: demo",
      "blocks:",
      "  - id: worker",
      "    name: Worker",
      "    kind: worker",
      "    cli: claude",
      "  - id: rev-lead",
      "    name: Review lead",
      "    kind: reviewer",
      "    cli: copilot",
      "edges:",
      "  - from: worker",
      "    to: rev-lead",
      "gates:",
      "  merge:",
      "    require: all-pass",
      "    reviewers: [rev-lead]",
      "",
    ].join("\n")
  ).workflow;
}

// ---------- what the inspector shows ----------

test("a selected block IS what the inspector shows — no second act to forget", () => {
  // The #880 headline, as a rule. Under the tabs this was two things (select, then switch tab)
  // and the canvas did one of them; with the inspector docked, the selection is the whole of it.
  const w = fixture();
  assert.deepEqual(pane.inspectorTarget({ kind: "block", index: 1 }, w, false), {
    kind: "block",
    index: 1,
  });
  assert.deepEqual(pane.inspectorTarget({ kind: "gate" }, w, false), { kind: "gate" });
  assert.deepEqual(pane.inspectorTarget({ kind: "edge", from: "worker", to: "rev-lead" }, w, false), {
    kind: "edge",
    from: "worker",
    to: "rev-lead",
  });
  assert.deepEqual(pane.inspectorTarget({ kind: "workflow" }, w, false), { kind: "workflow" });
});

test("a selection that outlived its block falls back — the inspector never renders over nothing", () => {
  // Deleting a block (here, or by hand in the YAML view) leaves the selection pointing past the
  // end of the roster. The failure mode this closes is not cosmetic: the block editor reads
  // `blocks[index]`, so rendering it anyway is a form full of `undefined` whose first keystroke
  // writes a field onto a block that isn't there.
  const w = fixture();
  assert.deepEqual(pane.inspectorTarget({ kind: "block", index: 7 }, w, false), { kind: "workflow" });
  assert.deepEqual(pane.inspectorTarget({ kind: "block", index: 2 }, w, false), { kind: "workflow" });
});

test("a selection that outlived its edge falls back too", () => {
  // Same shape, different list — and the edge case is the one an index-keyed selection would
  // have missed, since the edge is held by the pair of ids it joins.
  const w = fixture();
  assert.deepEqual(pane.inspectorTarget({ kind: "edge", from: "worker", to: "gone" }, w, false), {
    kind: "workflow",
  });
  // Direction is part of the identity: the reverse edge is a DIFFERENT edge, and this workflow
  // does not declare it.
  assert.deepEqual(pane.inspectorTarget({ kind: "edge", from: "rev-lead", to: "worker" }, w, false), {
    kind: "workflow",
  });
});

test("unparseable YAML blocks the inspector outright — it does not fall back to another editor", () => {
  // The one rule this pane has never been allowed to bend: an inspector edit serializes the
  // model back over the buffer, so editing a model we only half understood would destroy the
  // broken text the human is in the middle of fixing. Note this holds for EVERY selection,
  // including the workflow's own settings — the tempting "well, the name field is harmless"
  // fallback writes the whole file just the same.
  const w = fixture();
  for (const sel of [
    { kind: "workflow" } as const,
    { kind: "block", index: 0 } as const,
    { kind: "gate" } as const,
    { kind: "edge", from: "worker", to: "rev-lead" } as const,
  ]) {
    assert.deepEqual(pane.inspectorTarget(sel, w, true), { kind: "blocked" });
  }
});

test("#1388: a gate SEAT is a selection of its own, and it outlives nothing", () => {
  // The amber line from a reviewer to the gate box was drawn and unclickable — the one
  // ENFORCED thing on the canvas was the one thing you could not point at. It is a selection
  // now, held by the reviewer's id, because that is the whole of what a seat is: there is one
  // merge gate, and a seat on it is an entry in `gates.merge.reviewers`.
  const w = fixture();
  assert.deepEqual(pane.inspectorTarget({ kind: "gate-edge", reviewer: "rev-lead" }, w, false), {
    kind: "gate-edge",
    reviewer: "rev-lead",
  });
  // The same fallback an erased edge gets, and it has three ways to happen: erased here,
  // unticked in the gate form, or edited out of the YAML.
  assert.deepEqual(pane.inspectorTarget({ kind: "gate-edge", reviewer: "worker" }, w, false), {
    kind: "workflow",
  });
  assert.deepEqual(pane.inspectorTarget({ kind: "gate-edge", reviewer: "gone" }, w, false), {
    kind: "workflow",
  });
  // A file with no gate at all cannot be showing a seat on one.
  const ungated: Workflow = { ...w, gates: {} };
  assert.deepEqual(pane.inspectorTarget({ kind: "gate-edge", reviewer: "rev-lead" }, ungated, false), {
    kind: "workflow",
  });
  // And the blocked rule covers it like every other selection: an inspector edit serializes
  // the model back over text the human is mid-repair on.
  assert.deepEqual(pane.inspectorTarget({ kind: "gate-edge", reviewer: "rev-lead" }, w, true), {
    kind: "blocked",
  });
});

test("#1388: the heading says which line you clicked — advisory, or the one that stops a merge", () => {
  // The sub-line is the one place the canvas's two kinds of line are named side by side, and
  // which of them can actually block a merge is the thing worth learning from a click.
  const w = fixture();
  const seat = pane.inspectorHeading({ kind: "gate-edge", reviewer: "rev-lead" }, w);
  assert.equal(seat.title, "rev-lead → merge gate");
  assert.match(seat.sub, /enforced/);
  const edge = pane.inspectorHeading({ kind: "edge", from: "worker", to: "rev-lead" }, w);
  assert.match(edge.sub, /advisory/);
  assert.notEqual(seat.sub, edge.sub, "the two lines do not get the same label");
});

// ---------- what the inspector calls it ----------

test("the inspector names the selected block by its ID, not only its name", () => {
  // What the docked header is FOR: the canvas is still on screen beside it, so "did I click the
  // node I meant?" has to be answerable at a glance. The id is the answer — it is what the
  // edges and the merge gate reference, and it is the thing that can never be changed later.
  const w = fixture();
  const h = pane.inspectorHeading({ kind: "block", index: 1 }, w);
  assert.equal(h.title, "Review lead");
  assert.match(h.sub, /rev-lead/);
});

test("a block with no id says so, instead of borrowing its name", () => {
  // The blocks that most need repairing are the ones whose id is missing, and a header that
  // quietly showed the display name would hide exactly that.
  const w = parseWorkflow(
    ["version: 1", "blocks:", "  - name: Nameless", "    kind: worker", "    cli: claude", ""].join(
      "\n"
    )
  ).workflow;
  const h = pane.inspectorHeading({ kind: "block", index: 0 }, w);
  assert.equal(h.title, "Nameless");
  assert.match(h.sub, /no id/);
  assert.doesNotMatch(h.sub, /Nameless/);
});

test("every target names itself — an inspector with a blank header is a pane you can't read", () => {
  const w = fixture();
  for (const target of [
    { kind: "workflow" } as const,
    { kind: "block", index: 0 } as const,
    { kind: "gate" } as const,
    { kind: "edge", from: "worker", to: "rev-lead" } as const,
    { kind: "blocked" } as const,
  ]) {
    const h = pane.inspectorHeading(target, w);
    assert.ok(h.title.trim().length > 0, `${target.kind} has no title`);
    assert.ok(h.sub.trim().length > 0, `${target.kind} has no sub-line`);
  }
  // The edge header says which way it points — an edge is a direction, and `a → b` is not `b → a`.
  assert.equal(
    pane.inspectorHeading({ kind: "edge", from: "worker", to: "rev-lead" }, w).title,
    "worker → rev-lead"
  );
});

// ---------- the policy sections (#1020) ----------

test("the three policy sections are selections like any other — and survive not being declared", () => {
  // `intake:`, `merge_queue:` and `resources:` are OPTIONAL sections: the common case is a file
  // that declares none of them, and their forms are how you declare one. So — unlike a block or
  // an edge — an undeclared section must NOT fall back to the workflow's own settings: falling
  // back would make the row unclickable in exactly the state the human needs it (there is
  // nothing there yet, and they want to add it).
  const w = fixture();
  assert.equal(w.intake, undefined, "the fixture declares none of them");
  for (const sel of [
    { kind: "intake" } as const,
    { kind: "merge_queue" } as const,
    { kind: "resources" } as const,
  ]) {
    assert.deepEqual(pane.inspectorTarget(sel, w, false), sel, `${sel.kind} must stay selected`);
    // …and the one rule that has no exceptions: a buffer that doesn't parse blocks the editor,
    // whatever is selected. These sections write the whole file back like every other form.
    assert.deepEqual(pane.inspectorTarget(sel, w, true), { kind: "blocked" });
  }
});

test("each policy section names itself in the header", () => {
  const w = fixture();
  const headings = (["intake", "merge_queue", "resources"] as const).map((kind) =>
    pane.inspectorHeading({ kind }, w)
  );
  for (const h of headings) {
    assert.ok(h.title.trim().length > 0, "a policy section with no title is an unreadable pane");
    assert.ok(h.sub.trim().length > 0);
  }
  // Titles are distinct: three rows in one roster column that read the same are three rows the
  // human cannot tell apart.
  assert.equal(new Set(headings.map((h) => h.title)).size, 3);
});

// ---------- where a finding navigates ----------

test("a finding that names a LINE goes to the YAML; one that names a BLOCK moves nothing", () => {
  // The `setTab("form")` that used to sit at the end of the finding handler is now a no-op by
  // construction: the inspector is docked, so selecting the block IS the navigation. Switching
  // surface as well would yank the human off the canvas they were reading to show them an
  // editor that was already on screen.
  assert.equal(pane.surfaceForFinding({ line: 12 }), "yaml");
  assert.equal(pane.surfaceForFinding({}), null);
  assert.equal(pane.surfaceForFinding({ line: 0 }), null); // no such line — nothing to focus
});

// ---------- when Delete may erase ----------

test("Delete erases the canvas selection only on the canvas, and never inside a field", () => {
  // The in-field half is not belt-and-braces, and it matters MORE now than it did under the
  // tabs: the form and the graph used to be mutually exclusive tabs, so "typing in a prompt"
  // and "looking at the canvas" could not co-occur. Docked, they always do — the block editor
  // sits beside the block it would delete.
  assert.equal(pane.canvasDeleteAllowed({ surface: "canvas", inField: false }), true);
  assert.equal(pane.canvasDeleteAllowed({ surface: "canvas", inField: true }), false);
  assert.equal(pane.canvasDeleteAllowed({ surface: "yaml", inField: false }), false);
  assert.equal(pane.canvasDeleteAllowed({ surface: "yaml", inField: true }), false);
});
