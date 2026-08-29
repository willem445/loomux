// THE soak/liveness lane (#1603, plan #1600 §3 Phase 4.1).
//
// Every other spec in this directory is a SHAPE test: it opens something,
// measures the DOM, and asserts a structure. This one is not. It asserts a
// LIVENESS property — after the app has been running for a while against a
// large corpus with its poll paths ticking, does a keystroke still reach a
// pane, and does the MCP still answer? — because that is the property four
// consecutive hangs (#1564, #1592, #1595, and the beta6 field report) broke
// while every shape guard in the repo stayed green. Plan #1600 §2.2 is the
// argument; this file is the test that argument asks for.
//
// ## What this spec can and cannot exercise
//
// Stated up front, because a liveness lane that quietly covers less than it
// reads as covering is worse than none:
//
// - **The 4 s tab-strip poll: yes.** `src/tabbar.ts` arms it at construction
//   ("the app's one app-lifetime poll") and `pollStatus` issues
//   `orch_group_summary` + `orch_group_usage` for every GROUP-BOUND tab. A tab
//   is group-bound purely because its persisted `groupIds` names a group, so
//   the corpus's `tabs.json` alone puts that poll under load — no clicking,
//   and no agent CLI.
// - **The 2 s group-view poll: no, and it cannot be.** `src/groupview.ts`'s
//   timer is armed only while the view is shown, and the view can only be
//   opened from a pane whose `groupBtn` is visible — which
//   `applyOrchIdentity` reveals only for a LIVE orchestrator-role pane. That
//   needs a real agent CLI running, which CLAUDE.md constraint 3 forbids for
//   automated E2E. So the poll load here is the tab-strip half; both halves
//   park threads on the same registry mutexes, so the mechanism is exercised,
//   but the per-tick fan-out is smaller than a real orchestrating session's.
//   Closing that gap needs a safe stand-in agent process
//   (doc/design/e2e-testing.md's standing limitation), not a change here.
// - **Blocking-pool exhaustion: only on a long local run.** Plan #1600 §1.2
//   step 4 puts saturation of tokio's 512-thread blocking pool minutes into a
//   hold. The CI defaults (a 30 s hold) reach roughly a hundred parked
//   threads, so what CI catches is the FIRST symptom in the chain — "MCP dies
//   first" — not the last one. Raising `ORRERIX_SOAK_LOCK_HOLD_MS` past ~120 s
//   locally is what reaches the pane-input half.
//
// ## The expected failure
//
// The class assertion below is marked `test.fail()`. It is expected to FAIL on
// today's `main` — that is the whole point: under the beta6 mechanism a long
// registry hold takes the MCP down immediately, and Phases 1 and 2 of plan
// #1600 are what make it pass. `test.fail()` was chosen over `test.skip()`
// deliberately: the assertion runs at full strength, the E2E job stays green
// while the fix is outstanding, and the moment the fix lands Playwright
// reports "expected to fail but passed" — so the person who lands it is told
// to flip the marker, rather than the lane quietly never being re-armed.
import * as path from "node:path";
import { fileURLToPath } from "node:url";

import { test, expect } from "../fixtures";
import { createTerminalPane, paneByName } from "../helpers";
import { buildCorpus, type CorpusReport } from "../corpus";
import {
  CORPUS_AUDIT_LINES,
  CORPUS_GROUPS,
  CORPUS_SESSIONS,
  HOLD_ENV_VAR,
  LIVENESS_BOUND_MS,
  LOCK_HOLD_MS,
  SOAK_MS,
  holdStillHeld,
  installInvokeCounter,
  installPtyTap,
  isJsonRpcResult,
  jsonRpcErrorCode,
  mcpCall,
  mintMcpEndpoint,
  orchInvokeTotal,
  ptyRoundTrip,
  readInvokeCounts,
  requestLockHold,
  sleep,
  waitForHoldAcquired,
} from "../liveness";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
/** A real directory to name as each synthetic group's repo, and as the pane's
 *  cwd — this checkout itself. Nothing writes to it. */
const REPO_ROOT = path.resolve(__dirname, "../..");

/** Tabs bound to a group. Each one costs two `orch_*` invokes every 4 s, so
 *  this is the poll load's multiplier. */
const BOUND_TABS = Math.min(4, CORPUS_GROUPS);

/** How long to let the app settle after launch before measuring anything. */
const WARMUP_MS = 10_000;

function corpusSpec() {
  return {
    groups: CORPUS_GROUPS,
    auditLinesPerGroup: CORPUS_AUDIT_LINES,
    sessions: CORPUS_SESSIONS,
    groupBoundTabs: BOUND_TABS,
    repo: REPO_ROOT,
  };
}

/** Both describes seed the same corpus. Captured per launch so the assertions
 *  can check what was actually written rather than what was asked for — a
 *  corpus that silently failed to generate would otherwise turn this into a
 *  soak against an empty install, which passes. */
let seeded: CorpusReport | null = null;

const seedCorpus = (dataDir: string) => {
  seeded = buildCorpus(dataDir, corpusSpec());
  return seeded.env;
};

