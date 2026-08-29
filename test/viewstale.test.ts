// Unit tests for the published-view staleness badge (src/viewstale.ts). Pure —
// no DOM, no async, no clock — exercised directly. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  formatAge,
  needsViewTierRetry,
  staleState,
  recordSweepFailure,
  recordSweepSuccess,
  tabStatusStats,
  VIEW_TIER_RETRY_MS,
  type GroupViewMeta,
  type ViewMeta,
} from "../src/viewstale.ts";

/** The backend's `VIEW_STALE_AFTER_MS`. Re-declared rather than imported from
 *  anywhere: the frontend deliberately does NOT own this threshold — the
 *  backend decides `stale` and this module only renders it — so the number
 *  appears here purely to build fixtures that straddle it. A test that
 *  re-derived `stale` from this constant would be testing its own arithmetic
 *  rather than the renderer. */
const BACKEND_STALE_AFTER_MS = 5000;

function meta(over: Partial<ViewMeta> = {}): ViewMeta {
  return {
    seq: 7,
    published_at_ms: 1_700_000_000_000,
    age_ms: 0,
    compute_ms: 3,
    stale: false,
    partial: false,
    ...over,
  };
}

function groupMeta(over: Partial<GroupViewMeta> = {}): GroupViewMeta {
  return { ...meta(), view_ready: true, ...over };
}

// ---------- the badge is the backend's decision, never a re-derivation ----------

test("a fresh payload renders no badge", () => {
  const s = staleState(meta({ age_ms: 120, stale: false }));
  assert.equal(s.stale, false);
  assert.equal(s.label, "", "a fresh payload has no label to render");
  assert.equal(s.detail, "");
});

test("the age exactly AT the backend threshold is not stale", () => {
  // The backend's rule is `age_ms > VIEW_STALE_AFTER_MS`, so a payload sitting
  // exactly on the threshold arrives with `stale: false`. The edge is pinned
  // because a badge that appears one millisecond early on every healthy tick
  // is a badge nobody believes.
  const s = staleState(meta({ age_ms: BACKEND_STALE_AFTER_MS, stale: false }));
  assert.equal(s.stale, false);
});

test("a stale payload renders the badge with its age", () => {
  const s = staleState(meta({ age_ms: BACKEND_STALE_AFTER_MS + 1, stale: true }));
  assert.equal(s.stale, true);
  assert.equal(s.label, "stale 5s");
  assert.match(s.detail, /snapshot from 5s ago/);
  assert.match(s.detail, /updates itself as soon as the backend answers/);
});

test("the renderer never re-derives `stale` from `age_ms`", () => {
  // Two clocks and one threshold is how the two halves drift apart, and the
  // backend is the half that knows when the snapshot was actually taken. So a
  // huge age with `stale: false` renders NOTHING, and a zero age with
  // `stale: true` renders the badge. Both directions, because a renderer that
  // ORs the two would pass the first and a renderer that ANDs them the second.
  assert.equal(
    staleState(meta({ age_ms: 600_000, stale: false })).stale,
    false,
    "the backend said fresh; the renderer does not overrule it from age_ms"
  );
  assert.equal(
    staleState(meta({ age_ms: 0, stale: true })).stale,
    true,
    "the backend said stale; the renderer does not overrule it from age_ms"
  );
});

test("a missing payload is not a stale one", () => {
  // A refused/unknown group id, or a group created since the last publish,
  // answers `null` — the caller keeps its previous render. Reporting that as
  // stale would put a permanent badge on a group the backend simply does not
  // know about, which is a different condition with a different remedy.
  assert.deepEqual(staleState(null), { stale: false, label: "", detail: "" });
  assert.deepEqual(staleState(undefined), { stale: false, label: "", detail: "" });
});

// ---------- `partial` (reserved for plan Phase 2.1) ----------

test("partial without stale renders nothing — the payload is current", () => {
  assert.equal(staleState(meta({ partial: true, stale: false })).stale, false);
});

test("partial WITH stale renders a distinct label, not the whole-panel one", () => {
  const partial = staleState(meta({ age_ms: 12_000, stale: true, partial: true }));
  const whole = staleState(meta({ age_ms: 12_000, stale: true, partial: false }));
  assert.equal(partial.label, "partly stale 12s");
  assert.equal(whole.label, "stale 12s");
  assert.notEqual(
    partial.label,
    whole.label,
    "Phase 2.1 leaves SOME sections current; a label claiming the whole panel is frozen would " +
      "be a wrong answer rendered as a right one"
  );
  assert.match(partial.detail, /rest is current/);
});

