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
//   ("the app's one app-lifetime poll") and `pollStatus` issues one
//   `orch_strip_view` covering every GROUP-BOUND tab (it was
//   `orch_group_summary` + `orch_group_usage` per tab until #1608 served the
//   strip from a published snapshot). A tab is group-bound purely because its
//   persisted `groupIds` names a group, so the corpus's `tabs.json` alone puts
//   that poll under load — no clicking, and no agent CLI. What the collapse
//   changes for this lane is the SHAPE of the load, not its presence: one
//   command per sweep still crosses the IPC boundary and still reaches the
//   backend on a 4 s cadence. It no longer takes a registry lock, which is why
//   the class assertion below is about the MCP path rather than this one.
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
// - **Blocking-pool exhaustion: NOT reachable, at either end of the chain,
//   and no hold duration changes that.** Plan #1600 §1.2 step 4 describes
//   ticks accumulating parked `spawn_blocking` threads until the 512-thread
//   pool exhausts, at which point `write_pty` can no longer be scheduled and
//   every pane stops accepting input at once. Both ends are now cut,
//   independently: #1604 single-flights this sweep, so a tick firing while
//   the previous one is still outstanding SKIPS and at most one call is
//   parked however long a lock is held — though since #1608 that sweep reads a
//   published snapshot and takes no registry lock, so a held lock no longer
//   parks it and the gate has nothing to engage on; and #1607 moved `write_pty` off the
//   shared pool onto a per-pane writer thread, so even an exhausted pool
//   could not starve pane input. An earlier version of this header said
//   raising the hold past ~120 s locally would reach the pane-input half —
//   true against the base this branch was cut from, false here, and now
//   false twice over.
//
//   What the class assertion demonstrates is the half neither of those fixes
//   governs: `orchestration/mcp.rs` spawns a thread per request and each
//   parks on the registry mutex, ungoverned by any single-flight and served
//   by no pool. That is exactly why the MCP probe is the one that died while
//   pane input stayed healthy — and it is what #1609 closes.
//
//   The pty probe is NOT a regression guard on #1607, and saying so was an
//   overclaim (#1606 review R2). A revert of #1607 would not redden it:
//   single-flighting keeps at most one poll call outstanding and the MCP
//   threads are its own, so the shared pool never fills here and `write_pty`
//   gets a slot whether or not it has its own writer thread. What the probe
//   does say is what `liveness.ts` says at `ptyRoundTrip`: pool exhaustion is
//   no longer among the ways it can fail, so a red points at the writer
//   thread or the ConPTY write behind it.
//
// ## The class assertion is ARMED (#1609)
//
// It was marked `test.fail()` from #1606 until Phase 2.1 landed, because under
// the beta6 mechanism a long registry hold took the MCP down immediately and
// nothing in the tree bounded it. #1609 bounds `resolve_token` under
// `MCP_AUTH_BUDGET`, so the ping now ANSWERS during a hold — with a retryable
// `-32001`, not a result: the registry is deliberately still unavailable, and
// what changed is that saying so is now possible. The marker is gone and the
// assertion is hard.
//
// **The controls still live OUTSIDE this test, and that is not left over from
// the marker.** While `test.fail()` was on, it absorbed every failure inside
// its own test — an injector that silently never took a lock would have left
// both probes measuring an idle app and reported a healthy expected failure.
// That hazard is gone, but the placement is still right: a positive control
// inside the test it controls can only ever fail the same way the test does,
// so it cannot distinguish "the property broke" from "the fixture broke":
//
// - *the probes work at all* is spec 1's job — it runs both at t=0 and after
//   the soak, so a broken probe reddens there;
// - *a hold really takes the mutex, and gives it back* is
//   `src-tauri/tests/e2ehold_guard.rs`'s job, proven differentially against a
//   real registry with no marker anywhere near it;
// - *the injector is armed in THIS build* is the short non-xfail test that
//   runs first in the same describe below.
//
// The probe assertions inside the test use `expect.soft` so BOTH are
// evaluated and both land in the report — the artefact this lane exists to
// produce is WHICH half died, and a hard assertion on the first one would
// hide the second. Soft is not weak here: the test still fails.
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
  jsonRpcErrorData,
  MCP_BUSY_CODE,
  jsonRpcErrorCode,
  mcpCall,
  mintMcpEndpoint,
  orchInvokeTotal,
  singleFlightStats,
  ptyRoundTrip,
  parseHoldCrumbs,
  readBreadcrumbs,
  readInvokeCounts,
  tabStatusStats,
  stripStaleState,
  requestLockHold,
  sleep,
  waitForHoldAcquired,
  waitForLockHoldCrumb,
} from "../liveness";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
/** A real directory to name as each synthetic group's repo, and as the pane's
 *  cwd — this checkout itself. Nothing writes to it. */
