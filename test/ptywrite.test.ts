// Unit tests for the ordered PTY writer (the paste half of #65). Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createOrderedWriter, chunkForPty, PTY_WRITE_CHUNK } from "../src/ptywrite.ts";

const wait = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

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
  await allDelivered;
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
