// Unit tests for the visibility policy behind #743 S6 (`src/pollgate.ts`).
//
// What these are written to catch: a gate that keeps polling behind a hidden
// window (the state the census found the app in), a gate that comes back
// without refreshing (a panel showing data as stale as the hidden stretch was
// long), and — the one that is not about the happy path at all — a gate that
// can only be released by the same `visibilitychange` event that suppressed
// it. That last one is the repo's standing rule about suppressions driven by a
// fallible signal (#496/#513/#518, performance.md §2 P4), and it is why the
// dropped-event test below never delivers the event it is waiting for.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  PollGate,
  documentVisibility,
  pollGateStats,
  resetPollGateStats,
  HIDDEN_RECHECK_MS,
  type Cancel,
  type VisibilitySource,
} from "../src/pollgate.ts";

/** A `Poll` that records what the gate asked it to do, in order. The order is
 *  part of the contract: `arm` before `refresh` means the resumed cadence is
 *  running while the catch-up is in flight, not after it returns. */
function recorder(): { poll: { arm(): void; disarm(): void; refresh(): void }; log: string[] } {
  const log: string[] = [];
  return {
    log,
    poll: {
      arm: () => void log.push("arm"),
      disarm: () => void log.push("disarm"),
      refresh: () => void log.push("refresh"),
    },
  };
}

interface FakeVisibility {
  source: VisibilitySource;
  /** Change the state. `notify: false` is a DROPPED event — the state moved
   *  and nobody was told, which is the failure the recheck ticker exists for. */
  set(visible: boolean, notify?: boolean): void;
  /** Fire a change notification without moving the state (browsers do). */
  notify(): void;
  subscribers(): number;
}

function fakeVisibility(initial = true): FakeVisibility {
  let visible = initial;
  const subs = new Set<() => void>();
  const fire = (): void => {
    for (const cb of [...subs]) cb();
  };
  return {
    source: {
      visible: () => visible,
      subscribe: (cb) => {
        subs.add(cb);
        return () => void subs.delete(cb);
      },
    },
    set: (v, notify = true) => {
      visible = v;
      if (notify) fire();
    },
    notify: fire,
    subscribers: () => subs.size,
  };
}

interface FakeRecheck {
  ticker: (onWake: () => void) => Cancel;
  /** Fire one recheck wake, as the shipped ticker's interval would. */
  wake(): void;
  running(): boolean;
  /** How many tickers have ever been started — a gate that starts a second one
   *  without cancelling the first leaks a timer per hide/show cycle. */
  starts(): number;
}

function fakeRecheck(): FakeRecheck {
  let onWake: (() => void) | null = null;
  let starts = 0;
  return {
    ticker: (cb) => {
      starts++;
      onWake = cb;
      return () => {
        if (onWake === cb) onWake = null;
      };
    },
    wake: () => onWake?.(),
    running: () => onWake !== null,
    starts: () => starts,
  };
}

function gateWith(initialVisible = true): {
  gate: PollGate;
  log: string[];
  vis: FakeVisibility;
  recheck: FakeRecheck;
} {
  const { poll, log } = recorder();
  const vis = fakeVisibility(initialVisible);
  const recheck = fakeRecheck();
  const gate = new PollGate(poll, { visibility: vis.source, recheck: recheck.ticker });
  return { gate, log, vis, recheck };
}

test("a poll enabled while the window is visible arms, and does not double the view's own load", () => {
  const { gate, log } = gateWith(true);
  gate.enable();
  assert.deepEqual(log, ["arm"], "the first arm must not refresh: the view has just loaded once itself");
  assert.equal(gate.armed, true);
});

test("the window going hidden stops the timer outright, rather than letting it fire and discard", () => {
  const { gate, log, vis } = gateWith(true);
  gate.enable();
  vis.set(false);
  assert.deepEqual(log, ["arm", "disarm"]);
  assert.equal(gate.armed, false, "a hidden window must leave NO armed poll — that is the whole slice");
});

