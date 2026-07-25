import { defineConfig } from "@playwright/test";

// E2E PoC (spike — see doc/design/e2e-testing.md). Drives the real WebView2
// webview over CDP (Playwright's native WebView2 support), never a Tauri
// plugin or a bundled browser: `chromium.connectOverCDP` just needs the CDP
// client, not a downloaded Chromium, so there's nothing to `playwright
// install` here.
//
// Single worker: each test launches its own loomux.exe against its own
// isolated profile (e2e/fixtures.ts), but they'd otherwise contend for a
// fixed CDP port. Parallelizing across ports is a follow-up, not a PoC need.
export default defineConfig({
  testDir: "./e2e/tests",
  // CI runners are slower to first-open the CDP port than a dev machine
  // (e2e/fixtures.ts itself waits up to 60s for that) — leave headroom above
  // that plus the post-connect page-ready waits.
  timeout: 120_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI
    ? [["list"], ["html", { outputFolder: "playwright-report", open: "never" }]]
    : "list",
});
