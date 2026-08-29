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

/** How long the injected registry-lock hold lasts. It has to comfortably
 *  outlast both probes: a hold that expires mid-probe leaves the rest of the
 *  measurement taken against an idle app. At the defaults the probes need
 *  ~70 s worst case, so 90 s. Raising this past ~120 s is what a local run
 *  does to reach blocking-pool exhaustion, the second half of the beta6
 *  mechanism (plan #1600 §1.2 step 4). */
export const LOCK_HOLD_MS = envInt("SOAK_LOCK_HOLD_MS", 90_000);

/** The bound each PHASE of a liveness probe must answer within.
 *
 *  20 s, and generous on purpose. The input phase types with real key events,
 *  and `src/ptywrite.ts` keeps exactly one `write_pty` in flight per pane
 *  (#65), so every character is a full IPC round trip. How long that takes
 *  has been observed to VARY by orders of magnitude between CI runs of this
 *  same spec: once only 19 of 23 characters echoed within 8 s, and on the next
 *  run the whole input phase finished in 38 ms. The cause is not established
 *  and is deliberately not guessed at here — guessing is what plan #1600 §4
 *  says produced beta5 and beta6. What follows from it is this bound and the
 *  two-phase split: the failure this lane is about is a probe that NEVER
 *  answers, not a slow one, so the bound clears a slow machine by a wide
 *  margin rather than policing latency, which would be a flake generator
 *  wearing an invariant's clothes. If the slow case recurs it is worth
 *  chasing under #1600 in its own right. */
export const LIVENESS_BOUND_MS = envInt("SOAK_BOUND_MS", 20_000);

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

/** Arms the app's OWN dispatch counter (`src/transport.ts`'s `__invokeStats`)
 *  and returns how it was reached, for the CI log.
 *
 *  The counter lives in the app rather than here because Tauri's
 *  `window.__TAURI_INTERNALS__.invoke` is a frozen own property
 *  (`writable:false, configurable:false`, measured on the E2E build — CI run
 *  33230049810) that assignment, `defineProperty` and even a `Proxy` cannot
 *  intercept; a Proxy `get` trap is forbidden by the language from returning
 *  anything but the real value for such a property. `src/transport.ts` is the
 *  ONE module allowed to touch Tauri IPC (CLAUDE.md constraint 5), so a
 *  counter there sees every dispatch by construction and does not depend on
 *  the shape of Tauri's internals at all. It ships disarmed. */
export async function installInvokeCounter(page: Page): Promise<string> {
  return await page.evaluate(() => {
    const stats = (window as unknown as { __invokeStats?: (arm?: "arm") => unknown })
      .__invokeStats;
    if (typeof stats !== "function") {
      throw new Error(
        "window.__invokeStats is missing — src/transport.ts's dispatch counter is not in " +
          "this build, so nothing here can say whether the poll paths ran"
      );
    }
    stats("arm");
    return "transport seam (__invokeStats)";
  });
}

export async function readInvokeCounts(page: Page): Promise<Record<string, number>> {
  const stats = await page.evaluate(() => {
    const f = (window as unknown as {
      __invokeStats?: (arm?: "arm") => { armed: boolean; counts: Record<string, number> };
    }).__invokeStats;
    return f ? f() : null;
  });
  if (!stats) throw new Error("window.__invokeStats disappeared mid-run");
  if (!stats.armed) {
    throw new Error(
      "the app's dispatch counter reports itself DISARMED — a disarmed counter " +
        "returns an empty map, which is indistinguishable from an app that never " +
        "dispatched anything"
    );
  }
  return stats.counts;
}
/**
 * Proves the counter is on the app's OWN dispatch path, using invokes the app
 * has already been made to perform.
 *
 * This is not belt-and-braces. The counter's whole job is to make a later
 * reading of "the poll paths ran" trustworthy, and its failure mode is silent:
 * a counter that is not counting reports `{}`, which is bit-for-bit what an app
 * that never polled reports. That is not hypothetical — it is what the third CI
 * run on this branch did, reporting zero dispatches over a three-minute soak
 * whose own baseline probe had just driven `write_pty` successfully. So: call
 * this once something is KNOWN to have been invoked, and let a blind instrument
 * fail here, loudly, instead of two hundred seconds later wearing a finding's
 * clothes.
 */
