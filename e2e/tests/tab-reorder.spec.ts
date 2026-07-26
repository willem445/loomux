// Regression class: tab-strip drag-and-drop reorder (#379) reported broken in
// a real dev build (#402 live-demo round 5) after the pure `moveTab`/
// `dropTargetIndex` logic (test/tabs.test.ts) and a code-review DOM-wiring
// fix both looked correct — a "grab works, drop is refused everywhere"
// symptom node:test structurally cannot see: it needs a real drag session
// against real DOM elements in a real browser.
//
// Root cause (see src/tabbar.ts's `wireDrag` doc comment for the full
// investigation): the tab strip used native HTML5 `draggable="true"`
// drag-and-drop. Neither manual `DragEvent` dispatch nor Playwright's
// `locator.dragTo()` could ever reproduce a failure against it — both
// techniques succeeded reliably, with or without the prior review's
// dragenter/dropEffect fix — pointing at native DnD's initiation against a
// REAL OS-level mouse gesture inside a WebView2-hosted Tauri window as the
// actual gap, a layer no CDP-driven test can reach. The fix replaces native
// DnD with the same POINTER-EVENT mechanism `src/grid.ts`'s pane-reorder
// drag already uses successfully (see `pane-reorder.spec.ts`, which drives
// it with this exact `locator.dragTo()` API) — so THIS spec's job is no
// longer "prove native DnD is reachable" but "prove the reorder itself
// works," which is directly, reliably testable this way.
import { test, expect } from "../fixtures";

test("dragging a tab past another one reorders the tab strip", async ({ appPage: page }) => {
  const tabBar = page.locator("#tab-bar");

  // The boot default tab is already "Tab 1" (tabs.ts's sequential auto-name);
  // "+" creates "Tab 2" / "Tab 3" the same way, with no pane setup needed —
  // the tab strip renders a `.tab` entry for a workspace the instant it
  // exists, before its welcome pane is ever submitted.
  await tabBar.locator(".tab-add").click();
  await tabBar.locator(".tab-add").click();
  await expect(tabBar.locator(".tab")).toHaveCount(3);

  const tabNames = () => tabBar.locator(".tab .tab-name").allTextContents();
  await expect.poll(tabNames).toEqual(["Tab 1", "Tab 2", "Tab 3"]);

  const tabByName = (name: string) =>
    tabBar.locator(".tab", { has: page.locator(".tab-name", { hasText: name, exact: true }) });

  // Drop on the target's right 15% rather than its exact center: `dropsBefore`
  // (tabbar.ts) is a strict `<` comparison against the target's midpoint, so a
  // drop at the mathematically exact center is a genuine tie the app breaks
  // toward "before" — landing right-of-center is what a human dragging "onto"
  // Tab 3 actually does, and it's the only unambiguous way to assert a
  // specific resulting order rather than either side of the tie.
  const target3Box = (await tabByName("Tab 3").boundingBox())!;
  await tabByName("Tab 1").dragTo(tabByName("Tab 3"), {
    targetPosition: { x: target3Box.width * 0.85, y: target3Box.height / 2 },
  });

  await expect
    .poll(tabNames, {
      message: "tab order did not change after the drag-and-drop — the drop was refused or never handled",
    })
    .toEqual(["Tab 2", "Tab 3", "Tab 1"]);
});
