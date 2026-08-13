// The delivery-queue depth badge (#814), driven end to end through the real
// event path — this is the coverage that stands in for the human DOM look the
// PR originally asked for, so it deliberately asserts the things an eyeball
// would have caught and a unit test cannot:
//
//   1. the chip actually renders, with the count/cap/age in its own label
//      (#813's lesson: a detail only a hover reveals is a detail nobody sees);
//   2. a stalled queue is visibly different — asserted as a real computed-style
//      difference, so a stylesheet that never applied fails here rather than
//      looking fine to a selector;
//   3. a minimized pane keeps its count on the dock chip (delegate panes open
//      minimized, so this is where agent queues are actually seen);
//   4. on a narrow pane wearing several chips the header degrades gracefully
//      and — constraint 1 — the terminal's geometry does not move.
//
// **How it drives the badge, and what that does and does not prove.** The spec
// emits `orch-queue-depth` from the page through `plugin:event|emit`, which is
// exactly what the bundled `emit()` compiles to (see @tauri-apps/api's
// `event.js`/`core.js`) — so the event round-trips through the backend's real
// broadcaster and arrives at the app's real `listen()` handler. What is
// exercised is therefore the whole frontend half: handler → `readingsByPty` →
// `setQueueDepth` → DOM/CSS → dock mirror. What is NOT exercised is the Rust
// producer that decides *when* to emit; that half is pinned by the eight
// backend tests in `src-tauri/tests/orchestration.rs` (queue depth, the
// oldest-entry clock, the stall threshold, the coarsening, and the emit skip
// with its release). Splitting it this way is deliberate: producing a real
// queued delivery would mean spawning a real agent CLI, which this repo
// forbids outright.
//
// Every pane here is a plain shell (`createTerminalPane`), never an agent kind.
import { type Page } from "@playwright/test";
import { test, expect } from "../fixtures";
import { createTerminalPane, paneByName } from "../helpers";

/** One pane's reading, mirroring the Rust `queue::QueueDepthItem`. */
interface QueueDepthItem {
  pty_id: number;
  agent_id: string;
  depth: number;
  cap: number;
  waiting_ms: number;
  stalled: boolean;
}

/** Every pty id a handful of freshly-opened panes can plausibly hold.
 *
 *  The backend keys this event by pty id and nothing in the DOM exposes one, so
 *  rather than add a test-only hook to product code the spec addresses a small
 *  RANGE and lets the panes on screen match whichever ids they were given. The
 *  ids are minted by a per-process counter against the isolated `LOOMUX_DATA_DIR`
 *  this harness creates, so a handful of panes cannot get out of this range;
 *  ids nobody holds are simply unused by the handler, which looks panes up by
 *  their own pty. */
const PTYS = [1, 2, 3, 4, 5, 6, 7, 8];

function readings(over: Partial<QueueDepthItem>): QueueDepthItem[] {
  return PTYS.map((pty_id) => ({
    pty_id,
    agent_id: "w-1",
    depth: 3,
    cap: 8,
    waiting_ms: 12_000,
    stalled: false,
    ...over,
  }));
}

/** Emit a backend event from inside the page, the way the app's own `emit()`
 *  does. Permitted by the shipped ACL (`core:default` covers `core:event`), so
 *  this needs no capability widening and no test-only build flag. */
async function emitEvent(page: Page, event: string, payload: unknown): Promise<void> {
  await page.evaluate(
    async ([name, body]) => {
      const internals = (window as unknown as {
        __TAURI_INTERNALS__?: { invoke(cmd: string, args: unknown): Promise<unknown> };
      }).__TAURI_INTERNALS__;
      if (!internals) throw new Error("__TAURI_INTERNALS__ missing — not running inside the app");
      await internals.invoke("plugin:event|emit", { event: name, payload: body });
    },
    [event, payload] as const
  );
}

const pushDepths = (page: Page, items: QueueDepthItem[]) => emitEvent(page, "orch-queue-depth", items);

test("a queued delivery badges the pane header with its count and age, and draining clears it", async ({
  appPage: page,
}) => {
  await createTerminalPane(page, { name: "Queue A" });
  const chip = paneByName(page, "Queue A").locator(".pane-queue");

  await expect(chip, "a pane with nothing queued must wear no chip at all").toBeHidden();

  await pushDepths(page, readings({ depth: 3, waiting_ms: 12_000 }));
  await expect(chip).toBeVisible();
  // The requirement, not the implementation: count, cap and age all legible
  // without hovering anything.
  await expect(chip).toHaveText(/3\/8\s+queued/);
  await expect(chip).toHaveText(/12s/);

  // Absence is how a badge clears — there is no paired "cleared" event.
  await pushDepths(page, []);
  await expect(chip, "a pane absent from the pushed set must lose its badge").toBeHidden();
});

