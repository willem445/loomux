//! Pane-plugins backend host (#360 Slice B): manifest parsing/validation,
//! local-folder discovery, install, and the `plugin://` asset scheme.
//!
//! This is the backend half of the contract `doc/design/pane-plugins.md`
//! (Slice A) fixes for every later slice. Two rules from that note are
//! load-bearing here and nowhere else in this module is either one relaxed:
//!
//!   * **A manifest violation always fails closed, with a specific reason —
//!     never a partial accept, never a silent coercion.** `parse_manifest` is
//!     the single place that rule is enforced; every caller (discovery,
//!     install, the design-note's own worked example) goes through it.
//!   * **`plugin://` never resolves outside a plugin's own installed folder.**
//!     `safe_resolve_in_plugin` is the one choke point both manifest
//!     validation (an `entry` must resolve *inside* its own folder) and the
//!     scheme handler route every path through — the same discipline
//!     `fileedit.rs`'s `safe_resolve` applies to the file-editor pane,
//!     duplicated here per this codebase's per-module house style: lexical
//!     `.`/`..` folding (never `fs::canonicalize`), then a symlink check on
//!     every component below the root.
//!
//! Every response the `plugin://` scheme handler returns — success or error —
//! carries the restrictive `Content-Security-Policy` header
//! `doc/design/pane-plugins.md`'s "Content-Security-Policy on plugin content"
//! section requires: `sandbox="allow-scripts"` (Slice C's concern) stops
//! DOM/storage/IPC reach but not network egress, so this header is the only
//! thing standing between a plugin and phoning home. Omitting it on even one
//! response (a 404 included) would silently falsify that threat-table row.
//!
//! Scope: this module is backend host only (Slice B). It does not implement
//! the broker, the sandboxed-frame wiring, or the `"plugin"` pane-kind union
//! member — those are Slices C and D, parallel/downstream work that builds on
//! the `list_plugins`/`install_plugin` commands and the scheme handler below.

use serde::Serialize;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Which broker wire contract this build implements (`doc/design/pane-plugins.md`
/// "Versioning"). A plugin declaring higher is a newer plugin on an older
/// loomux and is refused at manifest-validation time, not left to the broker
/// to discover later.
pub const CURRENT_API_VERSION: u32 = 1;

/// The closed capability enum (`doc/design/pane-plugins.md` "The v1 enum").
/// `capabilities` may only select from this set — never invent a new one, and
/// nothing widens what an entry here grants. `panel` is listed even though it
/// is implicit (every plugin gets it merely by existing as a pane) because
/// the design note's own sample manifest declares it explicitly; accepting it
/// as a harmless no-op keeps that example valid.
pub const ALLOWED_CAPABILITIES: &[&str] = &["panel", "storage", "fs.read", "metrics.system"];

/// Served on every `plugin://` response (`doc/design/pane-plugins.md`
/// "Content-Security-Policy on plugin content"): no network egress
/// whatsoever, assets limited to the plugin's own bundle, no further
/// frame/object embedding. `form-action`/`base-uri` are reviewer-requested
/// hardening beyond the design note's "at minimum" floor: `sandbox` tokens
/// alone don't stop a form submission or a `<base>`-tag rewrite of relative
/// URLs, so both are pinned closed here too, on the served-content side of
/// the guarantee (Slice C's `sandbox` attribute is the other, independent
/// half — see the module doc comment).
pub const PLUGIN_CSP: &str = "default-src 'self' plugin:; script-src 'self' plugin:; \
     img-src 'self' plugin:; style-src 'self' plugin:; connect-src 'none'; \
     frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'";

/// Length caps on untrusted manifest string fields — an abusive manifest
/// can't carry an unbounded `id`/`name`/`version`/`entry` string. Generous
/// enough for any real plugin (a slug id, a display name, a semver string, a
/// relative asset path) and small enough to bound the cost of holding and
/// echoing one back over IPC. Reject-with-reason past the cap, same as any
/// other manifest violation — never silently truncated.
const MAX_ID_LEN: usize = 128;
const MAX_NAME_LEN: usize = 200;
const MAX_VERSION_LEN: usize = 64;
const MAX_ENTRY_LEN: usize = 512;

/// A validated `plugin.json`. Every field here has already passed the
/// required-fields-fail-closed / closed-capability-enum / versioning rules in
/// `parse_manifest` — nothing downstream needs to re-check manifest shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub entry: String,
    pub capabilities: Vec<String>,
    pub rootless: bool,
}

