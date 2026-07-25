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

  // Instrumentation for the CI-only failure this spec has hit twice now (a
  // hand-rolled mouse sequence, then Playwright's own `dragTo` — both
  // complete with zero API-level errors, at correct coordinates per the
  // failure screenshots, but the swap never happens). Records every
  // pointerdown/move/up the window actually receives during the drag so a
  // third failure is diagnosable from the CI log directly instead of another
  // guess-and-push round.
  await page.evaluate(() => {
    const events: unknown[] = [];
    (window as unknown as { __e2eDrag: unknown[] }).__e2eDrag = events;
    for (const type of ["pointerdown", "pointermove", "pointerup"]) {
      window.addEventListener(
        type,
        (e) => {
          const pe = e as PointerEvent;
          const target = pe.target as HTMLElement | null;
          events.push({
            type,
            x: pe.clientX,
            y: pe.clientY,
            targetClass: target?.className ?? null,
          });
        },
        true
      );
    }
  });

  await paneB.locator(".pane-header").dragTo(paneA);

  try {
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
  } catch (err) {
    const events = await page.evaluate(
      () => (window as unknown as { __e2eDrag: unknown[] }).__e2eDrag
    );
    // eslint-disable-next-line no-console
    console.log("DRAG_DIAGNOSTIC", JSON.stringify(events));
    throw err;
  }
});
