// The pane's one audit-log read (#1317) — the concurrency and freshness
// contract that lets the audit viewer and the progress timeline share it.
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";

import { AuditStore, AUDIT_READ_MAX_AGE_MS } from "../src/auditstore.ts";
import type { AuditEntry } from "../src/auditsummary.ts";

const entry = (ts_ms: number, action = "prompt"): AuditEntry => ({
  ts_ms,
  actor: "orch-1",
  action,
  detail: { text: "x" },
});

/** A fetch whose runs are resolved by hand, so a test can hold two callers
 *  inside one in-flight read. */
function deferredFetch() {
  const pending: Array<{ resolve: (r: AuditEntry[]) => void; reject: (e: unknown) => void }> = [];
  let calls = 0;
  const fetchRows = () => {
    calls += 1;
    return new Promise<AuditEntry[]>((resolve, reject) => pending.push({ resolve, reject }));
  };
  return { fetchRows, pending, calls: () => calls };
}

/** Await a store read, but FAIL rather than hang if it never settles.
 *
 *  Every read here is fed by `deferredFetch`, so a mutation that makes the
 *  store start a fetch it should not have started leaves the await pending
 *  forever — and a hung suite is a timeout with no assertion in it, which is
 *  not evidence about anything (`.claude/skills/ci-validate`: "a mutation that
 *  hangs the suite is a timeout, not a red"). This turns that into a named
 *  failure. The timer is real, unlike the store's own clock, and 250 ms is
 *  orders of magnitude above what a resolved promise needs. */
function settles<T>(p: Promise<T>, what: string): Promise<T> {
  return Promise.race([
    p,
    new Promise<T>((_, reject) =>
      setTimeout(() => reject(new Error(`${what}: never settled — the store started a read nobody resolved`)), 250).unref()
    ),
  ]);
}

/** A clock the test moves by hand — the window is measured, never slept. */
function clock(start = 1000) {
  let t = start;
  return { now: () => t, advance: (ms: number) => (t += ms) };
}

test("a second reader inside an in-flight read JOINS it — one fetch, not two", async () => {
  const f = deferredFetch();
  const c = clock();
  const store = new AuditStore(f.fetchRows, c.now);

  const a = store.read(0);
  const b = store.read(0);
  assert.equal(f.calls(), 1, "the second caller must not start its own read");

  f.pending[0].resolve([entry(1), entry(2)]);
  assert.deepEqual((await settles(a, "first")).map((e) => e.ts_ms), [1, 2]);
  assert.deepEqual((await settles(b, "joiner")).map((e) => e.ts_ms), [1, 2],
    "and it gets the joined run's answer");
  assert.equal(f.calls(), 1);
});

test("both readers get the SAME array — the de-duplication, not two copies", async () => {
  const f = deferredFetch();
  const store = new AuditStore(f.fetchRows, clock().now);
  const a = store.read(0);
  const b = store.read(0);
  const rows = [entry(1)];
  f.pending[0].resolve(rows);
  const [ra, rb] = [await settles(a, "first"), await settles(b, "joiner")];
  assert.equal(ra, rb, "one array, handed to both");
  assert.equal(ra, store.cached);
});

test("a read younger than the window is served without touching IPC", async () => {
  const f = deferredFetch();
  const c = clock();
  const store = new AuditStore(f.fetchRows, c.now);

  const first = store.read(0);
  f.pending[0].resolve([entry(1)]);
  await settles(first, "first");
  assert.equal(f.calls(), 1);

  c.advance(AUDIT_READ_MAX_AGE_MS - 1);
  const served = store.read();
  // Asserted BEFORE the await: this is the whole property, and a store that
  // wrongly starts a read would otherwise leave the await hanging instead of
  // failing here.
  assert.equal(f.calls(), 1, "inside the window: the other view's tick is free");
  assert.deepEqual((await settles(served, "windowed")).map((e) => e.ts_ms), [1]);

  // POSITIVE CONTROL: the window is a bound, not a cache-forever switch — one
  // more millisecond and it reads again.
  c.advance(1);
  const again = store.read();
  assert.equal(f.calls(), 2, "past the window it re-reads");
  f.pending[1].resolve([entry(1), entry(2)]);
  assert.deepEqual((await settles(again, "past the window")).map((e) => e.ts_ms), [1, 2]);
});