const REPO_ROOT = path.resolve(__dirname, "../..");

/** Tabs bound to a group. Each one cost two `orch_*` invokes every 4 s until
 *  #1608; the sweep is now one `orch_strip_view` whatever the tab count, so
 *  this is no longer the poll load's multiplier — it is how many groups the
 *  strip resolves, which is what `tabStatusStats` witnesses. */
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
    // Per-command baseline for the strip read, so the floor below measures THIS
    // soak rather than every orch_strip_view since boot.
    const stripBefore = (await readInvokeCounts(page))["orch_strip_view"] ?? 0;

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

    // And the two instruments have to agree — but at what, changed under this
    // spec, and the change is worth stating because it took a witness away.
    //
    // WHAT THIS USED TO ASSERT. A sweep issued `orch_group_summary` +
    // `orch_group_usage` per group-bound tab, so a full sweep dispatched
    // `BOUND_TABS × 2` and `polled` tracked `sweeps × BOUND_TABS × 2`. The
    // floor was raised to half that nominal fan-out in #1606 review round 2
    // (N1) to catch a corpus that binds only SOME of its tabs: with the older
    // `polled >= sweeps` floor, one that bound 1 of 4 dispatched 90 against 45
    // sweeps and passed, running the soak at a quarter of its intended registry
    // load with `assertCorpusReallyLanded` none the wiser — it checks groups,
    // sessions and audit bytes, never the binding.
    //
    // WHY IT NO LONGER CAN. #1608 serves the whole strip from one published
    // snapshot: a sweep now dispatches ONE `orch_strip_view` whatever the tab
    // count. That is the improvement — the strip's IPC stops growing with the
    // human's tabs — and it deletes the SCALING the binding witness rode on.
    // Not a stale constant: the fan-out that carried the signal is gone.
    //
    // So the two properties that rode on one assertion are now asserted
    // separately, and the binding one is read where binding actually lives.
    // Nearly a tautology, and honestly so: SingleFlight.run counts a run
    // around a body that reaches stripView unconditionally, so this holds by
    // construction against a 50% floor. It can still fail if the sweep body
    // stops reaching the backend, which is what its message says and is worth
    // keeping for. What it does NOT do any more is witness BINDING — the
    // `tabStatusStats` assertions below are what carry that (#1625 review
    // round 2, N2).
    const stripDispatches = (counts["orch_strip_view"] ?? 0) - stripBefore;
    const dispatchFloor = Math.floor(sweeps * 0.5);
    expect(
      stripDispatches,
      `${sweeps} status sweeps ran but only ${stripDispatches} orch_strip_view commands were ` +
        `dispatched (expected at least ${dispatchFloor}, half of one per sweep). A sweep issues ` +
        `exactly one since #1608, so a shortfall means the sweep body is not reaching the ` +
        `backend at all. Counts: ${JSON.stringify(counts)}`
    ).toBeGreaterThanOrEqual(dispatchFloor);

    // THE BINDING WITNESS, replacing the fan-out one #1608 removed. `bound` is
    // every tab whose persisted `groupIds` names a group; `seen` the subset the
    // published snapshot carried. A corpus that binds 1 of 4 reddens here — the
    // case N1 was raised about — and so does one whose groups never reach the
    // snapshot, which the fan-out floor could not distinguish.
    const tabs = await tabStatusStats(page);
    expect(
      tabs.bound.length,
      `the strip resolved ${tabs.bound.length} group-bound tabs, expected ${BOUND_TABS}. The ` +
        `corpus's tabs.json did not bind what it reports, so the soak applied less registry ` +
        `load than it claims. Bound: ${JSON.stringify(tabs.bound)}`
    ).toBe(BOUND_TABS);
    expect(
      tabs.seen.length,
      `${tabs.bound.length} tabs are group-bound but only ${tabs.seen.length} of their groups ` +
        `were in the published snapshot, so the strip rendered stale or absent status for the ` +
        `rest. Bound: ${JSON.stringify(tabs.bound)}; seen: ${JSON.stringify(tabs.seen)}`
    ).toBe(tabs.bound.length);

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
  // failure absorbs its own assertions: that a held registry lock does not
  // stall the poll sweep, which is the INVERSE of what this test asserted
  // before #1608 and is the point of that change.
  //
  // It used to assert `skipped >= 1`: `orch_group_summary` took `groups`, so a
  // hold parked the sweep, the next tick found it outstanding, and #1604's gate
  // skipped. #1608 serves the sweep from a published snapshot — no registry
  // lock, so it settles under a hold and a skip would now mean the poll path is
  // contending again.
  //
  // Stated plainly because it is a loss as well as a gain: `skipped >= 1` has
  // NO witness left anywhere in this lane, and cannot have one, because the
  // condition that produced a skip is the condition #1608 removed. The gate
  // itself is still tested — `test/singleflight.test.ts` covers it directly —
  // just not through a held registry lock.
  test("the injector is armed, and a held registry lock does not stall the poll sweep", async ({
    appPage: page,
    appDataDir,
  }) => {
    // `appPage` is named, and used, on purpose: Playwright instantiates
    // fixtures LAZILY, so a test that asked only for `appDataDir` would get a
    // temp directory and no running app — and would then sit waiting for an
    // injector that was never started, failing for a reason with nothing to do
    // with whether the injector is armed.
    await expect(page.locator("#tab-bar")).toBeAttached();

    // `groups` rather than `agents`: it is what `resolve_token` takes on every
    // MCP request, so holding it is what stalls the probe this spec is about.
    // It used to stall the tab-strip sweep too — `orch_group_summary` took it —
    // but since #1608 that sweep reads a published snapshot and no longer
    // acquires a registry lock at all, which is the change that made the class
    // assertion below an MCP-side statement rather than a poll-path one.
    // Long enough to span several 4 s ticks, short enough to stay cheap.
    const holdMs = 12_000;
    const before = await singleFlightStats(page);
    requestLockHold(appDataDir, "groups", holdMs);
    const held = await waitForHoldAcquired(appDataDir);
    expect(held.target, "the injector honoured a different target than requested").toBe("groups");

    // Wait out the hold, then read what the gate did during it.
    const releasedBy = Date.now() + holdMs + 20_000;
    // Sampled INSIDE the wait, because by the time the hold has released the
    // publisher has had a second to catch up and the badge is already gone.
    // `sawStale` is what the human would have seen while the backend was stuck.
    let sawStale = false;
    while (Date.now() < releasedBy && holdStillHeld(appDataDir)) {
      await sleep(200);
      if (!sawStale) sawStale = (await stripStaleState(page)).stale;
    }
    expect(holdStillHeld(appDataDir), `the injector never released a ${holdMs}ms hold`).toBe(false);

    const after = await singleFlightStats(page);
    const ran = after.ran - before.ran;
    const skipped = after.skipped - before.skipped;

    // Phase 0 (#1605) put a self-watchdog behind every registry mutex, and it
    // reports a hold outliving `lockwatch::DEFAULT_HOLD_WARN_MS` (5 s) as a
    // breadcrumb naming the lock, the duration, the waiter count and the
    // holder's call site. This hold is 12 s on a lock named `groups`, so the
    // app's OWN instrument has to have seen exactly that — which is the one
    // thing this lane could not previously check: its own premise. Until now
    // "the injector really took the mutex" rested on the injector's own state
    // file, which is the injector agreeing with itself.
    //
    // It must name `groups`, and it must be the COMPLETED report. Two earlier
    // versions got this wrong in two different ways, both measured rather
    // than reasoned: one asserted a COUNT of long-hold breadcrumbs and went
    // green on a `lock=usage_memo_cell held_ms=8538` crumb from unrelated
    // background work; the next read the first `lock-slow`, which is the
    // watchdog noticing a hold still IN PROGRESS, so a 12 s hold reported as
    // `held_ms=5037`. `lock-freed` is the one whose duration is final.
    const crumb = await waitForLockHoldCrumb(appDataDir, "groups", "lock-freed", 5_000);
    const allCrumbs = parseHoldCrumbs(readBreadcrumbs(appDataDir));
    // The in-progress reports are logged rather than asserted, because they
    // carry something no other instrument here does: the WAITER count, which
    // is the poll path piling up behind the held mutex as it happens.
    const slow = allCrumbs.filter((c) => c.lock === "groups" && c.event === "lock-slow");
    console.log(
      `[soak] during a ${holdMs}ms hold on groups: ${ran} sweeps ran, ${skipped} ticks ` +
        `skipped; watchdog long-hold reports: ${allCrumbs.length} total, ` +
        `${slow.length} in-progress on groups${slow.length ? ` (last: ${slow[slow.length - 1].raw})` : ""}; ` +
        `completed ${crumb ? `= ${crumb.raw}` : "NOT FOUND"}`
    );

    expect(
      crumb?.lock,
      `Phase 0's self-watchdog never reported a long hold on \`groups\` while this test ` +
        `held it for ${holdMs}ms — more than twice its 5 s threshold. Either the injector ` +
        `is not taking a TRACKED mutex (so this lane's premise is unverified), or the ` +
        `watchdog thread is not running in this build. It reported ${allCrumbs.length} ` +
        `long hold(s) on other locks: ${JSON.stringify(allCrumbs.map((c) => c.lock))}`
    ).toBe("groups");
    // The two instruments have to agree about the same event's DURATION, not
    // merely both have noticed something. 2 s of slack covers the watchdog's
    // 1 Hz sampling and the injector's own request/acquire gap.
    expect(
      crumb?.heldMs ?? 0,
      `the watchdog's completed report for \`groups\` disagrees with the duration the ` +
        `injector claims to have held it (${holdMs}ms): ${crumb?.raw}`
    ).toBeGreaterThanOrEqual(holdMs - 2_000);

    // THE CONTRACT THIS ASSERTS CHANGED UNDER PHASE 1, and the direction is
    // the whole point of that phase.
    //
    // #1606 asserted `skipped >= 1`: holding `groups` stalled the sweep,
    // because `orch_group_summary` took that mutex, so the next 4 s tick found
    // its predecessor outstanding and the single-flight gate skipped it. That
    // was the right assertion against a poll path that contends.
    //
    // #1608 serves the strip from a published snapshot. The sweep takes NO
    // registry lock, so it settles normally while `groups` is held — and a
    // skip would now mean something is wrong. Keeping `skipped >= 1` would
    // pin the defect this epic removed.
    //
    // What replaces it is INV-6's actual promise: under a hold the surface
    // keeps answering and says it is stale, rather than parking threads.
    expect(
      ran,
      `no status sweep completed while the \`groups\` mutex was held for ${holdMs}ms. Since ` +
        `#1608 the sweep reads a published snapshot and takes no registry lock, so a hold ` +
        `must not stall it — if nothing ran, the poll path is contending again and the ` +
        `blocking-pool accumulation this epic removed is reachable once more.`
    ).toBeGreaterThanOrEqual(1);
    expect(
      skipped,
      `${skipped} tick(s) were skipped while \`groups\` was held for ${holdMs}ms. A skip means ` +
        `a sweep did not settle within its own 4 s tick, which since #1608 it has no reason ` +
        `to do: the read is a pointer clone. Either something reintroduced a registry ` +
        `acquisition on the poll path, or the strip read is blocking on something new.`
    ).toBe(0);

    // ...and the disclosure half. A snapshot the publisher cannot refresh ages,
    // and the strip has to SAY so — that is what makes a stalled backend a
    // stale panel rather than a frozen one that looks live (#1604 review N3,
    // the requirement Phase 1 carries). The publisher parks on the same
    // `groups` this test holds, so 12 s of hold is well past
    // VIEW_STALE_AFTER_MS (5 s).
    expect(
      sawStale,
      `the strip never reported itself stale during ${holdMs}ms with \`groups\` held. The ` +
        `publisher parks on that mutex, so its snapshot cannot have been refreshed past ` +
        `VIEW_STALE_AFTER_MS (5 s) — a strip that keeps rendering without disclosure is the ` +
        `silent freeze this phase exists to remove.`
    ).toBe(true);

    // RELEASED ON EVIDENCE, not on a timer: the badge comes down only because
    // the publisher got the lock back and stored a fresh snapshot.
    const clearedBy = Date.now() + 10_000;
    let cleared = false;
    while (Date.now() < clearedBy && !cleared) {
      await sleep(250);
      cleared = !(await stripStaleState(page)).stale;
    }
    expect(
      cleared,
      `the strip stayed stale after the hold released. The badge is cleared by the next ` +
        `successful publish, so if it never clears the publisher did not recover — which ` +
        `would make the disclosure permanent rather than bounded (INV-6).`
    ).toBe(true);
  });

  // Armed since #1609 (Phase 2.1). See this file's header for what the
  // `test.fail()` marker was for and why it is gone.
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

    // The one artefact this whole lane exists to produce is WHICH half died
    // under a held lock. Logged before anything is asserted so it is in the CI
    // log even on a pass, and so a claim about it is never a guess.
    console.log(
      `[soak] under a ${LOCK_HOLD_MS}ms hold on groups: ` +
        `pty ok=${pty.ok} in ${pty.ms}ms (${pty.detail}); ` +
        `mcp ok=${isJsonRpcResult(ping)} in ${ping.ms}ms (${ping.detail})`
    );

    // Read AFTER the probes: a hold that had already expired would leave both
    // of them measuring an idle app. Now that the marker is gone this is a real
    // gate rather than a sentence in a report — if it fails, neither probe
    // result below is evidence about anything.
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

    // The MCP half, and the shape matters as much as the fact.
    //
    // NOT `isJsonRpcResult`: that predicate requires `result !== undefined`,
    // i.e. it asserts the registry was AVAILABLE — which under a deliberate
    // 90 s hold it is not, and never will be. What #1609 changed is that the
    // server can now SAY so within a bound instead of never answering. So the
    // liveness property is "it answered", and the contract is that the answer
    // is the retryable busy rather than anything else.
    const pingCode = jsonRpcErrorCode(ping);
    expect.soft(
      isJsonRpcResult(ping) || pingCode === MCP_BUSY_CODE,
      `with a registry lock held for ${LOCK_HOLD_MS}ms, the MCP did not ANSWER a ping within ` +
        `${LIVENESS_BOUND_MS}ms. ${ping.detail} body=${JSON.stringify(ping.body)}`
    ).toBe(true);
    expect.soft(
      pingCode,
      `the MCP answered, but not with the retryable busy code. Under a held lock the ` +
        `contract is ${MCP_BUSY_CODE} (loomux busy), not -32000 (permanent auth refusal) ` +
        `and not a result. body=${JSON.stringify(ping.body)}`
    ).toBe(MCP_BUSY_CODE);
    const busyData = jsonRpcErrorData(ping);
    expect.soft(
      busyData?.retryable,
      `the busy error must carry data.retryable — it is what tells a client this is ` +
        `worth re-issuing. body=${JSON.stringify(ping.body)}`
    ).toBe(true);
    expect.soft(
      typeof busyData?.retry_after_ms,
      `the busy error must carry a numeric data.retry_after_ms backoff hint. ` +
        `body=${JSON.stringify(ping.body)}`
    ).toBe("number");
  });
});
