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
 *  device pixels — measured at up to ~0.7px on the CI runner. Deliberately
 *  small: with it, a sibling is allowed to move 5px, which is still under one
 *  ~8.4px terminal cell — so this bound cannot hide a re-share (tens of px),
 *  and it does not pretend to exclude the one-column nudge described above,
 *  which the share assertions below are the real witness for. Nothing about
 *  the guarantee rests on this number; every load-bearing assertion in this
 *  spec is in the share domain, where rounding is three orders of magnitude
 *  smaller than the effect. */
const ROUNDING_PX = 1;
const MOVE_BUDGET_PX = DIVIDER_FOOTPRINT_PX + ROUNDING_PX;

/** How far a pane's absolute vertical geometry may move during a same-direction
 *  ROW split. In this module's own arithmetic the answer is zero — a row split
 *  cannot touch heights — but the number measured against the viewport also
 *  carries the chrome above the grid, and on this base adding a pane nudges the
 *  whole grid by a fraction of a pixel (0.55px and 0.84px on two CI attempts).
 *  Two device pixels is comfortably above that and three orders of magnitude
 *  below a real vertical re-flow, which is what these two bounds exist to
 *  catch. The property itself — B does not move WITHIN its row — is asserted
 *  exactly, relative to the row's own top edge. */
const VERTICAL_JITTER_PX = 2;

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
  // Compared as SHARES, not as pixels. Two equal-weight flex siblings are not
  // guaranteed equal *pixel* widths: the engine distributes fractional free
  // space item by item and rounds, so they land up to about a pixel apart —
  // this exact assertion, written as `toBeCloseTo(width, 0)`, failed on the
  // CI runner at 249.31 vs 249.96 (0.66px) and passed on retry, which is a
  // flaky spec, not a caught defect. The equality `halve` actually guarantees
  // is on the weights, so assert it where it holds. SHARE_EPSILON is ~2px of
  // a 1000px row here, and the nearest wrong policy (the newcomer taking 1/N
  // instead of half) separates these two shares by ~0.09 — 45x this bound.
  expect(
    Math.abs(shareOf(after.fresh!.width, widthsAfter) - shareOf(after.a!.width, widthsAfter)),
    "the split pane and the newcomer should be the same size"
  ).toBeLessThan(SHARE_EPSILON);

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
    //    divider's own footprint.
    //
    //    Vertically, measured WITHIN THE ROW rather than against the viewport.
    //    A same-direction split in a row cannot touch heights, and that is
    //    still what is asserted — but a pane's absolute `y` also carries
    //    whatever the chrome above the grid is doing, and on this base adding
    //    a pane nudges the whole grid down by a fraction of a pixel (0.55px
    //    and 0.84px on two CI attempts, against a 0.5px bound). That is not
    //    this spec's property: it happens to every pane at once, including the
    //    one being split, so comparing B's top edge to the ROW's top edge
    //    isolates the claim instead of coupling it to the top bar's height.
    //    See the PR body — the jitter itself is reported separately, not
    //    silently absorbed here.
    const rowTopBefore = before.a!.y;
    const rowTopAfter = after.a!.y;
    expect(
      now.y - rowTopAfter,
      `${name} moved vertically within its row`
    ).toBeCloseTo(was.y - rowTopBefore, 0);
    // Height gets the same treatment for the same reason: if the chrome above
    // the grid grows by a fraction of a pixel, the grid — and every pane in it
    // — loses that fraction of height. Budgeted rather than exact, because the
    // regression this is here to catch (a split that re-flows the layout
    // vertically) is tens of pixels, not tenths.
    expect(
      Math.abs(now.height - was.height),
      `${name} changed height by more than the grid's own sub-pixel jitter`
    ).toBeLessThanOrEqual(VERTICAL_JITTER_PX);
    expect(
      Math.abs(now.y - was.y),
      `${name} moved vertically by more than the grid's own sub-pixel jitter`
    ).toBeLessThanOrEqual(VERTICAL_JITTER_PX);
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

test("a pane rejoining the grid from the dock comes back at a fair slice, not as the runt of the row", async ({
  appPage: page,
}) => {
  // The OTHER half of the policy, end to end. Every assertion above is about
  // `halve`; this one is the only place the `share` arm is exercised in a real
  // app, and it is the arm the three pane slices had to be reconciled over —
  // grid.ts routes it to `paneequalize.planEvenInsert` (newcomer at the row's
  // MEAN), not to splitfloor's own `share` branch (newcomer at 1/N on top of
  // the row's total), which is the staircase #936 reported.
  //
  // A dock restore is the one `share` call site reachable without spawning an
  // agent CLI — the multi-agent fan-out, the other one, is off limits to tests
  // by CLAUDE.md constraint 3, and no bounding box would be worth a paid
  // agent run anyway.
  await createTerminalPane(page, { name: "Pane A" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane B" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane C" });

  const [paneA, paneB, paneC] = ["Pane A", "Pane B", "Pane C"].map((n) => paneByName(page, n));
  await expect(paneC).toBeVisible();

  // Guard the premise: one flat row, or "the runt of the row" means nothing.
  const rowBefore = {
    a: await paneA.boundingBox(),
    b: await paneB.boundingBox(),
    c: await paneC.boundingBox(),
  };
  expect(rowBefore.a!.y, "A and B should share a row").toBeCloseTo(rowBefore.b!.y, 0);
  expect(rowBefore.b!.y, "B and C should share a row").toBeCloseTo(rowBefore.c!.y, 0);

  // Park C in the dock, then bring it back by clicking its chip.
  await paneC.locator('button.pane-btn[title="Minimize to dock (Alt+M)"]').click();
  const chip = page.locator(".dock-chip", { hasText: "Pane C" });
  await expect(chip).toHaveCount(1);
  await expect(paneC).toHaveCount(0);
  await chip.click();
  await expect(paneC).toBeVisible();

  const after = {
    a: await paneA.boundingBox(),
    b: await paneB.boundingBox(),
    c: await paneC.boundingBox(),
  };
  for (const [name, box] of Object.entries(after)) {
    expect(box, `pane ${name} should have a bounding box after the restore`).not.toBeNull();
  }
  const widths = [after.a!.width, after.b!.width, after.c!.width];
  const restored = shareOf(after.c!.width, widths);
  const siblings = [shareOf(after.a!.width, widths), shareOf(after.b!.width, widths)];

  // The intent, phrased so it holds for any even-matrix policy and fails for
  // the 1/N-on-top one: a pane rejoining a row it used to be part of must not
  // come back smaller than the pane that was already smallest. Under the
  // routed policy it lands on the row's mean (~1/3 of this row against a
  // smallest sibling of ~1/4); under 1/N-on-top it lands at ~1/5 against a
  // smallest sibling of ~3/10, i.e. as the runt — an 8-point margin one way
  // and a 10-point margin the other.
  expect(
    restored,
    `the restored pane came back at ${restored} of the row, under its smallest sibling ${Math.min(
      ...siblings
    )} — the share arm is not the even-matrix one`
  ).toBeGreaterThan(Math.min(...siblings) + SHARE_EPSILON);

  // And it is a usable pane, not a sliver. 0.25 rather than the 1/N policy's
  // exact 0.2, so this is a bound with margin on both sides (the routed policy
  // lands at ~0.333) rather than an assertion sitting on the wrong answer.
  expect(restored, `the restored pane is a sliver at ${restored} of the row`).toBeGreaterThan(0.25);
});
