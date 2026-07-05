// Unit tests for OSC 52 payload parsing (the copy half of #65). Run with
// `npm test`. Node strips the TypeScript types natively, so no test framework
// is pulled into the build.
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseOsc52, OSC52_MAX_B64_LEN } from "../src/clipboard.ts";

/** Build the `<Pc>;<Pd>` payload xterm hands an OSC 52 handler, base64-ing the
 *  UTF-8 of `text` exactly as a real emitter would. */
function osc52(text: string, pc = "c"): string {
  return `${pc};${Buffer.from(text, "utf8").toString("base64")}`;
}

test("decodes a clipboard write to its text", () => {
  assert.deepEqual(parseOsc52(osc52("hello world")), { ok: true, text: "hello world" });
});

test("survives a round-trip of non-ASCII (UTF-8) text", () => {
  const text = "café — 日本語 — 🚀";
  assert.deepEqual(parseOsc52(osc52(text)), { ok: true, text });
});

test("an empty Pc still copies (default clipboard selection)", () => {
  assert.deepEqual(parseOsc52(osc52("x", "")), { ok: true, text: "x" });
});

test("a read request (?) is ignored, not treated as text", () => {
  assert.deepEqual(parseOsc52("c;?"), { ok: false, reason: "ignore" });
});

test("an empty payload is ignored", () => {
  assert.deepEqual(parseOsc52("c;"), { ok: false, reason: "ignore" });
  assert.deepEqual(parseOsc52("c"), { ok: false, reason: "ignore" }); // no Pc;Pd separator
});

test("malformed base64 is ignored rather than yielding garbage", () => {
  // '@' is outside the base64 alphabet; atob throws and we swallow it.
  assert.deepEqual(parseOsc52("c;@@@not-base64@@@"), { ok: false, reason: "ignore" });
});

test("an oversized payload is refused before decode, not silently ignored", () => {
  // One byte past the cap: distinct 'oversize' reason so the UI can surface it,
  // and (crucially) we never call atob on the giant string.
  const huge = "c;" + "A".repeat(OSC52_MAX_B64_LEN + 1);
  assert.deepEqual(parseOsc52(huge), { ok: false, reason: "oversize" });
  // Exactly at the cap still decodes (valid base64 of N..N).
  const atCap = "c;" + "A".repeat(OSC52_MAX_B64_LEN);
  assert.equal(parseOsc52(atCap).ok, true);
});
