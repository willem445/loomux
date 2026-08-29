// Liveness probes for the soak lane (#1603, plan #1600 §3 Phase 4.1).
//
// Everything in this file exists to answer one question the rest of this
// repo's guards structurally cannot (plan #1600 §2.2): **after the app has
// been running for a while under load, does a keystroke still reach a pane
// and does the MCP still answer?** Four hangs — #1564, #1592, #1595 and the
// beta6 field report — shipped past a growing wall of source scans, because a
// source scan pins the shape of the last incident and this is a liveness
// property.
//
// Three design choices here are load-bearing rather than incidental:
//
// - **Every probe is bounded by its own deadline, not by Playwright's test
//   timeout.** The failure under test is a HANG. A probe that simply awaited
//   would turn every red into "Test timeout of 120000ms exceeded" with no
//   statement of which half died, and would make the expected-failure marker
//   on the class assertion unreliable. Each probe returns `{ ok, ms }` and the
//   spec asserts on that, so a hang reads as a named assertion failure with a
//   measured elapsed time.
// - **The MCP probe goes over real HTTP from the Playwright process**, not
//   through the webview. The webview is the half of the app that stays alive
//   in the beta6 mechanism (its thread is untouched — that is why the window
//   still paints), so asking it whether the backend is well is asking the
//   wrong process. `tiny_http` + `std::thread::spawn` per request is what the
//   orchestrator actually talks to, so that is what gets probed.
// - **Nothing here spawns an agent CLI** (CLAUDE.md constraint 3). The MCP
//   token comes from `orch_solo_prepare`, which mints a token and writes the
//   config file *before* any pane process is spawned and independently of
//   whether the CLI is even installed — so calling that command alone yields
//   a working MCP identity with no child process anywhere.
import * as fs from "node:fs";
import * as path from "node:path";
import { type Locator, type Page } from "@playwright/test";

/** Reads a soak knob, accepting both brand spellings. Emit sites use
 *  `ORRERIX_`; a reader keeps every accepted spelling
 *  (doc/design/rebrand-protocol.md). */
function envInt(suffix: string, fallback: number): number {
  const raw = process.env[`ORRERIX_${suffix}`] ?? process.env[`LOOMUX_${suffix}`];
  if (raw === undefined || raw.trim() === "") return fallback;
  const n = Number(raw);
  if (!Number.isFinite(n) || n < 0) {
    throw new Error(`ORRERIX_${suffix}=${raw} is not a non-negative number`);
  }
  return Math.floor(n);
}

// ---------------------------------------------------------------------------
// Budget. Every number here is an env knob, because the CI default and a long
// local soak want different answers and the whole point of a soak lane is that
// somebody can turn it up. See doc/design/e2e-testing.md for the CI budget.
// ---------------------------------------------------------------------------

/** How long the app idles under poll load before anything is asserted.
 *  Default 180 s: `src/tabbar.ts`'s status poll is 4 s, so that is ~45 ticks
 *  per bound tab — many dozens of poll invokes in total, which is the scale
 *  the plan asks for, at a cost this job can afford on every PR. */
export const SOAK_MS = envInt("SOAK_MS", 180_000);

/** How long the injected registry-lock hold lasts. Default 30 s: long enough
 *  to span the two liveness probes several times over. Raising this past
 *  ~120 s is what a local run does to reach blocking-pool exhaustion, which is
 *  the second half of the beta6 mechanism (plan #1600 §1.2 step 4). */
export const LOCK_HOLD_MS = envInt("SOAK_LOCK_HOLD_MS", 30_000);

/** The bound every liveness probe must answer within. */
export const LIVENESS_BOUND_MS = envInt("SOAK_BOUND_MS", 8_000);

/** Corpus size. Defaults chosen so the fixture build stays under a couple of
 *  seconds while still being the "large corpus" shape #1592 was reported
 *  against. */
export const CORPUS_GROUPS = envInt("SOAK_GROUPS", 8);
export const CORPUS_SESSIONS = envInt("SOAK_SESSIONS", 900);
export const CORPUS_AUDIT_LINES = envInt("SOAK_AUDIT_LINES", 4_000);