/// Errors are plain strings (house style — see `fileedit.rs`/`git.rs`) with a
/// stable machine code before the first ": ", so a caller (a future
/// `src/pluginhost.ts`, or a test) can branch without parsing prose.
fn err(code: &str, msg: impl AsRef<str>) -> String {
    format!("{code}: {}", msg.as_ref())
}

/// Is `id` exactly one plain path segment — no separators, no `.`/`..`? This
/// is what makes an installed plugin's folder name and its `plugin://`
/// address space safe to derive directly from the manifest's own `id`.
fn is_single_segment(id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    let mut comps = Path::new(id).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

fn require_str(v: &Value, field: &str, max_len: usize) -> Result<String, String> {
    match v.get(field) {
        Some(Value::String(s)) if s.is_empty() => {
            Err(err("invalid-manifest", format!("`{field}` must not be empty")))
        }
        Some(Value::String(s)) if s.len() > max_len => Err(err(
            "invalid-manifest",
            format!("`{field}` exceeds the {max_len}-byte limit"),
        )),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(err("invalid-manifest", format!("`{field}` must be a string"))),
        None => Err(err("invalid-manifest", format!("missing required field `{field}`"))),
    }
}

fn require_api_version(v: &Value) -> Result<u32, String> {
    match v.get("apiVersion") {
        Some(Value::Number(n)) => n
            .as_u64()
            .filter(|&i| (1..=u32::MAX as u64).contains(&i))
            .map(|i| i as u32)
            .ok_or_else(|| err("invalid-manifest", "`apiVersion` must be a positive integer")),
        Some(_) => Err(err("invalid-manifest", "`apiVersion` must be an integer")),
        None => Err(err("invalid-manifest", "missing required field `apiVersion`")),
    }
}

fn require_capabilities(v: &Value) -> Result<Vec<String>, String> {
    match v.get("capabilities") {
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                match item {
                    Value::String(s) => {
                        if !ALLOWED_CAPABILITIES.contains(&s.as_str()) {
                            return Err(err("unknown-capability", format!("unknown capability: {s}")));
                        }
                        out.push(s.clone());
                    }
                    _ => return Err(err("invalid-manifest", "`capabilities` entries must be strings")),
                }
            }
            Ok(out)
        }
        Some(_) => Err(err("invalid-manifest", "`capabilities` must be an array")),
        None => Err(err("invalid-manifest", "missing required field `capabilities`")),
    }
}

fn optional_bool(v: &Value, field: &str, default: bool) -> Result<bool, String> {
    match v.get(field) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(err("invalid-manifest", format!("`{field}` must be a boolean"))),
    }
}

/// `entry` may only be a syntactically relative path — no absolute path, no
/// Windows drive/UNC prefix. Whether it actually resolves *inside* the
/// plugin's own folder needs a real folder on disk (a bare manifest string
/// can't prove that) and is checked separately, by `safe_resolve_in_plugin`,
/// at discovery/install time.
fn validate_entry_syntax(entry: &str) -> Result<(), String> {
    let p = Path::new(entry);
    if p.is_absolute()
        || p.has_root()
        || p.components().any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return Err(err("invalid-entry", format!("entry must be a relative path: {entry}")));
    }
    Ok(())
}

