// #1320 theme: capture the real themed UI, and assert the two things about it
// that a screenshot alone cannot prove.
//
// WHY THIS SPEC EXISTS. The theme change is mostly visual, and the visual half
// is validated by a human looking at it — this repo does not diff screenshots
// (no baseline images are committed, and a pixel baseline on a WebView2 runner
// would be a flake factory). So the screenshots here are ATTACHMENTS for the
// review, not assertions: they ride in the Playwright report artifact so the
// reviewer and the human can see the shipped theme without building the app.
//
// The two assertions are the parts that ARE mechanical, and both are things a
// screenshot would hide:
//
//   1. the tokens actually resolved. A stylesheet that failed to load, or a
//      `:root` that drifted from theme.ts, still screenshots as *a* dark app —
//      it just looks a bit wrong, and nobody can tell from an image whether
//      the ground is #111111 or the old #0f1114. Read the computed value and
//      compare it to theme.ts's own export.
//   2. the ground carries no hue AS COMPUTED. theme.test.ts pins the hex in
//      the source; this pins what the browser actually paints after the
//      cascade, which is the claim "#1320 killed the blue cast" really makes.
import { test, expect } from "../fixtures";
import { createTerminalPane } from "../helpers";
import { SEMANTIC, PALETTE } from "../../src/theme.ts";

/** `rgb(r, g, b)` / `rgba(...)` -> [r,g,b]. Throws rather than returning a
 *  default, so a selector that matched nothing fails loudly instead of
 *  silently asserting about black. */
function rgb(value: string): [number, number, number] {
  const m = value.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  if (!m) throw new Error(`not an rgb() colour: ${JSON.stringify(value)}`);
  return [Number(m[1]), Number(m[2]), Number(m[3])];
}
const hexOf = (v: string) =>
  "#" + rgb(v).map((c) => c.toString(16).padStart(2, "0")).join("");

test("the themed shell paints theme.ts's own tokens, and its ground is achromatic", async ({
  appPage: page,
}, testInfo) => {
  await createTerminalPane(page, "theme-shot");
  // Let the pane settle so the shot is not caught mid-fit.
  await expect(page.locator(".pane").first()).toBeVisible();

  const tokens = await page.evaluate(() => {
    const s = getComputedStyle(document.documentElement);
    const read = (n: string) => s.getPropertyValue(n).trim();
    return {
      surface0: read("--surface-0"),
      surface1: read("--surface-1"),
      surface2: read("--surface-2"),
      line: read("--line"),
      ink: read("--ink"),
      accent: read("--accent"),
      stateOk: read("--state-ok"),
      bodyBg: getComputedStyle(document.body).backgroundColor,
    };
  });

  // 1. the tokens resolved to what theme.ts says they are.
  expect(tokens.surface0.toLowerCase()).toBe(SEMANTIC.surface0);
  expect(tokens.surface1.toLowerCase()).toBe(SEMANTIC.surface1);
  expect(tokens.surface2.toLowerCase()).toBe(SEMANTIC.surface2);
  expect(tokens.line.toLowerCase()).toBe(SEMANTIC.line);
  expect(tokens.ink.toLowerCase()).toBe(SEMANTIC.ink);
  expect(tokens.accent.toLowerCase()).toBe(PALETTE.gold);
  expect(tokens.stateOk.toLowerCase()).toBe(SEMANTIC.stateOk);

  // 2. what the browser actually paints on <body> carries no hue. This is the
  //    computed end of the cascade, not the source hex — a later rule that
  //    re-tinted the ground would pass theme.test.ts and fail here.
  const [r, g, b] = rgb(tokens.bodyBg);
  expect(
    [g - r, b - r],
    `the painted app ground ${hexOf(tokens.bodyBg)} must be achromatic — r=${r} g=${g} ` +
      `b=${b} (#1320 ask 1)`
  ).toEqual([0, 0]);

  // --- the attachments the review actually looks at.
  await testInfo.attach("themed-shell.png", {
    body: await page.screenshot(),
    contentType: "image/png",
  });
  const header = page.locator(".topbar, .top-bar, header").first();
  if (await header.count()) {
    await testInfo.attach("themed-chrome.png", {
      body: await header.screenshot(),
      contentType: "image/png",
    });
  }
  await testInfo.attach("themed-pane.png", {
    body: await page.locator(".pane").first().screenshot(),
    contentType: "image/png",
  });
});
