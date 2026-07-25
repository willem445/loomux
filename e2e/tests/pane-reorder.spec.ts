// Regression class: pane split + drag-reorder geometry (the kind of bug this
// spike exists to catch — see doc/design/e2e-testing.md). Splits a fresh tab
// into two plain shell panes, drags one onto the other's center (a "swap"
// drop, src/grid.ts `Grid.swap`), and asserts their left-to-right DOM order
// actually flips — a pure unit test of grid.ts can't see this because it
// depends on real layout geometry and real pointer events.
import { test, expect } from "../fixtures";
import { createTerminalPane, paneByName } from "../helpers";

test("dragging a pane onto another's center swaps their on-screen order", async ({ appPage: page }) => {
  await createTerminalPane(page, { name: "Pane A" });

  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane B" });

  const paneA = paneByName(page, "Pane A");
  const paneB = paneByName(page, "Pane B");

  const boxA1 = await paneA.boundingBox();
  const boxB1 = await paneB.boundingBox();
  expect(boxA1, "Pane A should have a bounding box").not.toBeNull();
  expect(boxB1, "Pane B should have a bounding box").not.toBeNull();
  const aWasLeftOfB = boxA1!.x < boxB1!.x;

  const headerB = paneB.locator(".pane-header");
  const headerBox = await headerB.boundingBox();
  expect(headerBox).not.toBeNull();

  // Re-read A's box right before dragging: it's the drop TARGET, so drag onto
  // its live center rather than a value captured before the split settled.
  const targetBox = await paneA.boundingBox();
  expect(targetBox).not.toBeNull();
  const targetCenterX = targetBox!.x + targetBox!.width / 2;
  const targetCenterY = targetBox!.y + targetBox!.height / 2;

  await page.mouse.move(headerBox!.x + headerBox!.width / 2, headerBox!.y + headerBox!.height / 2);
  await page.mouse.down();
  // Multiple intermediate steps: grid.ts only arms the drag past a pixel
  // threshold (DRAG_THRESHOLD_PX), so a single jump could land before the
  // drop-zone hit-test logic ever runs.
  await page.mouse.move(targetCenterX, targetCenterY, { steps: 20 });
  await page.mouse.up();

  await expect
    .poll(async () => {
      const a = await paneA.boundingBox();
      const b = await paneB.boundingBox();
      if (!a || !b) return null;
      return a.x < b.x;
    })
    .toBe(!aWasLeftOfB);
});
