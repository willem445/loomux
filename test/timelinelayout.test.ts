// Unit tests for the progress timeline's pure geometry (src/timelinelayout.ts):
// the x scale and its inverse, tick placement, lane assignment and clustering.
// The SVG that consumes this is hand-validated (Slice C) — everything with an
// answer that can be wrong lives here. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_CLUSTER_GAP_PX,
  DEFAULT_LANE_HEIGHT_PX,
  DEFAULT_PAD_LEFT_PX,
  DEFAULT_PAD_RIGHT_PX,
  TICK_STEPS_MS,
  layoutTimeline,
  makeScale,
  niceTicks,
  tsForX,
  xForTs,
  type LayoutItem,
} from "../src/timelinelayout.ts";

const T0 = Date.UTC(2026, 6, 1, 12, 0, 0);
const MIN = 60_000;
const HOUR = 3_600_000;
const DAY = 86_400_000;

const twelveHours = { startMs: T0 - 12 * HOUR, endMs: T0 };

// --- scale -----------------------------------------------------------------

test("the scale spans the padded plot area, ends inclusive", () => {
  const s = makeScale(twelveHours, 1000);
  assert.equal(s.x0, DEFAULT_PAD_LEFT_PX);
  assert.equal(s.x1, 1000 - DEFAULT_PAD_RIGHT_PX);
  assert.equal(xForTs(s, twelveHours.startMs), s.x0);
  assert.equal(xForTs(s, twelveHours.endMs), s.x1);
  assert.equal(xForTs(s, T0 - 6 * HOUR), (s.x0 + s.x1) / 2);
});

test("x -> ts -> x round-trips across the plot", () => {
  const s = makeScale(twelveHours, 800);
  for (const frac of [0, 0.1, 0.25, 0.5, 0.75, 1]) {
    const x = s.x0 + frac * (s.x1 - s.x0);
    assert.ok(Math.abs(xForTs(s, tsForX(s, x)) - x) < 1e-6, `round trip failed at ${frac}`);
  }
  assert.equal(tsForX(s, s.x0), twelveHours.startMs);
  assert.equal(tsForX(s, s.x1), twelveHours.endMs);
});

test("a panel narrower than its own padding collapses instead of inverting", () => {
  // Real state mid divider-drag: width < padLeft + padRight.
  const s = makeScale(twelveHours, 20);
  assert.equal(s.x1, s.x0, "never x1 < x0 — that would mirror the whole axis");
  assert.equal(xForTs(s, T0 - HOUR), s.x0);
  assert.equal(tsForX(s, 999), twelveHours.startMs);
});

test("a zero-span range yields finite coordinates, not NaN", () => {
  const s = makeScale({ startMs: T0, endMs: T0 }, 500);
  assert.equal(xForTs(s, T0), s.x0);
  assert.equal(Number.isFinite(xForTs(s, T0 + HOUR)), true);
});

test("a non-finite width is treated as no width", () => {
  const s = makeScale(twelveHours, Number.NaN);
  assert.equal(Number.isFinite(s.x1), true);
  assert.equal(s.x1, s.x0);
});

// --- ticks -----------------------------------------------------------------

test("ticks land on multiples of the step, inside the window", () => {
  const { stepMs, ticks } = niceTicks(twelveHours, 6);
  assert.ok(ticks.length > 0);
  assert.ok(ticks.length <= 12, `expected a readable number of ticks, got ${ticks.length}`);
  for (const t of ticks) {
    assert.equal(t % stepMs, 0, "a tick that is not a multiple of the step reads as a random time");
    assert.ok(t >= twelveHours.startMs && t <= twelveHours.endMs, "no tick outside the window");
  }
  assert.deepEqual([...ticks].sort((a, b) => a - b), ticks, "ascending");
});

test("a window edge exactly on a step keeps that edge as the first tick", () => {
  const range = { startMs: T0 - 6 * HOUR, endMs: T0 }; // both on the hour
  const { ticks } = niceTicks(range, 6);
  assert.equal(ticks[0], range.startMs);
  assert.equal(ticks[ticks.length - 1], range.endMs);
});

test("a window narrower than one tick step still produces a valid (possibly empty) axis", () => {
  const range = { startMs: T0 + 1, endMs: T0 + 400 }; // 399ms, below the 1s floor
  const { stepMs, ticks } = niceTicks(range, 6);
  assert.equal(stepMs, TICK_STEPS_MS[0]);
  for (const t of ticks) assert.ok(t >= range.startMs && t <= range.endMs);
  assert.ok(ticks.length <= 1);
});