export async function assertCounterSeesTheApp(
  page: Page,
  command: string
): Promise<Record<string, number>> {
  const counts = await readInvokeCounts(page);
  if ((counts[command] ?? 0) < 1) {
    throw new Error(
      `the invoke counter recorded no \`${command}\` even though the app has just ` +
        `completed an operation that requires it. The counter is not on the app's ` +
        `dispatch path, so any count it reports later — a zero most of all — is a ` +
        `fact about this instrument and not about the app. Counts seen: ` +
        `${JSON.stringify(counts)}`
    );
  }
  return counts;
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
      if (!payload || typeof payload.data !== "string") return;
      // The payload is BASE64 of the raw pty bytes, not text — `src/pty.ts`'s
      // own router `atob`s it before handing it to a pane. A tap that skipped
      // this matches nothing and reports it as "no output", which is exactly
      // how a working pty round trip reads as a dead one (measured: the first
      // CI run's failure dump was legible base64).
      let text: string;
      try {
        text = atob(payload.data);
      } catch {
        return;
      }
      // Bounded: a soak can run for an hour locally, and an unbounded string
      // in the page would be this harness's own INV-8 violation.
      const next = (w.__soakPtyText ?? "") + text;
      w.__soakPtyText = next.length > 65_536 ? next.slice(next.length - 65_536) : next;
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

/** Waits for `pred` to hold over the ANSI-stripped pty tap, bounded. */
async function waitForTap(
  page: Page,
  pred: (text: string) => boolean,
  boundMs: number
): Promise<{ ok: boolean; ms: number; text: string }> {
  const started = Date.now();
  for (;;) {
    const text = stripAnsi(await ptyTapText(page));
    if (pred(text)) return { ok: true, ms: Date.now() - started, text };
    if (Date.now() - started >= boundMs) return { ok: false, ms: Date.now() - started, text };
    await sleep(150);
  }
}

/**
 * Types a command into the focused pane and waits for the CHILD's own output
 * to come back. Two separately-bounded phases, never the test timeout.
 *
 * **Why two phases.** They are two different properties, they fail for
 * different reasons, and they cost wildly different amounts of time:
 *
 * - *Input.* The command is typed with real key events, and `src/ptywrite.ts`
 *   keeps exactly one `write_pty` in flight per pane, chaining the next on the
 *   previous promise (#65) — so every character is a full IPC round trip, and
 *   this phase is the one the beta6 report is actually about: a `write_pty`
 *   that never resolves stops that pane accepting input forever, with the
 *   window still painting. It ends when the typed stem echoes back.
 * - *Answer.* Enter is pressed and the CHILD's own output has to arrive. This
 *   is one write and one read, so it is fast whenever it works at all.
 *
 * Collapsing them into one clock is what the first CI run on this branch did,
 * and the result was a probe that timed out mid-typing and reported it as "no
 * output" — a slow input path indistinguishable from a dead child.
 *
 * **Why the marker is shaped the way it is.** The command is
 * `echo <marker>%RANDOM%` and the match is `/<marker>\d+/`. The text typed in
 * contains the literal `%RANDOM%`, whose next character is `%` and not a
 * digit, so the echo of the typed line cannot match — only `cmd.exe` expanding
 * it can. A pane that rendered the keystrokes locally but never reached, or
 * never heard back from, its child cannot produce a match. The marker is kept
 * SHORT for the reason above: every character costs a round trip.
 */
export async function ptyRoundTrip(
  page: Page,
  paneTerm: Locator,
  nonce: number,
  boundMs: number
): Promise<ProbeResult> {
  const marker = `zq${nonce}`;
  const expanded = new RegExp(`${marker}\\d+`);
  const started = Date.now();

  await paneTerm.click();
  await page.keyboard.type(`echo ${marker}%RANDOM%`);

  const input = await waitForTap(page, (t) => t.includes(marker), boundMs);
  if (!input.ok) {
    return {
      ok: false,
      ms: Date.now() - started,
      detail:
        `INPUT phase: the typed marker "${marker}" never echoed back within ${boundMs}ms, ` +
        `so the keystrokes did not reach the pane — this is the beta6 symptom itself ` +
        `(write_pty stops resolving and src/ptywrite.ts's per-pane chain stops ` +
        `dispatching). Last 400 chars: ${JSON.stringify(input.text.slice(-400))}`,
    };
  }

  await page.keyboard.press("Enter");
  const answer = await waitForTap(page, (t) => expanded.test(t), boundMs);
  if (!answer.ok) {
    return {
      ok: false,
      ms: Date.now() - started,
      detail:
        `ANSWER phase: the keystrokes DID reach the pane (input echoed in ${input.ms}ms), ` +
        `but no output matching ${expanded} came back within ${boundMs}ms — the child ` +
        `never answered. Last 400 chars: ${JSON.stringify(answer.text.slice(-400))}`,
    };
  }

  return {
    ok: true,
    ms: Date.now() - started,
    detail: `input echoed in ${input.ms}ms, child answered ${expanded} in ${answer.ms}ms`,
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
