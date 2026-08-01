// Unit tests for the progress timeline's chrome decisions
// (src/timelinechrome.ts). Each one pins a choice whose WRONG answer is a real
// defect the view would ship silently: an axis of identical labels, lanes that
// reorder under the human's hands, a gh layer that never refreshes again after
// a clock jump, an error note that hides which half of the data vanished, and
// a detail body that stops at 50 rows without saying so. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DETAIL_MAX_ROWS,
  GH_REFRESH_MS,
  detailSlice,
  ghCoverageFloorMs,
  ghFloorNote,
  ghUnavailableNote,
  laneLabel,
  shouldRefreshGh,
  tickScale,
  toggleCategory,
} from "../src/timelinechrome.ts";

const CATEGORY_ORDER = ["group", "agents", "work", "gates", "github", "ops"];

// --- tick granularity ------------------------------------------------------

test("tick granularity follows the step: sub-minute shows seconds, a day step shows dates", () => {
  assert.equal(tickScale(1_000), "seconds");
  assert.equal(tickScale(30_000), "seconds");
  assert.equal(tickScale(60_000), "minutes"); // exactly a minute is minute-scale
  assert.equal(tickScale(30 * 60_000), "minutes");
  assert.equal(tickScale(3_600_000), "hours"); // exactly an hour is hour-scale
  assert.equal(tickScale(12 * 3_600_000), "hours");
  assert.equal(tickScale(86_400_000), "days");
  assert.equal(tickScale(7 * 86_400_000), "days");
});

test("a 12h-stepped axis is NOT labelled to the second (the row-of-00:00:00 bug)", () => {
  // The whole point of the function: the 72h preset's ticks land 12h apart,
  // and second-precision labels there are six identical strings.
  assert.notEqual(tickScale(12 * 3_600_000), "seconds");
});

test("a non-finite step degrades to the finest scale rather than throwing", () => {
  assert.equal(tickScale(Number.NaN), "seconds");
});

// --- lane labels -----------------------------------------------------------

test("every known category has a heading, and an unknown one falls back to its key", () => {
  assert.equal(laneLabel("github"), "GitHub");
  assert.equal(laneLabel("work"), "work");
  // layoutTimeline APPENDS a lane it has never seen instead of dropping it, so
  // a kind added to the model before this table catches up must still render a
  // named lane — never a blank one.
  assert.equal(laneLabel("weather"), "weather");
  assert.notEqual(laneLabel("weather"), "");
});

// --- category chips --------------------------------------------------------

test("toggling a category off removes only it", () => {
  const next = toggleCategory(["group", "agents", "work"], "agents", CATEGORY_ORDER);
  assert.deepEqual(next, ["group", "work"]);
});

test("toggling one back on restores LANE ORDER, not click order", () => {
  // Clicking `group` back on after `github` must not leave group below github:
  // the chips and the lanes would then disagree about top-to-bottom position.
  const next = toggleCategory(["agents", "github"], "group", CATEGORY_ORDER);
  assert.deepEqual(next, ["group", "agents", "github"]);
});

test("turning the LAST category off yields an empty selection, not a silent reset to all", () => {
  // The view says "every category is off" rather than quietly plotting
  // everything — chips that contradict the chart are the failure this feature
  // exists to avoid.
  assert.deepEqual(toggleCategory(["ops"], "ops", CATEGORY_ORDER), []);
});

test("a category outside the caller's order survives instead of being filtered away", () => {
  const next = toggleCategory(["group", "weather"], "agents", CATEGORY_ORDER);
  assert.deepEqual(next, ["group", "agents", "weather"]);
});

// --- gh refresh cadence ----------------------------------------------------

test("gh is due when it has never been fetched", () => {
  assert.equal(shouldRefreshGh(null, 1_000), true);
});

test("gh is not re-fetched on every audit tick, and is due once the interval elapses", () => {
  const t0 = 1_000_000;
  assert.equal(shouldRefreshGh(t0, t0 + 1_500), false); // one audit follow tick later
  assert.equal(shouldRefreshGh(t0, t0 + GH_REFRESH_MS - 1), false);
  assert.equal(shouldRefreshGh(t0, t0 + GH_REFRESH_MS), true);
});