test("a stalled queue is visibly different from a merely busy one", async ({ appPage: page }) => {
  await createTerminalPane(page, { name: "Queue B" });
  const chip = paneByName(page, "Queue B").locator(".pane-queue");

  await pushDepths(page, readings({ depth: 2, waiting_ms: 12_000, stalled: false }));
  await expect(chip).toBeVisible();
  const busy = await chip.evaluate((el) => getComputedStyle(el).backgroundColor);

  await pushDepths(page, readings({ depth: 2, waiting_ms: 240_000, stalled: true }));
  await expect(chip).toHaveAttribute("data-stalled", "true");
  await expect(chip).toHaveText(/stalled/);
  const stalled = await chip.evaluate((el) => getComputedStyle(el).backgroundColor);

  // The part a human was going to eyeball: the amber treatment is really
  // applied, not merely selected for. A rule that never matched — a typo in the
  // attribute selector, a stylesheet that did not load — leaves these equal
  // while every other assertion above still passes.
  expect(stalled, `stalled and busy chips painted identically (${busy})`).not.toBe(busy);
});

test("a minimized pane keeps its queue count on the dock chip", async ({ appPage: page }) => {
  await createTerminalPane(page, { name: "Queue C" });
  const pane = paneByName(page, "Queue C");

  await pushDepths(page, readings({ depth: 5, stalled: true }));
  await expect(pane.locator(".pane-queue")).toBeVisible();

  await pane.locator('button.pane-btn[title="Minimize to dock (Alt+M)"]').click();
  const dockChip = page.locator(".dock-chip", { hasText: "Queue C" });
  await expect(dockChip).toBeVisible();

  // The pane's header is out of the DOM now, so this marker is the only
  // surface left — and it has to carry the NUMBER, not just a dot: "5 waiting"
  // versus "1 waiting" is not something styling can say.
  const marker = dockChip.locator(".dock-chip-queue");
  await expect(marker).toBeVisible();
  await expect(marker).toHaveText(/5/);
  await expect(marker).toHaveAttribute("data-stalled", "true");

  await pushDepths(page, []);
  await expect(marker, "the dock marker clears by absence too").toHaveCount(0);
});

test("a narrow pane wearing several chips stays usable, and the terminal never moves", async ({
  appPage: page,
}) => {
  // Three columns in a 1280px window, so each header is ~420px and has to fit a
  // title, the chips, and nine `flex: none` buttons — the crowding case review
  // finding 4 asked a human to look at.
  await createTerminalPane(page, { name: "Narrow A" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Narrow B" });
  await page.locator("#btn-split-right").click();
  await createTerminalPane(page, { name: "Narrow C" });

  const pane = paneByName(page, "Narrow C");
  const header = pane.locator(".pane-header");
  const term = pane.locator(".pane-term");

  const before = await term.boundingBox();
  expect(before, "the pane's terminal should have a bounding box").not.toBeNull();

  // Two chips at once, both keyed purely by pty so neither needs a roster: the
  // queue badge and #246's delivery-held chip.
  await pushDepths(page, readings({ depth: 8, cap: 8, waiting_ms: 600_000, stalled: true }));
  for (const pty of PTYS) {
    await emitEvent(page, "orch-delivery-held", { pty_id: pty, reason: "question" });
  }
  await expect(pane.locator(".pane-queue")).toBeVisible();
  await expect(pane.locator(".pane-held")).toBeVisible();

  // 1. Constraint 1, mechanically: header chrome must never reach the
  //    terminal's geometry. A resize would move this box.
  await expect
    .poll(async () => JSON.stringify(await term.boundingBox()), { timeout: 5_000 })
    .toBe(JSON.stringify(before));

  // 2. The header itself must not spill: `.pane-queue` gives way (it is the one
  //    chip allowed to shrink) instead of pushing the button cluster out.
  const overflow = await header.evaluate((el) => el.scrollWidth - el.clientWidth);
  expect(overflow, "the header overflowed horizontally on a narrow pane").toBeLessThanOrEqual(1);

  // 3. …and it must not give way to NOTHING. The chip is the one element here
  //    allowed to shrink, so a narrow header may clip it — but a chip clipped
  //    to zero has lost the count it exists to show. Clipping keeps the leading
  //    characters, which is why the label leads with the count; that ordering
  //    is a property of the CSS rather than something a textContent assertion
  //    could prove, so what is asserted is the width those characters need,
  //    plus (separately) that the label really does carry the depth.
  const chipBox = await pane.locator(".pane-queue").boundingBox();
  expect(chipBox?.width, "the queue chip collapsed to nothing on a narrow pane").toBeGreaterThan(20);
  await expect(pane.locator(".pane-queue")).toHaveText(/8\/8/);

  // 4. The title stays a usable drag source — the same guard pane-reorder.spec
  //    learned to assert, now with more fixed-width content beside it.
  const titleBox = await pane.locator(".pane-title").boundingBox();
  expect(titleBox?.width, "the title collapsed to an unusable drag source").toBeGreaterThan(10);
});
