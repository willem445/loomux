// Regression class: WHO PAYS for a split (#885 slice A). Builds a 3-wide row
// of shell panes, splits one of them, and asserts the other panes keep their
// exact share of the row.
//
// WHAT THIS SPEC PROVES, EXACTLY: that a human split does not RE-SHARE the
// row. Under `halve` each untouched sibling's fraction of the row is
// preserved; under the `share` policy (still used by the multi-agent fan-out)
// the newcomer's weight is added on top of the row's total, so on this exact
// layout every untouched pane loses about 15% of its width and shifts tens of
// pixels. That is the difference this spec is built to catch, and it catches
// it by an order of magnitude.
//
// WHAT IT DOES NOT PROVE: that no other pane's PTY was resized. It cannot,
// and no bounding-box spec could. A same-direction split also inserts a
// DIVIDER, which is a real flex sibling (`.split.row > .divider`: 6px wide
// with -1px side margins → a 4px outer footprint) taken off the row's free
// space before it is distributed, so every sibling loses its ratio's slice of
// those 4px — about 1px here. `shouldResizePty` (src/panefit.ts) compares a
// `cols x rows` string, and at the default font a cell is ~8.4px wide, so a
// sibling whose width happened to sit within that 1px of a cell boundary does
// issue one real PTY resize. The honest claim — one guaranteed resize (the
// pane being split) plus a rare sub-cell nudge per sibling, versus `share`
// resizing every sibling every time — is what the assertions below are
// written to witness. Don't relabel this spec as proof of the stronger one.
import { test, expect } from "../fixtures";
import { createTerminalPane, paneByName } from "../helpers";

/** One divider's outer main-axis footprint: `width: 6px` with `margin: 8px
 *  -1px` (src/styles.css), so 6 − 1 − 1 = 4px of the row. This is the entire
 *  budget for how far an untouched sibling may move — it loses at most
 *  `ratio × 4px` of width, and its x can shift by at most the same 4px (the
 *  new divider pushes it right; everything upstream of it shrinking pulls it
 *  back, and those two never sum past one divider). */
const DIVIDER_FOOTPRINT_PX = 4;

/** Slack on the pixel bounds only, for fractional flex widths rounding to
 *  device pixels. Deliberately small: with it, a sibling is allowed to move
 *  5px, which is still under one ~8.4px terminal cell — so this bound cannot
 *  hide a re-share (tens of px), and it does not pretend to exclude the
 *  one-column nudge described above, which the ratio assertion below is the
 *  real witness for. */
const ROUNDING_PX = 1;
const MOVE_BUDGET_PX = DIVIDER_FOOTPRINT_PX + ROUNDING_PX;

/** How far a pane's SHARE of the row may drift, as a fraction of the row.
 *  Zero in exact arithmetic — `halve` leaves both the sibling's weight and
 *  the row's total untouched — so this is pure measurement rounding. For
 *  scale: the `share` policy moves these same shares by ~0.036 (18x this) on
 *  the layout below. */
const SHARE_EPSILON = 0.002;

/** Every pane in the row is `flex: <grow> 1 0` (flex-basis 0), so the panes
 *  between them hold ALL of the row's distributable space: their widths sum
 *  to exactly the free space the grows are shared out of. That makes each
 *  pane's share of the row measurable without knowing the row's width, the
 *  divider count, or the divider width — the assertion needs no magic
 *  numbers at all. */
const shareOf = (width: number, widths: number[]): number =>
  width / widths.reduce((a, b) => a + b, 0);

