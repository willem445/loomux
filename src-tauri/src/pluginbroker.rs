//! The pane-plugins trust core (#360 Slice C, Option B — see
//! `doc/design/pane-plugins.md`'s Isolation section and the Phase-0/Phase-0.5
//! spike findings on #360).
//!
//! **Slice D hosting (this module's [`plugin_open_window`]):** a plugin runs
//! in a child `Webview` embedded in the `main` window via `Window::add_child`
//! (the multiwebview API, `unstable` feature) — a real embedded pane, not a
//! separate top-level OS window (the #360 multiwebview spike,
//! `fix/360-plugin-embed` commit e337c95, and the follow-up findings comment
//! on #360). A child webview never calls raw `invoke`. Its capability
//! (`webviews: ["plugin-*"]` in `capabilities/plugin.json`) grants it exactly
//! two commands — [`plugin_broker_request`] and [`plugin_broker_open_channel`]
//! — and nothing else in the app's ~120+ command surface. Those two commands
//! *are* the broker: every other function in this module is the pure decision
//! logic and capability-scoped handlers they call.
//!
//! **Why `webviews`, not `windows`, in `capabilities/plugin.json` (and
//! `capabilities/default.json`'s own `main` grant):** `add_child` attaches
//! the plugin's webview to the *existing* `main` window, so its window label
//! is always `"main"` — never `plugin-*`. Tauri's ACL resolver grants a
//! command if EITHER the requesting webview's own label matches a
//! capability's `webviews` patterns OR its window's label matches `windows`
//! patterns (`tauri-utils::acl::capability::Capability`'s own doc comment:
//! a `windows`-scoped grant "will be enabled on all the webviews of that
//! window, regardless of the value of `webviews`"). A `windows: ["main"]`
//! grant — this repo's pre-Slice-D `default.json` — would therefore hand a
//! plugin's embedded child webview `main`'s entire command surface. Both
//! `default.json` and `plugin.json` are `webviews`-scoped for exactly this
//! reason; `tests/acl_manifest.rs`'s
//! `webview_scope_guard_denies_windows_scoped_leak_to_child_webview` is the
//! CI guard against ever reintroducing a `windows`-scoped grant here.
//!
//! **Why `invoke` instead of literal `postMessage`, unlike the design note's
//! iframe-era wording:** a child webview is a separate top-level browsing
//! context from `main`'s own document — `window.opener` is `null` (confirmed
//! by the Phase-0.5 spike, and again by the multiwebview spike) and there is
//! no live JS window reference between it and `main` at all, so a literal
//! `window.postMessage` bridge (the iframe model's transport) has nothing to
//! target. Tauri's own IPC channel to these two ACL-gated commands *is* the
//! postMessage-equivalent boundary for Option B: it replaces the iframe
//! model's `event.source === frame.contentWindow` identity check with
//! something structurally equivalent — the webview label + capability system
//! enforced by Tauri's resolver, checked before this module's code ever
//! runs. Host→plugin pushes (the `PluginEvent` direction: resize/theme/
//! metrics ticks) ride a `tauri::ipc::Channel`, opened once via
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
//! 1. **Identity** — enforced structurally: only a webview matching
//!    `capabilities/plugin.json`'s `webviews: ["plugin-*"]` pattern can reach
//!    [`plugin_broker_request`] at all, and this module then looks up that
//!    webview's own registered [`PluginSession`] by its (unforgeable) label.
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
//! [`plugin_open_window`] points its child `Webview` at the URLs that handler
//! serves (`plugin://localhost/<id>/<entry>` — see [`build_plugin_url`]) and
//! [`is_navigation_allowed`] locks navigation to that same address space;
//! neither resolves or serves an asset itself.
//!
//! **No `WindowEvent::Destroyed` for a child webview.** Unlike a top-level
//! window, a webview embedded via `add_child` never fires an OS-level
//! destroyed event Tauri can observe — [`plugin_close_window`] is the
//! explicit command the frontend calls on pane teardown instead (mirroring
//! this codebase's existing `killPty` pattern: `pluginpaneview.ts`'s
//! `dispose()` calls it fire-and-forget, the same posture as PTY teardown).

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

