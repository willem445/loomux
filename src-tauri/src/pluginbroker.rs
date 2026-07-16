//! The pane-plugins trust core (#360 Slice C, Option B — see
//! `doc/design/pane-plugins.md`'s Isolation section and the Phase-0/Phase-0.5
//! spike findings on #360).
//!
//! A plugin's isolated `WebviewWindow` never calls raw `invoke`. Its capability
//! (`windows: ["plugin-*"]` in `capabilities/plugin.json`) grants it exactly
//! two commands — [`plugin_broker_request`] and [`plugin_broker_open_channel`]
//! — and nothing else in the app's ~120+ command surface. Those two commands
//! *are* the broker: every other function in this module is the pure decision
//! logic and capability-scoped handlers they call.
//!
//! **Why `invoke` instead of literal `postMessage`, unlike the design note's
//! iframe-era wording:** a child `WebviewWindow` is a separate top-level
//! browsing context — `window.opener` is `null` (confirmed by the Phase-0.5
//! spike) and there is no live JS window reference between it and `main` at
//! all, so a literal `window.postMessage` bridge (the iframe model's
//! transport) has nothing to target. Tauri's own IPC channel to these two
//! ACL-gated commands *is* the postMessage-equivalent boundary for Option B:
//! it replaces the iframe model's `event.source === frame.contentWindow`
//! identity check with something structurally equivalent — the window label
//! + capability system enforced by Tauri's resolver, checked before this
//! module's code ever runs. Host→plugin pushes (the `PluginEvent` direction:
//! resize/theme/metrics ticks) ride a `tauri::ipc::Channel`, opened once via
//! `plugin_broker_open_channel` — this sidesteps the app's global `listen()`
//! surface deliberately: a plugin granted `core:event:allow-listen` could
//! listen for *any* event name emitted anywhere in the app (e.g. `pty-output`,
//! which broadcasts every pane's terminal output), since Tauri's permission
//! gates whether `listen()` may be called at all, not which event names it
//! may hear. A `Channel` has no such surface — it is scoped to the one
//! invocation that created it.
//!
//! The four-step per-message check from the design note becomes, in this
//! transport:
//!
//! 1. **Identity** — enforced structurally: only a window matching
//!    `capabilities/plugin.json`'s `windows: ["plugin-*"]` pattern can reach
//!    [`plugin_broker_request`] at all, and this module then looks up that
//!    window's own registered [`PluginSession`] by its (unforgeable) label.
//! 2. **`apiVersion` check** — [`check_request`].
//! 3. **Capability check** — [`check_request`].
//! 4. **Params validation** — the per-method handlers below
//!    ([`storage_get`], [`storage_set`], [`fs_read`]).
//!
//! Adding a capability or a method is a reviewed contract change (design
//! note's "why closed, not extensible") — never a per-plugin escape hatch.
//!
//! **The `plugin://` scheme itself belongs to `plugins.rs` (#360 Slice B),
//! not this module.** Tauri allows exactly one handler per registered
//! scheme; `lib.rs` registers `plugins::plugin_protocol_handler` only.
//! [`plugin_open_window`] points its `WebviewWindow` at the URLs that handler
//! serves (`plugin://localhost/<id>/<entry>` — see [`build_plugin_url`]) and
//! [`is_navigation_allowed`] locks navigation to that same address space;
//! neither resolves or serves an asset itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::obs::LockExt;

/// The apiVersion this build's broker speaks (design note's Versioning
/// section). A plugin declaring a higher value is refused at window-open time.
pub const BROKER_API_VERSION: u32 = 1;

/// The v1 capability enum (design note, "The v1 enum") — closed by
/// construction. Adding a variant is a reviewed contract change, mirrored
/// verbatim in `src/pluginbroker.ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Implicit — every plugin gets this merely by existing as a pane; never
    /// declared in a manifest's `capabilities` array and never a broker
    /// method's requirement. Kept in this enum only for completeness/typing.
    Panel,
    Storage,
    FsRead,
    MetricsSystem,
}

impl Capability {
    pub fn parse(s: &str) -> Option<Capability> {
        match s {
            "panel" => Some(Capability::Panel),
            "storage" => Some(Capability::Storage),
            "fs.read" => Some(Capability::FsRead),
            "metrics.system" => Some(Capability::MetricsSystem),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Panel => "panel",
            Capability::Storage => "storage",
            Capability::FsRead => "fs.read",
            Capability::MetricsSystem => "metrics.system",
        }
    }
}