test("splitting a pane in a 3-wide row halves that pane and leaves its siblings' share of the row untouched", async ({
  appPage: page,
}) => {
  // Pane A fills the tab; each split lands beside the pane that was just
  // created (the new pane takes focus), so this builds the row A | B | C.
  await createTerminalPane(page, { name: "Pane A" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane B" });

  const paneA = paneByName(page, "Pane A");
  const paneB = paneByName(page, "Pane B");
  await expect(paneA).toBeVisible();
  await expect(paneB).toBeVisible();

  // The TOOLBAR gesture, pinned on the way past: `#btn-split-right` splits the
  // ACTIVE pane (B) — the same `openPane` → `openWelcomeIn` path Ctrl+Shift+E
  // and Ctrl+Shift+O take, and a different one from the per-pane header button
  // the main assertion below uses. Under `halve` A is not the target and keeps
  // its half of the row; under `share` the newcomer's 1/2 lands on top of a
  // total of 2, taking A from 1/2 of the row to 1/2.5.
  const aBeforeToolbarSplit = await paneA.boundingBox();
  const bBeforeToolbarSplit = await paneB.boundingBox();
  expect(aBeforeToolbarSplit, "Pane A should have a bounding box").not.toBeNull();
  expect(bBeforeToolbarSplit, "Pane B should have a bounding box").not.toBeNull();
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane C" });

  const paneC = paneByName(page, "Pane C");
  await expect(paneC).toBeVisible();
  const before = {
    a: await paneA.boundingBox(),
    b: await paneB.boundingBox(),
    c: await paneC.boundingBox(),
  };
  for (const [name, box] of Object.entries(before)) {
    expect(box, `pane ${name} should have a bounding box before the split`).not.toBeNull();
  }

  expect(
    shareOf(before.a!.width, [before.a!.width, before.b!.width, before.c!.width]),
    "the toolbar split re-shared the row instead of halving the active pane"
  ).toBeCloseTo(
    shareOf(aBeforeToolbarSplit!.width, [aBeforeToolbarSplit!.width, bBeforeToolbarSplit!.width]),
    2
  );

  // Guard the premise: all three really are side by side in one row (a nested
  // layout would make the sibling assertion below vacuous — a pane in another
  // subtree cannot move whatever the policy is).
  expect(before.a!.y, "A and B should share a row").toBeCloseTo(before.b!.y, 0);
  expect(before.b!.y, "B and C should share a row").toBeCloseTo(before.c!.y, 0);
  expect(before.a!.x).toBeLessThan(before.b!.x);
  expect(before.b!.x).toBeLessThan(before.c!.x);

  // Split pane A — the LEFTMOST, so both siblings sit downstream of the change
  // and would be pushed by any re-share of the row. Its own header button, so
  // this exercises the `onSplit` gesture wiring (src/main.ts `eventsFor`).
  await paneA.locator('.pane-btn[title="Split right"]').click();
  // The new pane opens on the welcome/setup form — wait for it to exist before
  // measuring anything, so no read races the relayout.
  const paneNew = page.locator(".pane", { has: page.locator(".welcome-form") });
  await expect(paneNew).toHaveCount(1);

  const after = {
    a: await paneA.boundingBox(),
    b: await paneB.boundingBox(),
    c: await paneC.boundingBox(),
    fresh: await paneNew.boundingBox(),
  };
  for (const [name, box] of Object.entries(after)) {
    expect(box, `pane ${name} should have a bounding box after the split`).not.toBeNull();
  }

  const widthsBefore = [before.a!.width, before.b!.width, before.c!.width];
  const widthsAfter = [after.a!.width, after.fresh!.width, after.b!.width, after.c!.width];

  // 1. The pane that was split paid for the newcomer out of its own share, and
  //    they split it evenly.
  expect(
    shareOf(after.a!.width, widthsAfter) + shareOf(after.fresh!.width, widthsAfter),
    "the split pane and the newcomer should hold exactly what the split pane held"
  ).toBeCloseTo(shareOf(before.a!.width, widthsBefore), 2);
  expect(after.fresh!.width, "the two halves should be the same size").toBeCloseTo(
    after.a!.width,
    0
  );

  // 2. Nobody else's SHARE of the row moved — the exact property `halve`
  //    guarantees, and the one a re-share of the row cannot satisfy.
  for (const [name, was, now] of [
    ["Pane B", before.b!, after.b!],
    ["Pane C", before.c!, after.c!],
  ] as const) {
    const drift = Math.abs(shareOf(now.width, widthsAfter) - shareOf(was.width, widthsBefore));
    expect(drift, `${name}'s share of the row moved — the split re-shared the row`).toBeLessThan(
      SHARE_EPSILON
    );

    // 3. And in plain pixels, what a human would see: nothing beyond the new
    //    divider's own footprint. Vertical geometry is exact — a
    //    same-direction split in a row cannot touch heights at all.
    expect(now.y, `${name} moved vertically`).toBeCloseTo(was.y, 0);
    expect(now.height, `${name} changed height`).toBeCloseTo(was.height, 0);
    expect(
      Math.abs(now.x - was.x),
      `${name} moved further than one divider's worth`
    ).toBeLessThanOrEqual(MOVE_BUDGET_PX);
    expect(
      Math.abs(now.width - was.width),
      `${name} resized further than one divider's worth`
    ).toBeLessThanOrEqual(MOVE_BUDGET_PX);
  }
});