test("coming back arms and refreshes exactly once, in that order", () => {
  const { gate, log, vis } = gateWith(true);
  gate.enable();
  vis.set(false);
  vis.set(true);
  assert.deepEqual(log, ["arm", "disarm", "arm", "refresh"]);
  // …and the catch-up is owed once per hidden stretch, not once per event.
  vis.notify();
  vis.notify();
  assert.deepEqual(log, ["arm", "disarm", "arm", "refresh"]);
});

test("a poll suppressed while hidden is released by re-READING visibility, not by the event", () => {
  // The bounded-release requirement. The event that would normally wake the
  // gate is never delivered here: `notify: false` moves the state and tells
  // nobody, which is exactly what a lost listener, a dropped webview
  // notification, or a platform that simply does not fire the event looks
  // like from inside this module. Without the recheck ticker the poll stays
  // suppressed forever and the panel silently stops updating.
  const { gate, log, vis, recheck } = gateWith(true);
  gate.enable();
  vis.set(false);
  assert.equal(recheck.running(), true, "a suppressed poll must be rechecking");

  vis.set(true, false); // the window is back; the event was lost
  assert.deepEqual(log, ["arm", "disarm"], "nothing has told the gate yet, by construction");

  recheck.wake();
  assert.deepEqual(log, ["arm", "disarm", "arm", "refresh"]);
  assert.equal(gate.armed, true);
  assert.equal(recheck.running(), false, "a running poll needs no recheck ticker");
});

test("a recheck that finds the window still hidden changes nothing and keeps one ticker", () => {
  const { gate, log, vis, recheck } = gateWith(true);
  gate.enable();
  vis.set(false);
  recheck.wake();
  recheck.wake();
  assert.deepEqual(log, ["arm", "disarm"], "a recheck is a read, not a tick — it issues no poll");
  assert.equal(recheck.starts(), 1, "a second ticker per wake would leak one timer per recheck");
});

test("enabling while the window is already hidden never arms a timer at all", () => {
  const { gate, log, vis, recheck } = gateWith(false);
  gate.enable();
  assert.deepEqual(log, [], "no arm, and nothing to disarm — the poll was never started");
  assert.equal(gate.armed, false);
  assert.equal(recheck.running(), true);
  vis.set(true);
  assert.deepEqual(log, ["arm", "refresh"]);
});

test("disable() tears down the timer, the subscription and the ticker, and is idempotent", () => {
  const { gate, log, vis, recheck } = gateWith(true);
  gate.enable();
  assert.equal(vis.subscribers(), 1);
  gate.disable();
  assert.deepEqual(log, ["arm", "disarm"]);
  assert.equal(vis.subscribers(), 0, "a disposed view must not leave a visibility listener behind");
  assert.equal(recheck.running(), false);

  gate.disable();
  vis.set(false);
  vis.set(true);
  assert.deepEqual(log, ["arm", "disarm"], "a disabled gate reacts to nothing");
});

test("disable() while suppressed cancels the recheck ticker rather than leaving it spinning", () => {
  const { gate, vis, recheck } = gateWith(true);
  gate.enable();
  vis.set(false);
  assert.equal(recheck.running(), true);
  gate.disable();
  assert.equal(recheck.running(), false, "closing a panel behind a hidden window must leave no timer");
});

test("a view re-enabled after a hidden stretch does not inherit the previous stretch's catch-up", () => {
  // A closed-and-reopened view loads on open like any other open, so a stale
  // `owesRefresh` surviving `disable()` would make every reopen a double load.
  const { gate, log, vis } = gateWith(true);
  gate.enable();
  vis.set(false);
  gate.disable();
  vis.set(true);
  gate.enable();
  assert.deepEqual(log, ["arm", "disarm", "arm"]);
});

test("enable() twice arms once and subscribes once", () => {
  const { gate, log, vis } = gateWith(true);
  gate.enable();
  gate.enable();
  assert.deepEqual(log, ["arm"]);
  assert.equal(vis.subscribers(), 1);
});

test("a change notification that does not move the state re-arms nothing", () => {
  const { gate, log, vis } = gateWith(true);
  gate.enable();
  vis.notify();
  vis.notify();
  assert.deepEqual(log, ["arm"]);
});

