// Pure geometry for the progress timeline (#608, Slice B): window -> x scale,
// tick generation, lane placement and dot clustering. No DOM, no dates
// formatted, no colors — timelineview.ts (Slice C) turns this into SVG.
//
// Self-contained by the same rule as timelinemodel.ts: no intra-src imports at
// all (TS5097 — see embedsplit.ts), which is why this module knows nothing
// about `TimelineEvent`. It lays out anything with an instant and a lane key,
// and the view supplies the lane key from `categoryOf()`. That is not just
// import hygiene: it means the lane grouping the view wants (by category now,
// sub-laned by agent later) is a caller decision, not a rewrite here.
//
// Two properties everything else leans on:
//  - **Epoch-ms math only.** Ticks land on multiples of the step in UTC, so
//    tick placement is identical in every timezone and across a DST boundary.
//    Labels are the view's job precisely because labels are where locale
//    belongs.
//  - **Nothing is dropped silently.** An item outside the range is counted in
//    `dropped` rather than laid out at a clamped edge, where it would read as
//    a real event at a time it did not happen.

export interface TimelineRangeLike {
  startMs: number;
  endMs: number;
}

/** Linear ms -> px mapping across the plot area. `x0`/`x1` are the INNER
 *  bounds (padding already removed), so `xForTs(scale, startMs) === x0`. */
export interface TimelineScale {
  startMs: number;
  endMs: number;
  x0: number;
  x1: number;
}

/** Room the view needs left of the axis for lane labels, and right of it so a
 *  dot at "now" is not half-clipped by the panel edge. Defaults, overridable
 *  per call — the pure layer must not assume one chrome. */
export const DEFAULT_PAD_LEFT_PX = 96;
export const DEFAULT_PAD_RIGHT_PX = 16;

/** Row height per lane, and the dot radius the clustering gap is derived
 *  from. Mirrors nothing in CSS by force — the view passes its own values if
 *  it styles differently. */
export const DEFAULT_LANE_HEIGHT_PX = 34;

/** Two dots closer than this on the x axis are one cluster. Sized at a little
 *  over a dot's diameter so neighbours touch rather than overlap. */
export const DEFAULT_CLUSTER_GAP_PX = 10;

export function makeScale(
  range: TimelineRangeLike,
  widthPx: number,
  padLeftPx: number = DEFAULT_PAD_LEFT_PX,
  padRightPx: number = DEFAULT_PAD_RIGHT_PX
): TimelineScale {
  const x0 = padLeftPx;
  // A panel narrower than its own padding is a real state during a divider
  // drag; collapse to a zero-width plot rather than an inverted one.
  const x1 = Math.max(x0, (Number.isFinite(widthPx) ? widthPx : 0) - padRightPx);
  return { startMs: range.startMs, endMs: range.endMs, x0, x1 };
}

/** Where an instant sits. A zero-span range or a zero-width plot puts
 *  everything at `x0` — degenerate but finite, never NaN. */
export function xForTs(scale: TimelineScale, tsMs: number): number {
  const span = scale.endMs - scale.startMs;
  if (span <= 0) return scale.x0;
  const frac = (tsMs - scale.startMs) / span;
  return scale.x0 + frac * (scale.x1 - scale.x0);
}

/** The inverse, for hit-testing a hover/click back to an instant. Exact
 *  round-trip with `xForTs` for any x in [x0, x1] on a non-degenerate scale. */
export function tsForX(scale: TimelineScale, x: number): number {
  const width = scale.x1 - scale.x0;
  if (width <= 0) return scale.startMs;
  const frac = (x - scale.x0) / width;
  return scale.startMs + frac * (scale.endMs - scale.startMs);
}

/** The tick ladder, in ms. Every step divides the next one that shares its
 *  unit, so zooming never lands ticks between the previous set's. Stops at 30
 *  days: beyond that the ladder would need calendar months, which are not a
 *  fixed number of ms and would break the "epoch-ms only" property. */
export const TICK_STEPS_MS: readonly number[] = [
  1_000, // 1s
  5_000,
  15_000,
  30_000,
  60_000, // 1m
  5 * 60_000,
  15 * 60_000,
  30 * 60_000,
  3_600_000, // 1h
  3 * 3_600_000,
  6 * 3_600_000,
  12 * 3_600_000,
  86_400_000, // 1d
  2 * 86_400_000,
  7 * 86_400_000,
  30 * 86_400_000,
];

export interface TimelineTicks {
  stepMs: number;
  /** Ascending epoch-ms values, all inside [startMs, endMs]. */
  ticks: number[];
}

/** Ticks at "nice" round instants: the smallest ladder step that yields at
 *  most `target` ticks, aligned to multiples of the step since the epoch.
 *
 *  Alignment is deliberate and is what makes the axis readable — an hour tick
 *  lands on the hour, a day tick on 00:00 UTC — and it is also why this is
 *  DST-free: no local midnight is ever computed. */
export function niceTicks(range: TimelineRangeLike, target = 6): TimelineTicks {
  const span = range.endMs - range.startMs;
  if (!(span > 0) || !Number.isFinite(span)) {
    return { stepMs: TICK_STEPS_MS[0], ticks: [] };
  }
  const wanted = Math.max(1, Math.floor(target));
  let stepMs = TICK_STEPS_MS[TICK_STEPS_MS.length - 1];
  for (const s of TICK_STEPS_MS) {
    if (span / s <= wanted) {
      stepMs = s;
      break;
    }
  }
  // Span wider than the ladder's top step: scale that step up by whole
  // multiples rather than returning hundreds of ticks.
  if (span / stepMs > wanted) {
    stepMs = Math.ceil(span / wanted / stepMs) * stepMs;
  }
  const ticks: number[] = [];
  const first = Math.ceil(range.startMs / stepMs) * stepMs;
  for (let t = first; t <= range.endMs; t += stepMs) ticks.push(t);
  return { stepMs, ticks };
}