/// Parse and validate a `plugin.json` manifest against the closed contract in
/// `doc/design/pane-plugins.md`. Every violation fails closed with a stable
/// reason code — never a partial accept, never a guessed default — mirroring
/// the workflow-block `kind` precedent (`orchestration/workflow.rs`) the
/// design note cites directly.
pub fn parse_manifest(raw: &str) -> Result<PluginManifest, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| err("invalid-json", e.to_string()))?;
    if !v.is_object() {
        return Err(err("invalid-manifest", "manifest root must be a JSON object"));
    }

    let id = require_str(&v, "id", MAX_ID_LEN)?;
    if !is_single_segment(&id) {
        return Err(err(
            "invalid-manifest",
            format!("`id` must be a single path segment (no separators or `..`): {id}"),
        ));
    }
    let name = require_str(&v, "name", MAX_NAME_LEN)?;
    let version = require_str(&v, "version", MAX_VERSION_LEN)?;
    let api_version = require_api_version(&v)?;
    let entry = require_str(&v, "entry", MAX_ENTRY_LEN)?;
    validate_entry_syntax(&entry)?;
    let capabilities = require_capabilities(&v)?;
    let rootless = optional_bool(&v, "rootless", false)?;

    if api_version > CURRENT_API_VERSION {
        return Err(err(
            "unsupported-api-version",
            format!(
                "plugin declares apiVersion {api_version}; this loomux build implements up to {CURRENT_API_VERSION}"
            ),
        ));
    }
    if rootless && capabilities.iter().any(|c| c == "fs.read") {
        return Err(err(
            "invalid-combination",
            "a rootless plugin has no root to jail reads to, so it cannot declare `fs.read`",
        ));
    }

    Ok(PluginManifest {
        id,
        name,
        version,
        api_version,
        entry,
        capabilities,
        rootless,
    })
}

// ---------- path safety (duplicated from fileedit.rs per house style) ----------

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = Vec::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.iter().collect()
}

fn ensure_no_symlink(root: &Path, target: &Path) -> Result<(), String> {
    let rest = target
        .strip_prefix(root)
        .map_err(|_| err("outside-root", "path escapes the plugin folder"))?;
    let mut cur = root.to_path_buf();
    for comp in rest.components() {
        cur.push(comp);
        if let Ok(md) = std::fs::symlink_metadata(&cur) {
            if md.file_type().is_symlink() {
                return Err(err("symlink", format!("refusing to traverse symlink: {}", cur.display())));
            }
        }
    }
    Ok(())
}

/// Resolve `rel` (a manifest `entry`, or the tail of a `plugin://` request
/// path) to an absolute path strictly inside `plugin_dir`. The one choke
/// point every path — manifest-validation's entry-in-folder check and the
/// `plugin://` scheme handler alike — routes through.
fn safe_resolve_in_plugin(plugin_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    if !plugin_dir.is_dir() {
        return Err(err("not-found", format!("plugin not installed: {}", plugin_dir.display())));
    }
    let root_norm = lexical_normalize(plugin_dir);

    let rel_path = Path::new(rel);
    if rel_path.is_absolute() || rel_path.has_root() {
        return Err(err("invalid-path", format!("path must be relative: {rel}")));
    }
    if rel_path
        .components()
        .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return Err(err("invalid-path", format!("path must be relative: {rel}")));
    }

    let joined = lexical_normalize(&root_norm.join(rel_path));
    if !joined.starts_with(&root_norm) {
        return Err(err("outside-root", format!("path escapes the plugin folder: {rel}")));
    }
    ensure_no_symlink(&root_norm, &joined)?;
    Ok(joined)
}

// ---------- discovery ----------

/// `<user data dir>/loomux/plugins` — the recommended install location
/// (`doc/design/pane-plugins.md` "Install / discovery"), a sibling of
/// `orchestration/` and `tabs.json` under the same `<data dir>/loomux/…` tree
/// (see `orchestration::OrchRegistry::default_root`, `uistate::state_dir`).
/// The design note flags a repo-scoped `.loomux/plugins/` as a live
/// alternative pending human veto; every discovery/install function here
/// takes its root as a parameter for exactly that reason — adding or
/// switching to a second location is a call-site change, not a rewrite.
pub fn plugins_root_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("loomux")
        .join("plugins")
}

