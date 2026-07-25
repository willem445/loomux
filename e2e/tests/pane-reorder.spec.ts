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

  // Root-caused via a CI-only failure (see git history on this file for the
  // investigation): dragging from `.pane-header`'s overall center is NOT safe
  // — the header packs ~9 icon buttons (editor/issues/git/file-edit/split
  // ×2/minimize/maximize/close) against a short title, so the header's true
  // midpoint can land on a button rather than empty space depending on
  // exactly how wide the rendered buttons/title are. Locally that midpoint
  // happened to fall clear; on the CI runner it landed on a button's SVG
  // icon (confirmed by instrumenting pointer events: the pointerdown target
  // was inside a `<button>`) — and `Grid.onPointerDown` (src/grid.ts)
  // explicitly refuses to start a drag when the target is inside
  // `button, input, .pane-meta-item`, by design (so clicking a header button
  // never accidentally starts a reorder). `.pane-title` is always
  // left-anchored ahead of every button, so it can never collide with one —
  // but it's `flex: 0 1 auto; min-width: 0` (styles.css), so at a narrow
  // enough header it shrinks toward zero width and stops being a usable grab
  // point. Assert it's a real target rather than assume: a title collapsed to
  // nothing is a loud, specific failure here instead of a silent no-op.
  const titleBox = await paneB.locator(".pane-title").boundingBox();
  expect(titleBox?.width, "Pane B's title collapsed to an unusable drag source").toBeGreaterThan(10);
  await paneB.locator(".pane-title").dragTo(paneA);

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