function assertCorpusReallyLanded(): CorpusReport {
  expect(seeded, "the corpus builder never ran — the fixture's seed hook was not called").not.toBeNull();
  const c = seeded as CorpusReport;
  expect(c.groupIds.length, "orchestration groups written").toBe(CORPUS_GROUPS);
  expect(c.sessionIds.length, "synthetic copilot sessions written").toBe(CORPUS_SESSIONS);
  // A floor, not a pin: the exact byte count is an implementation detail of
  // the line format, but "the audit logs are large" is the property #1592 was
  // about, and a builder that wrote empty ones would sail past a count check.
  expect(c.auditBytes, "total audit-log bytes written").toBeGreaterThan(1_000_000);
  return c;
}

// ---------------------------------------------------------------------------

test.describe("soak: a large corpus and a steady poll load", () => {
  test.use({ appLaunch: { seedDataDir: seedCorpus } });

  test("a keystroke still reaches a pane and the MCP still answers after a long idle soak", async ({
    appPage: page,
    appDataDir,
  }) => {
    // The soak plus both probe budgets plus the fixture's own launch and
    // teardown windows. Generous on purpose: a timeout here would report as
    // "test timeout" rather than as the named assertion that actually failed.
    test.setTimeout(SOAK_MS + 300_000);

    const corpus = assertCorpusReallyLanded();
    console.log(
      `[soak] corpus: ${corpus.groupIds.length} groups, ${corpus.sessionIds.length} sessions, ` +
        `${(corpus.auditBytes / 1_048_576).toFixed(1)} MiB of audit logs; ` +
        `${BOUND_TABS} group-bound tabs; soak ${SOAK_MS}ms; bound ${LIVENESS_BOUND_MS}ms`
    );

    await installInvokeCounter(page);
    await installPtyTap(page);

    // One plain shell pane to type into. `cmd` rather than the form's default
    // PowerShell: the round trip reads the child's OUTPUT, and PSReadLine
    // redraws the input line as you type (see `createTerminalPane`'s doc).
    await createTerminalPane(page, { name: "soak-pane", repo: REPO_ROOT, shell: "cmd" });
    const paneTerm = paneByName(page, "soak-pane").locator(".pane-term");
    await expect(paneTerm).toBeVisible();

    // Baseline BEFORE the soak. If the probe machinery itself is broken, this
    // says so in ten seconds rather than after three minutes of idling — and
    // it is also the control that makes the post-soak result mean something:
    // a probe that never worked failing later would prove nothing about the
    // soak.
    await sleep(WARMUP_MS);
    const baselinePty = await ptyRoundTrip(page, paneTerm, 1, LIVENESS_BOUND_MS);
    expect(baselinePty.ok, `pty round trip at t=0: ${baselinePty.detail}`).toBe(true);

    const baselineMcp = await mcpCall(
      await mintMcpEndpoint(page, appDataDir, REPO_ROOT),
      "ping",
      {},
      LIVENESS_BOUND_MS
    );
    expect(isJsonRpcResult(baselineMcp), `MCP ping at t=0: ${baselineMcp.detail}`).toBe(true);

    const countsBefore = orchInvokeTotal(await readInvokeCounts(page));

    // ---- the soak itself: the app is left completely alone ----
    await sleep(SOAK_MS);

    // The soak's own positive control. Without it this test asserts that an
    // app survives being ignored: if the tab-strip poll were disarmed, or the
    // corpus's tabs failed to bind, the app would idle doing nothing at all
    // and both probes below would pass.
    const counts = await readInvokeCounts(page);
    const polled = orchInvokeTotal(counts) - countsBefore;
    const ticks = Math.floor(SOAK_MS / 4_000);
    // 0.4 of the nominal rate: the poll gate suppresses while the window is
    // hidden and a tick can be skipped, so this is a floor on "the poll paths
    // really ran, many dozens of times", not a pin on the cadence.
    const floor = Math.max(2, Math.floor(ticks * BOUND_TABS * 2 * 0.4));
    const gate = await page.evaluate(
      () => (window as unknown as { __pollGateStats?: () => unknown }).__pollGateStats?.() ?? null
    );
    expect(
      polled,
      `only ${polled} orch_* invokes during a ${SOAK_MS}ms soak with ${BOUND_TABS} group-bound ` +
        `tabs (expected at least ${floor}). The poll paths this lane exists to load were not ` +
        `running, so the liveness result below would be about an idle app. ` +
        `Per-command counts: ${JSON.stringify(counts)}; poll gate: ${JSON.stringify(gate)}`
    ).toBeGreaterThanOrEqual(floor);

    // ---- (4) the two liveness assertions ----
    const pty = await ptyRoundTrip(page, paneTerm, 2, LIVENESS_BOUND_MS);
    expect(
      pty.ok,
      `after ${SOAK_MS}ms of soak, a keystroke did not round-trip through the pane's child ` +
        `within ${LIVENESS_BOUND_MS}ms. ${pty.detail}`
    ).toBe(true);

    const endpoint = await mintMcpEndpoint(page, appDataDir, REPO_ROOT);
    const ping = await mcpCall(endpoint, "ping", {}, LIVENESS_BOUND_MS);
    expect(
      isJsonRpcResult(ping),
      `after ${SOAK_MS}ms of soak, the MCP did not answer a ping within ` +
        `${LIVENESS_BOUND_MS}ms. ${ping.detail} body=${JSON.stringify(ping.body)}`
    ).toBe(true);
    expect(ping.ms, "MCP ping latency").toBeLessThan(LIVENESS_BOUND_MS);

    // Negative control for the probe, not for the app: a bogus token must be
    // REFUSED. Without it, "something answered on 127.0.0.1" would read as
    // "orrerix's authenticated MCP server answered" — and a probe that would
    // pass against any listener is not evidence about this one.
    const refused = await mcpCall(endpoint, "ping", {}, LIVENESS_BOUND_MS, "not-a-real-token");
    expect(
      jsonRpcErrorCode(refused),
      `an unauthenticated ping was not refused: ${JSON.stringify(refused.body)}`
    ).toBe(-32000);
  });
});

