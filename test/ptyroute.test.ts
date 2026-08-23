// The per-pty router's retention policy (#1301) — ptyroute.ts.
//
// What these pin is not "the map works". It is the three properties whose
// absence made a many-hour orchestration session run the webview out of heap,
// each of which is invisible to a test of any single call:
//
//   1. An OWNER holds at most one attachment. The leak was a pane that
//      respawned in place, attached under a new pty id, and left the old id's
//      handler — a closure over the pane, its `Terminal` and its whole
//      scrollback — in a module-level map that `Pane.dispose` had no way to
//      name. `attachedCount()` is the number that used to climb.
//   2. A RELEASED id stays released. Bytes arriving a tick after teardown must
//      not open a fresh buffer nobody will ever drain.
//   3. The pre-attach buffer is CAPPED, in bytes per id and in ids held, so an
//      id that never attaches costs a bounded amount rather than the session.
//
// Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  PtyRouter,
  MAX_PREATTACH_BYTES,
  MAX_PREATTACH_IDS,
  MAX_RETIRED_IDS,
} from "../src/ptyroute.ts";

/** A stand-in for a `Pane`: the router only ever needs reference identity. */
const owner = (name: string): object => ({ name });

/** A chunk of `n` bytes. The router is told the size explicitly (pty.ts passes
 *  the decoded `Uint8Array`'s length), so the payload itself is opaque. */
const chunk = (n: number): Uint8Array => new Uint8Array(n);

// ---------- 1. one live attachment per owner ----------

test("attaching an owner to a new id releases the id it held before", () => {
  const r = new PtyRouter<() => void>();
  const pane = owner("pane");
  const first = (): void => {};
  const second = (): void => {};

  r.attach(pane, 1, first);
  r.attach(pane, 2, second);

  // The leak, stated as an assertion: under a flat id-keyed map both handlers
  // survive and id 1's closure keeps the pane alive forever.
  assert.equal(r.attachedCount(), 1);
  assert.equal(r.handler(1), undefined);
  assert.equal(r.handler(2), second);
});

test("a pane that respawns N times still holds exactly one attachment", () => {
  const r = new PtyRouter<() => void>();
  const pane = owner("respawner");
  for (let id = 1; id <= 40; id++) r.attach(pane, id, () => {});
  assert.equal(r.attachedCount(), 1);
  assert.equal(r.handler(40) !== undefined, true);
});

test("releaseOwner tears down whichever id the owner ended up on", () => {
  const r = new PtyRouter<() => void>();
  const pane = owner("pane");
  r.attach(pane, 7, () => {});
  r.attach(pane, 9, () => {});

  // The call `Pane.dispose` makes: it needs no record of the ids used, which
  // is exactly what an id-aimed detach demanded and a respawned pane no
  // longer has.
  assert.equal(r.releaseOwner(pane), 9);
  assert.equal(r.attachedCount(), 0);
  assert.equal(r.releaseOwner(pane), null); // idempotent
});

test("releasing the id an owner has already moved off does not unbind the new one", () => {
  const r = new PtyRouter<() => void>();
  const pane = owner("pane");
  const live = (): void => {};
  r.attach(pane, 1, () => {});
  r.attach(pane, 2, live);

  r.release(1); // a late teardown aimed at the stale id

  assert.equal(r.handler(2), live);
  assert.equal(r.attachedCount(), 1);
});

test("two owners cannot share one id — the newcomer takes it over", () => {
  const r = new PtyRouter<() => void>();
  const a = owner("a");
  const b = owner("b");
  const bHandler = (): void => {};
  r.attach(a, 5, () => {});
  r.attach(b, 5, bHandler);

  assert.equal(r.attachedCount(), 1);
  assert.equal(r.handler(5), bHandler);
  // `a` must be left holding nothing, not holding a binding to an id that now
  // routes elsewhere — otherwise releasing `a` would unbind `b`.
  assert.equal(r.releaseOwner(a), null);
  assert.equal(r.handler(5), bHandler);
});

// ---------- 2. a released id stays released ----------

test("bytes arriving after release are dropped, not re-buffered", () => {
  const r = new PtyRouter<() => void>();
  const pane = owner("pane");
  r.attach(pane, 3, () => {});
  r.release(3);

  const result = r.hold(3, chunk(1024), 1024);

  assert.deepEqual(result, { kind: "drop", reason: "retired" });
  assert.equal(r.heldBytes(3), 0);
  assert.equal(r.heldIds(), 0);
});

