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
  hitTestDropTarget,
  gateRect,
  PORT_DROP_TOLERANCE,
  GATE_KEY,
  GATE_GAP,
  GATE_ROW_H,
  GATE_CHROME_H,
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
  LAYOUT_BASENAME,
  layoutFileFor,
  LAYOUT_VERSION,
  type WorkflowLayout,
  type Rect,
  type Point,
} from "../src/workflowlayout.ts";
import { deriveGraph, starterWorkflow, analyzeWorkflow, type Workflow } from "../src/workflowmodel.ts";

const graph = () => deriveGraph(starterWorkflow());

// ---------- the layout file lives apart from the workflow ----------

test("the layout file is a separate, gitignorable file — never the workflow", () => {
  // The commitment from §4, stated as a test so it cannot quietly stop being true: positions
  // go in a workflow.layout.json BESIDE the workflow, and NOTHING about them is in the
  // semantic file. Dify, ComfyUI and Langflow all embed x/y, so a canvas nudge churns the
  // logic diff.
  assert.equal(layoutFileFor(".orrerix/workflow.yml"), ".orrerix/workflow.layout.json");
  const w = starterWorkflow();
  const moved = withPosition(emptyLayout(), "worker", { x: 304, y: 120 });
  // The workflow is untouched by a drag: nothing to re-serialize, nothing to save, no diff.
  assert.deepEqual(w, starterWorkflow());
  assert.deepEqual(own(moved.positions), { worker: { x: 304, y: 120 } });
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
    own(parseLayout('{"positions": {"good": {"x": 1, "y": 2}, "bad": {"x": null, "y": 2}}}').positions),
    { good: { x: 1, y: 2 } }
  );
});

/** Own entries as a plain object. The position table has NO PROTOTYPE (that is the F3 fix), and
 *  `deepStrictEqual` compares prototypes — so a null-proto table never equals an object literal
 *  however identical its contents. Comparing the CONTENTS is what these tests mean. */
const own = (positions: Record<string, Point>): Record<string, Point> => ({ ...positions });

test("a block whose id names an Object.prototype member is still just a block (rev-15 F3)", () => {
  // `id: constructor` is a LEGAL workflow — `isValidBlockId` says yes and the validator reports
  // zero findings — and it can arrive from a hand edit, the raw YAML, or an agent, so tightening
  // the +Block dialog would not have closed this. On a plain object literal,
  // `positions["constructor"]` returned the INHERITED Object function: truthy, so the caller took
  // the "it has a stored position" branch and read `{x: undefined, y: undefined}` off it. The NaN
  // reached the SVG's width/height and the canvas did not render at all — for a workflow that is
  // entirely valid, keyed by an id that can never be changed.
  const a = analyzeWorkflow(
    "version: 1\nname: x\nblocks:\n  - id: constructor\n    kind: worker\n    cli: claude\n" +
      "  - id: worker\n    kind: worker\n    cli: claude\n"
  );
  assert.deepEqual(a.findings, [], "this is a VALID workflow — that is what makes it a real bug");

  const pos = resolvePositions(a.graph, emptyLayout());
  for (const p of pos.values()) {
    assert.ok(Number.isFinite(p.x) && Number.isFinite(p.y), `not a coordinate: ${JSON.stringify(p)}`);
  }
  assert.ok(Number.isFinite(freeSlot(pos).y), "…and the NEXT block must still be placeable");

  // A position stored FOR it round-trips like any other block's.
  const layout = withPosition(emptyLayout(), "constructor", { x: 80, y: 40 });
  assert.deepEqual(own(parseLayout(serializeLayout(layout)).positions), { constructor: { x: 80, y: 40 } });
  assert.deepEqual(resolvePositions(a.graph, layout).get(blockKey(0)), { x: 80, y: 40 });
});

test("an id the validator REJECTS still cannot produce a NaN coordinate", () => {
  // `__proto__`, `hasOwnProperty` and `toString` are not legal block ids (leading underscore,
  // uppercase) — the validator says so. But a broken file still OPENS, as stubs with findings,
  // which means the canvas still has to draw them: "we reject it" is not the same as "it never
  // reaches the geometry", and the whole contract of this pane is that it renders files it
  // disapproves of.
  for (const id of ["__proto__", "hasOwnProperty", "toString"]) {
    const a = analyzeWorkflow(
      `version: 1\nblocks:\n  - id: ${id}\n    kind: worker\n    cli: claude\n` +
        `  - id: worker\n    kind: worker\n    cli: claude\n`
    );
    assert.ok(
      a.findings.some((f) => f.code === "block-id-invalid"),
      `"${id}" should be reported as an invalid id`
    );
    const layout = withPosition(emptyLayout(), id, { x: 24, y: 24 });
    for (const p of resolvePositions(a.graph, layout).values()) {
      assert.ok(Number.isFinite(p.x) && Number.isFinite(p.y), `${id}: ${JSON.stringify(p)}`);
    }
  }
});

