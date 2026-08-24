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
  assert.deepEqual((await a).map((e) => e.ts_ms), [1, 2]);
  assert.deepEqual((await b).map((e) => e.ts_ms), [1, 2], "and it gets the joined run's answer");
  assert.equal(f.calls(), 1);
});

test("both readers get the SAME array — the de-duplication, not two copies", async () => {
  const f = deferredFetch();
  const store = new AuditStore(f.fetchRows, clock().now);
  const a = store.read(0);
  const b = store.read(0);
  const rows = [entry(1)];
  f.pending[0].resolve(rows);
  const [ra, rb] = [await a, await b];
  assert.equal(ra, rb, "one array, handed to both");
  assert.equal(ra, store.cached);
});

test("a read younger than the window is served without touching IPC", async () => {
  const f = deferredFetch();
  const c = clock();
  const store = new AuditStore(f.fetchRows, c.now);

  const first = store.read(0);
  f.pending[0].resolve([entry(1)]);
  await first;
  assert.equal(f.calls(), 1);

  c.advance(AUDIT_READ_MAX_AGE_MS - 1);
  assert.deepEqual((await store.read()).map((e) => e.ts_ms), [1]);
  assert.equal(f.calls(), 1, "inside the window: the other view's tick is free");

  // POSITIVE CONTROL: the window is a bound, not a cache-forever switch — one
  // more millisecond and it reads again.
  c.advance(1);
  const again = store.read();
  assert.equal(f.calls(), 2, "past the window it re-reads");
  f.pending[1].resolve([entry(1), entry(2)]);
  assert.deepEqual((await again).map((e) => e.ts_ms), [1, 2]);
});

test("an explicit gesture passes 0 and is never served a cached answer", async () => {
  const f = deferredFetch();
  const c = clock();
  const store = new AuditStore(f.fetchRows, c.now);
  const first = store.read(0);
  f.pending[0].resolve([entry(1)]);
  await first;

  const forced = store.read(0);
  assert.equal(f.calls(), 2, "the ⟳ button asked for a read, so it gets one");
  f.pending[1].resolve([entry(1), entry(2)]);
  await forced;
});

test("a failed read keeps the last good rows, does not throw, and does not latch", async () => {
  const f = deferredFetch();
  const c = clock();
  const store = new AuditStore(f.fetchRows, c.now);

  const first = store.read(0);
  f.pending[0].resolve([entry(1)]);
  await first;
  assert.equal(store.loaded, true);

  const failed = store.read(0);
  f.pending[1].reject(new Error("group id refused"));
  assert.deepEqual((await failed).map((e) => e.ts_ms), [1], "the log a view is showing survives");
  assert.equal(store.loaded, true);

  // Not latched: "I could not look" must not become "there is nothing new"
  // for the rest of the session, so the very next request re-reads rather
  // than being served the failure's stale stamp.
  const retry = store.read(0);
  assert.equal(f.calls(), 3, "the next request retries");
  f.pending[2].resolve([entry(1), entry(2)]);
  assert.deepEqual((await retry).map((e) => e.ts_ms), [1, 2]);
});

test("a failed FIRST read leaves the store unloaded rather than empty-and-loaded", async () => {
  const f = deferredFetch();
  const store = new AuditStore(f.fetchRows, clock().now);
  const first = store.read(0);
  f.pending[0].reject(new Error("nope"));
  assert.deepEqual(await first, []);
  assert.equal(store.loaded, false, "a rejected read is not 'we have the log'");

  // …so the window cannot serve it either: an unloaded store always reads.
  const next = store.read();
  assert.equal(f.calls(), 2);
  f.pending[1].resolve([entry(7)]);
  assert.deepEqual((await next).map((e) => e.ts_ms), [7]);
});
