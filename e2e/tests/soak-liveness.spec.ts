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
//   contend on the same registry mutexes, so the mechanism is exercised, but
//   the per-tick fan-out is smaller than a real orchestrating session's.
//   Closing that gap needs a safe stand-in agent process
//   (doc/design/e2e-testing.md's standing limitation), not a change here.
// - **Blocking-pool exhaustion: NOT reachable from the poll path at all, and
//   no hold duration changes that.** Plan #1600 §1.2 step 4 describes ticks
//   accumulating parked `spawn_blocking` threads until the 512-thread pool
//   exhausts. #1604 single-flights this sweep, so a tick firing while the
//   previous one is still outstanding SKIPS: at most one call is parked,
//   however long a lock is held. That is the fix working, and it means this
//   lane cannot demonstrate the pane-input half of the chain — an earlier
//   version of this header said raising the hold past ~120 s locally would
//   reach it, which was true against the base this branch was cut from and is
//   false here. What the class assertion still demonstrates is the half
//   #1604 does not govern: `orchestration/mcp.rs` spawns a thread per request
//   and each parks on the registry mutex, ungoverned by any single-flight,
//   which is exactly why the MCP probe is the one that dies while pane input
//   stays healthy.
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
//
// **What that marker costs, stated rather than left implicit**: it absorbs
// EVERY failure inside its own test, its positive controls included. An
// injector that silently never took a lock would leave both probes measuring
// an idle app, the test would fail, and Playwright would report a healthy
// expected failure. So none of that test's controls live inside it:
//
// - *the probes work at all* is spec 1's job — it runs both at t=0 and after
//   the soak, and it is not an expected failure, so a broken probe reddens
//   there;
// - *a hold really takes the mutex, and gives it back* is
//   `src-tauri/tests/e2ehold_guard.rs`'s job, proven differentially against a
//   real registry with no marker anywhere near it;
// - *the injector is armed in THIS build* is the short non-xfail test that
//   runs first in the same describe below.
//
// The assertions still inside the expected failure are diagnostics: they put
// the reason in the report. They are not evidence, and are not treated as any.
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
  assertCounterSeesTheApp,
  installInvokeCounter,
  installPtyTap,
  isJsonRpcResult,
  jsonRpcErrorCode,
  mcpCall,
  mintMcpEndpoint,
  orchInvokeTotal,
  singleFlightStats,
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

    console.log(`[soak] invoke counter installed via: ${await installInvokeCounter(page)}`);
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

    // Check the INSTRUMENT before trusting any number it produces. The
    // baseline round trip above cannot have succeeded without `write_pty`, so
    // a counter that has not seen one is blind — and a blind counter reports
    // the same `{}` as an app that never polled.
    const countsBefore = orchInvokeTotal(await assertCounterSeesTheApp(page, "write_pty"));
    const sweepsBefore = await singleFlightStats(page);

    // ---- the soak itself: the app is left completely alone ----
    await sleep(SOAK_MS);

    // The soak's own positive control. Without it this test asserts that an
    // app survives being ignored: if the tab-strip sweep were disarmed, or the
    // corpus's tabs failed to bind, the app would idle doing nothing at all
    // and both probes below would pass.
    //
    // The primary measure is SWEEPS, not invokes, and that is a consequence of
    // #1604. Before it, every 4 s tick issued a sweep, so `ticks × tabs × 2`
    // predicted the invoke count and a floor could be derived from arithmetic.
    // Now a tick whose predecessor has not settled skips — legitimately, that
    // is the fix — so the number of sweeps is something to READ rather than
    // derive, and a floor derived from the old model would be pricing a
    // mechanism this base no longer has.
    const counts = await readInvokeCounts(page);
    const sweepsAfter = await singleFlightStats(page);
    const sweeps = sweepsAfter.ran - sweepsBefore.ran;
    const skipped = sweepsAfter.skipped - sweepsBefore.skipped;
    const polled = orchInvokeTotal(counts) - countsBefore;
    const ticks = Math.floor(SOAK_MS / 4_000);
    const gate = await page.evaluate(
      () => (window as unknown as { __pollGateStats?: () => unknown }).__pollGateStats?.() ?? null
    );

    // Logged on SUCCESS, not only on failure (#1606 review N4): a green run
    // that records nothing leaves the next person to re-derive this floor from
    // the same arithmetic that just went stale under them.
    console.log(
      `[soak] over ${SOAK_MS}ms: ${sweeps} status sweeps ran, ${skipped} ticks skipped ` +
        `(of ~${ticks} opportunities); ${polled} orch_* invokes across ${BOUND_TABS} ` +
        `group-bound tabs; poll gate ${JSON.stringify(gate)}; counts ${JSON.stringify(counts)}`
    );

    // A floor on SWEEPS. 0.4 of the tick count, because a skip is expected
    // rather than a defect now and the property being asserted is "the poll
    // body ran repeatedly under load", not a cadence. Deliberately not a pin:
    // pinning the rate would pin this incident's shape, which is the mistake
    // this lane exists to stop making.
    const sweepFloor = Math.max(2, Math.floor(ticks * 0.4));
    expect(
      sweeps,
      `only ${sweeps} tab-strip status sweeps ran during a ${SOAK_MS}ms soak (expected at ` +
        `least ${sweepFloor} of ~${ticks} opportunities, ${skipped} skipped). The poll path ` +
        `this lane exists to load was not running, so the liveness result below would be ` +
        `about an idle app. Poll gate: ${JSON.stringify(gate)}`
    ).toBeGreaterThanOrEqual(sweepFloor);

    // And the two instruments have to agree. A sweep issues
    // `orch_group_summary` + `orch_group_usage` per group-bound tab, so any
    // sweep that ran dispatched something — sweeps climbing while dispatches
    // do not would mean the sweep is running and finding no bound tabs, which
    // is the corpus failing to bind rather than the app being healthy.
    expect(
      polled,
      `${sweeps} status sweeps ran but only ${polled} orch_* commands were dispatched. A ` +
        `sweep issues two per group-bound tab, so this means the sweep found no bound ` +
        `tabs — the corpus's tabs.json did not bind, and the soak applied no registry ` +
        `load at all. Counts: ${JSON.stringify(counts)}`
    ).toBeGreaterThanOrEqual(sweeps);

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

  // The one E2E-level control that the expected-failure marker below cannot
  // absorb: this build, launched this way, really does honour a hold request.
  // Everything about the mechanism is proven in Rust; what only a real launch
  // can show is that the injector was compiled in and armed — a release build,
  // or a missing opt-in, would leave the class assertion failing for a reason
  // that has nothing to do with the bug class, and reporting as a pass.
  //
  // It also carries the OTHER claim this lane makes about the merged tree, and
  // carries it here rather than in the class assertion because an expected
  // failure absorbs its own assertions: that #1604's single-flight really does
  // engage when a registry lock is held, so the poll path parks one call and
  // not a queue of them. The 180 s soak measured 46 sweeps and ZERO skips —
  // healthy, and therefore no evidence at all about the skip path. A held lock
  // is the condition the gate was built for, so it is the condition to observe
  // it under.
  test("the injector is armed, and a held registry lock makes the poll sweep single-flight", async ({
    appPage: page,
    appDataDir,
  }) => {
    // `appPage` is named, and used, on purpose: Playwright instantiates
    // fixtures LAZILY, so a test that asked only for `appDataDir` would get a
    // temp directory and no running app — and would then sit waiting for an
    // injector that was never started, failing for a reason with nothing to do
    // with whether the injector is armed.
    await expect(page.locator("#tab-bar")).toBeAttached();

    // `groups` rather than `agents`: it is what the polled `orch_group_summary`
    // takes, so holding it is what stalls a sweep. Long enough to span several
    // 4 s ticks, short enough to stay cheap.
    const holdMs = 12_000;
    const before = await singleFlightStats(page);
    requestLockHold(appDataDir, "groups", holdMs);
    const held = await waitForHoldAcquired(appDataDir);
    expect(held.target, "the injector honoured a different target than requested").toBe("groups");

    // Wait out the hold, then read what the gate did during it.
    const releasedBy = Date.now() + holdMs + 20_000;
    while (Date.now() < releasedBy && holdStillHeld(appDataDir)) await sleep(200);
    expect(holdStillHeld(appDataDir), `the injector never released a ${holdMs}ms hold`).toBe(false);

    const after = await singleFlightStats(page);
    const ran = after.ran - before.ran;
    const skipped = after.skipped - before.skipped;
    console.log(`[soak] during a ${holdMs}ms hold on groups: ${ran} sweeps ran, ${skipped} ticks skipped`);

    expect(
      skipped,
      `no tick was skipped while the \`groups\` mutex was held for ${holdMs}ms (${ran} sweeps ` +
        `ran). Either the status sweep does not contend on that mutex — in which case this ` +
        `lane's claim that #1604 bounds the poll path under a held lock is unevidenced — or ` +
        `the sweep is settling despite the hold, which would mean the hold is not reaching ` +
        `the read the sweep makes.`
    ).toBeGreaterThanOrEqual(1);
  });

  // Expected to fail on today's main. See this file's header for why the
  // marker is `fail` rather than `skip`, what it absorbs, and what is supposed
  // to happen to it when plan #1600's Phases 1 and 2 land.
  test("the app still accepts pane input and the MCP still answers while a registry lock is held", async ({
    appPage: page,
    appDataDir,
  }) => {
    test.fail();
    test.setTimeout(LOCK_HOLD_MS + 300_000);
    assertCorpusReallyLanded();

    await installInvokeCounter(page);
    await installPtyTap(page);
    await createTerminalPane(page, { name: "hold-pane", repo: REPO_ROOT, shell: "cmd" });
    const paneTerm = paneByName(page, "hold-pane").locator(".pane-term");
    await expect(paneTerm).toBeVisible();

    // Let the poll paths get going, then record ONE cheap before-reading.
    // Diagnostics, not a control — the marker on this test absorbs them (see
    // the file header); spec 1 is where a broken probe reddens. The pty probe
    // is deliberately NOT run here: it costs a full input-phase budget, and
    // buying an absorbed diagnostic with a minute of every CI run is a bad
    // trade when spec 1 already runs it twice.
    await sleep(WARMUP_MS);
    const warmMcp = await mcpCall(
      await mintMcpEndpoint(page, appDataDir, REPO_ROOT),
      "ping",
      {},
      LIVENESS_BOUND_MS
    );
    console.log(`[soak] before the hold: mcp ok=${isJsonRpcResult(warmMcp)} in ${warmMcp.ms}ms`);
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

    // Playwright prints NOTHING for a test that fails as expected, so the one
    // artefact this whole lane exists to produce — WHICH half died under a held
    // lock — would otherwise be invisible in the CI log, and any claim about it
    // would be a guess. Log it before asserting anything.
    console.log(
      `[soak] under a ${LOCK_HOLD_MS}ms hold on groups: ` +
        `pty ok=${pty.ok} in ${pty.ms}ms (${pty.detail}); ` +
        `mcp ok=${isJsonRpcResult(ping)} in ${ping.ms}ms (${ping.detail})`
    );

    // Read AFTER the probes: a hold that had already expired would leave both
    // of them measuring an idle app. Absorbed by the marker like everything
    // else here, so its value is the sentence it puts in the report — the
    // unabsorbed version of this check is the armed-injector test above.
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