// ---------------------------------------------------------------------------
// The lock-hold injector's file protocol. These three literals are the joint
// with src-tauri/src/orchestration/e2ehold.rs, and
// src-tauri/tests/e2ehold_guard.rs asserts this file still mentions all three
// — a rename on either side otherwise produces a soak run with no hold behind
// it, which is green and meaningless.
// ---------------------------------------------------------------------------

export const HOLD_ENV_VAR = "ORRERIX_E2E_LOCK_HOLD";
export const HOLD_REQUEST_FILE = "e2e-lock-hold.request";
export const HOLD_STATE_FILE = "e2e-lock-hold.state";

export type HoldTarget = "groups" | "agents" | "by_token";

export interface HoldState {
  target?: string;
  hold_ms?: number;
  requested_ms?: number;
  acquired_ms?: number | null;
  released_ms?: number | null;
  error?: string;
}

/** Asks the app under test to take a registry mutex and hold it. Returns as
 *  soon as the request is on disk — `waitForHoldAcquired` is what confirms it
 *  was honoured. */
export function requestLockHold(dataDir: string, target: HoldTarget, holdMs: number): void {
  fs.writeFileSync(
    path.join(dataDir, HOLD_REQUEST_FILE),
    JSON.stringify({ target, hold_ms: holdMs }),
    "utf8"
  );
}

/** Reads the injector's breadcrumb, tolerating "not written yet". The writer
 *  renames into place, so a torn read should not be possible — a malformed
 *  parse is still treated as "not yet" rather than thrown, because the only
 *  thing a throw here would achieve is turning a timing wobble into a red on
 *  a spec about something else. */
export function readHoldState(dataDir: string): HoldState | null {
  try {
    return JSON.parse(fs.readFileSync(path.join(dataDir, HOLD_STATE_FILE), "utf8")) as HoldState;
  } catch {
    return null;
  }
}

/** Blocks until the injector reports it has ACQUIRED the mutex.
 *
 *  This is the positive control for the whole class assertion. A hold that
 *  silently never happened is indistinguishable, from the assertion's side,
 *  from the app surviving one — so the spec must never run its bounded probes
 *  without first having seen `acquired_ms`. */
export async function waitForHoldAcquired(
  dataDir: string,
  timeoutMs = 15_000
): Promise<HoldState> {
  const deadline = Date.now() + timeoutMs;
  let last: HoldState | null = null;
  while (Date.now() < deadline) {
    last = readHoldState(dataDir);
    if (last?.error) {
      throw new Error(`the lock-hold injector refused the request: ${last.error}`);
    }
    if (typeof last?.acquired_ms === "number" && last.acquired_ms > 0) return last;
    await sleep(100);
  }
  throw new Error(
    `the lock-hold injector never reported acquiring a lock within ${timeoutMs}ms ` +
      `(last state: ${JSON.stringify(last)}). Either the app was not launched with ` +
      `${HOLD_ENV_VAR}=1, or it is a release build, where the injector does not exist.`
  );
}

/** Whether the hold is still in effect. Read AFTER the liveness probes: a
 *  hold that expired before they ran would leave them measuring an idle app. */