test("a hostile KEY in the layout file on disk is data, not a prototype member", () => {
  // The layout file is JSON that arrives from the repo. `__proto__` in it must land as an
  // ordinary entry that no lookup can confuse with an inherited one — and must pollute nothing.
  const layout = parseLayout('{"positions": {"__proto__": {"x": 1, "y": 2}, "a": {"x": 3, "y": 4}}}');
  assert.deepEqual(own(layout.positions).a, { x: 3, y: 4 });
  assert.equal(Object.getPrototypeOf(layout.positions), null, "the table has no prototype to inherit from");
  assert.equal(({} as Record<string, unknown>).x, undefined, "and nothing anywhere was polluted");
  assert.deepEqual(own(pruneLayout(layout, ["a"]).positions), { a: { x: 3, y: 4 } });
  assert.equal(serializeLayout(parseLayout(serializeLayout(layout))), serializeLayout(layout));
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
  assert.deepEqual(own(layout.positions), {});
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
  assert.deepEqual(own(withPosition(emptyLayout(), "", { x: 1, y: 1 }).positions), {});
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

// ---------- where a rubber band may be RELEASED (#1387, #1388) ----------

test("#1387: a release on the in-port the arrow points at connects — the body alone did not", () => {
  // The defect, stated as arithmetic. The in-port is drawn on the node's LEFT EDGE, so the
  // dot the arrowhead visibly points at is half outside the body rect that used to be the
  // whole drop target: a release aimed at it landed on nothing, and the release that DID
  // work (the far side of the box) is the one nothing on screen suggests.
  const rects = new Map<string, Rect>([["b:0", { x: 300, y: 100, w: NODE_W, h: NODE_H }]]);
  const port = inPort(rects.get("b:0")!);

  // The pin on the defect itself, so this test fails if the OLD target is ever restored as
  // the drop rule: the body still does not contain a point 4px to the port's left.
  const justLeft = { x: port.x - 4, y: port.y };
  assert.equal(hitTestNodes(rects, justLeft), null, "the body rect never did accept this drop");
  assert.equal(hitTestDropTarget(rects, justLeft), "b:0", "the drop target does");

  // The issue asks for at least 8px of slack; the constant is what the view uses, so the
  // bound is measured against IT rather than against a number retyped here.
  assert.ok(PORT_DROP_TOLERANCE >= 8, "the tolerance #1387 asks for, as a floor");
  const edge = { x: port.x - PORT_DROP_TOLERANCE, y: port.y };
  assert.equal(hitTestDropTarget(rects, edge), "b:0", "the tolerance is inclusive at its edge");
  const beyond = { x: port.x - PORT_DROP_TOLERANCE - 1, y: port.y };
  assert.equal(hitTestDropTarget(rects, beyond), null, "and it is a tolerance, not a whole column");

  // Diagonal, not just horizontal — the tolerance is a radius, and a human releasing at the
  // arrowhead is as likely to be a pixel high as a pixel left.
  assert.equal(hitTestDropTarget(rects, { x: port.x - 5, y: port.y - 5 }), "b:0");
  assert.equal(hitTestDropTarget(rects, { x: port.x - 9, y: port.y - 9 }), null, "12.7px away");

  // And the body still works, everywhere in it — that was the one gesture that DID connect
  // before, and #1387 must not trade one drop for another.
  assert.equal(hitTestDropTarget(rects, { x: 300 + NODE_W - 1, y: 100 + NODE_H - 1 }), "b:0");
});

test("the port tolerance only ADDS drops — it never moves one that already landed", () => {
  // The property that makes #1387 safe to ship: wherever the old rule (the node's body) had
  // an answer, the new one gives the SAME answer, and the tolerance fires only where the old
  // rule found nothing at all. Stated over a grid rather than at a point, because the
  // interesting region is where two overlapping nodes and a port radius all disagree — and a
  // single hand-picked coordinate there proves whichever rule you picked it to prove.
  const rects = new Map<string, Rect>([
    ["b:0", { x: 0, y: 0, w: NODE_W, h: NODE_H }],
    // Overlapping, drawn later ⇒ on top. Its in-port sits INSIDE b:0's body.
    ["b:1", { x: 40, y: 0, w: NODE_W, h: NODE_H }],
  ]);
  let bodies = 0;
  let added = 0;
  for (let x = -20; x <= NODE_W + 80; x += 2) {
    for (let y = -20; y <= NODE_H + 20; y += 2) {
      const p = { x, y };
      const before = hitTestNodes(rects, p);
      const after = hitTestDropTarget(rects, p);
      if (before) {
        bodies++;
        assert.equal(after, before, `a drop that landed on ${before} at (${x},${y}) still does`);
      } else if (after) {
        added++;
      }
    }
  }
  // Positive controls: the sweep must have crossed both regions, or "no disagreement" would
  // be a fact about an empty loop rather than about the rule.
  assert.ok(bodies > 100, `the sweep covered real body area (${bodies} points)`);
  assert.ok(added > 0, `and it covered points only the tolerance can reach (${added} points)`);

  // The overlap itself, named: b:1 is on top, so its own port and body both resolve to it.
  const port1 = inPort(rects.get("b:1")!);
  assert.equal(hitTestDropTarget(rects, port1), "b:1", "the overlap resolves to the top node");
  assert.equal(hitTestDropTarget(rects, { x: 10, y: 4 }), "b:0", "clear of b:1, it is b:0's body");
});

test("the merge gate is a drop target with the same geometry it is drawn with (#1388)", () => {
  // The box's rect is ONE function now, because since #1388 it is a picture and a drop target
  // at once — a hit-test that agrees with the drawing by coincidence is the defect class this
  // module exists to remove.
  const nodes: Point[] = [
    { x: PAD, y: PAD },
    { x: 400, y: PAD },
  ];
  const gr = gateRect(nodes, 2);
  assert.equal(gr.x, 400 + NODE_W + GATE_GAP, "to the right of the RIGHTMOST node, not the last one");
  assert.equal(gr.y, PAD);
  assert.equal(gr.w, NODE_W);
  assert.equal(gr.h, 2 * GATE_ROW_H + GATE_CHROME_H, "one row per reviewer, plus the box's chrome");

  // A gate naming nobody is still a box you can see and drop on: it is the gate you are about
  // to give its first reviewer, and a zero-height one could never be hit at all.
  assert.equal(gateRect(nodes, 0).h, Math.max(NODE_H, GATE_ROW_H + GATE_CHROME_H));
  assert.equal(gateRect([], 1).x, PAD + GATE_GAP, "with no nodes it still has a place to be");

  // In a drop map it behaves exactly like a node, in-port slack included.
  const rects = new Map<string, Rect>([
    ["b:0", { x: PAD, y: PAD, w: NODE_W, h: NODE_H }],
    [GATE_KEY, gr],
  ]);
  assert.equal(hitTestDropTarget(rects, { x: gr.x + 20, y: gr.y + 20 }), GATE_KEY, "on the box");
  const port = inPort(gr);
  assert.equal(hitTestDropTarget(rects, { x: port.x - 6, y: port.y }), GATE_KEY, "just short of it");
  assert.equal(hitTestDropTarget(rects, { x: gr.x - 40, y: gr.y }), null, "and nowhere near it");
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

test("the layout file STAMPS a version on write and deliberately ignores it on read", () => {
  // The honest name for what this asserts (rev-15 minor): the version is written, and a file
  // claiming to be a FUTURE version is still read — its positions are taken and its version is
  // discarded. That is the right behaviour for a disposable picture (a v2 file still has x/y in
  // it, and the worst case of misreading one is a node in the wrong place), but the old test
  // name promised a format check that does not exist and would have misled whoever writes v2.
  assert.equal(emptyLayout().version, LAYOUT_VERSION);
  const future = parseLayout('{"version": 99, "positions": {"a": {"x": 8, "y": 8}}}');
  assert.equal(future.version, LAYOUT_VERSION, "read as this build's format…");
  assert.deepEqual(own(future.positions), { a: { x: 8, y: 8 } }, "…and its positions are used anyway");
  assert.match(serializeLayout(emptyLayout()), /"version": 1/);
});

// ---------- the layout file follows the workflow it belongs to (#1153 phase 4) ----------

test("the layout file is the workflow file's SIBLING, whichever config dir the repo uses", () => {
  // The defect a hard-coded layout path had: a repo on the legacy `.loomux/` spelling would
  // have had its canvas positions written into `.orrerix/` — a directory it may not have —
  // silently separating the two files that only make sense together.
  assert.equal(layoutFileFor(".loomux/workflow.yml"), ".loomux/workflow.layout.json");
  assert.equal(layoutFileFor(".orrerix/workflow.yml"), ".orrerix/workflow.layout.json");
});

test("an explicitly-opened workflow keeps its layout beside IT, at any depth or separator", () => {
  // The pane can be pointed at any file (a restored tab, the editor's `getFile`), including
  // one nested or spelled with backslashes on Windows. The layout must follow it there.
  assert.equal(layoutFileFor("teams/api/.orrerix/workflow.yml"), "teams/api/.orrerix/workflow.layout.json");
  assert.equal(layoutFileFor(String.raw`.orrerix\workflow.yml`), ".orrerix/workflow.layout.json");
  // A workflow at the repo root has no directory to sit in — the basename alone, never a
  // leading "/" that would escape the root.
  assert.equal(layoutFileFor("workflow.yml"), LAYOUT_BASENAME);
});
