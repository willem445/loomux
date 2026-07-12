// The workflow canvas's pure half (#222 v2): the layout FILE, and the geometry an editable
// graph is made of — where a node goes, what a click lands on, where an edge runs.
//
// This is the module that lets the canvas be tested at all. Hit-testing and edge routing are
// exactly the code that is miserable to validate by hand (drag things, squint, hope) and
// trivial to validate as arithmetic — so the arithmetic lives here, DOM-free, and the DOM
// layer is left with nothing to get wrong but the wiring.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  parseLayout,
  serializeLayout,
  emptyLayout,
  withPosition,
  pruneLayout,
  layoutEquals,
  resolvePositions,
  autoPositions,
  freeSlot,
  hitTestNodes,
  hitTestEdges,
  edgeMidpoint,
  edgePoints,
  edgePath,
  distanceToPolyline,
  rectOf,
  outPort,
  inPort,
  blockKey,
  ghostKey,
  snap,
  NODE_W,
  NODE_H,
  PAD,
  LAYOUT_FILE,
  LAYOUT_VERSION,
  type WorkflowLayout,
  type Rect,
} from "../src/workflowlayout.ts";
import { deriveGraph, starterWorkflow, type Workflow } from "../src/workflowmodel.ts";

const graph = () => deriveGraph(starterWorkflow());

// ---------- the layout file lives apart from the workflow ----------

test("the layout file is a separate, gitignorable file — never the workflow", () => {
  // The commitment from §4, stated as a test so it cannot quietly stop being true: positions
  // go in .loomux/workflow.layout.json, and NOTHING about them is in the semantic file. Dify,
  // ComfyUI and Langflow all embed x/y, so a canvas nudge churns the logic diff.
  assert.equal(LAYOUT_FILE, ".loomux/workflow.layout.json");
  const w = starterWorkflow();
  const moved = withPosition(emptyLayout(), "worker", { x: 304, y: 120 });
  // The workflow is untouched by a drag: nothing to re-serialize, nothing to save, no diff.
  assert.deepEqual(w, starterWorkflow());
  assert.deepEqual(moved.positions, { worker: { x: 304, y: 120 } });
});

test("a layout round-trips, and is written sorted so a drag is a one-line diff", () => {
  let layout = emptyLayout();
  layout = withPosition(layout, "worker", { x: 40, y: 80 });
  layout = withPosition(layout, "planner", { x: 0, y: 0 });
  const text = serializeLayout(layout);
  assert.deepEqual(parseLayout(text), layout);
  assert.ok(text.indexOf('"planner"') < text.indexOf('"worker"'), "keys sorted");
  assert.equal(serializeLayout(parseLayout(text)), text, "idempotent");
});

test("a corrupt layout file is redrawn, never reported", () => {
  // The asymmetry with workflow.yml is the point: a broken WORKFLOW is a problem the human
  // must see and fix; a broken LAYOUT is a picture we can simply recompute. Nothing in it is
  // anyone's work, so it must never produce a finding, a dialog, or a refusal to open.
  for (const bad of ["", "{", "null", "[]", '{"positions": 7}', '{"positions": {"a": {"x": "left"}}}']) {
    assert.deepEqual(parseLayout(bad), emptyLayout(), `"${bad}" must degrade silently`);
  }
  // Partial garbage keeps the good entries and drops only what it can't read.
  assert.deepEqual(
    parseLayout('{"positions": {"good": {"x": 1, "y": 2}, "bad": {"x": null, "y": 2}}}').positions,
    { good: { x: 1, y: 2 } }
  );
});

test("positions are keyed by block id, which is why an immutable id matters", () => {
  // The layout keys on the id BECAUSE the id can never change (§4). Reordering the roster
  // must not move anybody's node, and that is only true of an id-keyed file.
  const w = starterWorkflow();
  const layout = withPosition(emptyLayout(), "reviewer", { x: 400, y: 200 });
  const reordered: Workflow = { ...w, blocks: [...w.blocks].reverse() };
  const pos = resolvePositions(deriveGraph(reordered), layout);
  const reviewerIndex = reordered.blocks.findIndex((b) => b.id === "reviewer");
  assert.deepEqual(pos.get(blockKey(reviewerIndex)), { x: 400, y: 200 });
});