test("release drops anything already held for the id", () => {
  const r = new PtyRouter<() => void>();
  r.hold(4, chunk(2048), 2048);
  assert.equal(r.heldBytes(4), 2048);

  r.release(4);

  assert.equal(r.heldBytes(4), 0);
  assert.equal(r.isRetired(4), true);
});

test("the retired ring is bounded, and an id that ages out falls back to the capped buffer", () => {
  const r = new PtyRouter<() => void>();
  r.release(1);
  for (let id = 2; id <= MAX_RETIRED_IDS + 1; id++) r.release(id);

  // Id 1 has aged out of the ring — that is the ring being bounded, which is
  // the whole reason it is a ring. What must NOT happen is unbounded growth
  // on the other side: the fallback is the pre-attach buffer, itself capped.
  assert.equal(r.isRetired(1), false);
  assert.equal(r.hold(1, chunk(8), 8).kind, "hold");
  assert.equal(r.heldBytes(1), 8);
});

// ---------- 3. the pre-attach buffer is capped ----------

test("held bytes for one id never exceed the ceiling", () => {
  const r = new PtyRouter<() => void>();
  const each = 64 * 1024;
  for (let i = 0; i < 40; i++) r.hold(1, chunk(each), each);

  assert.equal(r.heldBytes(1) <= MAX_PREATTACH_BYTES, true);
});

test("overflow sheds the OLDEST chunks and keeps the newest", () => {
  const r = new PtyRouter<() => void>();
  const each = MAX_PREATTACH_BYTES / 2;
  r.hold(1, "first", each);
  r.hold(1, "second", each);
  const third = r.hold(1, "third", each);

  assert.equal(third.kind, "hold");
  assert.equal(third.kind === "hold" && third.shed, 1);
  // A terminal's screen is its most recent bytes: shedding the newest would
  // leave the pane replaying a stale frame forever.
  assert.deepEqual(r.takeHeld<string>(1), ["second", "third"]);
});

test("a single chunk larger than the ceiling is refused outright", () => {
  const r = new PtyRouter<() => void>();
  const huge = MAX_PREATTACH_BYTES + 1;

  assert.deepEqual(r.hold(1, chunk(huge), huge), { kind: "drop", reason: "oversize" });
  assert.equal(r.heldIds(), 0);
});

test("the number of ids holding a buffer is capped, oldest evicted first", () => {
  const r = new PtyRouter<() => void>();
  for (let id = 1; id <= MAX_PREATTACH_IDS; id++) r.hold(id, chunk(8), 8);
  assert.equal(r.heldIds(), MAX_PREATTACH_IDS);

  r.hold(MAX_PREATTACH_IDS + 1, chunk(8), 8);

  assert.equal(r.heldIds(), MAX_PREATTACH_IDS);
  assert.equal(r.heldBytes(1), 0); // the oldest waiter went
  assert.equal(r.heldBytes(MAX_PREATTACH_IDS + 1), 8); // the newest is kept
});

test("a session of spawns nobody ever attaches stays inside the stated worst case", () => {
  const r = new PtyRouter<() => void>();
  const each = 32 * 1024;
  // 500 abandoned spawns, each spraying 1 MiB — the #1301 shape, where pane
  // creation kept timing out and the ptys behind it kept printing.
  for (let id = 1; id <= 500; id++) {
    for (let i = 0; i < 32; i++) r.hold(id, chunk(each), each);
  }
  let total = 0;
  for (let id = 1; id <= 500; id++) total += r.heldBytes(id);

  assert.equal(r.heldIds() <= MAX_PREATTACH_IDS, true);
  assert.equal(total <= MAX_PREATTACH_IDS * MAX_PREATTACH_BYTES, true);
});

// ---------- the lossless-startup guarantee this must not break ----------

test("everything held before an attach is handed over, in arrival order, once", () => {
  const r = new PtyRouter<() => void>();
  const pane = owner("pane");
  r.hold(1, "a", 1);
  r.hold(1, "b", 1);

  r.attach(pane, 1, () => {});

  assert.deepEqual(r.takeHeld<string>(1), ["a", "b"]);
  assert.deepEqual(r.takeHeld<string>(1), []); // destructive: never replayed twice
});

test("an attached id delivers rather than holding", () => {
  const r = new PtyRouter<() => void>();
  const pane = owner("pane");
  const handler = (): void => {};
  r.attach(pane, 1, handler);

  assert.equal(r.handler(1), handler);
  assert.equal(r.heldBytes(1), 0);
});
