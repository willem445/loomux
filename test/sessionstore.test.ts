// Unit tests for the shared session list (src/sessionstore.ts) — the #493 fix
// for "startup scans 826 session files TWICE". The scan itself is real disk I/O
// in the backend; what this pins is the CONCURRENCY CONTRACT around it, counted
// rather than timed: how many scans a given sequence of callers can produce.
//
// The bug, from the breadcrumb log: the sidebar's boot prefetch started one
// scan, and ~4s later a group-restore click called `listSessions()` directly and
// started a SECOND, concurrent one on the same files (12.9s + 16.7s, contending)
// — then waited on it. So the property under test is "no caller can start a scan
// that a completed or in-flight scan could have answered", which is exactly a
// fetch count. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { SessionStore } from "../src/sessionstore.ts";
import type { SessionInfo } from "../src/pty.ts";

const row = (id: string): SessionInfo => ({
  id,
  source: "claude",
  title: `title ${id}`,
  cwd: "C:/work",
  modified_ms: 1,
  resume_command: `claude --resume ${id}`,
  orch_role: null,
  orch_group: null,
});

/** A fake scan that counts calls and can be resolved by the test, so "while one
 *  is in flight" is a deterministic state rather than a timing race. */
function fakeScan(rows: SessionInfo[] = [row("a")]) {
  let calls = 0;
  let release: (() => void) | null = null;
  const fetch = (): Promise<SessionInfo[]> => {
    calls += 1;
    return new Promise((resolve) => {
      release = () => resolve(rows);
    });
  };
  return {
    fetch,
    get calls() {
      return calls;
    },
    /** Let the in-flight scan finish and drain the microtask queue. */
    async finish() {
      assert.ok(release, "no scan in flight to finish");
      const r = release;
      release = null;
      r();
      await Promise.resolve();
      await Promise.resolve();
    },
  };
}

test("ensureLoaded() joins the scan already in flight instead of starting a second", async () => {
  const scan = fakeScan([row("a"), row("b")]);
  const store = new SessionStore(scan.fetch);

  // The boot prefetch, unawaited — exactly how main.ts starts it.
  const prefetch = store.refresh();
  assert.equal(scan.calls, 1);

  // The group-restore click, arriving mid-prefetch. THIS is the line that used
  // to be a bare `listSessions()`.
  const restore = store.ensureLoaded();
  assert.equal(scan.calls, 1, "a second concurrent scan is the #493 bug");

  await scan.finish();
  const rows = await restore;
  await prefetch;
  assert.equal(scan.calls, 1, "one scan answered both callers");
  assert.deepEqual(
    rows.map((r) => r.id),
    ["a", "b"],
    "the joining caller gets the prefetch's rows, not an empty list"
  );
});

test("ensureLoaded() after a completed scan does not scan again", async () => {
  const scan = fakeScan([row("a")]);
  const store = new SessionStore(scan.fetch);

  const first = store.refresh();
  await scan.finish();
  await first;
  assert.equal(scan.calls, 1);
  assert.equal(store.loaded, true);

  const rows = await store.ensureLoaded();
  assert.equal(scan.calls, 1, "the cached list must answer it");
  assert.deepEqual(
    rows.map((r) => r.id),
    ["a"]
  );
});

test("any number of concurrent ensureLoaded() callers produce exactly one scan", async () => {
  const scan = fakeScan();
  const store = new SessionStore(scan.fetch);

  const all = Promise.all([store.ensureLoaded(), store.ensureLoaded(), store.ensureLoaded()]);
  assert.equal(scan.calls, 1, "the first starts the scan, the rest join it");

  await scan.finish();
  const results = await all;
  assert.equal(scan.calls, 1);
  for (const r of results) assert.deepEqual(r.map((x) => x.id), ["a"]);
});

test("concurrent refresh() callers join one scan; a later one still re-reads", async () => {
  // The two halves of refresh()'s contract. Overlapping callers must never
  // multiply the scan (that's #493) — but refresh() is still the accessor that
  // carries freshness, so a call arriving AFTER a scan finished must re-read
  // rather than serve the cache (that's what the ↻ button and the #440
  // reconciler need it for).
  const scan = fakeScan();
  const store = new SessionStore(scan.fetch);

  const a = store.refresh();
  const b = store.refresh();
  const c = store.refresh();
  assert.equal(scan.calls, 1, "three overlapping callers, one scan");
  await scan.finish();
  await Promise.all([a, b, c]);
  assert.equal(scan.calls, 1);

  const later = store.refresh();
  assert.equal(scan.calls, 2, "a call after the scan settled re-reads");
  await scan.finish();
  await later;
});

test("a failed scan is not remembered as loaded, and rejects its caller", async () => {
  // Why this matters, stated as the code actually behaves (rev-48 NB-1): main.ts's
  // `seenAny` guard treats a successful EMPTY list and a rejection alike — both
  // mean "assume every captured id is resumable" — so this is NOT about the caller
  // telling empty from error. It is that a failure must not be recorded as a
  // successful empty load: `loadedOnce` would latch, every later `ensureLoaded()`
  // would serve `[]` without rescanning, and one transient failure would strand
  // the sidebar empty and turn the resumability check into a permanent no-op.
  let calls = 0;
  const store = new SessionStore(() => {
    calls += 1;
    return calls === 1 ? Promise.reject(new Error("scan blew up")) : Promise.resolve([row("a")]);
  });

  await assert.rejects(() => store.ensureLoaded(), /scan blew up/);
  assert.equal(store.loaded, false, "a failed scan must not count as loaded");
  assert.deepEqual([...store.cached], [], "…and must leave the cache empty");

  const rows = await store.ensureLoaded();
  assert.equal(calls, 2, "a later caller retries rather than inheriting the failure");
  assert.deepEqual(
    rows.map((r) => r.id),
    ["a"]
  );
});

test("refresh() replaces the cached rows, so a stale list can't outlive a rescan", async () => {
  let calls = 0;
  const store = new SessionStore(() => {
    calls += 1;
    return Promise.resolve(calls === 1 ? [row("old")] : [row("new")]);
  });

  await store.refresh();
  assert.deepEqual([...store.cached].map((r) => r.id), ["old"]);

  await store.refresh();
  assert.deepEqual([...store.cached].map((r) => r.id), ["new"]);
  // ensureLoaded serves the NEWEST completed read, which is what makes it safe
  // for the resumability check: a transcript the newest scan saw exists.
  const rows = await store.ensureLoaded();
  assert.deepEqual(rows.map((r) => r.id), ["new"]);
  assert.equal(calls, 2, "ensureLoaded added no scan of its own");
});