test("an empty or inverted window has no ticks rather than an infinite loop", () => {
  assert.deepEqual(niceTicks({ startMs: T0, endMs: T0 }, 6).ticks, []);
  assert.deepEqual(niceTicks({ startMs: T0, endMs: T0 - HOUR }, 6).ticks, []);
  assert.deepEqual(niceTicks({ startMs: 0, endMs: Number.POSITIVE_INFINITY }, 6).ticks, []);
});

test("a span wider than the tick ladder scales the top step up", () => {
  // 3 years: the ladder tops out at 30 days, so the step must be multiplied
  // rather than emitting ~36 ticks.
  const range = { startMs: T0 - 1095 * DAY, endMs: T0 };
  const { stepMs, ticks } = niceTicks(range, 6);
  assert.ok(stepMs >= 30 * DAY);
  assert.equal(stepMs % (30 * DAY), 0, "still a whole number of ladder steps");
  assert.ok(ticks.length <= 6, `expected <= 6 ticks, got ${ticks.length}`);
});

test("tick placement does not depend on the local timezone", () => {
  // Epoch-ms multiples only: a day tick is 00:00 UTC everywhere, which is why
  // no DST transition can shift or duplicate one.
  const { ticks } = niceTicks({ startMs: Date.UTC(2026, 2, 6), endMs: Date.UTC(2026, 2, 14) }, 6);
  for (const t of ticks) {
    const d = new Date(t);
    assert.equal(d.getUTCHours(), 0);
    assert.equal(d.getUTCMinutes(), 0);
  }
});

// --- lanes -----------------------------------------------------------------

const item = (ts: number, lane: string): LayoutItem => ({ ts_ms: ts, lane });

test("lanes render in the declared order, stacked by lane height", () => {
  const items = [item(T0 - HOUR, "github"), item(T0 - 2 * HOUR, "agents")];
  const l = layoutTimeline(items, twelveHours, 1000, { laneOrder: ["agents", "work", "github"] });
  assert.deepEqual(l.lanes.map((x) => x.id), ["agents", "github"]);
  assert.equal(l.lanes[0].y, DEFAULT_LANE_HEIGHT_PX / 2);
  assert.equal(l.lanes[1].y, DEFAULT_LANE_HEIGHT_PX * 1.5);
  assert.equal(l.heightPx, 2 * DEFAULT_LANE_HEIGHT_PX);
});

test("laneKeys renders a lane that has nothing in this window", () => {
  // An empty lane is information, and lanes appearing/disappearing as the
  // window slides make the chart jump.
  const l = layoutTimeline([item(T0 - HOUR, "agents")], twelveHours, 1000, {
    laneKeys: ["group", "agents", "work"],
  });
  assert.deepEqual(l.lanes.map((x) => x.id), ["group", "agents", "work"]);
  assert.equal(l.dots.length, 1);
  assert.equal(l.dots[0].laneIndex, 1);
  assert.equal(l.dots[0].y, l.lanes[1].y);
});

test("a lane the caller never declared is appended, not dropped", () => {
  const l = layoutTimeline([item(T0 - HOUR, "surprise")], twelveHours, 1000, {
    laneKeys: ["agents"],
    laneOrder: ["agents"],
  });
  assert.deepEqual(l.lanes.map((x) => x.id), ["agents", "surprise"]);
  assert.equal(l.dots.length, 1);
});

// --- dots & clustering -----------------------------------------------------

test("well-separated events stay separate dots on their own lanes", () => {
  const items = [item(T0 - 10 * HOUR, "agents"), item(T0 - 2 * HOUR, "work")];
  const l = layoutTimeline(items, twelveHours, 1000, { laneOrder: ["agents", "work"] });
  assert.equal(l.dots.length, 2);
  assert.deepEqual(l.dots.map((d) => d.count), [1, 1]);
  assert.deepEqual(l.dots.map((d) => d.indices), [[0], [1]]);
  assert.ok(l.dots[0].x < l.dots[1].x);
});