// ---------- envelope wire types (design note, "Envelope shape") ----------

#[derive(Deserialize, Debug, Clone)]
pub struct PluginRequestWire {
    pub id: String,
    #[serde(rename = "apiVersion")]
    pub api_version: u32,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct PluginErrorWire {
    pub code: String,
    pub message: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct PluginResponseWire {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginErrorWire>,
}

#[derive(Serialize, Debug, Clone)]
pub struct PluginEventWire {
    pub event: String,
    pub payload: Value,
}

fn bad_request(message: impl Into<String>) -> PluginErrorWire {
    PluginErrorWire {
        code: "bad-request".into(),
        message: message.into(),
    }
}

// ---------- the method table: method name -> (capability, since apiVersion) ----------

struct MethodSpec {
    capability: Capability,
    since_api_version: u32,
}

fn method_spec(method: &str) -> Option<MethodSpec> {
    match method {
        "storage.get" => Some(MethodSpec {
            capability: Capability::Storage,
            since_api_version: 1,
        }),
        "storage.set" => Some(MethodSpec {
            capability: Capability::Storage,
            since_api_version: 1,
        }),
        "fs.read" => Some(MethodSpec {
            capability: Capability::FsRead,
            since_api_version: 1,
        }),
        "metrics.subscribe" => Some(MethodSpec {
            capability: Capability::MetricsSystem,
            since_api_version: 1,
        }),
        "metrics.unsubscribe" => Some(MethodSpec {
            capability: Capability::MetricsSystem,
            since_api_version: 1,
        }),
        _ => None,
    }
}

/// The pure decision the design note calls out by name: "is method M allowed
/// for granted capabilities C at apiVersion V" — steps 2 and 3 of the
/// per-message check (identity is structural for Option B, see the module
/// doc comment; params validation is step 4, handler-specific, below).
/// Lives once, here; DOM/command wiring only calls it.
pub fn check_request(
    granted: &[Capability],
    plugin_api_version: u32,
    req: &PluginRequestWire,
) -> Result<Capability, PluginErrorWire> {
    let spec = method_spec(&req.method)
        .ok_or_else(|| bad_request(format!("unknown method: {}", req.method)))?;
    if spec.since_api_version > plugin_api_version || req.api_version > plugin_api_version {
        return Err(PluginErrorWire {
            code: "unsupported-version".into(),
            message: format!(
                "method `{}` requires apiVersion >= {}; plugin declared {}",
                req.method, spec.since_api_version, plugin_api_version
            ),
        });
    }
    if !granted.contains(&spec.capability) {
        return Err(PluginErrorWire {
            code: "capability-denied".into(),
            message: format!(
                "capability `{}` not granted for method `{}`",
                spec.capability.as_str(),
                req.method
            ),
        });
    }
    Ok(spec.capability)
}

// ---------- capability handlers (step 4: params validation + the real work) ----------

/// `storage` capability: a namespaced per-plugin key/value store (design
/// note's capability table). Backed by the same atomic-write/quarantine
/// discipline as `uistate.rs`'s `tabs.json`, one JSON blob per plugin id.
/// `storage_dir` is injected (rather than read from a global) so this is
/// testable against a tempdir without touching the real user data dir — the
/// real call site (`dispatch`) passes `uistate::plugin_storage_dir()`.
fn storage_map_path(storage_dir: &Path, plugin_id: &str) -> PathBuf {
    storage_dir.join(format!("{plugin_id}.json"))
}

fn load_storage_map(storage_dir: &Path, plugin_id: &str) -> HashMap<String, Value> {
    let path = storage_map_path(storage_dir, plugin_id);
    crate::uistate::load_or_quarantine(&path)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_storage_map(
    storage_dir: &Path,
    plugin_id: &str,
    map: &HashMap<String, Value>,
) -> Result<(), PluginErrorWire> {
    let path = storage_map_path(storage_dir, plugin_id);
    let raw = serde_json::to_string(map).map_err(|e| PluginErrorWire {
        code: "io".into(),
        message: e.to_string(),
    })?;
    crate::uistate::write_atomic(&path, &raw).map_err(|e| PluginErrorWire {
        code: "io".into(),
        message: e,
    })
}

fn params_str<'a>(params: &'a Value, field: &str) -> Result<&'a str, PluginErrorWire> {
    params
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request(format!("params.{field} must be a string")))
}

