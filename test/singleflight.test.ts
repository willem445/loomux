// The single-flight poll gate (#1602, plan §3 Phase 2.2 of EPIC #1600) —
// `src/singleflight.ts`.
//
// The defect this pins: the group view's batch poll and the tab strip's
// per-tab status sweep both fire a fresh `Promise.all` on every `setInterval`
// tick without ever checking whether the PREVIOUS tick's call has settled. A
// slow or stuck backend call then accumulates one parked blocking-pool thread
// per tick — the beta6 mechanism (EPIC #1600 §1.2) — instead of skipping the
// tick outright. The fix is one predicate ("is a call from this gate already
// outstanding?"), so it is tested as one; the DOM wiring (groupview.ts's
// `load()`, tabbar.ts's `pollStatus()`) is not re-simulated here, per this
// repo's DOM-free pure-module convention (layout.ts/steer.ts).
//
// A deferred helper stands in for a backend call whose settlement the test
// controls directly, so "still outstanding" and "just settled" are exact
// moments rather than something timed with a real delay.
import { test } from "node:test";
import assert from "node:assert/strict";
import { SingleFlight, resetSingleFlightStats, singleFlightStats } from "../src/singleflight.ts";

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("a tick finding the previous call still outstanding is skipped, not re-issued", async () => {
  const calls: number[] = [];
  const gate = new SingleFlight();
  const first = deferred<string>();

  const p1 = gate.run(() => {
    calls.push(1);
    return first.promise;
  });
  assert.equal(gate.pending, true, "the first call is outstanding");

  // A tick that fires while the first call has not settled must not invoke
  // its body at all — this is the whole point, not an aside.
  const p2 = gate.run(async () => {
    calls.push(2);
    return "should never run";
  });

  assert.deepEqual(calls, [1], "the skipped tick's body never ran");
  assert.equal(await p2, undefined, "a skipped tick resolves to undefined, never queues");

  first.resolve("done");
  assert.equal(await p1, "done");
});

test("the next tick after resolution runs normally", async () => {
  const calls: number[] = [];
  const gate = new SingleFlight();

  await gate.run(async () => {
    calls.push(1);
    return "a";
  });
  assert.equal(gate.pending, false, "the gate releases once the call settles");

  await gate.run(async () => {
    calls.push(2);
    return "b";
  });

  assert.deepEqual(calls, [1, 2], "a tick after the flight cleared is not skipped");
});

test("a rejected call releases the flight — it never wedges the gate open", async () => {
  const gate = new SingleFlight();
  const failing = deferred<never>();

  const p1 = gate.run(() => failing.promise);
  assert.equal(gate.pending, true);
  failing.reject(new Error("backend refused"));
  await assert.rejects(p1, /backend refused/);

  assert.equal(gate.pending, false, "a REJECTED call must clear pending, same as a resolved one");

  const calls: number[] = [];
  await gate.run(async () => {
    calls.push(1);
  });
  assert.deepEqual(calls, [1], "the tick right after an error is not skipped");
});

test("two SingleFlight instances never interfere — the gate is per site, not global", async () => {
  // This is the negative control for "not global": a naive module-level
  // boolean would make gate B's tick observe gate A's outstanding call and
  // skip it too, which is exactly the cross-tenant coupling the module's
  // header argues against (one stuck group view must never silence another's
  // poll, or the tab strip's).
  const a = new SingleFlight();
  const b = new SingleFlight();
  const outstanding = deferred<string>();

  const pa = a.run(() => outstanding.promise);
  assert.equal(a.pending, true);
  assert.equal(b.pending, false, "gate B is untouched by gate A's outstanding call");

  const calls: string[] = [];
  const rb = await b.run(async () => {
    calls.push("b");
    return "b-result";
  });
  assert.deepEqual(calls, ["b"], "gate B's call ran even while gate A was in flight");
  assert.equal(rb, "b-result");

  outstanding.resolve("a-result");
  assert.equal(await pa, "a-result");
});

test("skips and runs are counted on the shared debug instrument, __singleFlightStats", async () => {
  resetSingleFlightStats();
  const gate = new SingleFlight();
  const first = deferred<void>();

  const p1 = gate.run(() => first.promise);
  // Two ticks land while the first call is still outstanding.
  await gate.run(async () => {});
  await gate.run(async () => {});
  assert.deepEqual(singleFlightStats(), { ran: 1, skipped: 2 });

  first.resolve();
  await p1;
  await gate.run(async () => {});
  assert.deepEqual(
    singleFlightStats(),
    { ran: 2, skipped: 2 },
    "a tick after the flight clears counts as ran, not skipped"
  );
});

test("pending is false before the first call and while nothing is outstanding", () => {
  const gate = new SingleFlight();
  assert.equal(gate.pending, false);
});
