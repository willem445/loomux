// Thin bridge to the Rust pane-plugins trust core (#360 Slice C — see
// doc/design/pane-plugins.md). The pure envelope/capability-check contract
// lives in pluginprotocol.ts (DOM-free, unit-tested); this file is the one
// place `main`'s trusted frontend may `invoke` the plugin-host commands
// (CLAUDE.md constraint 5 — no other module calls `invoke` for these).

import { invoke } from "@tauri-apps/api/core";
import type { PluginCapability } from "./pluginprotocol";

export type {
  PluginCapability,
  PluginRequest,
  PluginResponse,
  PluginEvent,
  PluginBrokerError,
  BrokerErrorCode,
} from "./pluginprotocol";
export { parsePluginRequest, checkCapability, errorResponse, okResponse, isPathWithinJail } from "./pluginprotocol";

/** The output of Slice B's manifest validation, not a manifest parser of its
 *  own — see `pluginbroker::OpenPluginWindowRequest` on the Rust side, which
 *  this mirrors field-for-field (camelCase over the wire). `x`/`y`/`width`/
 *  `height` are logical pixels relative to the MAIN window's own client area
 *  (`Window::add_child`'s own coordinate space — see pluginwindow.ts's
 *  module doc comment) — no `title` field: #360 Slice D embeds the plugin as
 *  a child webview (no OS window chrome to title), replacing the earlier
 *  overlay-window design. */
export interface OpenPluginWindowRequest {
  pluginId: string;
  /** Relative path inside the plugin's own folder, served over `plugin://`. */
  entry: string;
  /** Absolute path to the plugin's own root; omit for a `rootless: true` plugin. */
  root?: string;
  /** The manifest's declared capabilities, subset of the closed enum. */
  capabilities: PluginCapability[];
  apiVersion: number;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Embeds a plugin's isolated child webview into the main window
 *  (`Window::add_child`), bound to the curated `plugin-broker` capability
 *  (never `main-ui`). The only sanctioned way for `main`'s trusted frontend
 *  to do this — no other module may `invoke` `plugin_open_window` directly
 *  (CLAUDE.md constraint 5). Returns the new webview's label.
 *  `pluginpaneview.ts`'s `open()` calls this. */
export function openPluginWindow(req: OpenPluginWindowRequest): Promise<string> {
  return invoke<string>("plugin_open_window", { req });
}

/** Closes a plugin's child webview and releases its broker-side session
 *  state (`pluginbroker::plugin_close_window` — see that command's doc
 *  comment for why an explicit close call is needed at all: a child webview
 *  never fires `WindowEvent::Destroyed`, unlike a real top-level window).
 *  `pluginpaneview.ts`'s `dispose()` calls this fire-and-forget, mirroring
 *  the existing `killPty(...).catch(() => {})` teardown posture. */
export function closePluginWindow(label: string): Promise<void> {
  return invoke<void>("plugin_close_window", { label });
}