export function holdStillHeld(dataDir: string): boolean {
  const s = readHoldState(dataDir);
  return typeof s?.acquired_ms === "number" && (s.released_ms ?? null) === null;
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// ---------------------------------------------------------------------------
// Invoke accounting — the positive control for the soak itself.
// ---------------------------------------------------------------------------

/** Counts every command the frontend dispatches, by name.
 *
 *  Without this the soak is an assertion about a load nobody measured: if the
 *  tab-strip poll were disarmed, or the corpus's tabs failed to bind, the app
 *  would idle for three minutes doing nothing and the liveness probes would
 *  pass — a green run that proves the app survives being left alone.
 *
 *  `@tauri-apps/api`'s `invoke` reads `window.__TAURI_INTERNALS__.invoke` at
 *  call time (node_modules/@tauri-apps/api/core.js), so wrapping the property
 *  intercepts every dispatch the real frontend makes, through
 *  `src/transport.ts` and every bridge above it. */
export async function installInvokeCounter(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__?: {
        invoke(cmd: string, args?: unknown, options?: unknown): Promise<unknown>;
        __soakOriginalInvoke?: (cmd: string, args?: unknown, options?: unknown) => Promise<unknown>;
      };
      __soakInvokeCounts?: Record<string, number>;
    };
    const internals = w.__TAURI_INTERNALS__;
    if (!internals) throw new Error("__TAURI_INTERNALS__ missing — not running inside the app");
    if (internals.__soakOriginalInvoke) return;
    internals.__soakOriginalInvoke = internals.invoke.bind(internals);
    w.__soakInvokeCounts = {};
    internals.invoke = (cmd: string, args?: unknown, options?: unknown) => {
      const counts = w.__soakInvokeCounts as Record<string, number>;
      counts[cmd] = (counts[cmd] ?? 0) + 1;
      return internals.__soakOriginalInvoke!(cmd, args, options);
    };
  });
}

export async function readInvokeCounts(page: Page): Promise<Record<string, number>> {
  return await page.evaluate(
    () => ({ ...((window as unknown as { __soakInvokeCounts?: Record<string, number> }).__soakInvokeCounts ?? {}) })
  );
}

/** Total dispatches whose command name starts with `orch_` — the poll load,
 *  independent of which particular commands a given release polls (the
 *  group-view batch has been five, then nine, then ten; pinning a count here
 *  would pin the last incident's shape, which is the mistake this whole lane
 *  exists to stop making). */
export function orchInvokeTotal(counts: Record<string, number>): number {
  return Object.entries(counts)
    .filter(([cmd]) => cmd.startsWith("orch_"))
    .reduce((sum, [, n]) => sum + n, 0);
}

// ---------------------------------------------------------------------------
// Probe A — a keystroke reaches a pane, and the child's output comes back.
// ---------------------------------------------------------------------------

/** Taps the backend's `pty-output` event stream from the page.
 *
 *  xterm.js renders through the WebGL addon, so a terminal's contents are NOT
 *  in the DOM and cannot be read with a selector. Rather than add a debug hook
 *  to product code to expose the buffer, this listens to the same Tauri event
 *  the app's own router consumes (`src/pty.ts`'s `listen<{id,data}>
 *  ("pty-output")`), through the low-level bridge the bundled `listen()` uses
 *  underneath — the same technique `queue-badge.spec.ts` already uses in the
 *  emit direction, and permitted by the shipped ACL (`core:default` covers
 *  `core:event`). */
export async function installPtyTap(page: Page): Promise<void> {
  await page.evaluate(async () => {
    const w = window as unknown as {
      __TAURI_INTERNALS__?: {
        invoke(cmd: string, args?: unknown): Promise<unknown>;
        transformCallback(cb: (e: unknown) => void, once?: boolean): number;
      };
      __soakPtyText?: string;
    };
    const internals = w.__TAURI_INTERNALS__;
    if (!internals) throw new Error("__TAURI_INTERNALS__ missing — not running inside the app");
    if (typeof w.__soakPtyText === "string") return;
    w.__soakPtyText = "";
    const handler = internals.transformCallback((raw: unknown) => {
      const payload = (raw as { payload?: { data?: unknown } } | null)?.payload;
      if (payload && typeof payload.data === "string") {
        // Bounded: a soak can run for an hour locally, and an unbounded string
        // in the page would be this harness's own INV-8 violation.
        const next = (w.__soakPtyText ?? "") + payload.data;
        w.__soakPtyText = next.length > 65_536 ? next.slice(next.length - 65_536) : next;
      }
    });
    await internals.invoke("plugin:event|listen", {
      event: "pty-output",
      target: { kind: "Any" },
      handler,
    });
  });
}

export async function ptyTapText(page: Page): Promise<string> {
  return await page.evaluate(
    () => (window as unknown as { __soakPtyText?: string }).__soakPtyText ?? ""
  );
}

