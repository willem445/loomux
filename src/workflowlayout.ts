// The workflow graph's GEOMETRY and its LAYOUT FILE (#222 v2). Pure and DOM-free, so the
// hit-testing and edge-routing maths that an editable canvas lives or dies by are unit-
// tested (test/workflowlayout.test.ts) instead of being validated by dragging things around
// with a mouse and hoping.
//
// THE SEPARATION THIS MODULE EXISTS TO ENFORCE: where a node sits is NOT part of the
// workflow. `.loomux/workflow.yml` says what the workflow IS — blocks, edges, gates — and
// never where anything is drawn; positions live here, in `.loomux/workflow.layout.json`,
// which is gitignorable and which nothing but the canvas ever reads. Dify, ComfyUI and
// Langflow all embed x/y in the semantic file, so nudging a node churns the diff of the
// logic — and a teammate pulling your branch gets a "change" that is you having moved a box
// two pixels. (§4, and the reason the investigation put it in writing.)
//
// The layout file is keyed by BLOCK ID, which is safe precisely because ids are immutable
// (§4 again): a rename cannot orphan a position, and a reorder cannot swap two. A block with
// no id yet — a stub in a broken file — simply has no stored position and is auto-placed;
// there is nothing stable to key it by, and inventing a key would be inventing an identity.

import type { WorkflowGraph } from "./workflowmodel";

/** Where the canvas keeps its positions. Sits beside the workflow, is not part of it. */
export const LAYOUT_FILE = ".loomux/workflow.layout.json";

export const LAYOUT_VERSION = 1;

export interface Point {
  x: number;
  y: number;
}

export interface Rect extends Point {
  w: number;
  h: number;
}

/** The layout file: block id → where its node sits. Nothing else — no sizes (they are a
 *  constant), no colours (they come from the kind), no edge waypoints (an edge is routed
 *  from the two nodes it connects). Every one of those would be a second thing to keep in
 *  sync with a file that already knows the answer. */
export interface WorkflowLayout {
  version: number;
  positions: Record<string, Point>;
}

/** A position table with NO PROTOTYPE, and every read of it goes through `Object.hasOwn`.
 *
 *  Not defensive theatre — a plain object literal here is a live bug (rev-15 F3). A block id
 *  is arbitrary text from the file: `id: constructor` is a perfectly legal, zero-findings
 *  workflow (`isValidBlockId("constructor")` is true, and it can arrive from a hand edit, the
 *  YAML tab, or an agent — nothing forces it through the +Block dialog). On a plain object,
 *  `positions["constructor"]` returns the INHERITED `Object` function: truthy, so the caller
 *  takes the "it has a stored position" branch, and reads `{ x: undefined, y: undefined }` off
 *  it. That NaN then propagates into the SVG's width and height and the canvas does not render
 *  at all — for a workflow that is entirely valid, and for an id that can never be changed.
 *
 *  Fixing the id validator would not fix this; only the lookup can. So the lookup is fixed. */
const table = (): Record<string, Point> => Object.create(null) as Record<string, Point>;

/** The stored position for `id`, or undefined — the ONE place `positions` is read by key. */
const storedAt = (layout: WorkflowLayout, id: string): Point | undefined =>
  id && Object.hasOwn(layout.positions, id) ? layout.positions[id] : undefined;

export const emptyLayout = (): WorkflowLayout => ({ version: LAYOUT_VERSION, positions: table() });

// ---------- geometry ----------

export const NODE_W = 168;
export const NODE_H = 52;
/** Gap between auto-placed columns / rows. Only the AUTO placement uses these; once a human
 *  has dragged a node, its position is whatever they dragged it to. */
export const COL_GAP = 72;
export const ROW_GAP = 22;
export const PAD = 16;
/** How close to an edge a click has to land to mean that edge. Generous, because an edge is
 *  a 1.5px line and nobody can hit that; the ✕ that appears on hover is the real target and
 *  this is what makes it appear. */