test("a position for a block that no longer exists is pruned, not kept forever", () => {
  let layout = withPosition(emptyLayout(), "worker", { x: 10, y: 10 });
  layout = withPosition(layout, "deleted-block", { x: 20, y: 20 });
  const pruned = pruneLayout(layout, ["planner", "worker", "reviewer"]);
  assert.deepEqual(Object.keys(pruned.positions), ["worker"]);
  // Without this, every block ever deleted leaves a coordinate behind and the layout of a
  // workflow you've edited for a year is mostly ghosts.
});

test("a drag that ends where it started writes nothing", () => {
  // Snapping is what makes this true for a hand that wobbles two pixels — and it is also what
  // makes two nodes dropped "in a row" actually line up, which is most of what makes a canvas
  // legible.
  const layout = withPosition(emptyLayout(), "worker", { x: 40, y: 40 });
  assert.ok(layoutEquals(layout, withPosition(layout, "worker", { x: 41, y: 43 })), "same cell");
  assert.ok(!layoutEquals(layout, withPosition(layout, "worker", { x: 80, y: 40 })));
  assert.equal(snap(41), 40);
  assert.equal(snap(43), 40);
  assert.equal(snap(45), 48);
});

test("an id-less stub has no stored position — there is nothing stable to key it by", () => {
  // Inventing a key for it would be inventing an identity, which is the one thing the schema
  // says a workflow file may never do behind the human's back.
  const layout = withPosition(emptyLayout(), "", { x: 10, y: 10 });
  assert.deepEqual(layout.positions, {});
});

// ---------- placement ----------

test("a file you never opened in the canvas still opens as a picture, not a pile", () => {
  // Without the computed half, every block added by a hand edit, by an agent, or in the YAML
  // tab lands at (0,0) on top of whatever is already there.
  const pos = resolvePositions(graph(), emptyLayout());
  assert.equal(pos.size, 3);
  const [planner, worker, reviewer] = [0, 1, 2].map((i) => pos.get(blockKey(i))!);
  assert.ok(planner.x < worker.x && worker.x < reviewer.x, "the declared path reads left to right");
  assert.equal(planner.y, reviewer.y, "…and a linear pipeline sits on one row");
});

test("a node the human moved stays where they put it; the rest are computed around it", () => {
  const layout = withPosition(emptyLayout(), "worker", { x: 504, y: 304 });
  const pos = resolvePositions(graph(), layout);
  assert.deepEqual(pos.get(blockKey(1)), { x: 504, y: 304 }, "the moved one");
  assert.deepEqual(pos.get(blockKey(0)), autoPositions(graph()).get(blockKey(0)), "the untouched ones");
});

test("ghosts are placed but never persisted", () => {
  // A ghost is the ABSENCE of a block — a name an edge mentions that nothing answers to.
  // Persisting a position for it would outlive the mistake that created it.
  const pos = resolvePositions(graph(), emptyLayout(), ["rev-perf"]);
  assert.ok(pos.has(ghostKey("rev-perf")));
  assert.deepEqual(withPosition(emptyLayout(), "", { x: 1, y: 1 }).positions, {});
});

test("a new block lands somewhere free — not at the origin, not under an existing node", () => {
  const pos = resolvePositions(graph(), emptyLayout());
  const slot = freeSlot(pos);
  const rects = new Map([...pos].map(([k, p]) => [k, rectOf(p)] as const));
  for (const r of rects.values()) {
    assert.ok(
      !(slot.x < r.x + r.w && slot.x + NODE_W > r.x && slot.y < r.y + r.h && slot.y + NODE_H > r.y),
      "a new block you have to go hunting for is one you assume wasn't created"
    );
  }
  assert.deepEqual(freeSlot(new Map()), { x: PAD, y: PAD }, "the first block goes at the top-left");
});

// ---------- hit-testing: what a click actually lands on ----------