/** ANSI/control-sequence stripped view of the tap, so a shell's cursor
 *  positioning and colour codes do not sit between the characters being
 *  matched. Three families, because a Windows console emits all three: OSC
 *  (title sets, terminated by BEL or ST), CSI (the `ESC [` majority), and
 *  the bare two-character escapes. */
export function stripAnsi(text: string): string {
  return text
    .replace(/\u001b\][\s\S]*?(?:\u0007|\u001b\\)/g, "")
    .replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, "")
    .replace(/\u001b[@-Z\\-_]/g, "");
}

export interface ProbeResult {
  ok: boolean;
  ms: number;
  detail: string;
}

/**
 * Types a command into the focused pane and waits for the CHILD's own output
 * to come back — bounded by `boundMs`, never by the test timeout.
 *
 * The command is `echo soak<n>_%RANDOM%_end` and the match is
 * `/soak<n>_\d+_end/`. That asymmetry is deliberate and is what makes this a
 * round trip rather than a terminal echo: the text typed in contains the
 * literal `%RANDOM%`, so a match on the digit form can only have been produced
 * by `cmd.exe` expanding it. A pane that rendered the keystrokes locally but
 * never reached — or never heard back from — its child cannot produce it.
 *
 * This is the assertion the beta6 report is about: `src/ptywrite.ts` keeps one
 * `write_pty` in flight per pane and chains the next on the previous promise
 * (#65), so a `write_pty` that never resolves stops that pane accepting input
 * forever, with the window still painting.
 */
export async function ptyRoundTrip(
  page: Page,
  paneTerm: Locator,
  nonce: number,
  boundMs: number
): Promise<ProbeResult> {
  const marker = `soak${nonce}`;
  const expected = new RegExp(`${marker}_\\d+_end`);
  const started = Date.now();

  await paneTerm.click();
  await page.keyboard.type(`echo ${marker}_%RANDOM%_end`);
  await page.keyboard.press("Enter");

  while (Date.now() - started < boundMs) {
    const text = stripAnsi(await ptyTapText(page));
    if (expected.test(text)) {
      return { ok: true, ms: Date.now() - started, detail: `matched ${expected}` };
    }
    await sleep(150);
  }
  const tail = stripAnsi(await ptyTapText(page)).slice(-400);
  return {
    ok: false,
    ms: Date.now() - started,
    detail: `no output matching ${expected} within ${boundMs}ms; last 400 chars of the pty tap: ${JSON.stringify(tail)}`,
  };
}

// ---------------------------------------------------------------------------
// Probe B — the MCP still answers.
// ---------------------------------------------------------------------------

export interface McpEndpoint {
  url: string;
  header: string;
  token: string;
  agentId: string;
}

/**
 * Mints a real MCP identity without spawning anything.
 *
 * `orch_solo_prepare` mints a token, registers an `AgentEntry`, and writes the
 * agent's MCP config file — all before the launcher would spawn the pane's
 * process, and independently of whether that CLI is installed. Calling it
 * directly therefore yields a valid token and the server's ephemeral port
 * (which is bound at `127.0.0.1:0` in `mcp::serve` and is not otherwise
 * discoverable) with no child process anywhere. CLAUDE.md constraint 3 is
 * about spawning agent CLIs; this spawns nothing.
 *
 * The config's exact JSON layout differs per CLI, so the url/header pair is
 * located by SHAPE — an object carrying a `/mcp` url and a headers map —
 * rather than by a hard-coded key path that would break silently on the next
 * CLI added.
 */
