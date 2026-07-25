import { defineConfig } from "@playwright/test";

// E2E PoC (spike — see doc/design/e2e-testing.md). Drives the real WebView2
// webview over CDP (Playwright's native WebView2 support), never a Tauri
// plugin or a bundled browser: `chromium.connectOverCDP` just needs the CDP
// client, not a downloaded Chromium, so there's nothing to `playwright
// install` here.
//
// Single worker: every E2E instance shares one Tauri identifier
// (tauri.e2e.conf.json), and WebView2 keys its browser process off that
// identifier (#394) — so concurrent instances would share a browser process
// with EACH OTHER, not just with a production install. A fixed CDP port
// (e2e/fixtures.ts) is a symptom of the same constraint, not the cause: it's
// fixed because the CI job's HKLM policy workaround for WebView2 150+'s
// High-IL env-var restriction (WebView2Feedback#5640) needs one static value,
// not because ports themselves are scarce. Real parallelization needs
// per-worker identifiers or an isolation mechanism that survives High IL
// (WEBVIEW2_USER_DATA_FOLDER does — see e2e/fixtures.ts's module doc — but
// using it would break the identifier-based build verification that same
// file performs). See doc/design/e2e-testing.md's roadmap.
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
  // Screenshots/traces make a CI-only failure diagnosable from the report
  // artifact instead of blind-guessing another fix from the assertion alone.
  use: {
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
});
