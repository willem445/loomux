// Unit tests for the pane-plugins broker's pure contract (#360 Slice C):
// envelope parsing and the capability/apiVersion check. Run with `npm test`.
// Mirrors src-tauri/src/pluginbroker.rs's Rust-side tests one-for-one so both
// halves of the contract stay provably in sync.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  parsePluginRequest,
  checkCapability,
  errorResponse,
  okResponse,
  isPathWithinJail,
  type PluginCapability,
} from "../src/pluginprotocol.ts";

test("parsePluginRequest accepts a well-formed request envelope", () => {
  const parsed = parsePluginRequest({
    type: "request",
    id: "1",
    apiVersion: 1,
    method: "storage.get",
    params: { key: "x" },
  });
  assert.ok(parsed);
  assert.equal(parsed?.method, "storage.get");
});

test("parsePluginRequest rejects a wrong type discriminant", () => {
  assert.equal(
    parsePluginRequest({ type: "response", id: "1", apiVersion: 1, method: "storage.get" }),
    null,
  );
});

test("parsePluginRequest rejects missing/malformed fields", () => {
  assert.equal(parsePluginRequest(null), null);
  assert.equal(parsePluginRequest("not an object"), null);
  assert.equal(parsePluginRequest({ type: "request" }), null);
  assert.equal(
    parsePluginRequest({ type: "request", id: "", apiVersion: 1, method: "storage.get" }),
    null,
    "an empty id is not a valid correlation id",
  );
  assert.equal(
    parsePluginRequest({ type: "request", id: "1", apiVersion: 0, method: "storage.get" }),
    null,
    "apiVersion must be a positive integer",
  );
  assert.equal(
    parsePluginRequest({ type: "request", id: "1", apiVersion: 1.5, method: "storage.get" }),
    null,
  );
  assert.equal(
    parsePluginRequest({ type: "request", id: "1", apiVersion: 1, method: "" }),
    null,
  );
});

function granted(...caps: PluginCapability[]): Set<PluginCapability> {
  return new Set(caps);
}

test("checkCapability allows a granted capability at a matching apiVersion", () => {
  const err = checkCapability(granted("storage"), 1, { method: "storage.get", apiVersion: 1 });
  assert.equal(err, null);
});

test("checkCapability denies an ungranted capability", () => {
  const err = checkCapability(granted("fs.read"), 1, { method: "storage.get", apiVersion: 1 });
  assert.equal(err?.code, "capability-denied");
});

test("checkCapability rejects an unknown method as bad-request", () => {
  const err = checkCapability(granted("storage", "fs.read", "metrics.system"), 1, {
    method: "git.push",
    apiVersion: 1,
  });
  assert.equal(err?.code, "bad-request");
});

test("checkCapability rejects a method newer than the plugin's declared apiVersion", () => {
  const err = checkCapability(granted("storage"), 0, { method: "storage.get", apiVersion: 1 });
  assert.equal(err?.code, "unsupported-version");
});

test("checkCapability rejects a request that overclaims its own apiVersion", () => {
  // A message claiming a higher apiVersion than the plugin's registered
  // (authoritative) version must not unlock a newer method early.
  const err = checkCapability(granted("storage"), 1, { method: "storage.get", apiVersion: 2 });
  assert.equal(err?.code, "unsupported-version");
});

test("errorResponse/okResponse shape the response envelope correctly", () => {
  const ok = okResponse("42", { hello: "world" });
  assert.deepEqual(ok, { type: "response", id: "42", ok: true, result: { hello: "world" } });

  const err = errorResponse("42", { code: "capability-denied", message: "nope" });
  assert.deepEqual(err, {
    type: "response",
    id: "42",
    ok: false,
    error: { code: "capability-denied", message: "nope" },
  });
});

test("isPathWithinJail allows plain relative paths and harmless dot segments", () => {
  assert.equal(isPathWithinJail("index.html"), true);
  assert.equal(isPathWithinJail("./index.html"), true);
  assert.equal(isPathWithinJail("assets/img/logo.png"), true);
  assert.equal(isPathWithinJail(""), true);
  assert.equal(isPathWithinJail("a/../b"), true, "net depth stays inside the root");
});

test("isPathWithinJail denies traversal and absolute paths", () => {
  assert.equal(isPathWithinJail("../secret.txt"), false);
  assert.equal(isPathWithinJail("a/../../secret.txt"), false);
  assert.equal(isPathWithinJail("/etc/passwd"), false);
  assert.equal(isPathWithinJail("C:\\Windows\\System32"), false);
});