// ---------- the age format ----------

test("ages are written coarsely: seconds, then minutes, then hours", () => {
  assert.equal(formatAge(0), "0s");
  assert.equal(formatAge(999), "0s", "sub-second rounds down, never up to a second that has not passed");
  assert.equal(formatAge(1000), "1s");
  assert.equal(formatAge(59_999), "59s");
  assert.equal(formatAge(60_000), "1m");
  assert.equal(formatAge(3_599_999), "59m");
  assert.equal(formatAge(3_600_000), "1h");
  assert.equal(formatAge(86_400_000), "24h", "no day unit: past an hour the number is already the point");
});

test("a nonsense age degrades to 0 rather than rendering NaN at a human", () => {
  assert.equal(formatAge(Number.NaN), "0s");
  assert.equal(formatAge(-5), "0s");
  assert.equal(formatAge(Number.POSITIVE_INFINITY), "0s");
});

// ---------- the view-tier retry ladder ----------

test("a view tier that has not been published yet asks for one re-read", () => {
  assert.equal(needsViewTierRetry(groupMeta({ view_ready: false })), true);
});

test("a published view tier asks for nothing", () => {
  assert.equal(needsViewTierRetry(groupMeta({ view_ready: true })), false);
});

test("a missing payload does not trigger the retry ladder", () => {
  // `null` means the group is not in the snapshot at all. The caller's normal
  // cadence covers that; a retry here would turn every unknown group id into a
  // doubled call rate for as long as the panel is open.
  assert.equal(needsViewTierRetry(null), false);
  assert.equal(needsViewTierRetry(undefined), false);
});

test("the retry delay is shorter than the backend's publish interval", () => {
  // The backend publishes every 1000 ms. A retry at or past that would wait
  // out a whole extra pass for a panel opened just after one, which is the
  // blank-panel-on-open this ladder exists to remove.
  assert.ok(
    VIEW_TIER_RETRY_MS > 0 && VIEW_TIER_RETRY_MS < 1000,
    `VIEW_TIER_RETRY_MS must sit inside one publish interval, got ${VIEW_TIER_RETRY_MS}`
  );
});

// ---------- the tab strip's binding/disclosure witness (#1608; B6) ----------

test("a successful sweep records what it resolved", () => {
  recordSweepSuccess(["g1", "g2"], ["g1"], false, 120);
  const s = tabStatusStats();
  assert.deepEqual(s.bound, ["g1", "g2"]);
  assert.deepEqual(s.seen, ["g1"], "seen is the subset the snapshot carried, not every bound tab");
  assert.equal(s.stale, false);
  assert.equal(s.ageMs, 120);
});

test("a FAILED sweep is recorded too, and reports itself stale", () => {
  // B6. The strip renders its stale badge on this path, so a witness that
  // skipped it reported `stale: false` for a visibly stale strip — and the soak
  // lane's "did it recover?" assertion reads that field, so it passed in the
  // very state it exists to catch.
  recordSweepSuccess(["g1", "g2"], ["g1", "g2"], false, 90);
  recordSweepFailure(["g1", "g2"]);
  const s = tabStatusStats();
  assert.equal(s.stale, true, "a strip whose read threw is stale, not fresh");
  assert.deepEqual(s.seen, [], "that sweep resolved nothing, so it saw nothing");
  assert.deepEqual(s.bound, ["g1", "g2"], "binding is a fact about tabs; a failed read does not unbind them");
  assert.equal(
    s.ageMs,
    Number.POSITIVE_INFINITY,
    "the last good snapshot's age is unknown and unbounded — carrying the previous number " +
      "forward would be the same lie in another field"
  );
});

test("the witness never hands out its own array, so a caller cannot mutate it", () => {
  recordSweepSuccess(["g1"], ["g1"], false, 10);
  const first = tabStatusStats();
  first.bound.push("injected");
  assert.deepEqual(tabStatusStats().bound, ["g1"], "the recorded state is not aliased to a caller");
});