test("events closer than the cluster gap become one dot that remembers all of them", () => {
  const items = [item(T0 - HOUR, "work"), item(T0 - HOUR + 1000, "work"), item(T0 - HOUR + 2000, "work")];
  const l = layoutTimeline(items, twelveHours, 1000, { laneOrder: ["work"] });
  assert.equal(l.dots.length, 1);
  const dot = l.dots[0];
  assert.equal(dot.count, 3);
  assert.deepEqual(dot.indices, [0, 1, 2], "click-to-expand needs every source index");
  assert.equal(dot.tsMinMs, T0 - HOUR);
  assert.equal(dot.tsMaxMs, T0 - HOUR + 2000);
  assert.equal(dot.x, xForTs(l.scale, T0 - HOUR), "anchored to when the cluster started");
});

test("a dense run breaks into several clusters instead of one that eats the lane", () => {
  // Greedy from the left against the cluster's FIRST x: 60 events one minute
  // apart across 12h are ~1.2px apart, so they cluster — but each cluster is
  // bounded by the gap, not by the lane.
  const items = Array.from({ length: 60 }, (_, i) => item(T0 - 6 * HOUR + i * MIN, "work"));
  const l = layoutTimeline(items, twelveHours, 1000, { laneOrder: ["work"] });
  assert.ok(l.dots.length > 1, "not one giant cluster");
  assert.ok(l.dots.length < items.length, "not one dot per event either");
  const total = l.dots.reduce((n, d) => n + d.count, 0);
  assert.equal(total, items.length, "every event is inside exactly one dot");
  for (let i = 1; i < l.dots.length; i++) {
    assert.ok(l.dots[i].x - l.dots[i - 1].x > DEFAULT_CLUSTER_GAP_PX, "clusters do not overlap");
  }
});

test("events on different lanes never cluster together", () => {
  const items = [item(T0 - HOUR, "work"), item(T0 - HOUR, "agents")];
  const l = layoutTimeline(items, twelveHours, 1000, { laneOrder: ["agents", "work"] });
  assert.equal(l.dots.length, 2);
  assert.notEqual(l.dots[0].y, l.dots[1].y);
});

test("the whole audit cap on one instant collapses to a single countable dot", () => {
  // 5000 entries is what `orch_audit` can hand over; at one instant they are
  // one dot whose count is the only honest thing to render.
  const items = Array.from({ length: 5000 }, () => item(T0 - HOUR, "ops"));
  const l = layoutTimeline(items, twelveHours, 1200, { laneOrder: ["ops"] });
  assert.equal(l.dots.length, 1);
  assert.equal(l.dots[0].count, 5000);
  assert.equal(l.dots[0].indices.length, 5000);
  assert.equal(l.dots[0].tsMinMs, l.dots[0].tsMaxMs);
});

test("items outside the window are counted, never clamped onto the edge", () => {
  const items = [
    item(twelveHours.startMs - 1, "work"),
    item(twelveHours.endMs + 1, "work"),
    item(T0 - HOUR, "work"),
  ];
  const l = layoutTimeline(items, twelveHours, 1000, { laneOrder: ["work"] });
  assert.equal(l.dropped, 2);
  assert.equal(l.dots.length, 1);
  assert.deepEqual(l.dots[0].indices, [2]);
});

test("an empty window lays out cleanly", () => {
  const l = layoutTimeline([], twelveHours, 1000, { laneOrder: ["work"] });
  assert.deepEqual(l.dots, []);
  assert.deepEqual(l.lanes, []);
  assert.equal(l.heightPx, 0);
  assert.equal(l.dropped, 0);
  assert.ok(l.ticks.ticks.length > 0, "the axis still has ticks to draw");
});

test("a single event sits at the right instant on the axis", () => {
  const l = layoutTimeline([item(T0 - 3 * HOUR, "work")], twelveHours, 1000, { laneOrder: ["work"] });
  assert.equal(l.dots.length, 1);
  assert.equal(l.dots[0].x, xForTs(l.scale, T0 - 3 * HOUR));
  assert.equal(tsForX(l.scale, l.dots[0].x), T0 - 3 * HOUR);
});

test("dots are ordered by time within a lane whatever order the caller passed", () => {
  const items = [item(T0 - HOUR, "work"), item(T0 - 10 * HOUR, "work"), item(T0 - 5 * HOUR, "work")];
  const l = layoutTimeline(items, twelveHours, 2000, { laneOrder: ["work"] });
  assert.deepEqual(l.dots.map((d) => d.indices[0]), [1, 2, 0]);
  assert.deepEqual(
    l.dots.map((d) => d.x),
    [...l.dots.map((d) => d.x)].sort((a, b) => a - b)
  );
});