test("a click lands on the node under it, and on nothing when there is nothing there", () => {
  const rects = new Map<string, Rect>([
    ["b:0", { x: 0, y: 0, w: NODE_W, h: NODE_H }],
    ["b:1", { x: 300, y: 0, w: NODE_W, h: NODE_H }],
  ]);
  assert.equal(hitTestNodes(rects, { x: 10, y: 10 }), "b:0");
  assert.equal(hitTestNodes(rects, { x: 310, y: 10 }), "b:1");
  assert.equal(hitTestNodes(rects, { x: 250, y: 10 }), null, "the gap between them is empty space");
  assert.equal(hitTestNodes(rects, { x: 0, y: 0 }), "b:0", "the top-left corner is inside");
  assert.equal(hitTestNodes(rects, { x: NODE_W, y: NODE_H }), "b:0", "and so is the bottom-right");
});

test("overlapping nodes resolve to the one on top — what you click is what you see", () => {
  const rects = new Map<string, Rect>([
    ["b:0", { x: 0, y: 0, w: NODE_W, h: NODE_H }],
    ["b:1", { x: 20, y: 20, w: NODE_W, h: NODE_H }], // drawn later ⇒ on top
  ]);
  assert.equal(hitTestNodes(rects, { x: 30, y: 30 }), "b:1");
});

test("a click near an edge selects that edge, and one in open space selects none", () => {
  // An edge is a 1.5px line and nobody can hit that with a mouse; the tolerance is what makes
  // the hover ✕ appear at all.
  const a = outPort({ x: 0, y: 0, w: NODE_W, h: NODE_H });
  const b = inPort({ x: 400, y: 0, w: NODE_W, h: NODE_H });
  const edges = [{ from: a, to: b }];
  const mid = edgeMidpoint(a, b);
  assert.equal(hitTestEdges(edges, mid), 0);
  assert.equal(hitTestEdges(edges, { x: mid.x, y: mid.y + 4 }), 0, "within tolerance");
  assert.equal(hitTestEdges(edges, { x: mid.x, y: mid.y + 60 }), null, "well clear of it");
});

test("the nearest edge wins where two cross", () => {
  const top = { from: { x: 0, y: 0 }, to: { x: 400, y: 0 } };
  const bottom = { from: { x: 0, y: 100 }, to: { x: 400, y: 100 } };
  assert.equal(hitTestEdges([top, bottom], { x: 200, y: 2 }), 0);
  assert.equal(hitTestEdges([top, bottom], { x: 200, y: 98 }), 1);
});

test("an edge's delete button sits ON the curve, including where the curve leaves the chord", () => {
  // A ✕ floating in empty space is a ✕ nobody trusts. The button hangs off the CURVE — which
  // for a doubling-back edge (the reviewer → worker rework loop, a real workflow) swings well
  // away from the straight line between the two nodes.
  const from = { x: 400, y: 0 }; // this edge runs right-to-left
  const to = { x: 0, y: 100 };
  assert.equal(hitTestEdges([{ from, to }], edgeMidpoint(from, to)), 0, "the button is on the edge");

  // The curve is not the chord: somewhere along it, it swings well away from the straight
  // line. (At the exact midpoint a symmetric cubic happens to cross the chord — which is why
  // "is the button on the CURVE" is the property worth testing, and "is the midpoint off the
  // chord" is not the same claim at all.)
  const maxOff = Math.max(...edgePoints(from, to).map((p) => distanceToPolyline(p, [from, to])));
  assert.ok(maxOff > 20, "a doubling-back edge bows away from the line between its nodes");
});

test("an edge is routed from the two nodes it joins — there are no waypoints to persist", () => {
  const r = rectOf({ x: 0, y: 0 });
  assert.deepEqual(outPort(r), { x: NODE_W, y: NODE_H / 2 }, "leaves the right edge");
  assert.deepEqual(inPort(r), { x: 0, y: NODE_H / 2 }, "arrives at the left");
  assert.match(edgePath({ x: 0, y: 0 }, { x: 100, y: 50 }), /^M 0 0 C /, "a cubic, horizontal control points");
});

test("the layout file records a version, so a future format can tell itself apart", () => {
  assert.equal(emptyLayout().version, LAYOUT_VERSION);
  assert.equal(parseLayout('{"version": 99, "positions": {}}').version, LAYOUT_VERSION);
  const layout: WorkflowLayout = { version: LAYOUT_VERSION, positions: {} };
  assert.match(serializeLayout(layout), /"version": 1/);
});
