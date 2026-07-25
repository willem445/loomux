// Regression class: an overlay docked over a terminal pane rendering behind
// or clipped by the terminal instead of on top (the "plugin z-order / embed
// docking" bug class this spike targets — see doc/design/e2e-testing.md).
//
// Substitutes for a task-board overlay test: the task board only appears on
// an orchestrator-role pane, which requires actually running one of the
// supported agent CLIs (src/launcher.ts `ORCH_CLIS`) — forbidden for
// automated E2E (never spawn real agent CLIs, CLAUDE.md constraint 3). The
// git-view overlay (Alt+G) is available on any plain shell pane and is built
// from the exact same `.git-overlay` docking mechanism (src/pane.ts) that the
// task board, audit log, and group-lifecycle overlays all share, so it
// exercises the identical z-order/clipping code path safely.
import { test, expect } from "../fixtures";
import { createTerminalPane, paneByName } from "../helpers";

test("the git-view overlay docks visibly over its pane and is interactive", async ({
  appPage: page,
}) => {
  await createTerminalPane(page, { name: "Repo pane", repo: process.cwd() });
  const pane = paneByName(page, "Repo pane");

  const overlay = pane.locator(".git-overlay");
  await expect(overlay).toBeHidden();

  await pane.locator('.pane-btn[title^="Git view"]').click();
  await expect(overlay).toBeVisible();

  // Docked *over* the terminal, not squeezed beside it or behind it: the
  // overlay should occupy real, on-screen space within the pane's bounds.
  const paneBox = await pane.boundingBox();
  const overlayBox = await overlay.boundingBox();
  expect(paneBox).not.toBeNull();
  expect(overlayBox).not.toBeNull();
  expect(overlayBox!.width).toBeGreaterThan(50);
  expect(overlayBox!.height).toBeGreaterThan(50);
  expect(overlayBox!.x).toBeGreaterThanOrEqual(paneBox!.x - 1);
  expect(overlayBox!.x + overlayBox!.width).toBeLessThanOrEqual(paneBox!.x + paneBox!.width + 1);

  // Interactive, not just painted: it loaded real commit history for the repo
  // the pane is rooted in, and clicking a row is live (selects it).
  const firstRow = overlay.locator(".git-row").first();
  await expect(firstRow).toBeVisible({ timeout: 10_000 });
  await firstRow.click();
  await expect(firstRow).toHaveClass(/selected/);

  await pane.locator('.pane-btn[title^="Git view"]').click();
  await expect(overlay).toBeHidden();
});
