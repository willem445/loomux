// Unit tests for OSC 52 payload parsing (the copy half of #65). Run with
// `npm test`. Node strips the TypeScript types natively, so no test framework
// is pulled into the build.
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseOsc52 } from "../src/clipboard.ts";

/** Build the `<Pc>;<Pd>` payload xterm hands an OSC 52 handler, base64-ing the
 *  UTF-8 of `text` exactly as a real emitter would. */
function osc52(text: string, pc = "c"): string {
  return `${pc};${Buffer.from(text, "utf8").toString("base64")}`;
}

test("decodes a clipboard write to its text", () => {
  assert.equal(parseOsc52(osc52("hello world")), "hello world");
});

test("survives a round-trip of non-ASCII (UTF-8) text", () => {
  const text = "café — 日本語 — 🚀";
  assert.equal(parseOsc52(osc52(text)), text);
});

test("an empty Pc still copies (default clipboard selection)", () => {
  assert.equal(parseOsc52(osc52("x", "")), "x");
});

test("a read request (?) is ignored, not treated as text", () => {
  assert.equal(parseOsc52("c;?"), null);
});

test("an empty payload is ignored", () => {
  assert.equal(parseOsc52("c;"), null);
  assert.equal(parseOsc52("c"), null); // no Pc;Pd separator at all
});

test("malformed base64 yields null rather than garbage", () => {
  // '@' is outside the base64 alphabet; atob throws and we swallow it.
  assert.equal(parseOsc52("c;@@@not-base64@@@"), null);
});
