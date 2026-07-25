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

  // Playwright's own `dragTo` (hover source -> mouse down -> move to target in
  // steps, with actionability waits at each phase -> mouse up), rather than a
  // hand-rolled mouse.move/down/up sequence: two attempts at hand-timed pauses
  // around a manual sequence still failed to register the drop reliably on a
  // CI runner (passed 100% locally every time) even though every individual
  // mouse.* call completed without error per the failing run's trace — the
  // sequence "worked" but the app-side drag state never armed/committed.
  // `dragTo`'s built-in waits are more conservative than fixed pauses.
  await paneB.locator(".pane-header").dragTo(paneA);

  await expect
    .poll(
      async () => {
        const a = await paneA.boundingBox();
        const b = await paneB.boundingBox();
        if (!a || !b) return null;
        return a.x < b.x;
      },
      { timeout: 15_000 }
    )
    .toBe(!aWasLeftOfB);
});