pub fn storage_get(storage_dir: &Path, plugin_id: &str, params: &Value) -> Result<Value, PluginErrorWire> {
    let key = params_str(params, "key")?;
    let map = load_storage_map(storage_dir, plugin_id);
    Ok(map.get(key).cloned().unwrap_or(Value::Null))
}

pub fn storage_set(storage_dir: &Path, plugin_id: &str, params: &Value) -> Result<Value, PluginErrorWire> {
    let key = params_str(params, "key")?;
    let value = params
        .get("value")
        .cloned()
        .ok_or_else(|| bad_request("params.value is required"))?;
    let mut map = load_storage_map(storage_dir, plugin_id);
    map.insert(key.to_string(), value);
    save_storage_map(storage_dir, plugin_id, &map)?;
    Ok(Value::Null)
}

/// `fs.read` capability: read a file under the pane's own root only —
/// root-jailed, no exceptions (design note's capability table). Reuses
/// `fileedit`'s existing server-side path choke point rather than
/// reimplementing traversal checks a second time.
pub fn fs_read(root: &str, params: &Value) -> Result<Value, PluginErrorWire> {
    let rel = params_str(params, "path")?;
    match crate::fileedit::read_file(root, rel) {
        Ok(file_read) => Ok(serde_json::to_value(file_read).unwrap()),
        Err(e) => Err(wire_err_from_fileedit(e)),
    }
}

/// `fileedit`'s errors are `"<code>: <message>"` strings; split them back into
/// the broker's `{code, message}` shape so `fs.read` surfaces the same
/// specific codes (`outside-root`, `not-found`, `too-large`, …) `ft_read_file`
/// already does, per the design note's error-surface contract.
fn wire_err_from_fileedit(e: String) -> PluginErrorWire {
    match e.split_once(": ") {
        Some((code, message)) => PluginErrorWire {
            code: code.to_string(),
            message: message.to_string(),
        },
        None => PluginErrorWire {
            code: "io".into(),
            message: e,
        },
    }
}

/// `metrics.system` capability: gated here, served by Slice E's
/// `sys_processes`-shaped backend (design note — "never exposed to a plugin
/// except through this one broker capability"). Slice E hasn't landed yet;
/// the capability check above is real, the data handler is not.
fn metrics_not_implemented(method: &str) -> PluginErrorWire {
    PluginErrorWire {
        code: "not-implemented".into(),
        message: format!("`{method}` awaits Slice E's metrics backend"),
    }
}

// ---------- plugin id validation ----------

