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
 *  this mirrors field-for-field (camelCase over the wire). */
export interface OpenPluginWindowRequest {
  pluginId: string;
  /** Relative path inside the plugin's own folder, served over `plugin://`. */
  entry: string;
  /** Absolute path to the plugin's own root; omit for a `rootless: true` plugin. */
  root?: string;
  /** The manifest's declared capabilities, subset of the closed enum. */
  capabilities: PluginCapability[];
  apiVersion: number;
  title: string;
  width: number;
  height: number;
}

/** Opens a plugin's isolated `WebviewWindow`, bound to the curated
 *  `plugin-broker` capability (never `main-ui`). The only sanctioned way for
 *  `main`'s trusted frontend to do this — no other module may `invoke`
 *  `plugin_open_window` directly (CLAUDE.md constraint 5). Returns the new
 *  window's label. Slice D calls this from the `"plugin"` pane kind's
 *  `startContent()`; nothing does yet. */
export function openPluginWindow(req: OpenPluginWindowRequest): Promise<string> {
  return invoke<string>("plugin_open_window", { req });
}
