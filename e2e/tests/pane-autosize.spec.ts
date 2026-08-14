// Regression class: the Autosize gesture's real geometry (#936). A unit test
// can prove the weight math (test/paneautosize.test.ts does, exhaustively); it
// cannot prove that pressing the button in the actual app makes the actual
// panes equal, which is the whole claim. This spec does that end to end: build
// a row the split policy leaves deliberately uneven, press Autosize, and assert
// the panes come out equal.
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

  // Precondition, asserted rather than assumed: splitting does NOT leave the
  // row even — that is the point of the feature, and if a future split policy
  // ever did leave it even, this spec would be silently proving nothing.
  // Deliberately phrased as "not all equal" rather than as specific fractions,
  // so it holds for any split policy (today's even-share insert, #885 slice A's
  // halve, anything later) as long as some unevenness remains.
  const before = await paneShares(names, page);
  expect(before, "all three panes should be on screen").not.toBeNull();
  const spreadBefore = Math.max(...before!) - Math.min(...before!);
  expect(spreadBefore, `splits already left the row even (${JSON.stringify(before)})`).toBeGreaterThan(0.02);

  await page.locator("#btn-autosize").click();

  // 1% of the row, against a spread of 15-25% for any wrong policy: the naive
  // "one weight per child" would leave this flat row alone (it is already flat,
  // so it agrees here), and doing nothing at all leaves `spreadBefore`.
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