/// Load and fully validate the plugin installed at `plugin_dir`: its
/// `plugin.json` must parse, its declared `id` must match the folder's own
/// name (the folder name *is* the `plugin://` address-space key — see
/// `resolve_plugin_asset`), and its `entry` must resolve inside this exact
/// folder.
fn load_manifest_for(plugin_dir: &Path) -> Result<PluginManifest, String> {
    let dir_name = plugin_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| err("invalid-manifest", "plugin folder has no valid name"))?;
    let raw = std::fs::read_to_string(plugin_dir.join("plugin.json"))
        .map_err(|_| err("not-found", "plugin.json not found"))?;
    let manifest = parse_manifest(&raw)?;
    if manifest.id != dir_name {
        return Err(err(
            "id-mismatch",
            format!("manifest id `{}` does not match its folder name `{dir_name}`", manifest.id),
        ));
    }
    safe_resolve_in_plugin(plugin_dir, &manifest.entry)?;
    Ok(manifest)
}

/// Enumerate every installed plugin under `plugins_root`. A folder that isn't
/// a valid, self-consistent plugin (bad manifest, id/folder mismatch, an
/// entry escaping its own folder) is skipped, not an error that blocks
/// discovery of the rest — `doc/design/pane-plugins.md`'s "Install /
/// discovery" section states this explicitly, mirroring the workflow model's
/// audited-and-skipped failure policy. A missing `plugins_root` (nothing
/// installed yet) is simply an empty result, not an error.
pub fn discover_installed(plugins_root: &Path) -> Vec<PluginManifest> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(plugins_root) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(manifest) = load_manifest_for(&path) {
            out.push(manifest);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// ---------- install ----------

/// Copy `source_dir` (a plugin folder containing `plugin.json`) into
/// `plugins_root/<id>`, where `<id>` is the manifest's own declared id — never
/// the source folder's name, which is irrelevant. "Install" is exactly this
/// copy (`doc/design/pane-plugins.md`: "there is no build step, no
/// compilation, no fetch from anywhere"). A source whose manifest fails
/// validation is rejected with the specific reason and **nothing is copied**;
/// a source that would itself try to escape (a bad `id`, or an `entry`
/// resolving outside the source folder) is refused the same way.
/// Re-installing an id already present replaces it in place.
pub fn install_plugin_from(source_dir: &Path, plugins_root: &Path) -> Result<PluginManifest, String> {
    if !source_dir.is_dir() {
        return Err(err("not-found", format!("source is not a directory: {}", source_dir.display())));
    }
    let raw = std::fs::read_to_string(source_dir.join("plugin.json"))
        .map_err(|_| err("not-found", "plugin.json not found in source folder"))?;
    let manifest = parse_manifest(&raw)?;
    safe_resolve_in_plugin(source_dir, &manifest.entry)?;

    std::fs::create_dir_all(plugins_root).map_err(|e| err("io", e.to_string()))?;
    let plugins_root_norm = lexical_normalize(plugins_root);
    let dest = lexical_normalize(&plugins_root_norm.join(&manifest.id));
    // `manifest.id` is already known single-segment (parse_manifest), so this
    // can only ever be a direct child — checked again anyway, defense in
    // depth, matching fileedit.rs's own belt-and-suspenders discipline.
    if !dest.starts_with(&plugins_root_norm) || dest == plugins_root_norm {
        return Err(err("outside-root", "plugin id escapes the plugins directory"));
    }

    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| err("io", e.to_string()))?;
    }
    copy_dir_recursive(source_dir, &dest)?;
    Ok(manifest)
}

/// Recursively copy `src` into `dst`. A symlinked entry is never followed —
/// skipped outright, the same "shown/present but never traversed" discipline
/// `fileedit.rs` applies elsewhere — since a symlink inside a plugin's own
/// source folder is otherwise a way to smuggle files from outside it into the
/// installed copy.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| err("io", e.to_string()))?;
    for entry in std::fs::read_dir(src).map_err(|e| err("io", e.to_string()))? {
        let entry = entry.map_err(|e| err("io", e.to_string()))?;
        let ft = entry.file_type().map_err(|e| err("io", e.to_string()))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_symlink() {
            continue;
        } else if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to).map_err(|e| err("io", e.to_string()))?;
        }
    }
    Ok(())
}

