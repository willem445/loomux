// Unit tests for the ordered PTY writer (the paste half of #65). Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createOrderedWriter, chunkForPty, PTY_WRITE_CHUNK } from "../src/ptywrite.ts";

const wait = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** A `Promise.race` backstop against a condition-wait that could otherwise
 *  hang forever if the condition never becomes true — not a return to
 *  wall-clock guessing: the race still waits on the real condition first,
 *  this just bounds how long a genuine regression gets to hang the suite
 *  before the assertion below gets to run and fail with an actual diff
 *  (#232 review). `cancel()` clears the underlying timer on the happy path
 *  so a passing test doesn't hold the process open for the full `ms`.  */
function timeoutAfter(ms: number): { promise: Promise<void>; cancel: () => void } {
  let id: ReturnType<typeof setTimeout>;
  const promise = new Promise<void>((r) => {
    id = setTimeout(r, ms);
  });
  return { promise, cancel: () => clearTimeout(id) };
}

test("delivers writes in FIFO order even when an early send resolves last", async () => {
  const seen: string[] = [];
  const w = createOrderedWriter();
  // First send is the slowest: if writes ran concurrently, "A" would land
  // last. The chain must still deliver A, B, C in order.
  const delays: Record<string, number> = { A: 30, B: 5, C: 1 };
  // Wait on the observable condition (all three delivered) rather than a
  // fixed sleep: under load, real timers can run well past a guessed
  // duration and the test would assert before "C" ever lands (#232).
  let done: () => void;
  const allDelivered = new Promise<void>((r) => (done = r));
  w.ready(async (data) => {
    await wait(delays[data] ?? 0);
    seen.push(data);
    if (seen.length === 3) done();
  });
  w.write("A");
  w.write("B");
  w.write("C");
  // Generous (5s vs. ~36ms of real work): a dropped write now fails fast
  // with an assertion diff instead of hanging the suite indefinitely.
  const backstop = timeoutAfter(5000);
  await Promise.race([allDelivered, backstop.promise]);
  backstop.cancel();
  assert.deepEqual(seen, ["A", "B", "C"]);
});

test("buffers input produced before the PTY is ready, then flushes in order", async () => {
  const seen: string[] = [];
  const w = createOrderedWriter();
  w.write("typed-1");
  w.write("typed-2");
  assert.equal(w.pendingCount, 2, "both pre-ready writes are buffered");
  w.ready(async (data) => {
    seen.push(data);
  });
  assert.equal(w.pendingCount, 0, "buffer drained on ready");
  w.write("typed-3");
  await wait(10);
  assert.deepEqual(seen, ["typed-1", "typed-2", "typed-3"]);
});

test("a single send failure never stalls or drops later writes", async () => {
  const seen: string[] = [];
  const w = createOrderedWriter();
  w.ready(async (data) => {
    if (data === "B") throw new Error("backend blip");
    seen.push(data);
  });
  w.write("A");
  w.write("B"); // rejects
  w.write("C");
  await wait(10);
  assert.deepEqual(seen, ["A", "C"], "B is dropped but A and C still deliver in order");
});

test("chunkForPty leaves small writes as a single piece", () => {
  assert.deepEqual(chunkForPty("hi"), ["hi"]);
  assert.deepEqual(chunkForPty("x".repeat(PTY_WRITE_CHUNK)), ["x".repeat(PTY_WRITE_CHUNK)]);
});

test("chunkForPty splits large writes into bounded pieces that rejoin exactly", () => {
  const big = "abcdefghij".repeat(1000); // 10_000 chars
  const parts = chunkForPty(big, 4096);
  assert.ok(parts.length === 3, `expected 3 chunks, got ${parts.length}`);
  assert.ok(parts.every((p) => p.length <= 4096));
  assert.equal(parts.join(""), big);
});

test("chunkForPty never splits a surrogate pair", () => {
  // A rocket emoji is one astral code point = two UTF-16 units (a surrogate
  // pair). With max=3, a naive slice would cut it in half and corrupt it.
  const s = "ab🚀cd🚀ef"; // each 🚀 is 2 units
  const parts = chunkForPty(s, 3);
  for (const p of parts) {
    // No chunk may end on a lone high surrogate.
    const last = p.charCodeAt(p.length - 1);
    assert.ok(!(last >= 0xd800 && last <= 0xdbff), `chunk "${p}" ends on a high surrogate`);
  }
  assert.equal(parts.join(""), s);
});

test("a large paste is delivered as ordered chunks", async () => {
  const seen: string[] = [];
  const w = createOrderedWriter(4);
  w.ready(async (data) => {
    seen.push(data);
  });
  w.write("hello world"); // 11 chars → chunks of 4
  await wait(10);
  assert.equal(seen.join(""), "hello world");
  assert.ok(seen.length > 1, "was actually chunked");
  assert.ok(seen.every((c) => c.length <= 4));
});

// ---------- #518: the origin bit rides WITH the data ----------

test("the origin bit is captured at write time, not read at send time", async () => {
  // The whole reason the flag is a parameter rather than something the sender
  // reads for itself: sends are asynchronous and land turns later, long after
  // the key event's latch has closed. If the writer looked the origin up when
  // it sent, every human keystroke would arrive at the backend marked
  // non-human — the fix would be worse than the bug.
  const seen: { data: string; human: boolean }[] = [];
  const w = createOrderedWriter();
  let resolveFirst: () => void = () => {};
  const firstSent = new Promise<void>((r) => (resolveFirst = r));
  w.ready(async (data, human) => {
    await wait(5);
    seen.push({ data, human });
    if (seen.length === 2) resolveFirst();
  });

  w.write("typed", true); // a keystroke
  w.write("\x1b[?62;1;2c", false); // a device-attributes auto-reply

  const backstop = timeoutAfter(5000);
  await Promise.race([firstSent, backstop.promise]);
  backstop.cancel();
  assert.deepEqual(seen, [
    { data: "typed", human: true },
    { data: "\x1b[?62;1;2c", human: false },
  ]);
});

test("every chunk of one paste inherits that paste's origin", async () => {
  // A paste is one human act. Chunking is an internal detail of ConPTY's small
  // input pipe, and it must not make chunks 2..n look like they came from
  // somewhere else.
  const seen: boolean[] = [];
  const w = createOrderedWriter(4);
  w.ready(async (_data, human) => {
    seen.push(human);
  });

  w.write("hello world", true);

  await wait(10);
  assert.ok(seen.length > 1, "was actually chunked");
  assert.ok(
    seen.every((h) => h),
    "a chunked human paste must be human all the way through",
  );
});

test("origin survives the pre-ready buffer", async () => {
  // Input typed while the PTY is still starting is buffered and flushed later.
  // The flush happens in a completely different turn, so the origin has to
  // have been stored with the data — there is nothing left to read it from.
  const seen: { data: string; human: boolean }[] = [];
  const w = createOrderedWriter();
  w.write("typed-early", true);
  w.write("\x1b[I", false); // a focus report from the same startup window
  assert.equal(w.pendingCount, 2);

  w.ready(async (data, human) => {
    seen.push({ data, human });
  });

  await wait(10);
  assert.deepEqual(seen, [
    { data: "typed-early", human: true },
    { data: "\x1b[I", human: false },
  ]);
});

test("a write that says nothing about origin defaults to human", async () => {
  // The fail-safe direction, matching the backend command's own default:
  // believing a human typed only ever makes delivery hold MORE, so an
  // un-updated call site degrades to the pre-#518 behaviour rather than
  // silently switching the guard off.
  const seen: boolean[] = [];
  const w = createOrderedWriter();
  w.ready(async (_data, human) => {
    seen.push(human);
  });

  w.write("no origin stated");

  await wait(10);
  assert.deepEqual(seen, [true]);
});
