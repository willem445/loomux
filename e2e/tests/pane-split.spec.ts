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
 *  ROW split. In the arithmetic the answer is zero — a row split cannot touch
 *  heights — so this is a rounding budget, and it is deliberately one device
 *  pixel rather than a number chosen to make a red run green.
 *
 *  Its history is worth keeping, because the first two values were both wrong
 *  in instructive ways. It started as an exact `toBeCloseTo(y, 0)`, which broke
 *  when this branch was rebased onto a base whose chrome moved the grid; it was
 *  then set to 2px against measurements of 0.55px and 0.84px. Both readings
 *  were of an UNSETTLED layout. With #992's per-agent pane marks present the
 *  same measurement gave A 1.298px / B 0.565px / C 0.224px in one attempt and
 *  different numbers in the next — panes in one row cannot really drift apart
 *  from each other, so what was being measured was the layout still moving,
 *  not the layout being wrong.
 *
 *  `settledBoxes` removes that at the source, and this budget covers only what
 *  is left: genuine device-pixel rounding. If it ever needs raising again, the
 *  `[pane-split]` log line below prints the measured drift on every run —
 *  raise it from that number, and only after checking the settling is real. */
const VERTICAL_JITTER_PX = 1;

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

/** Read several panes' boxes once the layout has stopped moving.
 *
 *  `await expect(pane).toBeVisible()` says an element EXISTS and is painted; it
 *  does not say the layout around it has settled. On this base it demonstrably
 *  has not: the pane header carries per-agent marks (#992) that arrive after
 *  the pane does, and measuring straight after a split caught the grid
 *  mid-settle — panes in the SAME ROW reading different `y` deltas (A 1.298px,
 *  B 0.565px, C 0.224–0.362px) and different heights, varying between attempts
 *  of the same run. Budgeting for that would have been budgeting for a race.
 *
 *  So: sample until two consecutive reads agree to within a twentieth of a
 *  pixel, which is what "the layout has stopped" actually means. Everything
 *  vertical this spec asserts is measured through here. */
