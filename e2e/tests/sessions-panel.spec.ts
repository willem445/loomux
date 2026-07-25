// Regression class: an overlay/side-panel that resizes the workspace instead
// of floating (constraint: "never resize the PTY for a UI feature" — see
// CLAUDE.md and doc/design/e2e-testing.md) should still leave the workspace
// exactly where it found it once closed. `#sessions` animates its own width
// (src/styles.css) rather than the grid resizing itself, so this asserts the
// grid area's width returns to its pre-open value, not just that the panel
// closed.
import { test, expect } from "../fixtures";

test("opening and closing the sessions panel restores the workspace width", async ({
  appPage: page,
}) => {
  const gridArea = page.locator("#grid-area");
  const sessions = page.locator("#sessions");

  const widthBefore = (await gridArea.boundingBox())!.width;

  await page.locator("#btn-sessions").click();
  await expect(sessions).not.toHaveClass(/hidden/);
  await expect
    .poll(async () => (await sessions.boundingBox())!.width, { timeout: 5_000 })
    .toBeGreaterThan(100);

  const widthOpen = (await gridArea.boundingBox())!.width;
  expect(widthOpen).toBeLessThan(widthBefore);

  await page.locator("#btn-sessions").click();
  await expect(sessions).toHaveClass(/hidden/);
  await expect
    .poll(
      async () => Math.abs((await gridArea.boundingBox())!.width - widthBefore),
      { timeout: 5_000 }
    )
    .toBeLessThan(2);
});
