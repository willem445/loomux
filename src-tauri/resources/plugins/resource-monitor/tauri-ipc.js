// Minimal, dependency-free Tauri IPC bridge for a `plugin://`-hosted plugin
// window (#360 Slice F). A plugin never gets `@tauri-apps/api` from
// `node_modules` at install time — "install" is a plain folder copy, no
// build step (`doc/design/pane-plugins.md`) — and the CSP this plugin is
// served under (`connect-src 'none'`, script-src limited to `'self' plugin:`)
// forbids fetching it from anywhere at runtime either. So this plugin vendors
// the two primitives it actually needs, `invoke` and `Channel`, as a local,
// same-origin file instead.
//
// This is not a reimplementation-by-guesswork: both functions are adapted
// line-for-line from the real `@tauri-apps/api@2.6.0` `core.js`
// (Copyright 2019-2024 Tauri Programme within The Commons Conservancy,
// dual-licensed Apache-2.0/MIT — same license this project already depends
// on transitively), with the tslib private-field-emulation helpers (needed
// only for that package's older compile target) replaced by native `#private`
// class fields, which every WebView2/wry runtime this app ships on already
// supports. The wire behavior — `window.__TAURI_INTERNALS__.invoke`,
// `.transformCallback`, `.unregisterCallback`, and the `{message, index}` /
// `{end, index}` ordering envelope a Rust-side `tauri::ipc::Channel::send`
// actually emits (`tauri-2.11.5/src/ipc/channel.rs`) — is unchanged, so this
// stays wire-compatible with whatever `@tauri-apps/api` version `main`'s own
// frontend uses.

/** Invoke a Tauri command. `window.__TAURI_INTERNALS__` is injected into
 *  every webview Tauri manages (part of the runtime's own init script, not
 *  the npm package) — this plugin window's ACL grant
 *  (`capabilities/plugin.json`, `windows: ["plugin-*"]`) permits exactly two
 *  commands through it: `plugin_broker_request` and
 *  `plugin_broker_open_channel`. Calling anything else here would simply be
 *  denied by Tauri's own resolver before this plugin's code could do
 *  anything with the result. */
export function invoke(cmd, args = {}, options) {
  return window.__TAURI_INTERNALS__.invoke(cmd, args, options);
}

function transformCallback(callback, once = false) {
  return window.__TAURI_INTERNALS__.transformCallback(callback, once);
}

/** A `tauri::ipc::Channel<PluginEventWire>` counterpart: opened once via
 *  `plugin_broker_open_channel`, then receives every unsolicited
 *  `PluginEvent` (currently just `metrics.tick`) the broker pushes for as
 *  long as this window stays open. Preserves message order the same way the
 *  real implementation does — the Rust side tags each push with a
 *  monotonic `index` and a final `{end: true}` marker, so a message that
 *  arrives out of order (possible once the payload is large enough to route
 *  through the fetch-relay path instead of a direct `eval`) is queued rather
 *  than delivered early. */
export class Channel {
  #onmessage;
  #nextMessageIndex = 0;
  #pendingMessages = [];
  #messageEndIndex;

  constructor(onmessage) {
    this.#onmessage = onmessage || (() => {});
    this.id = transformCallback((rawMessage) => {
      const index = rawMessage.index;
      if ("end" in rawMessage) {
        if (index === this.#nextMessageIndex) {
          this.#cleanup();
        } else {
          this.#messageEndIndex = index;
        }
        return;
      }

      const message = rawMessage.message;
      if (index === this.#nextMessageIndex) {
        this.#onmessage(message);
        this.#nextMessageIndex += 1;
        while (this.#nextMessageIndex in this.#pendingMessages) {
          const pending = this.#pendingMessages[this.#nextMessageIndex];
          this.#onmessage(pending);
          delete this.#pendingMessages[this.#nextMessageIndex];
          this.#nextMessageIndex += 1;
        }
        if (this.#nextMessageIndex === this.#messageEndIndex) {
          this.#cleanup();
        }
      } else {
        this.#pendingMessages[index] = message;
      }
    });
  }

  #cleanup() {
    window.__TAURI_INTERNALS__.unregisterCallback(this.id);
  }

  set onmessage(handler) {
    this.#onmessage = handler;
  }

  get onmessage() {
    return this.#onmessage;
  }

  toJSON() {
    return `__CHANNEL__:${this.id}`;
  }
}