export const EDGE_HIT_TOLERANCE = 8;
/** The grid a dragged node snaps to. Small enough to feel free, large enough that two nodes
 *  dropped "in a row" actually line up — which is the whole reason a canvas is legible. */
export const SNAP = 8;

export const snap = (v: number): number => Math.round(v / SNAP) * SNAP;

/** Nodes are addressed by ROW, not by id — two id-less stubs share an id ("") and a
 *  duplicate-id pair shares one too, and both are exactly the blocks a broken file needs you
 *  to be able to see and click SEPARATELY. A ghost (a name an edge mentions that no block
 *  answers to) is not a row at all, so it is keyed by the name itself. */
export const blockKey = (index: number): string => `b:${index}`;
export const ghostKey = (id: string): string => `g:${id}`;

export const rectOf = (p: Point): Rect => ({ x: p.x, y: p.y, w: NODE_W, h: NODE_H });

export function pointInRect(p: Point, r: Rect): boolean {
  return p.x >= r.x && p.x <= r.x + r.w && p.y >= r.y && p.y <= r.y + r.h;
}

/** The node under `p`, or null. Later entries win: they are drawn last, so they are on top,
 *  and what you click must be what you see. */
export function hitTestNodes(rects: ReadonlyMap<string, Rect>, p: Point): string | null {
  let hit: string | null = null;
  for (const [key, r] of rects) if (pointInRect(p, r)) hit = key;
  return hit;
}

/** Where an edge leaves a node (right edge, vertically centred) and where it arrives (left
 *  edge). One port each, deliberately: multiple named ports would imply the edges MEAN
 *  different things, and they don't — an edge is an edge, and the gate (which does mean
 *  something else) is not drawn as one of these at all. */
export const outPort = (r: Rect): Point => ({ x: r.x + r.w, y: r.y + r.h / 2 });
export const inPort = (r: Rect): Point => ({ x: r.x, y: r.y + r.h / 2 });

/** The cubic the view draws, as an SVG `d`. Horizontal control points, so an edge leaves a
 *  node going RIGHT and arrives going right — which is what makes a left-to-right graph read
 *  as a flow rather than as a plate of spaghetti, including when it doubles back (the
 *  reviewer → worker rework loop, which is a real workflow and must not look like a mistake). */
export function edgePath(from: Point, to: Point): string {
  const dx = Math.max(40, Math.abs(to.x - from.x) * 0.5);
  return `M ${from.x} ${from.y} C ${from.x + dx} ${from.y}, ${to.x - dx} ${to.y}, ${to.x} ${to.y}`;
}

/** The same curve, sampled — the pure basis for hit-testing an edge and for finding the
 *  midpoint its delete button hangs off. `steps` trades accuracy for arithmetic; 24 puts the
 *  worst-case error well under a pixel at these sizes. */
export function edgePoints(from: Point, to: Point, steps = 24): Point[] {
  const dx = Math.max(40, Math.abs(to.x - from.x) * 0.5);
  const c1 = { x: from.x + dx, y: from.y };
  const c2 = { x: to.x - dx, y: to.y };
  const pts: Point[] = [];
  for (let i = 0; i <= steps; i++) {
    const t = i / steps;
    const u = 1 - t;
    pts.push({
      x: u * u * u * from.x + 3 * u * u * t * c1.x + 3 * u * t * t * c2.x + t * t * t * to.x,
      y: u * u * u * from.y + 3 * u * u * t * c1.y + 3 * u * t * t * c2.y + t * t * t * to.y,
    });
  }
  return pts;
}

/** Where to hang an edge's delete button: the curve's own middle, not the straight-line
 *  middle between the two nodes — on a doubling-back edge those are nowhere near each other,
 *  and a ✕ floating in empty space is a ✕ nobody trusts. */
export function edgeMidpoint(from: Point, to: Point): Point {
  const pts = edgePoints(from, to);
  return pts[Math.floor(pts.length / 2)]!;
}

/** Distance from `p` to the polyline `pts` — the segment-wise minimum. */
export function distanceToPolyline(p: Point, pts: readonly Point[]): number {
  let best = Infinity;
  for (let i = 1; i < pts.length; i++) {
    best = Math.min(best, distanceToSegment(p, pts[i - 1]!, pts[i]!));
  }
  return best;
}