/// Defensive validation independent of Slice B's manifest schema check (which
/// may not have run, or may not exist yet on this branch): `plugin_id` is used
/// as a filename component (storage), a `plugin://` URL segment, and part of
/// a window label, so it is re-validated here regardless of what validated it
/// upstream.
pub fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---------- navigation lock (spike residual-capability finding #1) ----------

/// A `WebviewWindow` has no `sandbox=""` equivalent, so nothing stops it
/// self-navigating to a remote origin the way an `allow-top-navigation`-less
/// iframe would be stopped (Phase-0.5 spike finding: `location.href =
/// 'https://example.com/'` succeeded completely). This is the mitigation:
/// `WebviewWindowBuilder::on_navigation` is wired to this pure predicate,
/// which allows only the plugin's own address space under
/// `plugins::plugin_protocol_handler` (#360 Slice B) — `plugin://localhost/<id>/…`
/// on the wire, rewritten by wry to `http://plugin.localhost/<id>/…` on
/// Windows (per `plugins.rs`'s own doc comment). Either way the authority is
/// fixed (`localhost` / `plugin.localhost`) and `<id>` is checked as the
/// first path segment, never the host — mirroring exactly how
/// `plugins::parse_plugin_request_path` addresses the same scheme, so a
/// navigation target is allowed only when it resolves to precisely the same
/// plugin the window was opened for (denying even another plugin's own
/// otherwise-legitimate `plugin://localhost/<other-id>/…` address).
pub fn is_navigation_allowed(plugin_id: &str, url: &tauri::Url) -> bool {
    let first_segment_matches = || {
        url.path_segments()
            .and_then(|mut segs| segs.next())
            .map(|first| first == plugin_id)
            .unwrap_or(false)
    };
    match url.scheme() {
        "plugin" => url.host_str() == Some("localhost") && first_segment_matches(),
        "http" if url.host_str() == Some("plugin.localhost") => first_segment_matches(),
        _ => false,
    }
}

// ---------- per-window plugin session registry ----------

#[derive(Clone)]
pub struct PluginSession {
    pub plugin_id: String,
    /// `None` for a `rootless: true` plugin — `fs.read` is then unreachable
    /// regardless of what the manifest declared (design note: the
    /// combination is rejected at validation time upstream; this is the
    /// runtime backstop).
    pub root: Option<String>,
    pub granted: Vec<Capability>,
    pub api_version: u32,
}

static PLUGIN_SESSIONS: Mutex<Option<HashMap<String, PluginSession>>> = Mutex::new(None);
static PLUGIN_CHANNELS: Mutex<Option<HashMap<String, tauri::ipc::Channel<PluginEventWire>>>> =
    Mutex::new(None);
static PLUGIN_WINDOW_SEQ: AtomicU64 = AtomicU64::new(0);

fn with_sessions<R>(f: impl FnOnce(&mut HashMap<String, PluginSession>) -> R) -> R {
    let mut guard = PLUGIN_SESSIONS.lock_safe();
    f(guard.get_or_insert_with(HashMap::new))
}

fn with_channels<R>(f: impl FnOnce(&mut HashMap<String, tauri::ipc::Channel<PluginEventWire>>) -> R) -> R {
    let mut guard = PLUGIN_CHANNELS.lock_safe();
    f(guard.get_or_insert_with(HashMap::new))
}

/// Sanitized, unique-per-open window label: `plugin-<id>-<seq>`. Not a
/// security token (uniqueness only), so a plain atomic counter is correct
/// here per this crate's getrandom ban — see `Cargo.toml`.
fn next_window_label(plugin_id: &str) -> String {
    let seq = PLUGIN_WINDOW_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("plugin-{plugin_id}-{seq}")
}

/// Cleanup hook called from `lib.rs`'s `on_window_event(Destroyed)` handler —
/// mirrors the existing PTY-kill-on-close pattern for plugin windows.
pub fn on_window_destroyed(label: &str) {
    with_sessions(|m| m.remove(label));
    with_channels(|m| m.remove(label));
}

// ---------- tauri commands ----------
//
// These two are the ONLY commands `capabilities/plugin.json` grants to a
// `plugin-*` window (see `permissions/sets/plugin-broker.toml`). Neither ever
// returns a Tauri-level `Err` for a plugin-side mistake — a denied or
// malformed request is always a `PluginResponseWire { ok: false, .. }`, per
// the design note's error-surface contract ("never a thrown exception that
// could crash the plugin's frame").

#[tauri::command]
pub fn plugin_broker_request(
    window: tauri::WebviewWindow,
    request: PluginRequestWire,
) -> PluginResponseWire {
    let label = window.label().to_string();
    let session = match session_for_window(&label) {
        Some(s) => s,
        None => {
            return PluginResponseWire {
                id: request.id,
                ok: false,
                result: None,
                error: Some(PluginErrorWire {
                    code: "not-found".into(),
                    message: "no plugin session registered for this window".into(),
                }),
            }
        }
    };
    match check_request(&session.granted, session.api_version, &request) {
        Err(e) => PluginResponseWire {
            id: request.id,
            ok: false,
            result: None,
            error: Some(e),
        },
        Ok(cap) => dispatch(&session, cap, &request),
    }
}

fn dispatch(session: &PluginSession, cap: Capability, req: &PluginRequestWire) -> PluginResponseWire {
    let storage_dir = crate::uistate::plugin_storage_dir();
    let result = match (cap, req.method.as_str()) {
        (Capability::Storage, "storage.get") => storage_get(&storage_dir, &session.plugin_id, &req.params),
        (Capability::Storage, "storage.set") => storage_set(&storage_dir, &session.plugin_id, &req.params),
        (Capability::FsRead, "fs.read") => match &session.root {
            Some(root) => fs_read(root, &req.params),
            None => Err(PluginErrorWire {
                code: "not-found".into(),
                message: "plugin has no fs root (rootless plugin)".into(),
            }),
        },
        (Capability::MetricsSystem, method) => Err(metrics_not_implemented(method)),
        _ => Err(bad_request("method/capability mismatch")),
    };
    match result {
        Ok(value) => PluginResponseWire {
            id: req.id.clone(),
            ok: true,
            result: Some(value),
            error: None,
        },
        Err(error) => PluginResponseWire {
            id: req.id.clone(),
            ok: false,
            result: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
pub fn plugin_broker_open_channel(
    window: tauri::WebviewWindow,
    channel: tauri::ipc::Channel<PluginEventWire>,
) {
    let label = window.label().to_string();
    with_channels(|m| {
        m.insert(label, channel);
    });
}

/// Push an unsolicited `PluginEvent` (resize/theme/metrics tick) to a plugin
/// window that has opened its channel. Silently a no-op if it hasn't (or has
/// already closed) — the same fire-and-forget posture `GitWatcher`'s change
/// notifications use for a pane that may no longer be listening. Slice D/E
/// call sites, not this slice.
pub fn push_event(label: &str, event: &str, payload: Value) {
    let sent = with_channels(|m| {
        m.get(label)
            .map(|ch| ch.send(PluginEventWire { event: event.to_string(), payload: payload.clone() }))
    });
    if let Some(Err(e)) = sent {
        // Best-effort push; a closed/dropped channel is not an error worth
        // surfacing anywhere a human would see it.
        let _ = e;
    }
}

/// Request payload for [`plugin_open_window`] — the *output* of Slice B's
/// manifest validation, not a manifest parser of its own (out of this
/// slice's scope; see `doc/design/pane-plugins.md`'s manifest section for the
/// shape Slice B owes this command).
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OpenPluginWindowRequest {
    pub plugin_id: String,
    /// Relative path inside the plugin's own folder, served over `plugin://`
    /// by `plugins::plugin_protocol_handler` (#360 Slice B).
    pub entry: String,
    /// Absolute path to the plugin's own installed folder — used for
    /// `fs.read`'s jail, and must be the same folder Slice B's scheme
    /// handler serves (`plugins::plugins_root_dir().join(&plugin_id)`) so a
    /// plugin's `fs.read` capability and its served assets are always the
    /// same jail. `None` for a `rootless: true` plugin.
    pub root: Option<String>,
    /// The manifest's declared `capabilities`, already validated by Slice B
    /// against the closed enum — this command re-validates anyway (defense
    /// in depth; never trust a caller's validation as the only check).
    pub capabilities: Vec<String>,
    pub api_version: u32,
    /// The plugin manifest's own `name` field — display-only, and **untrusted
    /// text** (an arbitrary third-party plugin author's choice, not audited
    /// by loomux — see the design note's manifest section). Passed straight
    /// to `WebviewWindowBuilder::title`, an OS window-chrome string, never
    /// HTML — but any future surface (Slice D's pane tab label included) that
    /// renders this string into the DOM must escape it, never interpolate it
    /// as markup.
    pub title: String,
    pub width: f64,
    pub height: f64,
}

/// Registers a plugin session (capabilities/root) under `label`, so
/// [`plugin_broker_request`] can find it. Split out from the
/// `#[tauri::command]` in `lib.rs`-adjacent wiring so it's callable without a
/// live `WebviewWindowBuilder` from tests.
pub fn register_session(label: String, req: &OpenPluginWindowRequest) -> Result<(), String> {
    if !is_valid_plugin_id(&req.plugin_id) {
        return Err(format!(
            "invalid plugin id `{}`: must be 1-64 ascii alphanumeric/-/_ characters",
            req.plugin_id
        ));
    }
    let granted: Vec<Capability> = req
        .capabilities
        .iter()
        .map(|s| Capability::parse(s).ok_or_else(|| format!("unknown capability: {s}")))
        .collect::<Result<_, _>>()?;
    if req.api_version > BROKER_API_VERSION {
        return Err(format!(
            "plugin declares apiVersion {}, this build only speaks up to {}",
            req.api_version, BROKER_API_VERSION
        ));
    }
    with_sessions(|m| {
        m.insert(
            label,
            PluginSession {
                plugin_id: req.plugin_id.clone(),
                root: req.root.clone(),
                granted,
                api_version: req.api_version,
            },
        );
    });
    Ok(())
}

/// Look up the registered session for a `plugin-*` window label — the one
/// thing [`plugin_broker_request`] needs to know which capabilities/apiVersion
/// to check a request against. Keyed by the window label Tauri itself
/// guarantees unique, not by plugin id (`plugins.rs`'s `plugin://` asset
/// resolution is entirely independent of this registry — it resolves
/// straight from `plugins::plugins_root_dir()`, not from anything Slice C
/// tracks).
pub fn session_for_window(label: &str) -> Option<PluginSession> {
    with_sessions(|m| m.get(label).cloned())
}

#[tauri::command]
pub fn plugin_open_window(
    app: tauri::AppHandle,
    req: OpenPluginWindowRequest,
) -> Result<String, String> {
    if !is_valid_plugin_id(&req.plugin_id) {
        return Err(format!(
            "invalid plugin id `{}`: must be 1-64 ascii alphanumeric/-/_ characters",
            req.plugin_id
        ));
    }
    let label = next_window_label(&req.plugin_id);
    let url = build_plugin_url(&req.plugin_id, &req.entry)?;
    let plugin_id_for_nav = req.plugin_id.clone();

    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::External(url))
        .title(&req.title)
        .inner_size(req.width, req.height)
        .on_navigation(move |url| is_navigation_allowed(&plugin_id_for_nav, url))
        .build()
        .map_err(|e| e.to_string())?;

    register_session(label.clone(), &req)?;
    Ok(label)
}

/// Builds the `plugin://localhost/<id>/<entry>` URL a plugin window loads —
/// the exact address space `plugins::plugin_protocol_handler` (#360 Slice B,
/// the sole registered `plugin://` scheme handler — see `lib.rs`) serves and
/// jails to that plugin's own installed folder. The authority is always the
/// literal `localhost`, **never** the plugin id: wry rewrites a custom
/// scheme's authority on Windows (`plugin://localhost/abc` becomes
/// `http://plugin.localhost/abc`, per `plugins.rs`'s own doc comment), so
/// only the URL *path* is guaranteed to carry `<id>` intact across platforms
/// — `plugins::parse_plugin_request_path` is the one place that address
/// space is parsed; `is_navigation_allowed` below mirrors its shape rather
/// than reimplementing it independently. `entry` is re-validated as a plain
/// relative segment set (never `..`/absolute) here too, because this is the
/// one place a bad `entry` becomes a navigation target rather than a read.
fn build_plugin_url(plugin_id: &str, entry: &str) -> Result<tauri::Url, String> {
    use std::path::{Component, Path};
    let entry_path = Path::new(entry);
    if entry_path.is_absolute()
        || entry_path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir))
    {
        return Err(format!("entry escapes the plugin folder: {entry}"));
    }
    tauri::Url::parse(&format!("plugin://localhost/{plugin_id}/{entry}")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, api_version: u32, params: Value) -> PluginRequestWire {
        PluginRequestWire {
            id: "1".into(),
            api_version,
            method: method.into(),
            params,
        }
    }

    #[test]
    fn check_request_allows_granted_capability_at_matching_version() {
        let granted = vec![Capability::Storage];
        let r = req("storage.get", 1, Value::Null);
        assert_eq!(check_request(&granted, 1, &r), Ok(Capability::Storage));
    }

    #[test]
    fn check_request_denies_ungranted_capability() {
        let granted = vec![Capability::FsRead];
        let r = req("storage.get", 1, Value::Null);
        let err = check_request(&granted, 1, &r).unwrap_err();
        assert_eq!(err.code, "capability-denied");
    }

    #[test]
    fn check_request_rejects_unknown_method_as_bad_request() {
        let granted = vec![Capability::Storage, Capability::FsRead, Capability::MetricsSystem];
        let r = req("git.push", 1, Value::Null);
        let err = check_request(&granted, 1, &r).unwrap_err();
        assert_eq!(err.code, "bad-request");
    }

    #[test]
    fn check_request_rejects_method_newer_than_declared_api_version() {
        // storage.get is introduced at apiVersion 1; a plugin declaring 0
        // (hypothetical future-proofing) must not get it.
        let granted = vec![Capability::Storage];
        let r = req("storage.get", 1, Value::Null);
        let err = check_request(&granted, 0, &r).unwrap_err();
        assert_eq!(err.code, "unsupported-version");
    }

    #[test]
    fn is_valid_plugin_id_rejects_traversal_and_empty() {
        assert!(is_valid_plugin_id("resource-monitor"));
        assert!(is_valid_plugin_id("a_b-9"));
        assert!(!is_valid_plugin_id(""));
        assert!(!is_valid_plugin_id("../etc"));
        assert!(!is_valid_plugin_id("a/b"));
        assert!(!is_valid_plugin_id(&"x".repeat(65)));
    }

    #[test]
    fn navigation_lock_allows_own_origin_denies_everything_else() {
        let own_plugin = tauri::Url::parse("plugin://localhost/demo/index.html").unwrap();
        let own_windows = tauri::Url::parse("http://plugin.localhost/demo/index.html").unwrap();
        let other_plugin = tauri::Url::parse("plugin://localhost/other/index.html").unwrap();
        let other_windows_host = tauri::Url::parse("http://plugin.localhost/other/index.html").unwrap();
        // A navigation target using the plugin id as the *authority* instead
        // of the literal `localhost` Slice B's scheme requires — must be
        // denied even though the path segment happens to match.
        let wrong_authority = tauri::Url::parse("plugin://demo/index.html").unwrap();
        let remote = tauri::Url::parse("https://example.com/").unwrap();
        let data_uri = tauri::Url::parse("data:text/html,<script>1</script>").unwrap();

        assert!(is_navigation_allowed("demo", &own_plugin));
        assert!(is_navigation_allowed("demo", &own_windows));
        assert!(!is_navigation_allowed("demo", &other_plugin));
        assert!(!is_navigation_allowed("demo", &other_windows_host));
        assert!(!is_navigation_allowed("demo", &wrong_authority));
        assert!(!is_navigation_allowed("demo", &remote));
        assert!(!is_navigation_allowed("demo", &data_uri));
    }

    #[test]
    fn build_plugin_url_rejects_traversal_entry() {
        assert!(build_plugin_url("demo", "../../secret.html").is_err());
        assert!(build_plugin_url("demo", "/abs.html").is_err());
        assert!(build_plugin_url("demo", "index.html").is_ok());
    }

    #[test]
    fn storage_round_trips_through_a_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let set = storage_set(dir, "demo-plugin", &serde_json::json!({"key": "pos", "value": 42}));
        assert!(set.is_ok());
        let got = storage_get(dir, "demo-plugin", &serde_json::json!({"key": "pos"})).unwrap();
        assert_eq!(got, serde_json::json!(42));
        let missing = storage_get(dir, "demo-plugin", &serde_json::json!({"key": "nope"})).unwrap();
        assert_eq!(missing, Value::Null);
    }

    #[test]
    fn storage_is_namespaced_per_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        storage_set(dir, "plugin-a", &serde_json::json!({"key": "k", "value": "a-value"})).unwrap();
        storage_set(dir, "plugin-b", &serde_json::json!({"key": "k", "value": "b-value"})).unwrap();
        let a = storage_get(dir, "plugin-a", &serde_json::json!({"key": "k"})).unwrap();
        let b = storage_get(dir, "plugin-b", &serde_json::json!({"key": "k"})).unwrap();
        assert_eq!(a, serde_json::json!("a-value"));
        assert_eq!(b, serde_json::json!("b-value"));
    }

    #[test]
    fn fs_read_denies_traversal_outside_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("inside.txt"), "hi").unwrap();
        let root = tmp.path().to_str().unwrap();

        let ok = fs_read(root, &serde_json::json!({"path": "inside.txt"}));
        assert!(ok.is_ok());

        let escape = fs_read(root, &serde_json::json!({"path": "../outside.txt"}));
        let err = escape.unwrap_err();
        assert_eq!(err.code, "outside-root");
    }

    #[test]
    fn build_plugin_url_points_at_slice_b_address_space() {
        // localhost authority, id as the first path segment — the exact shape
        // plugins::parse_plugin_request_path (#360 Slice B) resolves.
        let url = build_plugin_url("demo", "index.html").unwrap();
        assert_eq!(url.scheme(), "plugin");
        assert_eq!(url.host_str(), Some("localhost"));
        assert_eq!(url.path(), "/demo/index.html");
    }
}
