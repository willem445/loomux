// E2E harness (spike, see doc/design/e2e-testing.md): launches the built
// loomux.exe against an isolated profile and hands back a connected Playwright
// Page talking to its main WebView2 webview over CDP.
//
// Isolation (issue #394 overlap — deliberately generic, not test-only):
// - `LOOMUX_DATA_DIR` points loomux's own app-data root (orchestration/,
//   logs/, tabs.json, running.lock) at a fresh temp dir per test run, so an
//   E2E run never touches — or collides with — a real install's state.
// - The exe under test must be built with `tauri.e2e.conf.json`'s
//   `identifier` override (`dev.loomux.e2e`, vs the product's
//   `dev.loomux.app`). WebView2 keys its user-data folder (and thus its
//   *shared browser process*, see #394) off that identifier, so a
//   differently-identified build can never share — or hard-kill — a running
//   production instance's WebView2 process, regardless of how this harness
//   tears the process down.
// Never point LOOMUX_E2E_EXE at an installed/production build.
import { test as base, chromium, type Page } from "@playwright/test";
import { type ChildProcess, spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const DEFAULT_EXE = path.resolve(__dirname, "../src-tauri/target/debug/loomux.exe");
const EXE = process.env.LOOMUX_E2E_EXE ?? DEFAULT_EXE;

// One CDP port per worker so parallel workers (if ever enabled) don't race on
// the same debugging endpoint.
function cdpPort(workerIndex: number): number {
  return 9333 + workerIndex;
}

async function connectWithRetry(
  url: string,
  proc: ChildProcess,
  output: { stdout: string; stderr: string },
  timeoutMs = 60_000
): Promise<Awaited<ReturnType<typeof chromium.connectOverCDP>>> {
  const deadline = Date.now() + timeoutMs;
  let lastErr: unknown;
  while (Date.now() < deadline) {
    // A dead process will never open the port — fail fast with whatever it
    // printed instead of burning the rest of the timeout on retries.
    if (proc.exitCode !== null || proc.signalCode !== null) {
      throw new Error(
        `loomux.exe exited early (code=${proc.exitCode} signal=${proc.signalCode}) before opening ` +
          `the CDP port.\n--- stdout ---\n${output.stdout}\n--- stderr ---\n${output.stderr}`
      );
    }
    try {
      return await chromium.connectOverCDP(url);
    } catch (err) {
      lastErr = err;
      await new Promise((r) => setTimeout(r, 500));
    }
  }
  throw new Error(
    `could not connect to WebView2 CDP endpoint at ${url} within ${timeoutMs}ms: ${String(lastErr)}\n` +
      `--- stdout ---\n${output.stdout}\n--- stderr ---\n${output.stderr}`
  );
}

export const test = base.extend<{ appPage: Page }>({
  // eslint-disable-next-line no-empty-pattern
  appPage: async ({}, use, testInfo) => {
    if (!fs.existsSync(EXE)) {
      throw new Error(
        `loomux exe not found at ${EXE} — build it first with:\n` +
          `  npx tauri build --debug --no-bundle --config src-tauri/tauri.e2e.conf.json`
      );
    }

    const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "loomux-e2e-"));
    const port = cdpPort(testInfo.workerIndex);

    const proc: ChildProcess = spawn(EXE, [], {
      env: {
        ...process.env,
        LOOMUX_DATA_DIR: dataDir,
        // --disable-gpu: on a CI runner with no real GPU/driver, WebView2's
        // Chromium renderer can hang indefinitely during GPU-process init
        // instead of erroring — the exact symptom seen on windows-latest
        // (process alive the whole timeout, zero output, CDP port never
        // opens). Forces software rendering instead.
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} --disable-gpu`,
      },
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: false,
    });
    const output = { stdout: "", stderr: "" };
    proc.stdout?.on("data", (d: Buffer) => (output.stdout += d.toString()));
    proc.stderr?.on("data", (d: Buffer) => (output.stderr += d.toString()));
    // A spawn failure (e.g. bad EXE path) emits 'error' async; without a
    // listener Node treats it as unhandled and crashes the whole worker.
    proc.on("error", (err) => {
      output.stderr += `\n[spawn error] ${String(err)}`;
    });

    try {
      const browser = await connectWithRetry(`http://127.0.0.1:${port}`, proc, output);
      const context = browser.contexts()[0] ?? (await browser.waitForEvent("context"));
      const page = context.pages()[0] ?? (await context.waitForEvent("page"));
      await page.waitForSelector("#tab-bar", { state: "attached", timeout: 30_000 });
      await page.waitForSelector("#workspace-stack .pane, #workspace-stack .welcome-form", {
        timeout: 30_000,
      });

      await use(page);

      await browser.close();
    } finally {
      // A hard kill here only ever tears down THIS isolated identifier's
      // WebView2 browser process (see module doc) — never a shared one.
      proc.kill();
      fs.rmSync(dataDir, { recursive: true, force: true });
    }
  },
});

export { expect } from "@playwright/test";