/// `metrics.system` capability: gated here, served by `procmetrics`'s
/// `sys_processes`-shaped backend (design note — "never exposed to a plugin
/// except through this one broker capability"). `metrics.subscribe`/
/// `metrics.unsubscribe` are the only methods `method_spec` maps to this
/// capability, so this fallback should be unreachable in practice — it exists
/// as a defensive default should the method table ever widen without this
/// match arm being updated to match.
fn metrics_not_implemented(method: &str) -> PluginErrorWire {
    PluginErrorWire {
        code: "not-implemented".into(),
        message: format!("`{method}` has no handler wired for the metrics.system capability"),
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

/// A plugin's child webview has no `sandbox=""` equivalent, so nothing stops
/// it self-navigating to a remote origin the way an `allow-top-navigation`-less
/// iframe would be stopped (Phase-0.5 spike finding: `location.href =
/// 'https://example.com/'` succeeded completely, confirmed unchanged for
/// `add_child` embedding by the multiwebview spike). This is the mitigation:
/// `WebviewBuilder::on_navigation` is wired to this pure predicate,
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

/// Sanitized, unique-per-open child-webview label: `plugin-<id>-<seq>`. Not a
/// security token (uniqueness only), so a plain atomic counter is correct
/// here per this crate's getrandom ban — see `Cargo.toml`.
fn next_window_label(plugin_id: &str) -> String {
    let seq = PLUGIN_WINDOW_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("plugin-{plugin_id}-{seq}")
}

/// Cleanup hook called from [`plugin_close_window`] — a child webview has no
/// `WindowEvent::Destroyed` for `lib.rs` to observe (see the module doc
/// comment), so this runs on the frontend's explicit close instead of a
/// window-lifecycle event. Mirrors the existing PTY-kill-on-close pattern.
pub fn on_plugin_webview_closed(label: &str) {
    with_sessions(|m| m.remove(label));
    with_channels(|m| m.remove(label));
}

// ---------- tauri commands ----------
//
// These two are the ONLY commands `capabilities/plugin.json` grants to a
// `plugin-*`-labeled webview (see `permissions/sets/plugin-broker.toml`).
// Both take `tauri::Webview`, not `tauri::WebviewWindow` — a plugin's child
// webview is not itself a top-level window (`Webview::is_webview_window()`
// is false for it), so `WebviewWindow`'s `CommandArg` impl would reject the
// call outright. Neither command ever returns a Tauri-level `Err` for a
// plugin-side mistake — a denied or malformed request is always a
// `PluginResponseWire { ok: false, .. }`, per the design note's
// error-surface contract ("never a thrown exception that could crash the
// plugin's frame").

#[tauri::command]
pub fn plugin_broker_request(
    webview: tauri::Webview,
    request: PluginRequestWire,
) -> PluginResponseWire {
    let label = webview.label().to_string();
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
        Ok(cap) => dispatch(&label, &session, cap, &request),
    }
}

fn dispatch(label: &str, session: &PluginSession, cap: Capability, req: &PluginRequestWire) -> PluginResponseWire {
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
        (Capability::MetricsSystem, "metrics.subscribe") => {
            crate::procmetrics::subscribe(label, &req.params)
        }
        (Capability::MetricsSystem, "metrics.unsubscribe") => crate::procmetrics::unsubscribe(label),
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
    webview: tauri::Webview,
    channel: tauri::ipc::Channel<PluginEventWire>,
) {
    let label = webview.label().to_string();
    with_channels(|m| {
        m.insert(label, channel);
    });
}

/// Push an unsolicited `PluginEvent` (resize/theme/metrics tick) to a plugin
/// webview that has opened its channel. Silently a no-op if it hasn't (or has
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
    /// The pane's own content-box position/size at open time, in logical
    /// pixels RELATIVE TO THE MAIN WINDOW'S OWN CLIENT AREA — `Window::add_child`
    /// positions a child webview against its parent window directly (standard
    /// Win32 child-window semantics), not in absolute screen coordinates, so
    /// unlike the overlay-window design this replaces there is no
    /// scale-factor/`innerPosition()` translation needed here or on the
    /// frontend (`pluginpaneview.ts`'s `reposition()`, which corrects
    /// `x`/`y`/`width`/`height` again immediately after open via the
    /// frontend's own `Webview.setPosition`/`setSize` — this is just the
    /// best-effort initial box, same role the old `width`/`height` played for
    /// `WebviewWindowBuilder::inner_size`).
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Every check that can fail on an [`OpenPluginWindowRequest`] — an unknown
/// capability string, or an `apiVersion` this build doesn't speak — run once,
/// here, and called by [`plugin_open_window`] *before* building the
/// `WebviewWindow` (rev-65 NB-2 on #369) specifically so a bad request never
/// gets as far as creating a real OS window that then has nothing registered
/// for it (fail-closed either way — no session means the broker denies
/// everything — but this way there's no stranded window to clean up at all).
fn validate_open_request(req: &OpenPluginWindowRequest) -> Result<Vec<Capability>, String> {
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
    Ok(granted)
}

/// Look up the registered session for a `plugin-*` webview label — the one
/// thing [`plugin_broker_request`] needs to know which capabilities/apiVersion
/// to check a request against. Keyed by the webview label Tauri itself
/// guarantees unique, not by plugin id (`plugins.rs`'s `plugin://` asset
/// resolution is entirely independent of this registry — it resolves
/// straight from `plugins::plugins_root_dir()`, not from anything Slice C
/// tracks).
pub fn session_for_window(label: &str) -> Option<PluginSession> {
    with_sessions(|m| m.get(label).cloned())
}

/// **Must stay `async fn`** — this is the documented Tauri/wry Windows
/// footgun (wry#583), which applies identically to `add_child`: both
/// `WebviewWindowBuilder::new` and the plain `WebviewBuilder::new` this
/// function now uses carry the same rustdoc warning verbatim ("this function
/// deadlocks when used in a synchronous command or event handlers... use
/// `async` commands"), because `Window::add_child` itself blocks on a
/// `channel::recv()` round-trip to the main thread
/// (`tauri::window::Window::add_child`'s own body) — the identical shape
/// `WebviewWindowBuilder::build()` has. A non-async `#[tauri::command]` runs
/// its body inline on the same WebView2/UI thread that dispatched the IPC
/// call; the round-trip then has nowhere to land and the whole app hangs
/// (the plugin surface paints nothing, and the main window freezes too,
/// since it's the same UI thread). Marking this `async` makes Tauri dispatch
/// the command body onto the async-runtime threadpool instead.
#[tauri::command]
pub async fn plugin_open_window(
    window: tauri::Window,
    req: OpenPluginWindowRequest,
) -> Result<String, String> {
    // Every fallible check runs BEFORE the webview is built (rev-65 NB-2 on
    // #369, carried over from the WebviewWindow design): building first and
    // validating after would leave a stranded, inert webview on screen if
    // e.g. `apiVersion` were rejected — fail-closed either way (no session
    // means the broker denies everything), but validating first means a bad
    // request never creates a real child webview at all, rather than one
    // that has to be cleaned up.
    let granted = validate_open_request(&req)?;
    let url = build_plugin_url(&req.plugin_id, &req.entry)?;

    let label = next_window_label(&req.plugin_id);
    let plugin_id_for_nav = req.plugin_id.clone();
    crate::obs::breadcrumb(
        "pluginbroker",
        &format!("plugin_open_window: label={label} url={url}"),
    );
    let builder = tauri::webview::WebviewBuilder::new(&label, tauri::WebviewUrl::External(url))
        // Devtools/right-click-Inspect for a sandboxed plugin webview: only
        // ever in a debug build. The `devtools` Cargo feature this crate does
        // NOT enable (see Cargo.toml) already makes this a structural no-op
        // in release regardless of the bool here — explicit anyway so the
        // intent isn't left to an implicit default.
        .devtools(cfg!(debug_assertions))
        .on_navigation(move |nav_url| {
            let allowed = is_navigation_allowed(&plugin_id_for_nav, nav_url);
            crate::obs::breadcrumb(
                "pluginbroker",
                &format!(
                    "plugin_open_window: navigation {} -> {}",
                    nav_url,
                    if allowed { "allow" } else { "deny" }
                ),
            );
            allowed
        });
    // `window` is the CALLING webview's own window — always "main", since
    // only main's capability grants this command (permissions/sets/misc.toml)
    // — so this embeds the child directly into main with no window lookup.
    // Position/size are relative to main's own client area (add_child's own
    // contract), matching `req.x`/`req.y`/`req.width`/`req.height`'s doc
    // comment on `OpenPluginWindowRequest`.
    window
        .add_child(
            builder,
            tauri::LogicalPosition::new(req.x, req.y),
            tauri::LogicalSize::new(req.width, req.height),
        )
        .map_err(|e| e.to_string())?;

    // Nothing below this point can fail: `granted`/`api_version` were already
    // validated above, so this is a plain insert, not a second fallible check.
    with_sessions(|m| {
        m.insert(
            label.clone(),
            PluginSession {
                plugin_id: req.plugin_id.clone(),
                root: req.root.clone(),
                granted,
                api_version: req.api_version,
            },
        );
    });
    Ok(label)
}

/// Explicit close for a plugin's child webview, called by `pluginpaneview.ts`
/// `dispose()` (fire-and-forget, mirroring the existing `killPty(...).catch(()
/// => {})` posture) — a child webview never fires `WindowEvent::Destroyed`
/// for `lib.rs` to hook cleanup onto the way a real top-level window closing
/// would (see this module's doc comment), so this command IS that hook:
/// closes the underlying webview AND releases the [`PluginSession`]/broker
/// channel/procmetrics-poll-thread state [`on_plugin_webview_closed`] and
/// `procmetrics::on_plugin_webview_closed` own. `label` is validated against
/// the `plugin-*` shape before touching anything — main is fully trusted
/// already, so this isn't a security boundary, just a guard against a stray
/// bug in main's own JS closing something that isn't a plugin webview (e.g.
/// "main" itself).
#[tauri::command]
pub fn plugin_close_window(app: tauri::AppHandle, label: String) -> Result<(), String> {
    if !label.starts_with("plugin-") {
        return Err(format!("refusing to close non-plugin webview label: {label}"));
    }
    if let Some(webview) = tauri::Manager::get_webview(&app, &label) {
        webview.close().map_err(|e| e.to_string())?;
    }
    on_plugin_webview_closed(&label);
    crate::procmetrics::on_plugin_webview_closed(&label);
    Ok(())
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
    fn check_request_denies_metrics_subscribe_without_capability() {
        // #360 Slice E: confirms wiring in the real data handler did not
        // weaken Slice C's gate — a session that never granted
        // `metrics.system` must still be denied `metrics.subscribe`.
        let granted = vec![Capability::Storage, Capability::FsRead];
        let r = req("metrics.subscribe", 1, Value::Null);
        let err = check_request(&granted, 1, &r).unwrap_err();
        assert_eq!(err.code, "capability-denied");
    }

    #[test]
    fn check_request_denies_metrics_unsubscribe_without_capability() {
        let granted: Vec<Capability> = vec![];
        let r = req("metrics.unsubscribe", 1, Value::Null);
        let err = check_request(&granted, 1, &r).unwrap_err();
        assert_eq!(err.code, "capability-denied");
    }

    #[test]
    fn dispatch_metrics_subscribe_is_wired_to_real_data_not_a_stub() {
        // Before #360 Slice E, this match arm returned `not-implemented`
        // unconditionally, even for a session that legitimately holds
        // `metrics.system`. This is the red-before-green for the wiring: on
        // the pre-Slice-E stub this asserts `resp.ok == false` with code
        // `not-implemented`; after wiring in `procmetrics`, a granted session
        // gets a real ack instead.
        let session = PluginSession {
            plugin_id: "demo".into(),
            root: None,
            granted: vec![Capability::MetricsSystem],
            api_version: 1,
        };
        let label = "test-dispatch-metrics-subscribe";

        let sub = req("metrics.subscribe", 1, Value::Null);
        let resp = dispatch(label, &session, Capability::MetricsSystem, &sub);
        assert!(
            resp.ok,
            "expected metrics.subscribe to succeed for a granted session, got {:?}",
            resp.error
        );
        assert!(resp.error.is_none());

        // Clean up the background poll thread `subscribe` starts.
        let unsub = req("metrics.unsubscribe", 1, Value::Null);
        let resp2 = dispatch(label, &session, Capability::MetricsSystem, &unsub);
        assert!(resp2.ok);
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

    fn open_request(capabilities: Vec<&str>, api_version: u32) -> OpenPluginWindowRequest {
        OpenPluginWindowRequest {
            plugin_id: "demo".into(),
            entry: "index.html".into(),
            root: None,
            capabilities: capabilities.into_iter().map(String::from).collect(),
            api_version,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        }
    }

    #[test]
    fn validate_open_request_rejects_bad_input_before_any_window_would_be_built() {
        // rev-65 NB-2 on #369: plugin_open_window calls this — and only this —
        // before touching the child webview builder, so a request like this
        // one (the exact `apiVersion: 999` example from the finding) never
        // gets as far as creating a real embedded webview.
        assert!(validate_open_request(&open_request(vec!["not-a-real-capability"], 1)).is_err());
        assert!(validate_open_request(&open_request(vec![], 999)).is_err());
        assert!(validate_open_request(&open_request(vec!["storage"], 1)).is_ok());
    }
}
