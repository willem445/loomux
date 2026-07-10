// The restore-vs-fresh-vs-ask decision matrix (#194). Pure — restoredecision.ts.
import { test } from "node:test";
import assert from "node:assert/strict";
import { decideRestore } from "../src/restoredecision.ts";

test("with no snapshot, every preference goes fresh (never prompt over nothing)", () => {
  assert.equal(decideRestore("ask", false), "fresh");
  assert.equal(decideRestore("restore", false), "fresh");
  assert.equal(decideRestore("fresh", false), "fresh");
});

test("with a snapshot, the remembered preference decides", () => {
  assert.equal(decideRestore("restore", true), "restore", "remembered restore → silent restore");
  assert.equal(decideRestore("fresh", true), "fresh", "remembered fresh → silent fresh");
  assert.equal(decideRestore("ask", true), "prompt", "first-run ask → show the splash");
});