// ---------- plugin:// asset resolution ----------

fn guess_mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Split a `plugin://` request path into `(plugin_id, asset_rel_path)`.
///
/// The address space lives entirely in the URL PATH, not the host: wry
/// rewrites a custom scheme's authority on Windows (`{scheme}://localhost/abc`
/// becomes `http://{scheme}.localhost/abc`, per wry's own `with_url` doc
/// comment) so only the path is guaranteed to survive intact across
/// platforms. The frontend (Slice C/D) builds `src` URLs as
/// `plugin://localhost/<id>/<rest>` (or bare `plugin://localhost/<id>` for the
/// manifest's declared `entry`) — this is the seam those slices attach to.
fn parse_plugin_request_path(path: &str) -> Result<(String, String), String> {
    let trimmed = path.trim_start_matches('/');
    let mut parts = trimmed.splitn(2, '/');
    let id = parts.next().unwrap_or("").to_string();
    let rest = parts.next().unwrap_or("").to_string();
    if !is_single_segment(&id) {
        return Err(err("bad-request", format!("invalid plugin id in request path: {id}")));
    }
    Ok((id, rest))
}

/// Resolve a `plugin://` request path to the asset bytes + MIME type it
/// names, strictly jailed to that one plugin's installed folder — never able
/// to resolve into another plugin's folder or anywhere else under
/// `plugins_root`, by construction (`safe_resolve_in_plugin`). A bare
/// `/<id>` request (no asset path) serves the manifest's own declared `entry`.
pub fn resolve_plugin_asset(plugins_root: &Path, request_path: &str) -> Result<(Vec<u8>, &'static str), String> {
    let (id, rel) = parse_plugin_request_path(request_path)?;
    let plugin_dir = plugins_root.join(&id);
    let rel = if rel.is_empty() {
        load_manifest_for(&plugin_dir)?.entry
    } else {
        rel
    };
    let asset_path = safe_resolve_in_plugin(&plugin_dir, &rel)?;
    if asset_path.is_dir() {
        return Err(err("is-dir", format!("path is a directory: {rel}")));
    }
    let bytes = std::fs::read(&asset_path).map_err(|e| err("io", e.to_string()))?;
    Ok((bytes, guess_mime(&asset_path)))
}

/// A `plugin://` response, independent of any Tauri runtime type. Pulling the
/// status/content-type/CSP/body decision out of `tauri::http` types means a
/// plain `#[test]` can pin that `PLUGIN_CSP` rides on every outcome —
/// including an error response — without needing a `UriSchemeContext`, which
/// only a running app can construct. `plugin_protocol_handler` below is the
/// thin, untested-by-necessity conversion of this into a real
/// `tauri::http::Response`.
pub struct AssetResponse {
    pub status: u16,
    pub content_type: String,
    pub csp: String,
    pub body: Vec<u8>,
}

/// Resolve a `plugin://` request into the exact response it gets: bytes +
/// MIME on success, a stable error string on failure — either way carrying
/// `PLUGIN_CSP`, so a future refactor that drops the header on one branch
/// (the design note's fear: "silently falsifies the threat table's network
/// row") shows up as a failing assertion here, not a silent regression.
pub fn build_asset_response(plugins_root: &Path, request_path: &str) -> AssetResponse {
    match resolve_plugin_asset(plugins_root, request_path) {
        Ok((bytes, mime)) => AssetResponse {
            status: 200,
            content_type: mime.to_string(),
            csp: PLUGIN_CSP.to_string(),
            body: bytes,
        },
        Err(e) => AssetResponse {
            status: 404,
            content_type: "text/plain; charset=utf-8".to_string(),
            csp: PLUGIN_CSP.to_string(),
            body: e.into_bytes(),
        },
    }
}

