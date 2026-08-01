// Zero-dependency client SDK for a loomux pane plugin (#360 Slice G — see
// doc/design/pane-plugins.md for the broker contract and
// docs/features/pane-plugins.md for the full authoring guide). Copy this
// file as-is into your own plugin; it has no imports and nothing to build.
//
// WHY THIS EXISTS RATHER THAN `import { invoke } from "@tauri-apps/api/core"`:
// a plugin has no build step and no npm (doc/design/pane-plugins.md: "there
// is no build step, no compilation, no fetch from anywhere" — a plugin folder
// is copied byte-for-byte on install), so it cannot bundle that package. What
// every plugin `WebviewWindow` DOES get, regardless of its ACL grant, is
// `window.__TAURI_INTERNALS__` — the same low-level bridge
// `@tauri-apps/api/core`'s own `invoke`/`Channel` wrap (checked against this
// repo's own installed `@tauri-apps/api@2.6.0`: its `invoke` is literally
// `window.__TAURI_INTERNALS__.invoke(cmd, args)`, and its `Channel` registers
// a callback via `transformCallback` and serializes to the string
// `` `__CHANNEL__:${id}` `` for the Rust side's `tauri::ipc::Channel<T>` to
// recognize). This module re-implements just those two pieces, faithfully
// enough to interoperate on the wire, so a plugin author never has to.
//
// Isolation does NOT depend on this file being used, or used correctly: the
// broker's own capability/apiVersion check (`pluginbroker.rs::check_request`)
// is the real enforcement point, re-run host-side on every request regardless
// of what sent it. This SDK is convenience, not a trust boundary.

const CHANNEL_MARKER_PREFIX = "__CHANNEL__:";

/** Builds a `PluginRequest` envelope (see `src/pluginprotocol.ts`). Exported
 *  for unit testing; `createPluginClient` is the API a plugin actually calls. */
export function buildRequestEnvelope(id, method, params, apiVersion) {
  return { type: "request", id, apiVersion, method, params: params === undefined ? null : params };
}

/** Thrown by `request()` when the broker replies `ok: false`. Carries the
 *  same stable `error.code` (`capability-denied`, `bad-request`,
 *  `unsupported-version`, `not-found`, …) the design note's error-surface
 *  contract promises, so a plugin author can branch on `err.code` without
 *  parsing prose. */
export class PluginBrokerError extends Error {
  constructor(error) {
    super(error && error.message ? error.message : "plugin broker request failed");
    this.name = "PluginBrokerError";
    this.code = (error && error.code) || "unknown";
  }
}

/** Unwraps a `PluginResponse` (see `src/pluginprotocol.ts`): the result on
 *  `ok: true`, or throws `PluginBrokerError` on `ok: false`. Exported for
 *  unit testing. */
export function unwrapResponse(response) {
  if (!response || response.ok !== true) {
    throw new PluginBrokerError(response ? response.error : undefined);
  }
  return response.result;
}

/** Routes `PluginEvent`s (`{event, payload}`) delivered over the one broker
 *  channel a plugin window opens, keyed by event name (`metrics.tick` today;
 *  `resize`/`theme` are reserved on the wire but nothing pushes them yet —
 *  see docs/features/pane-plugins.md). Replays out-of-order deliveries by the
 *  message `index` every raw channel message carries — the same ordering
 *  `@tauri-apps/api/core.js`'s own `Channel` class implements, reproduced
 *  here because there is no bundler to import that file through (see the
 *  module doc comment). Exported standalone (no Tauri internals needed) so
 *  the ordering/dispatch logic is unit-testable without a real IPC channel. */
export class EventRouter {
  constructor() {
    this._listeners = new Map();
    this._nextIndex = 0;
    this._pending = [];
  }

  /** Subscribe to one event name; returns an unsubscribe function. */
  on(eventName, cb) {
    let set = this._listeners.get(eventName);
    if (!set) {
      set = new Set();
      this._listeners.set(eventName, set);
    }
    set.add(cb);
    return () => set.delete(cb);
  }

  _dispatch(pluginEvent) {
    const set = this._listeners.get(pluginEvent.event);
    if (!set) return;
    for (const cb of set) cb(pluginEvent.payload);
  }

  /** Feed one raw channel message: either `{index, message}` or `{index, end:
   *  true}` (channel-close marker, ignored) — the shape the callback
   *  registered with `window.__TAURI_INTERNALS__.transformCallback` receives. */
  handleRaw(raw) {
    if (raw && "end" in raw) return;
    const { index, message } = raw;
    if (index === this._nextIndex) {
      this._dispatch(message);
      this._nextIndex += 1;
      while (this._nextIndex in this._pending) {
        this._dispatch(this._pending[this._nextIndex]);
        delete this._pending[this._nextIndex];
        this._nextIndex += 1;
      }
    } else {
      this._pending[index] = message;
    }
  }
}

/** Creates the ergonomic client a plugin's entry script uses:
 *  `request(method, params)` for the request/response methods
 *  (`storage.get`/`storage.set`/`fs.read`/`metrics.subscribe`/
 *  `metrics.unsubscribe`) and `onEvent(eventName, cb)` for unsolicited pushes.
 *  `apiVersion` must match the plugin's own `plugin.json` — the broker
 *  rejects a method introduced after the apiVersion declared at install/open
 *  time.
 *
 *  `capability` is deliberately NOT a parameter here: which capability a
 *  method needs is the broker's own closed method table
 *  (`pluginbroker.rs::method_spec`), not something a caller declares per
 *  call — accepting one here would just be inert decoration a caller could
 *  get wrong. `options.internals` is for tests only; real plugin code omits
 *  it and this falls back to `window.__TAURI_INTERNALS__`. */
export function createPluginClient(options) {
  const opts = options || {};
  const apiVersion = opts.apiVersion || 1;
  const internals = opts.internals || (typeof window !== "undefined" ? window.__TAURI_INTERNALS__ : undefined);
  if (!internals) {
    throw new Error(
      "loomux-plugin-sdk: window.__TAURI_INTERNALS__ is unavailable — this file only runs inside a loomux plugin window",
    );
  }

  const router = new EventRouter();
  let channelPromise = null;
  let requestSeq = 0;

  function ensureChannel() {
    if (!channelPromise) {
      channelPromise = (async () => {
        const channelId = internals.transformCallback((raw) => router.handleRaw(raw), false);
        const channel = { toJSON: () => `${CHANNEL_MARKER_PREFIX}${channelId}` };
        await internals.invoke("plugin_broker_open_channel", { channel });
      })();
    }
    return channelPromise;
  }

  async function request(method, params) {
    // metrics.subscribe pushes ticks over the broker channel — open it
    // first, so no tick can arrive (and be silently dropped, per
    // `pluginbroker::push_event`'s fire-and-forget posture toward a window
    // that hasn't opened its channel yet) before a listener is wired up.
    if (method.startsWith("metrics.")) await ensureChannel();
    requestSeq += 1;
    const envelope = buildRequestEnvelope(String(requestSeq), method, params, apiVersion);
    const response = await internals.invoke("plugin_broker_request", { request: envelope });
    return unwrapResponse(response);
  }

  function onEvent(eventName, cb) {
    void ensureChannel();
    return router.on(eventName, cb);
  }

  return { request, onEvent };
}