async function settledBoxes(
  locators: Record<string, ReturnType<typeof paneByName>>
): Promise<Record<string, { x: number; y: number; width: number; height: number }>> {
  const read = async () => {
    const out: Record<string, { x: number; y: number; width: number; height: number }> = {};
    for (const [name, loc] of Object.entries(locators)) {
      const box = await loc.boundingBox();
      if (!box) throw new Error(`pane ${name} has no bounding box`);
      out[name] = box;
    }
    return out;
  };
  const same = (a: Record<string, { y: number; height: number; x: number; width: number }>, b: typeof a) =>
    Object.keys(a).every(
      (k) =>
        Math.abs(a[k].y - b[k].y) < 0.05 &&
        Math.abs(a[k].height - b[k].height) < 0.05 &&
        Math.abs(a[k].x - b[k].x) < 0.05 &&
        Math.abs(a[k].width - b[k].width) < 0.05
    );

  let prev = await read();
  for (let attempt = 0; attempt < 40; attempt++) {
    await new Promise((r) => setTimeout(r, 100));
    const next = await read();
    if (same(prev, next)) return next;
    prev = next;
  }
  throw new Error("the pane layout never settled: it was still moving after 4s");
}

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
  const beforeToolbarSplit = await settledBoxes({ a: paneA, b: paneB });
  const aBeforeToolbarSplit = beforeToolbarSplit.a;
  const bBeforeToolbarSplit = beforeToolbarSplit.b;
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane C" });

  const paneC = paneByName(page, "Pane C");
  await expect(paneC).toBeVisible();
  const before = await settledBoxes({ a: paneA, b: paneB, c: paneC });

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

  const after = await settledBoxes({ a: paneA, b: paneB, c: paneC, fresh: paneNew });

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

  // The measured vertical drift, reported on every run rather than only when
  // it breaks a bound: VERTICAL_JITTER_PX is calibrated from these numbers, and
  // the next person to re-calibrate it (after the next chrome change) should
  // not have to make a spec fail first to find out what it is.
  console.log(
    `[pane-split] vertical drift after the split — ` +
      `B dy ${(after.b!.y - before.b!.y).toFixed(3)} dh ${(after.b!.height - before.b!.height).toFixed(3)}, ` +
      `C dy ${(after.c!.y - before.c!.y).toFixed(3)} dh ${(after.c!.height - before.c!.height).toFixed(3)}, ` +
      `A dy ${(after.a!.y - before.a!.y).toFixed(3)} dh ${(after.a!.height - before.a!.height).toFixed(3)} ` +
      `(budget ${VERTICAL_JITTER_PX}px)`
  );

  // 2. Nobody else's SHARE of the row moved — the exact property `halve`
  //    guarantees, and the one a re-share of the row cannot satisfy.
  //
  //    Pane A — the split TARGET — is in this loop for its VERTICAL geometry
  //    only (rev-lead N3): its width is supposed to halve, but a same-direction
  //    row split must not move it vertically either, and a regression that
  //    nested A into a column would otherwise leave every assertion here green.
  //    Its share is asserted separately above.
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
    //    Vertically, measured WITHIN THE ROW rather than against the viewport:
    //    a pane's absolute `y` also carries whatever the chrome above the grid
    //    is doing, while the property this spec owns is that B does not move
    //    relative to the row it is in. Both are asserted — this one exactly,
    //    the absolute pair against the rounding budget above — and both are
    //    read from `settledBoxes`, since the drift that used to defeat them
    //    turned out to be a layout still settling rather than a layout moved.
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

  // 4. The split TARGET's vertical geometry (rev-lead N3). A's width is meant
  //    to halve, so it is not in the loop above — but a same-direction row
  //    split must leave it in the same row, at the same height as its
  //    siblings. A regression that nested A into a column instead of inserting
  //    a flat sibling would satisfy every assertion above (B and C would keep
  //    their shares and their row) and be caught only here.
  expect(after.a!.y, "the split target left its row vertically").toBeCloseTo(after.b!.y, 0);
  expect(
    after.a!.height,
    "the split target's height stopped matching its row — nested, not split flat"
  ).toBeCloseTo(after.b!.height, 0);
  expect(
    Math.abs(after.a!.y - before.a!.y),
    "the split target moved vertically by more than the grid's own sub-pixel jitter"
  ).toBeLessThanOrEqual(VERTICAL_JITTER_PX);
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
  //
  // WHY THIS SPEC PROVES ITS OWN DISCRIMINATING POWER BEFORE IT ASSERTS
  // ANYTHING (rev-lead B1). The two policies coincide EXACTLY when the row's
  // total weight is 1: `planEvenInsert` gives the newcomer `total/n`, the
  // unrouted branch gives `1/n`. So a spec that lands on a total-1 row asserts
  // something both policies satisfy and silently proves nothing — and no
  // measurement it takes can tell you that has happened. The row this builds
  // has a total of 2 today (the FIRST split is cross-direction, and that path
  // resets both children to `1 1 0` — grid.ts's cross-direction branch), so it
  // does discriminate; but that is an incidental property of a code path this
  // spec never mentions, which is exactly the kind of thing a later change
  // breaks silently.
  //
  // So the spec no longer depends on that derivation being right. It takes a
  // SECOND dock/restore cycle (which moves the total further off 1), reads the
  // row's actual flex weights out of the DOM, computes what each policy would
  // put the restored pane at, and FAILS if those two predictions are too close
  // to tell apart. The discriminating power is asserted, not assumed.
  await createTerminalPane(page, { name: "Pane A" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane B" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane C" });

  const [paneA, paneB, paneC] = ["Pane A", "Pane B", "Pane C"].map((n) => paneByName(page, n));
  await expect(paneC).toBeVisible();

  // Guard the premise: one flat row, or "the runt of the row" means nothing.
  const rowBefore = await settledBoxes({ a: paneA, b: paneB, c: paneC });
  expect(rowBefore.a!.y, "A and B should share a row").toBeCloseTo(rowBefore.b!.y, 0);
  expect(rowBefore.b!.y, "B and C should share a row").toBeCloseTo(rowBefore.c!.y, 0);

  /** Park C in the dock and bring it straight back — one `share` insert. */
  const dockAndRestore = async (): Promise<void> => {
    await paneC.locator('button.pane-btn[title="Minimize to dock (Alt+M)"]').click();
    const chip = page.locator(".dock-chip", { hasText: "Pane C" });
    await expect(chip).toHaveCount(1);
    await expect(paneC).toHaveCount(0);
    await chip.click();
    await expect(paneC).toBeVisible();
  };

  // Cycle 1 — moves the row's total off whatever the split policy left it at.
  await dockAndRestore();

  // Park C again, and read the row as the planner will see it: the live
  // `flex-grow` of every pane still in the row. This is the one place the spec
  // looks at weights rather than pixels, and it is what lets it check its own
  // discriminating power.
  await paneC.locator('button.pane-btn[title="Minimize to dock (Alt+M)"]').click();
  await expect(paneC).toHaveCount(0);
  const grows = await page
    .locator(".pane")
    .evaluateAll((els) => els.map((el) => parseFloat((el as HTMLElement).style.flexGrow || "1")));
  const rowTotal = grows.reduce((a, b) => a + b, 0);
  const n = grows.length;

  // What each policy would give the returning pane, as a share of the row it
  // creates: `planEvenInsert` inserts the mean (total/n), the unrouted branch
  // inserts 1/n, and both land in a row whose total grows by that amount.
  const routedShare = rowTotal / n / (rowTotal + rowTotal / n);
  const unroutedShare = 1 / n / (rowTotal + 1 / n);

  // THE ANTI-VACUITY GUARD. If the two predictions are within noise of each
  // other, every assertion below passes under either policy and this spec is
  // decoration. That happens exactly at rowTotal === 1, and it is a real
  // possibility — a different split policy, or a change to the cross-direction
  // branch's `1 1 0` reset, gets there without touching this file.
  expect(
    Math.abs(routedShare - unroutedShare),
    `the two share policies would both put the restored pane at ~${routedShare.toFixed(4)} of ` +
      `this row (total ${rowTotal}, ${n} panes), so this spec cannot tell them apart — ` +
      `rebuild the layout so the row's total is not 1`
  ).toBeGreaterThan(0.05);

  // Cycle 2 — the one that is measured.
  const chip2 = page.locator(".dock-chip", { hasText: "Pane C" });
  await expect(chip2).toHaveCount(1);
  await chip2.click();
  await expect(paneC).toBeVisible();

  const after = await settledBoxes({ a: paneA, b: paneB, c: paneC });
  const widths = [after.a!.width, after.b!.width, after.c!.width];
  const restored = shareOf(after.c!.width, widths);
  const siblings = [shareOf(after.a!.width, widths), shareOf(after.b!.width, widths)];

  // 1. The routed policy's actual guarantee, which is sharper than "not the
  //    runt": a pane inserted at the row's MEAN comes back holding exactly
  //    1/(n+1) of the row — an equal share — whatever the row's total or skew.
  //    The unrouted branch cannot satisfy this off a total-1 row, and the
  //    guard above has already established that this row is not one.
  expect(
    restored,
    `the restored pane holds ${restored.toFixed(4)} of the row; the routed policy predicts ` +
      `${routedShare.toFixed(4)} and the unrouted 1/N branch predicts ${unroutedShare.toFixed(4)}`
  ).toBeCloseTo(routedShare, 2);

  // 2. And the plain-language version, which is what a human notices: it did
  //    not come back as the smallest pane in the row.
  expect(
    restored,
    `the restored pane came back at ${restored.toFixed(4)} of the row, under its smallest ` +
      `sibling ${Math.min(...siblings).toFixed(4)} — the share arm is not the even-matrix one`
  ).toBeGreaterThan(Math.min(...siblings) + SHARE_EPSILON);
});