/// The `plugin://` scheme handler registered on the `Builder` in `lib.rs`.
/// All of the actual decision-making lives in `build_asset_response` (pure,
/// tested); this is only the conversion into a real `tauri::http::Response`.
/// This is the seam Slice C attaches its broker/sandboxed-frame wiring to;
/// nothing here forwards to `invoke` or grants a plugin anything beyond its
/// own folder's bytes.
pub fn plugin_protocol_handler(
    _ctx: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    let resp = build_asset_response(&plugins_root_dir(), request.uri().path());
    tauri::http::Response::builder()
        .status(resp.status)
        .header(tauri::http::header::CONTENT_TYPE, resp.content_type)
        .header(tauri::http::header::CONTENT_SECURITY_POLICY, resp.csp.clone())
        .body(resp.body)
        .unwrap_or_else(|_| {
            // Defensive only — reaching this means Response::builder itself
            // rejected a header/status this module constructed, which none of
            // the fixed inputs above should ever trigger. Still carries the
            // CSP: no response path, however unreachable, skips it.
            tauri::http::Response::builder()
                .status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR)
                .header(tauri::http::header::CONTENT_SECURITY_POLICY, resp.csp)
                .body(Vec::new())
                .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
        })
}

// ---------- bundled first-party example (#360 Slice F) ----------

/// The id of the one first-party example plugin loomux ships — the resource
/// monitor at `src-tauri/resources/plugins/resource-monitor/`
/// (`doc/design/pane-plugins.md`'s Open Decision #4: shipped **already
/// installed**, not merely bundled-but-requiring-the-picker, so the demo
/// works with zero setup). A single constant, not a registry: v1 ships
/// exactly one bundled example; a second one is a deliberate future addition,
/// not a case this needs to generalize for today.
pub const BUNDLED_EXAMPLE_PLUGIN_ID: &str = "resource-monitor";

/// Seed the bundled example plugin into `plugins_root` from wherever this
/// build's Tauri resources were unpacked (`resource_dir/plugins/<id>`, the
/// destination `tauri.conf.json`'s `bundle.resources` entry for this plugin
/// maps to). Called once, from `lib.rs`'s `.setup()`, on every boot —
/// `resource_dir`/`plugins_root` are both injected so this is testable
/// against tempdirs without a running Tauri app.
///
/// **Never overwrites an already-installed folder.** The bundled copy is
/// seeded once, the first time `plugins_root/<id>` is missing; a human who
/// customizes the installed copy, upgrades it, or deletes it to "uninstall"
/// the example keeps that choice across every later restart — reseeding on
/// every boot would silently undo an uninstall, which is not what "ships
/// already installed" is asking for.
pub fn seed_bundled_example_plugin(resource_dir: &Path, plugins_root: &Path) {
    let dest = plugins_root.join(BUNDLED_EXAMPLE_PLUGIN_ID);
    if dest.exists() {
        return;
    }
    let source = resource_dir.join("plugins").join(BUNDLED_EXAMPLE_PLUGIN_ID);
    if let Err(e) = install_plugin_from(&source, plugins_root) {
        // Best-effort: a missing/corrupt bundled resource must not block
        // startup — the human still has a working app, just without the
        // example pre-installed. Loud enough to find in the crash log, never
        // a dialog blocking the rest of boot.
        crate::obs::breadcrumb(
            "plugins",
            &format!("failed to seed bundled example plugin `{BUNDLED_EXAMPLE_PLUGIN_ID}`: {e}"),
        );
    }
}

// ---------- tauri commands ----------
//
// Thin wrappers: all logic lives in the `pub fn`s above so the integration
// test (`tests/plugins.rs`) can exercise it without a Tauri runtime.

#[tauri::command]
pub fn list_plugins() -> Vec<PluginManifest> {
    discover_installed(&plugins_root_dir())
}

#[tauri::command]
pub fn install_plugin(source: String) -> Result<PluginManifest, String> {
    install_plugin_from(Path::new(&source), &plugins_root_dir())
}