export async function mintMcpEndpoint(
  page: Page,
  dataDir: string,
  cwd: string
): Promise<McpEndpoint> {
  const prepared = (await page.evaluate(async (paneCwd: string) => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__?: { invoke(cmd: string, args: unknown): Promise<unknown> };
      }
    ).__TAURI_INTERNALS__;
    if (!internals) throw new Error("__TAURI_INTERNALS__ missing — not running inside the app");
    return (await internals.invoke("orch_solo_prepare", {
      cli: "copilot",
      cwd: paneCwd,
      name: "soak-probe",
    })) as { agent_id?: string; delivery_only?: boolean };
  }, cwd)) as { agent_id?: string; delivery_only?: boolean };

  const agentId = prepared?.agent_id;
  if (!agentId) {
    throw new Error(`orch_solo_prepare returned no agent_id: ${JSON.stringify(prepared)}`);
  }
  if (prepared.delivery_only) {
    throw new Error(
      `orch_solo_prepare reported delivery_only for this CLI — no token was minted, so there ` +
        `is no MCP identity to probe with`
    );
  }

  const cfgPath = path.join(dataDir, "orchestration", "__solo__", "configs", `${agentId}.json`);
  const cfg = JSON.parse(fs.readFileSync(cfgPath, "utf8")) as unknown;
  const found = findServerEntry(cfg);
  if (!found) {
    throw new Error(`no MCP server entry with a url and headers found in ${cfgPath}`);
  }
  return { ...found, agentId };
}

function findServerEntry(node: unknown): { url: string; header: string; token: string } | null {
  if (node === null || typeof node !== "object") return null;
  const obj = node as Record<string, unknown>;
  const url = obj.url;
  const headers = obj.headers;
  if (typeof url === "string" && url.includes("/mcp") && headers && typeof headers === "object") {
    for (const [header, token] of Object.entries(headers as Record<string, unknown>)) {
      if (typeof token === "string" && header.toLowerCase().endsWith("-agent")) {
        return { url, header, token };
      }
    }
  }
  for (const child of Object.values(obj)) {
    const hit = findServerEntry(child);
    if (hit) return hit;
  }
  return null;
}

export interface McpResult extends ProbeResult {
  status?: number;
  body?: unknown;
}

/**
 * One JSON-RPC call against the running app's MCP server, bounded.
 *
 * `ping` is the cheapest method and still the faithful probe: every method,
 * `ping` included, is authenticated first, and `resolve_token` locks
 * `by_token`, then `agents`, then `groups`. So a `ping` that answers is
 * evidence the registry is reachable, and a `ping` that does not is the exact
 * first symptom of the beta6 mechanism — "MCP dies first" (plan #1600 §1.2
 * step 2).
 */
export async function mcpCall(
  ep: McpEndpoint,
  method: string,
  params: unknown,
  boundMs: number,
  tokenOverride?: string
): Promise<McpResult> {
  const started = Date.now();
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), boundMs);
  try {
    const res = await fetch(ep.url, {
      method: "POST",
      headers: { "content-type": "application/json", [ep.header]: tokenOverride ?? ep.token },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
      signal: controller.signal,
    });
    const body = (await res.json()) as unknown;
    return {
      ok: true,
      ms: Date.now() - started,
      status: res.status,
      body,
      detail: `${method} → HTTP ${res.status}`,
    };
  } catch (err) {
    return {
      ok: false,
      ms: Date.now() - started,
      detail: `${method} did not answer within ${boundMs}ms: ${String(err)}`,
    };
  } finally {
    clearTimeout(timer);
  }
}

/** Whether an `McpResult` is a JSON-RPC SUCCESS (not an error envelope — the
 *  server returns those with HTTP 200 too, so status alone decides nothing). */
export function isJsonRpcResult(r: McpResult): boolean {
  const b = r.body as { jsonrpc?: unknown; result?: unknown } | undefined;
  return r.ok && b?.jsonrpc === "2.0" && b.result !== undefined;
}

/** Whether an `McpResult` is a JSON-RPC ERROR envelope. Used for the negative
 *  control: a bogus token must be REFUSED, which is what distinguishes "we are
 *  talking to orrerix's authenticated MCP server" from "something answered on
 *  that port". */
export function jsonRpcErrorCode(r: McpResult): number | null {
  const b = r.body as { error?: { code?: unknown } } | undefined;
  return typeof b?.error?.code === "number" ? b.error.code : null;
}