function distanceToSegment(p: Point, a: Point, b: Point): number {
  const vx = b.x - a.x;
  const vy = b.y - a.y;
  const len2 = vx * vx + vy * vy;
  // A degenerate segment is a point; clamping t to [0,1] below would divide by zero first.
  const t = len2 === 0 ? 0 : Math.max(0, Math.min(1, ((p.x - a.x) * vx + (p.y - a.y) * vy) / len2));
  const dx = p.x - (a.x + t * vx);
  const dy = p.y - (a.y + t * vy);
  return Math.sqrt(dx * dx + dy * dy);
}

/** Which drawn edge (by its index in the list) `p` is on, or null. Nearest wins, so two edges
 *  crossing under the cursor resolve to the one actually being pointed at. */
export function hitTestEdges(
  edges: readonly { from: Point; to: Point }[],
  p: Point,
  tolerance = EDGE_HIT_TOLERANCE
): number | null {
  let best: number | null = null;
  let bestD = tolerance;
  edges.forEach((e, i) => {
    const d = distanceToPolyline(p, edgePoints(e.from, e.to));
    if (d <= bestD) {
      bestD = d;
      best = i;
    }
  });
  return best;
}

// ---------- placement ----------

/** Where every node goes: the human's stored position when there is one, and a computed
 *  layered position when there isn't.
 *
 *  The two halves matter equally. Without the stored half a canvas is a toy — you arrange it
 *  and it springs back on reopen. Without the computed half, every block ever added by an
 *  agent, by a hand edit, or by the YAML tab lands at (0,0) on top of whatever is already
 *  there, and a file you didn't author in the canvas opens as a pile. So: the file always has
 *  a sensible picture, and any node you have moved stays where you put it. */
export function resolvePositions(
  graph: WorkflowGraph,
  layout: WorkflowLayout,
  ghosts: readonly string[] = []
): Map<string, Point> {
  const pos = new Map<string, Point>();
  const auto = autoPositions(graph, ghosts);
  for (const n of graph.nodes) {
    const stored = storedAt(layout, n.block.id);
    pos.set(blockKey(n.index), stored ? { x: stored.x, y: stored.y } : auto.get(blockKey(n.index))!);
  }
  // A ghost is never stored: it isn't a block, it is the ABSENCE of one, and persisting a
  // position for a name that doesn't exist would outlive the mistake that created it.
  for (const id of ghosts) pos.set(ghostKey(id), auto.get(ghostKey(id))!);
  return pos;
}

/** The computed picture: the layering the model derived, turned into columns and rows. */
export function autoPositions(
  graph: WorkflowGraph,
  ghosts: readonly string[] = []
): Map<string, Point> {
  const pos = new Map<string, Point>();
  graph.layers.forEach((indices, col) => {
    indices.forEach((index, row) => {
      pos.set(blockKey(index), {
        x: PAD + col * (NODE_W + COL_GAP),
        y: PAD + row * (NODE_H + ROW_GAP),
      });
    });
  });
  const ghostCol = graph.layers.length;
  ghosts.forEach((id, row) => {
    pos.set(ghostKey(id), {
      x: PAD + ghostCol * (NODE_W + COL_GAP),
      y: PAD + row * (NODE_H + ROW_GAP),
    });
  });
  return pos;
}

/** A free slot for a NEW block: below everything already placed, at the left. Not (0,0) and
 *  not the middle — either would drop it on top of an existing node, and a new block you have
 *  to go hunting for is a new block you assume didn't get created. */
export function freeSlot(positions: ReadonlyMap<string, Point>): Point {
  if (!positions.size) return { x: PAD, y: PAD };
  let maxY = 0;
  for (const p of positions.values()) maxY = Math.max(maxY, p.y);
  return { x: PAD, y: maxY + NODE_H + ROW_GAP };
}