test("an explicit gesture passes 0 and is never served a cached answer", async () => {
  const f = deferredFetch();
  const c = clock();
  const store = new AuditStore(f.fetchRows, c.now);
  const first = store.read(0);
  f.pending[0].resolve([entry(1)]);
  await settles(first, "first");

  const forced = store.read(0);
  assert.equal(f.calls(), 2, "the ⟳ button asked for a read, so it gets one");
  f.pending[1].resolve([entry(1), entry(2)]);
  await settles(forced, "forced");
});

test("a failed read keeps the last good rows, does not throw, and does not latch", async () => {
  const f = deferredFetch();
  const c = clock();
  const store = new AuditStore(f.fetchRows, c.now);

  const first = store.read(0);
  f.pending[0].resolve([entry(1)]);
  await settles(first, "first");
  assert.equal(store.loaded, true);

  const failed = store.read(0);
  f.pending[1].reject(new Error("group id refused"));
  assert.deepEqual((await settles(failed, "failed")).map((e) => e.ts_ms), [1],
    "the log a view is showing survives");
  assert.equal(store.loaded, true);

  // Not latched: "I could not look" must not become "there is nothing new"
  // for the rest of the session, so the very next request re-reads rather
  // than being served the failure's stale stamp.
  const retry = store.read(0);
  assert.equal(f.calls(), 3, "the next request retries");
  f.pending[2].resolve([entry(1), entry(2)]);
  assert.deepEqual((await settles(retry, "retry")).map((e) => e.ts_ms), [1, 2]);
});

test("the three states are distinguishable: not yet read, read failed, read landed", async () => {
  // `loaded` alone is a two-way answer to a three-way question (#1317 review
  // N5), and the timeline reaches the third on every fresh pane: its
  // ResizeObserver renders before the first read lands, so gating the failure
  // message on `!loaded` flashed it at a healthy group.
  const f = deferredFetch();
  const store = new AuditStore(f.fetchRows, clock().now);

  // 1. nothing has been read yet — neither flag is set.
  assert.equal(store.attempted, false, "no read has completed");
  assert.equal(store.loaded, false);

  // 2. a read is IN FLIGHT — still "not yet", because nothing has landed.
  const first = store.read(0);
  assert.equal(store.attempted, false, "an in-flight read has not completed");
  assert.equal(store.loaded, false);

  // 3. it failed — attempted, but not loaded. This is the state that must
  //    render "could not read", and the only one that may.
  f.pending[0].reject(new Error("nope"));
  await settles(first, "first");
  assert.equal(store.attempted, true, "a failure is still an attempt");
  assert.equal(store.loaded, false);

  // 4. a later read succeeds — both true.
  const next = store.read(0);
  f.pending[1].resolve([entry(1)]);
  await settles(next, "next");
  assert.equal(store.attempted, true);
  assert.equal(store.loaded, true);
});

test("a failed FIRST read leaves the store unloaded rather than empty-and-loaded", async () => {
  const f = deferredFetch();
  const store = new AuditStore(f.fetchRows, clock().now);
  const first = store.read(0);
  f.pending[0].reject(new Error("nope"));
  assert.deepEqual(await settles(first, "first"), []);
  assert.equal(store.loaded, false, "a rejected read is not 'we have the log'");

  // …so the window cannot serve it either: an unloaded store always reads.
  const next = store.read();
  assert.equal(f.calls(), 2, "an unloaded store reads even inside the window");
  f.pending[1].resolve([entry(7)]);
  assert.deepEqual((await settles(next, "next")).map((e) => e.ts_ms), [7]);
});
