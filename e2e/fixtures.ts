// E2E harness (spike, see doc/design/e2e-testing.md): launches the built
// orrerix.exe against an isolated profile and hands back a connected Playwright
// Page talking to its main WebView2 webview over CDP.
//
// Isolation (issue #394 overlap — deliberately generic, not test-only):
// - `ORRERIX_DATA_DIR` points orrerix's own app-data root (orchestration/,
//   logs/, tabs.json, running.lock) at a fresh temp dir per test run, so an
//   E2E run never touches — or collides with — a real install's state.
// - The exe under test must be built with `tauri.e2e.conf.json`'s
//   `identifier` override (`dev.loomux.e2e`, vs the product's
//   `dev.loomux.app`). WebView2 keys its user-data folder (and thus its
//   *shared browser process*, see #394) off that identifier, so a
//   differently-identified build can never share a running production
//   instance's WebView2 process.
// Deliberately NOT also setting `WEBVIEW2_USER_DATA_FOLDER` to redirect the
// user-data folder further: it's confirmed to survive elevation
// (MicrosoftEdge/WebView2Feedback#5640: "Folder was created and populated by
// WebView2 in both [elevated and non-elevated] cases", unlike
// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`), which would make it a candidate
// for isolating concurrent E2E workers from each other in the future — but it
// would also replace the identifier-derived path this file verifies below,
// destroying the one observable signal that distinguishes an E2E build from
// a stale production one. Not used here for that reason (see doc/design/
// e2e-testing.md's roadmap on parallelization).
//
// None of the above is asserted and trusted blindly: `verifyIsolatedBuild`
// below checks, from the OS process tree (not from the app's own claims),
// that the WebView2 child process our spawn actually produced is really
// running under the E2E identifier before anything drives or tears it down —
// a stale/mismatched build at the default exe path is refused loudly instead
// of silently driven and hard-killed.
//
// Known residual risks (documented rather than fully closed):
// - (untested) WebView2 shares one browser process per user-data folder
//   (#394's own premise). If a previous spec's browser process hasn't
//   finished exiting before the next spec spawns, the new instance can join
//   that still-live process instead of spawning its own child, and
//   `verifyIsolatedBuild` would see no child and refuse. `waitForExit` below
//   (called from teardown) reduces the window by waiting for both the host
//   process and its WebView2 child to actually disappear before returning,
//   but does not eliminate it.
// - `verifyIsolatedBuild` verifies the APP is ours (the spawned pid's own
//   WebView2 child is rooted at the E2E identifier) — it does not
//   additionally cross-check that the CDP endpoint `connectWithRetry` then
//   connects to (`127.0.0.1:${CDP_PORT}`) belongs to that exact verified
//   process. A foreign listener on the same port would still be attached to.
//   In practice this fails safely rather than silently: `page.waitForSelector
//   ("#tab-bar", ...)` gates everything downstream, so a wrong endpoint reads
//   as a confusing selector timeout, not a wrong-app drive, and
//   `browser.close()` on a `connectOverCDP` connection disconnects rather
//   than terminates (never closes someone else's browser). A `/json/version`
//   identity cross-check before handing back the page would close this
//   properly; not implemented here.
import { test as base, chromium, type BrowserContext, type Page } from "@playwright/test";
import { type ChildProcess, execFile, spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

const execFileAsync = promisify(execFile);

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Workspace-root `target/` since #888 slice A1 — the repo is a Cargo
// workspace now, and cargo puts every member's output in the ROOT target dir,
// so this is no longer under src-tauri/. ci.yml's LOOMUX_E2E_EXE says the same
// thing for the CI run; test/workspacelayout.test.ts pins that the two agree,
// because a drift here fails as "exe not found" long after the edit that
// caused it.
const DEFAULT_EXE = path.resolve(__dirname, "../target/debug/orrerix.exe");
const EXE_FROM_ENV = process.env.LOOMUX_E2E_EXE;
const EXE = EXE_FROM_ENV ?? DEFAULT_EXE;

// Must match `identifier` in src-tauri/tauri.e2e.conf.json.
const EXPECTED_IDENTIFIER = "dev.loomux.e2e";

// A single fixed port, not one per worker: `workers: 1` (playwright.config.ts)
// means there's never real port contention to avoid, and on CI the port is
// additionally constrained by the HKLM policy value ci.yml sets (WebView2
// Runtime 150+ drops the env-var channel at High integrity level —
// MicrosoftEdge/WebView2Feedback#5640 — so the port has to be the SAME fixed
// value the policy names, not something computed per worker/retry). See
// doc/design/e2e-testing.md's roadmap on parallelization for what would
// actually need to change to make per-worker ports meaningful again.
const CDP_PORT = Number(process.env.LOOMUX_E2E_CDP_PORT ?? 9333);

/** `pid` and command line of a matching `msedgewebview2.exe`. */
interface Webview2Process {
  pid: number;
  cmdLine: string;
}

/** The WebView2 "browser process" our spawned `pid` produced (its direct
 *  child, per WebView2's process model), or `null` if none has appeared yet. */
async function webview2ChildOf(pid: number): Promise<Webview2Process | null> {
  try {
    const { stdout } = await execFileAsync("powershell", [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      `Get-CimInstance Win32_Process -Filter "ParentProcessId=${pid} AND Name='msedgewebview2.exe'" ` +
        `| Select-Object -First 1 ProcessId,CommandLine | ConvertTo-Json -Compress`,
    ]);
    const trimmed = stdout.trim();
    if (!trimmed) return null;
    const parsed = JSON.parse(trimmed) as { ProcessId: number; CommandLine: string };
    return { pid: parsed.ProcessId, cmdLine: parsed.CommandLine };
  } catch {
    return null;
  }
}

/** Any `msedgewebview2.exe` hosting `orrerix.exe` that is NOT a child of
 *  `excludePid` — i.e. a browser process some OTHER orrerix.exe instance
 *  already owns, which a same-identifier launch can join instead of
 *  spawning its own child (the #394 sharing mechanism, pointed at either a
 *  real production instance or a not-yet-exited previous E2E run). */
async function foreignLoomuxWebview2(excludePid: number): Promise<Webview2Process | null> {
  try {
    const { stdout } = await execFileAsync("powershell", [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      `Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" ` +
        `| Where-Object { $_.CommandLine -like '*--webview-exe-name=orrerix.exe*' -and $_.ParentProcessId -ne ${excludePid} } ` +
        `| Select-Object -First 1 ProcessId,CommandLine | ConvertTo-Json -Compress`,
    ]);
    const trimmed = stdout.trim();
    if (!trimmed) return null;
    const parsed = JSON.parse(trimmed) as { ProcessId: number; CommandLine: string };
    return { pid: parsed.ProcessId, cmdLine: parsed.CommandLine };
  } catch {
    return null;
  }
}

/** Confirms — from the OS process tree, not from anything the app itself
 *  claims — that `pid` really did launch a WebView2 browser process rooted at
 *  the E2E identifier's user-data folder before we do anything else with it.
 *  Returns that browser process's own pid on success (used by teardown to
 *  wait for it to actually exit — see `waitForExit`).
 *
 *  Throws (without touching the process) on any mismatch or timeout: refusing
 *  loudly beats silently driving, then hard-killing, whatever is actually at
 *  that path (a stale/production-identifier build shares a WebView2 browser
 *  process with a real running instance — see #394 — so a hard-kill of a
 *  misidentified process is the exact hazard this guards against). */
async function verifyIsolatedBuild(
  proc: ChildProcess,
  output: { stdout: string; stderr: string },
  timeoutMs = 20_000
): Promise<number> {
  const pid = proc.pid;
  if (pid === undefined) {
    throw new Error("spawn() returned no pid — the process never started");
  }
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (proc.exitCode !== null || proc.signalCode !== null) {
      throw new Error(
        `orrerix.exe exited early (code=${proc.exitCode} signal=${proc.signalCode}) before its ` +
          `WebView2 child could be verified.\n--- stdout ---\n${output.stdout}\n--- stderr ---\n${output.stderr}`
      );
    }
    const child = await webview2ChildOf(pid);
    if (child) {
      if (!child.cmdLine.includes(`\\${EXPECTED_IDENTIFIER}\\`)) {
        throw new Error(
          `refusing to drive or tear down pid ${pid}: its WebView2 child is not running under ` +
            `the expected E2E identifier ("${EXPECTED_IDENTIFIER}"). This exe was NOT built with the ` +
            `E2E config, or a stale build is sitting at ${EXE}. Rebuild with:\n` +
            `  npx tauri build --debug --no-bundle --config src-tauri/tauri.e2e.conf.json\n` +
            `Observed WebView2 command line:\n${child.cmdLine}`
        );
      }
      return child.pid;
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  // No child ever appeared under our own pid within the timeout. Distinguish
  // "still starting" from the two ways WebView2 could have joined a browser
  // process it doesn't own instead of spawning its own: a real production
  // instance (different identifier, #394's actual hazard) or a previous E2E
  // run's browser process that hasn't finished exiting yet.
  const foreign = await foreignLoomuxWebview2(pid);
  if (foreign) {
    const looksLikeProd = !foreign.cmdLine.includes(`\\${EXPECTED_IDENTIFIER}\\`);
    throw new Error(
      looksLikeProd
        ? `refusing to proceed: pid ${pid} never spawned its own WebView2 child, and a ` +
          `DIFFERENTLY-IDENTIFIED loomux instance's browser process is already running ` +
          `(pid ${foreign.pid}) — this looks like a production-identifier build (dev.loomux.app) ` +
          `that joined that instance's existing WebView2 browser process instead of spawning its ` +
          `own, per #394. Nothing was touched. Observed command line:\n${foreign.cmdLine}`
        : `refusing to proceed: pid ${pid} never spawned its own WebView2 child, and a previous ` +
          `E2E run's browser process (pid ${foreign.pid}) is still running — it likely hadn't ` +
          `finished exiting when this instance started and this one joined it instead of ` +
          `spawning its own. Nothing was touched; retrying should clear it. Observed command ` +
          `line:\n${foreign.cmdLine}`
    );
  }
  throw new Error(
    `no WebView2 child process appeared under pid ${pid} within ${timeoutMs}ms, and no other ` +
      `orrerix.exe WebView2 process was found either — cannot verify it's the E2E build. Refusing ` +
      `to proceed.\n--- stdout ---\n${output.stdout}\n--- stderr ---\n${output.stderr}`
  );
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
        `orrerix.exe exited early (code=${proc.exitCode} signal=${proc.signalCode}) before opening ` +
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

/** Best-effort: waits for `pid` to actually disappear (not just for `kill()`
 *  to have been called — that's fire-and-forget) so the next spec's spawn
 *  doesn't race a not-yet-exited browser process into joining it instead of
 *  spawning its own (see module doc). Never throws; a timeout here just means
 *  the next spec's own `verifyIsolatedBuild` might have to work harder. */
async function waitForExit(pid: number, timeoutMs = 5_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const { stdout } = await execFileAsync("powershell", [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        `[bool](Get-Process -Id ${pid} -ErrorAction SilentlyContinue)`,
      ]);
      if (stdout.trim().toLowerCase() !== "true") return;
    } catch {
      return;
    }
    await new Promise((r) => setTimeout(r, 200));
  }
}

/** `fs.rmSync` with retries: a WebView2 child process can hold a handle under
 *  `dir` for a moment after `proc.kill()` returns (kill is fire-and-forget,
 *  not "and it's gone"), which raises EPERM/EBUSY — `force` alone only
 *  swallows ENOENT. Never let a teardown-cleanup error mask the test's real
 *  failure. */
function rmSafely(dir: string): void {
  try {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  } catch (err) {
    console.error(`[e2e/fixtures] failed to remove temp dir ${dir}: ${String(err)}`);
  }
}

/** Per-spec launch customisation for the `appPage` fixture. Both hooks
 *  default to a no-op, so a spec that doesn't `test.use({ appLaunch })`
 *  gets exactly the launch the PoC specs have always had.
 *
 *  `seedDataDir` runs AFTER the fresh temp data dir is created and BEFORE
 *  the app is spawned — the only window in which a synthetic on-disk corpus
 *  (sessions, orchestration groups, audit logs) can exist before the app's
 *  boot listing path reads it. It may RETURN extra environment variables,
 *  which is what a corpus whose location depends on the data dir needs —
 *  `extraEnv` alone cannot express that, because a spec declares it before
 *  any dir exists.
 *
 *  `extraEnv` is for knobs that need no such knowledge. Both are merged in
 *  ABOVE the two isolation variables the harness owns, so neither can take
 *  those away. */
export interface AppLaunchOptions {
  seedDataDir?: (
    dataDir: string
  ) => void | Record<string, string> | Promise<void | Record<string, string>>;
  extraEnv?: Record<string, string>;
}

export const test = base.extend<{
  appPage: Page;
  appLaunch: AppLaunchOptions;
  appDataDir: string;
}>({
  appLaunch: [{}, { option: true }],

  /** The app-data root this test's instance runs against, exposed because a
   *  spec may have to READ it (an agent's generated MCP config, say — that
   *  file is the only place the ephemeral MCP port is discoverable) or WRITE
   *  into it. Its own fixture rather than a local inside `appPage` so the
   *  removal happens in fixture teardown order: `appPage` tears down first,
   *  killing the process, and only then is the directory removed. */
  appDataDir: async ({}, use) => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "loomux-e2e-data-"));
    await use(dir);
    rmSafely(dir);
  },

  appPage: async ({ appLaunch, appDataDir }, use) => {
    // Visible in the CI log regardless of outcome: if a future edit
    // re-parents or drops the `LOOMUX_E2E_EXE` env block (as happened once —
    // see git history on ci.yml), this line changes from "pinned" to
    // "default", which is the one thing a green run can't otherwise reveal.
    console.log(
      EXE_FROM_ENV
        ? `[e2e/fixtures] using pinned exe (LOOMUX_E2E_EXE): ${EXE}`
        : `[e2e/fixtures] using default exe path (LOOMUX_E2E_EXE not set): ${EXE}`
    );

    if (!fs.existsSync(EXE)) {
      throw new Error(
        `orrerix exe not found at ${EXE} — build it first with:\n` +
          `  npx tauri build --debug --no-bundle --config src-tauri/tauri.e2e.conf.json`
      );
    }

    const dataDir = appDataDir;

    // Before the spawn, never after: the boot listing path (#1592) reads
    // this tree once at startup, so a corpus written afterwards would be a
    // corpus the app under test never saw.
    const seededEnv = (await appLaunch.seedDataDir?.(dataDir)) ?? {};

    const proc: ChildProcess = spawn(EXE, [], {
      env: {
        ...process.env,
        // Spec-supplied knobs (`appLaunch.extraEnv`) sit here, ABOVE the
        // two isolation variables the harness owns, so a spec can add one
        // but can never override `ORRERIX_DATA_DIR` or the CDP port —
        // `verifyIsolatedBuild` would then be checking a process this
        // harness no longer controls.
        ...seededEnv,
        ...(appLaunch.extraEnv ?? {}),
        ORRERIX_DATA_DIR: dataDir,
        // Only takes effect at Medium integrity level (a normal dev machine):
        // WebView2 Runtime 150+ drops this env var at High IL as LPE hardening
        // (MicrosoftEdge/WebView2Feedback#5640) — the CI job sets an HKLM
        // policy with the same port instead (ci.yml), which survives
        // elevation. Harmless to also set this locally; --disable-gpu is a
        // no-op safety margin for a GPU-less CI box, unrelated to the High-IL
        // issue.
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${CDP_PORT} --disable-gpu`,
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

    // Verify BEFORE anything else touches this process — including before the
    // CDP connect attempt below, and definitely before the `finally` block's
    // teardown decides whether a kill is safe. A refusal here throws without
    // killing (see verifyIsolatedBuild's doc) — the process and its temp dir
    // are deliberately left in place, so the refusal says so explicitly
    // rather than leaving a silent, unexplained orphan.
    let webview2Pid: number;
    try {
      webview2Pid = await verifyIsolatedBuild(proc, output);
    } catch (err) {
      throw new Error(
        `${(err as Error).message}\n\n` +
          `pid ${proc.pid} was deliberately left running (not killed — a misidentified process ` +
          `may share a WebView2 browser process with a live instance, see #394) and its temp data ` +
          `dir was left in place: ${dataDir}. Close the process and remove the dir manually.`
      );
    }

    // Reaching here means verifyIsolatedBuild already confirmed this
    // process's WebView2 browser process is rooted at the E2E identifier's
    // own user-data folder, which nothing else shares (see module doc) — so
    // the unconditional `proc.kill()` in `finally` below is known-safe.
    try {
      const browser = await connectWithRetry(`http://127.0.0.1:${CDP_PORT}`, proc, output);
      try {
        // `Browser` has no `waitForEvent` (only `BrowserContext`/`Page` do —
        // #1428 review), so this waits on the 'context' event directly. That
        // loses `waitForEvent`'s default timeout, so give it the same 30s
        // budget the selector waits below use, named, rather than letting a
        // browser that never opens a context hang the fixture to Playwright's
        // whole-test timeout with no clue why (#1428 review N2).
        const context =
          browser.contexts()[0] ??
          (await new Promise<BrowserContext>((resolve, reject) => {
            const timer = setTimeout(
              () => reject(new Error("timed out after 30s waiting for the browser to open its first context")),
              30_000
            );
            browser.once("context", (ctx) => {
              clearTimeout(timer);
              resolve(ctx);
            });
          }));
        const page = context.pages()[0] ?? (await context.waitForEvent("page"));
        await page.waitForSelector("#tab-bar", { state: "attached", timeout: 30_000 });
        await page.waitForSelector("#workspace-stack .pane, #workspace-stack .welcome-form", {
          timeout: 30_000,
        });

        await use(page);
      } finally {
        // `browser` came from connectOverCDP, so close() disconnects rather
        // than terminates — always safe to call, success or failure.
        await browser.close().catch(() => {});
      }
    } finally {
      proc.kill();
      if (proc.pid !== undefined) await waitForExit(proc.pid);
      await waitForExit(webview2Pid);
      // The directory itself belongs to the `appDataDir` fixture, which is
      // torn down after this one — removing it here would race the process
      // this block has only just asked to exit.
    }
  },
});

export { expect } from "@playwright/test";
