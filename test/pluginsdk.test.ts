// Unit tests for the pane-plugins client SDK (#360 Slice G — templates/loomux-plugin/sdk/plugin-sdk.js).
// DOM-free: the pure envelope/response helpers and the EventRouter need no
// window at all, and createPluginClient takes its Tauri `internals` object as
// an injected dependency (real plugin code omits it and falls back to
// `window.__TAURI_INTERNALS__` — see the module doc comment) so the request/
// event wiring is testable without a live child webview.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildRequestEnvelope,
  unwrapResponse,
  PluginBrokerError,
  EventRouter,
  createPluginClient,
} from "../templates/loomux-plugin/sdk/plugin-sdk.js";

test("buildRequestEnvelope builds the wire envelope shape", () => {
  const env = buildRequestEnvelope("req-1", "storage.get", { key: "x" }, 1);
  assert.deepEqual(env, {
    type: "request",
    id: "req-1",
    apiVersion: 1,
    method: "storage.get",
    params: { key: "x" },
  });
});

test("buildRequestEnvelope defaults missing params to null", () => {
  const env = buildRequestEnvelope("req-2", "metrics.unsubscribe", undefined, 1);
  assert.equal(env.params, null);
});

test("unwrapResponse returns the result on ok:true", () => {
  assert.equal(unwrapResponse({ type: "response", id: "1", ok: true, result: 42 }), 42);
});

test("unwrapResponse throws PluginBrokerError carrying the broker's error code on ok:false", () => {
  assert.throws(
    () =>
      unwrapResponse({
        type: "response",
        id: "1",
        ok: false,
        error: { code: "capability-denied", message: "nope" },
      }),
    (err: unknown) =>
      err instanceof PluginBrokerError && err.code === "capability-denied" && err.message === "nope",
  );
});

test("EventRouter dispatches in-order messages only to listeners of the matching event name", () => {
  const router = new EventRouter();
  const ticks: unknown[] = [];
  const themes: unknown[] = [];
  router.on("metrics.tick", (payload: unknown) => ticks.push(payload));
  router.on("theme", (payload: unknown) => themes.push(payload));
  router.handleRaw({ index: 0, message: { event: "metrics.tick", payload: 1 } });
  router.handleRaw({ index: 1, message: { event: "theme", payload: "dark" } });
  assert.deepEqual(ticks, [1]);
  assert.deepEqual(themes, ["dark"]);
});

test("EventRouter replays an out-of-order message once the missing index arrives", () => {
  const router = new EventRouter();
  const seen: unknown[] = [];
  router.on("metrics.tick", (payload: unknown) => seen.push(payload));
  router.handleRaw({ index: 1, message: { event: "metrics.tick", payload: "b" } });
  assert.deepEqual(seen, [], "message 1 arrived before message 0 — must be buffered, not dispatched");
  router.handleRaw({ index: 0, message: { event: "metrics.tick", payload: "a" } });
  assert.deepEqual(seen, ["a", "b"], "message 0 arriving should flush the buffered message 1 right after it");
});

test("EventRouter.on's returned unsubscribe stops further dispatch to that listener", () => {
  const router = new EventRouter();
  const seen: unknown[] = [];
  const off = router.on("theme", (payload: unknown) => seen.push(payload));
  router.handleRaw({ index: 0, message: { event: "theme", payload: "dark" } });
  off();
  router.handleRaw({ index: 1, message: { event: "theme", payload: "light" } });
  assert.deepEqual(seen, ["dark"]);
});

test("createPluginClient.request round-trips through the injected invoke and unwraps ok:true", async () => {
  const calls: Array<[string, unknown]> = [];
  const fakeInternals = {
    invoke: async (cmd: string, args: unknown) => {
      calls.push([cmd, args]);
      const a = args as { request: { method: string; id: string } };
      assert.equal(cmd, "plugin_broker_request");
      assert.equal(a.request.method, "storage.get");
      return { type: "response", id: a.request.id, ok: true, result: "hello" };
    },
    transformCallback: () => {
      throw new Error("should not open a channel for a plain request");
    },
  };
  const client = createPluginClient({ apiVersion: 1, internals: fakeInternals });
  const result = await client.request("storage.get", { key: "greeting" });
  assert.equal(result, "hello");
  assert.equal(calls.length, 1);
});

test("createPluginClient.request rejects with PluginBrokerError when the broker denies it", async () => {
  const fakeInternals = {
    invoke: async () => ({
      type: "response",
      id: "x",
      ok: false,
      error: { code: "capability-denied", message: "nope" },
    }),
    transformCallback: () => 0,
  };
  const client = createPluginClient({ apiVersion: 1, internals: fakeInternals });
  await assert.rejects(
    () => client.request("fs.read", { path: "a.txt" }),
    (err: unknown) => err instanceof PluginBrokerError && err.code === "capability-denied",
  );
});

test("createPluginClient.onEvent opens the broker channel exactly once and routes ticks by event name", async () => {
  let opened = 0;
  let rawCallback: ((raw: unknown) => void) | null = null;
  const fakeInternals = {
    invoke: async (cmd: string, args: unknown) => {
      if (cmd === "plugin_broker_open_channel") {
        opened += 1;
        return null;
      }
      if (cmd === "plugin_broker_request") {
        const a = args as { request: { id: string } };
        return { type: "response", id: a.request.id, ok: true, result: null };
      }
      throw new Error(`unexpected command ${cmd}`);
    },
    transformCallback: (cb: (raw: unknown) => void) => {
      rawCallback = cb;
      return 1;
    },
  };
  const client = createPluginClient({ apiVersion: 1, internals: fakeInternals });
  const ticks: unknown[] = [];
  client.onEvent("metrics.tick", (payload: unknown) => ticks.push(payload));
  client.onEvent("metrics.tick", () => {}); // a second listener must not open a second channel
  await client.request("metrics.subscribe", {});
  assert.equal(opened, 1);
  assert.ok(rawCallback);
  (rawCallback as (raw: unknown) => void)({ index: 0, message: { event: "metrics.tick", payload: { cpuPercent: 12 } } });
  assert.deepEqual(ticks, [{ cpuPercent: 12 }]);
});
