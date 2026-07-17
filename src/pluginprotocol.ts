// Pure, DOM-free pane-plugins broker contract (#360 Slice C — see
// doc/design/pane-plugins.md's broker contract and Isolation section). Kept
// separate from pluginbroker.ts (the `invoke` wrapper) so it can be
// unit-tested in isolation (see test/pluginprotocol.test.ts) — nothing here
// touches Tauri IPC or the DOM. Mirrors src-tauri/src/pluginbroker.rs's
// envelope types and `check_request` one-for-one so both halves of the
// contract stay provably in sync. Also importable, unmodified, by a future
// plugin SDK (Slice G) that runs *inside* a plugin's own isolated child
// webview — that code is not part of loomux's own build, so the
// authoritative enforcement is (and must stay) the Rust side; this module
// exists so a mismatch between what a plugin sends and what the broker
// checks is a type error, not a runtime surprise.

/** The v1 capability enum (design note, "The v1 enum") — closed by
 *  construction, mirrored verbatim in `pluginbroker.rs`'s `Capability` enum.
 *  Adding a value is a reviewed contract change, never a per-plugin escape
 *  hatch. */
export type PluginCapability = "panel" | "storage" | "fs.read" | "metrics.system";

/** plugin -> host, expects a reply (design note's envelope shape). */
export interface PluginRequest {
  type: "request";
  id: string;
  apiVersion: number;
  method: string;
  params: unknown;
}

/** host -> plugin, replying to a request. */
export interface PluginResponse {
  type: "response";
  id: string;
  ok: boolean;
  result?: unknown;
  error?: PluginBrokerError;
}

/** host -> plugin, unsolicited (resize, theme, a metrics tick, …). */
export interface PluginEvent {
  type: "event";
  event: string;
  payload: unknown;
}

export type BrokerErrorCode =
  | "unsupported-version"
  | "capability-denied"
  | "bad-request"
  | "not-found"
  | "outside-root"
  | "not-implemented";

export interface PluginBrokerError {
  code: BrokerErrorCode | string;
  message: string;
}

interface MethodSpec {
  capability: Exclude<PluginCapability, "panel">;
  sinceApiVersion: number;
}

/** The method table: method name -> (capability, apiVersion it was introduced
 *  at) — mirrors `pluginbroker.rs`'s `method_spec`. Adding a method is the
 *  same reviewed contract change as adding a capability. */
const METHODS: Readonly<Record<string, MethodSpec>> = Object.freeze({
  "storage.get": { capability: "storage", sinceApiVersion: 1 },
  "storage.set": { capability: "storage", sinceApiVersion: 1 },
  "fs.read": { capability: "fs.read", sinceApiVersion: 1 },
  "metrics.subscribe": { capability: "metrics.system", sinceApiVersion: 1 },
  "metrics.unsubscribe": { capability: "metrics.system", sinceApiVersion: 1 },
});

/** Validates an arbitrary value against the `PluginRequest` shape — the
 *  "envelope parsing" half of the design note's per-message check. Returns
 *  `null` for anything that isn't a well-formed request envelope, rather
 *  than throwing: a malformed message is data, not a programmer error. */
export function parsePluginRequest(raw: unknown): PluginRequest | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (r.type !== "request") return null;
  if (typeof r.id !== "string" || r.id.length === 0) return null;
  if (typeof r.apiVersion !== "number" || !Number.isInteger(r.apiVersion) || r.apiVersion < 1) {
    return null;
  }
  if (typeof r.method !== "string" || r.method.length === 0) return null;
  return { type: "request", id: r.id, apiVersion: r.apiVersion, method: r.method, params: r.params };
}

/** The pure decision the design note calls out by name: "is method M allowed
 *  for granted capabilities C at apiVersion V" — steps 2 and 3 of the
 *  per-message check (step 1, identity, is structural for the child-webview
 *  transport — see `pluginbroker.rs`'s module doc comment; step 4, params
 *  validation, is method-specific and happens after this returns `null`).
 *  Lives once, here; any DOM/command wiring only calls it. */
export function checkCapability(
  granted: ReadonlySet<PluginCapability>,
  pluginApiVersion: number,
  req: Pick<PluginRequest, "method" | "apiVersion">,
): PluginBrokerError | null {
  const spec = METHODS[req.method];
  if (!spec) {
    return { code: "bad-request", message: `unknown method: ${req.method}` };
  }
  if (spec.sinceApiVersion > pluginApiVersion || req.apiVersion > pluginApiVersion) {
    return {
      code: "unsupported-version",
      message: `method \`${req.method}\` requires apiVersion >= ${spec.sinceApiVersion}; plugin declared ${pluginApiVersion}`,
    };
  }
  if (!granted.has(spec.capability)) {
    return {
      code: "capability-denied",
      message: `capability \`${spec.capability}\` not granted for method \`${req.method}\``,
    };
  }
  return null;
}

export function errorResponse(id: string, error: PluginBrokerError): PluginResponse {
  return { type: "response", id, ok: false, error };
}

export function okResponse(id: string, result: unknown): PluginResponse {
  return { type: "response", id, ok: true, result };
}

/** A relative path is inside its jail root when normalizing away `.`/`..`
 *  segments never climbs above the root — the same intent as
 *  `fileedit.rs`'s `safe_resolve` (which remains the authoritative check;
 *  this is early client-side feedback only, e.g. for a future plugin SDK
 *  that wants to fail fast on an obviously-bad path before round-tripping to
 *  the broker). An absolute path or a Windows drive segment is never inside
 *  a jail. */
export function isPathWithinJail(rel: string): boolean {
  if (rel.length === 0) return true;
  if (rel.startsWith("/") || rel.startsWith("\\") || /^[a-zA-Z]:/.test(rel)) return false;
  const segments = rel.split(/[/\\]/);
  let depth = 0;
  for (const seg of segments) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      depth -= 1;
      if (depth < 0) return false;
    } else {
      depth += 1;
    }
  }
  return true;
}