// ---------------------------------------------------------------------------

test.describe("soak: THE class assertion — a long registry-lock hold", () => {
  test.use({
    appLaunch: { seedDataDir: seedCorpus, extraEnv: { [HOLD_ENV_VAR]: "1" } },
  });

  // Expected to fail on today's main. See this file's header for why the
  // marker is `fail` rather than `skip`, and what is supposed to happen to it
  // when plan #1600's Phases 1 and 2 land.
  test.fail();

  test("the app still accepts pane input and the MCP still answers while a registry lock is held", async ({
    appPage: page,
    appDataDir,
  }) => {
    test.setTimeout(LOCK_HOLD_MS + 300_000);
    assertCorpusReallyLanded();

    await installInvokeCounter(page);
    await installPtyTap(page);
    await createTerminalPane(page, { name: "hold-pane", repo: REPO_ROOT, shell: "cmd" });
    const paneTerm = paneByName(page, "hold-pane").locator(".pane-term");
    await expect(paneTerm).toBeVisible();

    // Let the poll paths get going, and prove both probes work BEFORE the
    // hold. A probe that was already broken would otherwise make this test
    // "fail as expected" for a reason that has nothing to do with the hold —
    // which is exactly the way an expected-failure marker rots into noise.
    await sleep(WARMUP_MS);
    const warmPty = await ptyRoundTrip(page, paneTerm, 3, LIVENESS_BOUND_MS);
    expect(warmPty.ok, `pty round trip BEFORE the hold: ${warmPty.detail}`).toBe(true);
    const warmMcp = await mcpCall(
      await mintMcpEndpoint(page, appDataDir, REPO_ROOT),
      "ping",
      {},
      LIVENESS_BOUND_MS
    );
    expect(isJsonRpcResult(warmMcp), `MCP ping BEFORE the hold: ${warmMcp.detail}`).toBe(true);

    // ---- inject the hold ----
    // `groups` is on `resolve_token`'s path (by_token → agents → groups), so
    // every MCP method resolves through it, `ping` included; it is also what
    // the polled `orch_group_*` reads take. Holding it is the beta6 mechanism
    // with the culprit hold made explicit instead of guessed at.
    const endpoint = await mintMcpEndpoint(page, appDataDir, REPO_ROOT);
    requestLockHold(appDataDir, "groups", LOCK_HOLD_MS);
    const held = await waitForHoldAcquired(appDataDir);
    console.log(`[soak] lock hold acquired: ${JSON.stringify(held)}`);

    // Give the poll paths a few ticks to pile up behind the held mutex before
    // asking anything — the mechanism is an accumulation, not an instant.
    await sleep(Math.min(8_000, Math.floor(LOCK_HOLD_MS / 3)));

    // Both probes are gathered before either is asserted, so a red names which
    // half died rather than stopping at the first.
    const pty = await ptyRoundTrip(page, paneTerm, 4, LIVENESS_BOUND_MS);
    const ping = await mcpCall(endpoint, "ping", {}, LIVENESS_BOUND_MS);

    // The positive control, read AFTER the probes: a hold that had already
    // expired would leave both of them measuring an idle app, and this test
    // would then "pass" — which, under `test.fail()`, reports as a failure
    // claiming the bug is fixed.
    const stillHeld = holdStillHeld(appDataDir);
    expect(
      stillHeld,
      `the injected hold was no longer in effect when the probes finished, so neither result ` +
        `is evidence about a held lock. Raise ORRERIX_SOAK_LOCK_HOLD_MS above the probe budget.`
    ).toBe(true);

    expect.soft(
      pty.ok,
      `with a registry lock held for ${LOCK_HOLD_MS}ms, a keystroke did not round-trip through ` +
        `the pane's child within ${LIVENESS_BOUND_MS}ms. ${pty.detail}`
    ).toBe(true);
    expect.soft(
      isJsonRpcResult(ping),
      `with a registry lock held for ${LOCK_HOLD_MS}ms, the MCP did not answer a ping within ` +
        `${LIVENESS_BOUND_MS}ms. ${ping.detail} body=${JSON.stringify(ping.body)}`
    ).toBe(true);
  });
});
