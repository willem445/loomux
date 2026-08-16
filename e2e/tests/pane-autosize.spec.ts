// Regression class: the Autosize gesture's real geometry (#936). A unit test
// can prove the weight math (test/paneautosize.test.ts does, exhaustively); it
// cannot prove that pressing the button in the actual app makes the actual
// panes equal, which is the whole claim. This spec does that end to end: make a
// layout genuinely uneven, press Autosize, and assert the panes come out equal.
//
// HOW THE UNEVENNESS IS BUILT, AND WHY IT CHANGED. This spec used to lean on
// the split policy to leave the row uneven for it. Since #945 that is no longer
// true — a same-direction split lands the newcomer at the mean, so three splits
// give three equal thirds and the old precondition asserted something false. It
// was written to fail loudly if that ever happened rather than to quietly stop
// proving anything, which is exactly what it did. The two remaining sources of
// unevenness are the real ones, and they are what the two tests now use: a
// divider the human dragged (below) and nesting (the second test).
//
// Measured in the SHARE domain — each pane's width over the sum of the pane
// widths — not in pixels. Two reasons, one of them learned the hard way on
// #900's spec: equal-weight flex siblings are not guaranteed equal to the pixel
// (the engine distributes fractional free space item by item and rounds), and
// the dividers between panes are `flex: none`, so they are not part of what is
// shared out. Dividing by the sum of the pane widths takes both out of the
// picture and leaves exactly the quantity the feature promises.
import { type Page } from "@playwright/test";
import { test, expect } from "../fixtures";
import { createTerminalPane, paneByName } from "../helpers";

/** Every pane's width as a fraction of the total width the panes occupy. */
async function paneShares(names: string[], page: Page): Promise<number[] | null> {
  const widths: number[] = [];
  for (const name of names) {
    const box = await paneByName(page, name).boundingBox();
    if (!box) return null;
    widths.push(box.width);
  }
  const total = widths.reduce((a, b) => a + b, 0);
  if (total <= 0) return null;
  return widths.map((w) => w / total);
}

test("Autosize makes every pane in the tab an equal share", async ({ appPage: page }) => {
  const names = ["Pane A", "Pane B", "Pane C"];

  await createTerminalPane(page, { name: names[0] });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: names[1] });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: names[2] });

  // Skew the row the way a human does: drag the first divider left, shrinking
  // A and growing B. That is the source of unevenness a flat row still has
  // after #945, and it is the one a human presses Autosize to undo.
  const divider = page.locator(".divider").first();
  const dividerBox = await divider.boundingBox();
  expect(dividerBox, "the A|B divider should be on screen to drag").not.toBeNull();
  const midY = dividerBox!.y + dividerBox!.height / 2;
  const midX = dividerBox!.x + dividerBox!.width / 2;
  await page.mouse.move(midX, midY);
  await page.mouse.down();
  // In steps, so the grid's mousemove handler runs during the drag rather than
  // receiving one teleport, and clear of the MIN_PANE_PX clamp at either end.
  await page.mouse.move(midX - 120, midY, { steps: 10 });
  await page.mouse.up();

  // Precondition, asserted rather than assumed: the drag really did skew the
  // row. If it silently missed the divider, this fails loudly here instead of
  // letting the Autosize assertion below pass against an already-even row and
  // prove nothing. Phrased as "not all equal" rather than as fractions, so it
  // holds whatever the split policy underneath is.
  const before = await paneShares(names, page);
  expect(before, "all three panes should be on screen").not.toBeNull();
  const spreadBefore = Math.max(...before!) - Math.min(...before!);
  expect(spreadBefore, `the divider drag left the row even (${JSON.stringify(before)})`).toBeGreaterThan(
    0.02
  );

  await page.locator("#btn-autosize").click();

  // 1% of the row, against the ~19-point spread the drag above leaves: doing
  // nothing at all keeps `spreadBefore`, and any policy that reads the current
  // weights rather than the tree's shape keeps some of it too. (The naive "one
  // weight per child" agrees with the right answer on a flat row — that policy
  // is what the second test, and the unit suite, separate out.)
  await expect
    .poll(
      async () => {
        const shares = await paneShares(names, page);
        if (!shares) return null;
        return Math.max(...shares) - Math.min(...shares);
      },
      { timeout: 15_000 }
    )
    .toBeLessThan(0.01);

  const after = await paneShares(names, page);
  for (const share of after!) {
    expect(Math.abs(share - 1 / 3), `panes should each hold a third: ${JSON.stringify(after)}`).toBeLessThan(
      0.01
    );
  }
});

test("Autosize evens out panes at different depths, not just one row", async ({ appPage: page }) => {
  // The case the naive "give every child of every split the same weight" policy
  // gets wrong, and the reason the weight is a pane COUNT: one pane beside a
  // stacked pair must come out as three equal thirds of the width-times-height
  // area, not as a half and two quarters.
  const names = ["Pane A", "Pane B", "Pane C"];

  await createTerminalPane(page, { name: names[0] });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: names[1] });
  // Split DOWN from B: B's slot becomes a nested column of B and C.
  await page.locator("#btn-split-down").click();
  await createTerminalPane(page, { name: names[2] });

  await page.locator("#btn-autosize").click();

  // Areas, not widths: A is full height and a third of the width; B and C are
  // two thirds of the width and half the height each. Equal areas is what the
  // feature promises for a nested tree, so that is what is measured.
  await expect
    .poll(
      async () => {
        const areas: number[] = [];
        for (const name of names) {
          const box = await paneByName(page, name).boundingBox();
          if (!box) return null;
          areas.push(box.width * box.height);
        }
        const total = areas.reduce((a, b) => a + b, 0);
        if (total <= 0) return null;
        const shares = areas.map((a) => a / total);
        return Math.max(...shares) - Math.min(...shares);
      },
      { timeout: 15_000 }
    )
    // Looser than the flat-row bound on purpose: these panes sit at different
    // depths, so they carry different numbers of `flex: none` dividers and lose
    // slightly different amounts of area to them. 3% still separates this by an
    // order of magnitude from the 25-point gap the wrong policy leaves (a half
    // against two quarters).
    .toBeLessThan(0.03);
});
