// Regression class: WHO PAYS for a split (#885 slice A). Builds a 3-wide row
// of shell panes, splits the leftmost one right from its own header ◫, and
// asserts the other two panes stay exactly where they were.
//
// This assertion is the constraint-1 win, machine-checked. `halve` exists so a
// human split resizes ONE pane instead of every pane in the row — and "the
// other panes didn't move" is precisely "the other panes' terminals were never
// resized", since `applyFit` skips a same-size fit (src/panefit.ts). A unit
// test can prove the weight arithmetic (test/splitfloor.test.ts) but not that
// the arithmetic reaches real flex boxes; only real layout geometry can.
//
// Pre-#885 (the `share` policy, still used by the multi-agent fan-out) this
// same gesture inserted the newcomer at 1/N ON TOP of the row's weights, which
// on this exact layout shrinks each of the two untouched panes by roughly 15%
// of the row and shifts them tens of pixels — an order of magnitude past the
// tolerance below, so this spec discriminates the two policies rather than
// merely tolerating both.
import { test, expect } from "../fixtures";
import { createTerminalPane, paneByName } from "../helpers";

// `.divider` is `width: 6px` (src/styles.css) — a REAL flex sibling, so a
// same-direction split inserting a fourth pane also inserts a third divider
// and takes those pixels out of the row's free space. That is the one thing
// that can legitimately move an untouched sibling, and it is bounded by a
// single divider's width no matter how many panes the row holds. Hence
// "identical within one divider" rather than a bare pixel equality that could
// only be satisfied by not drawing the divider at all.
const DIVIDER_PX = 6;

test("splitting a pane in a 3-wide row halves that pane and leaves its siblings where they were", async ({
  appPage: page,
}) => {
  // Pane A fills the tab; each split lands beside the pane that was just
  // created (the new pane takes focus), so this builds the row A | B | C.
  await createTerminalPane(page, { name: "Pane A" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane B" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Pane C" });

  const paneA = paneByName(page, "Pane A");
  const paneB = paneByName(page, "Pane B");
  const paneC = paneByName(page, "Pane C");
  await expect(paneA).toBeVisible();
  await expect(paneB).toBeVisible();
  await expect(paneC).toBeVisible();

  const before = {
    a: await paneA.boundingBox(),
    b: await paneB.boundingBox(),
    c: await paneC.boundingBox(),
  };
  for (const [name, box] of Object.entries(before)) {
    expect(box, `pane ${name} should have a bounding box before the split`).not.toBeNull();
  }
  // Guard the premise: all three really are side by side in one row (a nested
  // layout would make the sibling assertion below vacuous — a pane in another
  // subtree cannot move whatever the policy is).
  expect(before.a!.y, "A and B should share a row").toBeCloseTo(before.b!.y, 0);
  expect(before.b!.y, "B and C should share a row").toBeCloseTo(before.c!.y, 0);
  expect(before.a!.x).toBeLessThan(before.b!.x);
  expect(before.b!.x).toBeLessThan(before.c!.x);

  // Split pane A — the LEFTMOST, so both siblings sit downstream of the change
  // and would be pushed by any re-share of the row. Its own header button, so
  // this exercises the `onSplit` gesture wiring (src/main.ts `eventsFor`)
  // rather than the toolbar's.
  await paneA.locator('.pane-btn[title="Split right"]').click();
  // The new pane opens on the welcome/setup form — wait for it to exist before
  // measuring anything, so no read races the relayout.
  await expect(page.locator(".welcome-form")).toHaveCount(1);

  const after = {
    a: await paneA.boundingBox(),
    b: await paneB.boundingBox(),
    c: await paneC.boundingBox(),
  };
  for (const [name, box] of Object.entries(after)) {
    expect(box, `pane ${name} should still have a bounding box after the split`).not.toBeNull();
  }

  // 1. The pane that was split paid for the newcomer, out of its own width.
  expect(
    after.a!.width,
    "the split pane should have given up about half its width"
  ).toBeGreaterThan(before.a!.width / 2 - DIVIDER_PX);
  expect(after.a!.width, "the split pane should have given up about half its width").toBeLessThan(
    before.a!.width / 2 + DIVIDER_PX
  );

  // 2. Nobody else moved. Vertical geometry is exact — a same-direction split
  // in a row cannot touch heights at all — while horizontal geometry is
  // allowed the one divider's worth of give explained above.
  for (const [name, was, now] of [
    ["Pane B", before.b!, after.b!],
    ["Pane C", before.c!, after.c!],
  ] as const) {
    expect(now.y, `${name} moved vertically`).toBeCloseTo(was.y, 0);
    expect(now.height, `${name} changed height`).toBeCloseTo(was.height, 0);
    expect(
      Math.abs(now.x - was.x),
      `${name} was pushed sideways — the split re-shared the whole row`
    ).toBeLessThanOrEqual(DIVIDER_PX);
    expect(
      Math.abs(now.width - was.width),
      `${name} was resized — the split re-shared the whole row`
    ).toBeLessThanOrEqual(DIVIDER_PX);
  }
});
