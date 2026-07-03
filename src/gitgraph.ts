// Commit-graph lane layout and per-row SVG rendering. Pure: no git or DOM
// state beyond the returned elements.
//
// Classic active-lanes algorithm over topologically ordered commits: each
// lane slot holds the hash it is waiting to reach. A commit lands on the
// first lane waiting for it (other lanes waiting for the same hash converge
// into it and free up), keeps its lane for its first parent, and diverts
// extra parents to existing or new lanes.

import type { CommitInfo } from "./git";

export const LANE_COLORS = [
  "#7aa2f7",
  "#9ece6a",
  "#e0af68",
  "#bb9af7",
  "#7dcfff",
  "#f7768e",
  "#73daca",
  "#ff9e64",
];

export interface LaneSegment {
  /** through = full-height line; in = row top → dot; out = dot → row bottom. */
  kind: "through" | "in" | "out";
  fromLane: number;
  toLane: number;
  color: number;
}

export interface LaneRow {
  /** Lanes occupied around this row (drives SVG width). */
  laneCount: number;
  dotLane: number;
  dotColor: number;
  segments: LaneSegment[];
}

export function computeLanes(commits: CommitInfo[]): LaneRow[] {
  const lanes: (string | null)[] = []; // hash each lane is waiting for
  const colors: number[] = [];
  let nextColor = 0;
  const rows: LaneRow[] = [];

  const firstFree = (): number => {
    const i = lanes.indexOf(null);
    if (i !== -1) return i;
    lanes.push(null);
    colors.push(0);
    return lanes.length - 1;
  };

  for (const c of commits) {
    const segments: LaneSegment[] = [];
    const lanesBefore = lanes.length;

    // Land the commit: first lane waiting for it, else a fresh lane.
    let dotLane = lanes.indexOf(c.hash);
    const hasIncoming = dotLane !== -1;
    if (!hasIncoming) {
      dotLane = firstFree();
      colors[dotLane] = nextColor++ % LANE_COLORS.length;
    }
    const dotColor = colors[dotLane];

    if (hasIncoming) {
      segments.push({ kind: "in", fromLane: dotLane, toLane: dotLane, color: dotColor });
    }
    // Converge every other lane waiting for this hash into the dot.
    for (let i = 0; i < lanes.length; i++) {
      if (i !== dotLane && lanes[i] === c.hash) {
        segments.push({ kind: "in", fromLane: i, toLane: dotLane, color: colors[i] });
        lanes[i] = null;
      }
    }
    // Unrelated occupied lanes pass straight through.
    for (let i = 0; i < lanes.length; i++) {
      if (i !== dotLane && lanes[i] !== null) {
        segments.push({ kind: "through", fromLane: i, toLane: i, color: colors[i] });
      }
    }

    // First parent continues on this lane; extras diverge.
    lanes[dotLane] = c.parents[0] ?? null;
    if (c.parents.length > 0) {
      segments.push({ kind: "out", fromLane: dotLane, toLane: dotLane, color: dotColor });
    }
    for (const parent of c.parents.slice(1)) {
      let target = lanes.indexOf(parent);
      if (target === -1 || target === dotLane) {
        target = firstFree();
        lanes[target] = parent;
        colors[target] = nextColor++ % LANE_COLORS.length;
      }
      segments.push({ kind: "out", fromLane: dotLane, toLane: target, color: colors[target] });
    }

    // Trim trailing free lanes so widths stay tight.
    while (lanes.length > 0 && lanes[lanes.length - 1] === null) {
      lanes.pop();
      colors.pop();
    }

    rows.push({
      laneCount: Math.max(lanesBefore, lanes.length, dotLane + 1),
      dotLane,
      dotColor,
      segments,
    });
  }
  return rows;
}

const SVG_NS = "http://www.w3.org/2000/svg";

/** Render one row's lanes as a small SVG. Lanes beyond `maxLanes` are
 *  clipped to keep pathological graphs cheap. */
export function renderRowSvg(
  row: LaneRow,
  rowH: number,
  colW: number,
  maxLanes: number
): SVGElement {
  const laneCount = Math.min(row.laneCount, maxLanes);
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("width", String(laneCount * colW));
  svg.setAttribute("height", String(rowH));
  svg.classList.add("git-lanes");

  const clamp = (lane: number): number => Math.min(lane, maxLanes - 1);
  const x = (lane: number): number => clamp(lane) * colW + colW / 2;
  const half = rowH / 2;

  const draw = (d: string, color: number): void => {
    const path = document.createElementNS(SVG_NS, "path");
    path.setAttribute("d", d);
    path.setAttribute("stroke", LANE_COLORS[color]);
    path.setAttribute("fill", "none");
    svg.appendChild(path);
  };

  for (const seg of row.segments) {
    if (seg.fromLane >= maxLanes && seg.toLane >= maxLanes) continue;
    const xf = x(seg.fromLane);
    const xt = x(seg.toLane);
    switch (seg.kind) {
      case "through":
        draw(`M ${xf} 0 L ${xf} ${rowH}`, seg.color);
        break;
      case "in":
        // Row top → the dot. S-curve: leave vertically, arrive vertically.
        if (xf === xt) draw(`M ${xf} 0 L ${xf} ${half}`, seg.color);
        else draw(`M ${xf} 0 C ${xf} ${half * 0.5}, ${xt} ${half * 0.5}, ${xt} ${half}`, seg.color);
        break;
      case "out":
        // The dot → row bottom.
        if (xf === xt) draw(`M ${xf} ${half} L ${xf} ${rowH}`, seg.color);
        else
          draw(
            `M ${xf} ${half} C ${xf} ${half * 1.5}, ${xt} ${half * 1.5}, ${xt} ${rowH}`,
            seg.color
          );
        break;
    }
  }

  const dot = document.createElementNS(SVG_NS, "circle");
  dot.setAttribute("cx", String(x(row.dotLane)));
  dot.setAttribute("cy", String(half));
  dot.setAttribute("r", "3.5");
  dot.setAttribute("fill", LANE_COLORS[row.dotColor]);
  svg.appendChild(dot);

  return svg;
}