test("a clock that jumps BACKWARDS makes gh due, instead of freezing it for the session", () => {
  // now < last would otherwise be a negative elapsed time — "not due" until the
  // clock catches up, which after a big correction is hours of a dead gh layer.
  assert.equal(shouldRefreshGh(5_000_000, 1_000), true);
});

// --- the gh failure note ---------------------------------------------------

test("the gh-unavailable note names exactly what is missing AND what still holds", () => {
  const note = ghUnavailableNote(new Error("gh: not authenticated"));
  assert.equal(note.id, "gh-unavailable");
  assert.match(note.text, /gh: not authenticated/);
  // Without this, an empty GitHub lane reads as "nobody opened a PR".
  assert.match(note.text, /issue or PR points/i);
  assert.match(note.text, /audit events are unaffected/i);
});

test("a multi-line error collapses to one line, and a null error still produces a note", () => {
  const multi = ghUnavailableNote(new Error("line one\n  line two\nline three"));
  assert.ok(!multi.text.includes("\n"));
  assert.match(multi.text, /line one line two line three/);
  const none = ghUnavailableNote(null);
  assert.match(none.text, /GitHub activity unavailable/);
  assert.match(none.text, /no detail/);
});

// --- the gh coverage floor -------------------------------------------------

test("the coverage floor is the OLDEST updated_at in the capped page", () => {
  const rows = [
    { updated_at: "2026-07-31T09:00:00Z" },
    { updated_at: "2026-07-30T22:15:00Z" }, // oldest
    { updated_at: "2026-07-31T12:00:00Z" },
  ];
  assert.equal(ghCoverageFloorMs(rows), Date.parse("2026-07-30T22:15:00Z"));
});

test("unparseable and absent timestamps are skipped, not treated as the epoch", () => {
  // A row with a broken timestamp costs that row. Coercing it to 0 would claim
  // the page is complete back to 1970 — the exact opposite of the truth.
  const rows = [{ updated_at: "" }, { updated_at: null }, { updated_at: "not a date" }, {}];
  assert.equal(ghCoverageFloorMs(rows), null);
  assert.equal(ghCoverageFloorMs([{ updated_at: "bogus" }, { updated_at: "2026-07-31T09:00:00Z" }]),
    Date.parse("2026-07-31T09:00:00Z"));
});

test("an empty page has no floor to state (the view must not invent one)", () => {
  assert.equal(ghCoverageFloorMs([]), null);
});

test("the floor note states what IS covered, and names only the list it applies to", () => {
  const note = ghFloorNote("issues", Date.parse("2026-07-30T22:15:00Z"));
  assert.equal(note.id, "gh-floor-issues");
  assert.match(note.text, /GitHub issues/);
  assert.match(note.text, /complete back to 2026-07-30T22:15:00Z/);
  // A truncated ISSUE list must never imply the PR list was truncated too.
  assert.ok(!note.text.includes("PRs"));
  assert.equal(ghFloorNote("PRs", 0).id, "gh-floor-PRs");
});

// --- detail body cap -------------------------------------------------------

test("a small cluster expands in full with nothing hidden", () => {
  assert.deepEqual(detailSlice([3, 9, 14]), { shown: [3, 9, 14], hidden: 0 });
});

test("a huge cluster is capped and REPORTS the remainder rather than stopping silently", () => {
  const indices = Array.from({ length: DETAIL_MAX_ROWS + 70 }, (_, i) => i);
  const { shown, hidden } = detailSlice(indices);
  assert.equal(shown.length, DETAIL_MAX_ROWS);
  assert.equal(hidden, 70);
  assert.equal(shown.length + hidden, indices.length); // nothing is lost, only deferred
});

test("exactly the cap renders in full (no off-by-one 'hidden: 0' that isn't)", () => {
  const indices = Array.from({ length: DETAIL_MAX_ROWS }, (_, i) => i);
  assert.deepEqual(detailSlice(indices), { shown: indices, hidden: 0 });
});
