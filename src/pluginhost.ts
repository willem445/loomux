// Typed bridge to the pane-plugins backend host (#360 Slice B). Mirrors the
// per-feature wrapper precedent set by `git.ts`/`fileapi.ts`: every `plugins`
// capability is a `#[tauri::command]` fronted by a typed wrapper here, and no
// other frontend module calls `invoke` for these (CLAUDE.md constraint #5).
//
// This module only lists/installs plugins. It has nothing to do with the
// sandboxed-frame/broker wiring (#360 Slice C) or the `"plugin"` pane-kind
// union member (#360 Slice D) — those build on top of `PluginManifest` below.

import { invoke } from "@tauri-apps/api/core";

/** A validated `plugin.json`, as the backend parsed it — see
 *  `doc/design/pane-plugins.md`'s manifest section for what each field means
 *  and `plugins.rs::parse_manifest` for the validation rules. */
export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  api_version: number;
  entry: string;
  capabilities: string[];
  rootless: boolean;
}

/** Machine-readable code the backend prefixes onto every error string (before
 *  the first ": "), kept in sync with the `err(code, …)` calls in
 *  `plugins.rs`, so the UI can branch (e.g. show the specific manifest
 *  violation on a rejected install) without parsing prose. */
export type PluginHostError =
  | "invalid-json"
  | "invalid-manifest"
  | "unknown-capability"
  | "unsupported-api-version"
  | "invalid-combination"
  | "invalid-entry"
  | "invalid-path"
  | "outside-root"
  | "symlink"
  | "not-found"
  | "io"
  | "unknown";

/** Extract the leading error code from a rejected command's error. Any value
 *  that isn't a known code (including a non-string) collapses to "unknown". */
export function pluginErrorCode(e: unknown): PluginHostError {
  const msg = typeof e === "string" ? e : e instanceof Error ? e.message : String(e ?? "");
  const code = msg.split(":", 1)[0]?.trim() ?? "";
  const known: PluginHostError[] = [
    "invalid-json",
    "invalid-manifest",
    "unknown-capability",
    "unsupported-api-version",
    "invalid-combination",
    "invalid-entry",
    "invalid-path",
    "outside-root",
    "symlink",
    "not-found",
    "io",
  ];
  return (known as string[]).includes(code) ? (code as PluginHostError) : "unknown";
}

/** Human-readable prose part of a backend error (everything after the code). */
export function pluginErrorMessage(e: unknown): string {
  const msg = typeof e === "string" ? e : e instanceof Error ? e.message : String(e ?? "");
  const idx = msg.indexOf(":");
  return idx >= 0 ? msg.slice(idx + 1).trim() : msg;
}

/** Enumerate every plugin currently installed (a local-folder scan on the
 *  backend, never a network call — no remote marketplace in v1). A folder
 *  with an invalid or self-inconsistent manifest is silently absent from the
 *  result, not surfaced as an error here — see `plugins.rs::discover_installed`. */
export const listPlugins = (): Promise<PluginManifest[]> => invoke("list_plugins");

/** Copy the plugin folder at `sourcePath` into the plugins directory. This is
 *  the whole install action (`doc/design/pane-plugins.md`: "there is no build
 *  step, no compilation, no fetch from anywhere") — a folder whose manifest
 *  fails validation is rejected with a specific `PluginHostError` and nothing
 *  is copied. Resolves with the installed manifest on success. */
export const installPlugin = (sourcePath: string): Promise<PluginManifest> =>
  invoke("install_plugin", { source: sourcePath });