export interface LayoutItem {
  ts_ms: number;
  /** Lane key — whatever the caller groups by (a category, an agent id). */
  lane: string;
}

export interface LayoutLane {
  id: string;
  index: number;
  /** Vertical centre of the lane's row, px from the top of the plot. */
  y: number;
}

/** One rendered dot. `indices` are positions in the ITEM ARRAY handed to
 *  `layoutTimeline`, so the view can map a click straight back to its events
 *  without re-deriving anything. `count > 1` is a cluster. */
export interface LayoutDot {
  x: number;
  y: number;
  lane: string;
  laneIndex: number;
  indices: number[];
  count: number;
  tsMinMs: number;
  tsMaxMs: number;
}

export interface LayoutOptions {
  /** Lane order, top to bottom. Lanes present in the items but missing here
   *  are appended in first-seen order rather than dropped. */
  laneOrder?: readonly string[];
  /** Render a lane even when it has no items in this window — an empty lane
   *  is information ("nothing happened here"), and lanes that appear and
   *  disappear as the window slides make the chart jump. */
  laneKeys?: readonly string[];
  laneHeightPx?: number;
  clusterGapPx?: number;
  padLeftPx?: number;
  padRightPx?: number;
}

export interface TimelineLayout {
  scale: TimelineScale;
  lanes: LayoutLane[];
  dots: LayoutDot[];
  ticks: TimelineTicks;
  /** Total plot height for the lanes rendered. */
  heightPx: number;
  /** Items outside [startMs, endMs]. Counted, never clamped onto the edge. */
  dropped: number;
}

/** Group items into lanes and cluster the ones that would overlap.
 *
 *  Clustering is per lane and greedy from the left: an item joins the open
 *  cluster while it is within `clusterGapPx` of that cluster's FIRST x, so a
 *  dense run becomes several fixed-width clusters rather than one cluster that
 *  keeps absorbing the whole lane. The cluster's x stays its first item's x —
 *  a cluster is anchored to when it started, not to a drifting mean. */
export function layoutTimeline(
  items: readonly LayoutItem[],
  range: TimelineRangeLike,
  widthPx: number,
  opts: LayoutOptions = {}
): TimelineLayout {
  const laneHeight = opts.laneHeightPx ?? DEFAULT_LANE_HEIGHT_PX;
  const gap = opts.clusterGapPx ?? DEFAULT_CLUSTER_GAP_PX;
  const scale = makeScale(range, widthPx, opts.padLeftPx, opts.padRightPx);

  // Lane set: the always-render lanes first (in their given order), then any
  // lane the items introduced. `laneOrder` only ORDERS; it does not conjure a
  // lane that has nothing in it, which is what `laneKeys` is for.
  const laneIds: string[] = [];
  const seen = new Set<string>();
  for (const id of opts.laneKeys ?? []) {
    if (!seen.has(id)) {
      seen.add(id);
      laneIds.push(id);
    }
  }
  const order = opts.laneOrder ?? [];
  const extras: string[] = [];
  for (const it of items) {
    if (seen.has(it.lane)) continue;
    seen.add(it.lane);
    extras.push(it.lane);
  }
  // An extra lane that IS in laneOrder keeps its declared position.
  extras.sort((a, b) => {
    const ia = order.indexOf(a);
    const ib = order.indexOf(b);
    if (ia === -1 && ib === -1) return 0;
    if (ia === -1) return 1;
    if (ib === -1) return -1;
    return ia - ib;
  });
  laneIds.push(...extras);

  const lanes: LayoutLane[] = laneIds.map((id, index) => ({
    id,
    index,
    y: index * laneHeight + laneHeight / 2,
  }));
  // Bucket item indices per lane, in time order.
  const perLane = new Map<string, number[]>();
  let dropped = 0;
  const ordered = items
    .map((it, i) => ({ it, i }))
    .filter(({ it }) => {
      const inside = it.ts_ms >= range.startMs && it.ts_ms <= range.endMs;
      if (!inside) dropped++;
      return inside;
    })
    .sort((a, b) => a.it.ts_ms - b.it.ts_ms || a.i - b.i);
  for (const { it, i } of ordered) {
    const bucket = perLane.get(it.lane);
    if (bucket) bucket.push(i);
    else perLane.set(it.lane, [i]);
  }

  const dots: LayoutDot[] = [];
  for (const lane of lanes) {
    const bucket = perLane.get(lane.id);
    if (!bucket || bucket.length === 0) continue;
    let open: LayoutDot | null = null;
    let openX = 0;
    for (const i of bucket) {
      const ts = items[i].ts_ms;
      const x = xForTs(scale, ts);
      if (open && x - openX <= gap) {
        open.indices.push(i);
        open.count++;
        open.tsMaxMs = Math.max(open.tsMaxMs, ts);
        continue;
      }
      open = {
        x,
        y: lane.y,
        lane: lane.id,
        laneIndex: lane.index,
        indices: [i],
        count: 1,
        tsMinMs: ts,
        tsMaxMs: ts,
      };
      openX = x;
      dots.push(open);
    }
  }

  return {
    scale,
    lanes,
    dots,
    ticks: niceTicks(range),
    heightPx: lanes.length * laneHeight,
    dropped,
  };
}