/** Record where a block was dropped. Snapped, so nodes line up. */
export function withPosition(layout: WorkflowLayout, id: string, p: Point): WorkflowLayout {
  if (!id) return layout; // an id-less stub has nothing stable to key a position by
  const positions = table();
  for (const [k, v] of entries(layout)) positions[k] = v;
  positions[id] = { x: snap(p.x), y: snap(p.y) };
  return { version: LAYOUT_VERSION, positions };
}

/** Every stored position, own keys only. `Object.entries` already skips the prototype, but the
 *  whole module reads the table through these two helpers so that no future edit can quietly
 *  reintroduce a raw `positions[id]`. */
const entries = (layout: WorkflowLayout): [string, Point][] => Object.entries(layout.positions);

/** Drop the positions of blocks that no longer exist. Without this the file grows forever —
 *  every block ever deleted leaves a coordinate behind, and the layout of a workflow you have
 *  edited for a year is mostly ghosts. Called on save, where we know the full block set. */
export function pruneLayout(layout: WorkflowLayout, liveIds: readonly string[]): WorkflowLayout {
  const live = new Set(liveIds.filter(Boolean));
  const positions = table();
  for (const [id, p] of entries(layout)) if (live.has(id)) positions[id] = p;
  return { version: LAYOUT_VERSION, positions };
}

/** True when the two layouts would write the same file — so a drag that ended where it
 *  started doesn't rewrite `workflow.layout.json` (and a save that changed nothing doesn't
 *  touch the disk). */
export function layoutEquals(a: WorkflowLayout, b: WorkflowLayout): boolean {
  const ak = Object.keys(a.positions).sort();
  const bk = Object.keys(b.positions).sort();
  if (ak.length !== bk.length || ak.some((k, i) => k !== bk[i])) return false;
  return ak.every((k) => {
    const pa = storedAt(a, k);
    const pb = storedAt(b, k);
    return !!pa && !!pb && pa.x === pb.x && pa.y === pb.y;
  });
}

// ---------- the layout file ----------

/** Read `.loomux/workflow.layout.json`. NEVER throws and never reports: a layout we can't
 *  read is a layout we compute instead, and the workflow still opens. That asymmetry with the
 *  workflow file is the point — a corrupt `workflow.yml` is a problem the human must see and
 *  fix, while a corrupt `workflow.layout.json` is a picture we can simply redraw. Nothing in
 *  it is anyone's WORK. */
export function parseLayout(text: string): WorkflowLayout {
  try {
    const raw = JSON.parse(text) as unknown;
    if (!raw || typeof raw !== "object") return emptyLayout();
    const positions = table();
    const src = (raw as { positions?: unknown }).positions;
    if (src && typeof src === "object") {
      // `Object.entries` walks own keys only, and the table it fills has no prototype — so a
      // hostile key in the FILE ("__proto__", "constructor") lands as an ordinary entry that
      // no lookup can confuse with an inherited member.
      for (const [id, v] of Object.entries(src as Record<string, unknown>)) {
        const p = v as { x?: unknown; y?: unknown };
        if (typeof p?.x === "number" && typeof p?.y === "number" && Number.isFinite(p.x) && Number.isFinite(p.y)) {
          positions[id] = { x: p.x, y: p.y };
        }
      }
    }
    // The version is READ and deliberately not honoured: there is exactly one format, and a
    // file claiming to be a future one still has x/y in it, which is all we want from it. When
    // there is a v2 this is where it forks — and until then, saying so here beats a version
    // field that looks like it does something.
    return { version: LAYOUT_VERSION, positions };
  } catch {
    return emptyLayout();
  }
}

/** Write it back with sorted keys, so a drag produces a one-line diff for the node that
 *  moved — the same reason the workflow file has a canonical formatter. It is gitignorable,
 *  but plenty of teams will commit it, and it should be legible when they do. */
export function serializeLayout(layout: WorkflowLayout): string {
  const positions = table();
  for (const id of Object.keys(layout.positions).sort()) positions[id] = storedAt(layout, id)!;
  return JSON.stringify({ version: LAYOUT_VERSION, positions }, null, 2) + "\n";
}