test("gates are independent: one view's close does not release another's suppression", () => {
  const vis = fakeVisibility(true);
  const a = recorder();
  const b = recorder();
  const ga = new PollGate(a.poll, { visibility: vis.source, recheck: fakeRecheck().ticker });
  const gb = new PollGate(b.poll, { visibility: vis.source, recheck: fakeRecheck().ticker });
  ga.enable();
  gb.enable();
  vis.set(false);
  ga.disable();
  vis.set(true);
  assert.deepEqual(a.log, ["arm", "disarm"], "the disabled gate stays down");
  assert.deepEqual(b.log, ["arm", "disarm", "arm", "refresh"], "the live one comes back");
});

test("pollGateStats counts what the human is asked to look at while minimizing the window", () => {
  // The hand-validation instrument: agents cannot run the GUI, so this is the
  // observable the PR asks the human to read from devtools. `armed` falling to
  // zero with the window hidden IS the slice's claim.
  //
  // Read as deltas against whatever else is live: the registry is module-wide
  // on purpose (the whole app's gates, seen from one devtools call), so a test
  // that asserted absolutes would be asserting the order node ran the file in.
  resetPollGateStats();
  const base = pollGateStats();
  const vis = fakeVisibility(true);
  const one = new PollGate(recorder().poll, { visibility: vis.source, recheck: fakeRecheck().ticker });
  const two = new PollGate(recorder().poll, { visibility: vis.source, recheck: fakeRecheck().ticker });
  one.enable();
  two.enable();
  const delta = (): Record<string, number> => {
    const now = pollGateStats();
    return {
      enabled: now.enabled - base.enabled,
      armed: now.armed - base.armed,
      suppressions: now.suppressions - base.suppressions,
      resumes: now.resumes - base.resumes,
    };
  };
  assert.deepEqual(delta(), { enabled: 2, armed: 2, suppressions: 0, resumes: 0 });

  vis.set(false);
  assert.deepEqual(delta(), { enabled: 2, armed: 0, suppressions: 2, resumes: 0 });

  vis.set(true);
  assert.deepEqual(delta(), { enabled: 2, armed: 2, suppressions: 2, resumes: 2 });

  one.disable();
  two.disable();
  assert.deepEqual(
    delta(),
    { enabled: 0, armed: 0, suppressions: 2, resumes: 2 },
    "a disabled gate must leave the registry, not leak into it"
  );
  resetPollGateStats();
});

test("documentVisibility reads visibilityState and unsubscribes cleanly", () => {
  const listeners = new Set<() => void>();
  const doc = {
    visibilityState: "visible",
    addEventListener: (_t: "visibilitychange", cb: () => void) => void listeners.add(cb),
    removeEventListener: (_t: "visibilitychange", cb: () => void) => void listeners.delete(cb),
  };
  const source = documentVisibility(doc);
  assert.equal(source.visible(), true);
  doc.visibilityState = "hidden";
  assert.equal(source.visible(), false);
  // Anything that is not "hidden" is on screen as far as a poll is concerned —
  // "prerender" included, since the page is live enough to paint into.
  doc.visibilityState = "prerender";
  assert.equal(source.visible(), true);

  let fired = 0;
  const off = source.subscribe(() => fired++);
  assert.equal(listeners.size, 1);
  for (const cb of listeners) cb();
  assert.equal(fired, 1);
  off();
  assert.equal(listeners.size, 0, "unsubscribe must remove the same listener it added");
});

test("the recheck cadence is a real interval, not a value that degenerates", () => {
  // HIDDEN_RECHECK_MS is the worst-case extra staleness when the change event
  // never arrives. Zero would make the fallback a spin loop on the GUI thread —
  // a bounded release that costs more than the thing it releases.
  assert.ok(
    HIDDEN_RECHECK_MS >= 1000 && HIDDEN_RECHECK_MS <= 30_000,
    `HIDDEN_RECHECK_MS is ${HIDDEN_RECHECK_MS} ms: below a second it is a spin, above thirty ` +
      `seconds a restored window feels stuck when the event does not fire`
  );
});
