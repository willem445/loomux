//! Integration tests for the pane-plugins backend host (#360 Slice B).
//!
//! Must be an integration test, not a unit test: linking `loomux_lib` pulls in
//! the full UI dependency graph, and on Windows the resulting test exe only
//! loads because build.rs embeds the comctl32-v6 manifest via `-tests`-scoped
//! link args (CLAUDE.md constraint #4). These drive the public `plugins::*`
//! helpers the Tauri commands (`list_plugins`, `install_plugin`) wrap, so no
//! Tauri runtime is needed — same shape as `tests/fileedit.rs`.
//!
//! Every manifest-shape rule and the `plugin://` jail here is load-bearing
//! per `doc/design/pane-plugins.md` (the #360 Slice A contract): a manifest
//! violation is always a reject-with-reason, never a partial accept or a
//! silent coercion, and asset serving never resolves outside a plugin's own
//! folder.

use loomux_lib::pluginbroker::{validate_open_request, OpenPluginWindowRequest};
use loomux_lib::plugingrants::{
    approval_status, approve_capabilities, grants_path, is_approved, load_grants, record_grant,
    seed_declared_grants, ApprovalStatus, NOT_APPROVED_CODE, PRE_SEEDED_GRANT_PLUGIN_IDS,
};
use loomux_lib::plugins::{
    build_asset_response, discover_installed, install_plugin_from, manifest_for_installed, parse_manifest,
    resolve_plugin_asset, seed_bundled_example_plugin, BUNDLED_EXAMPLE_PLUGIN_ID, PLUGIN_CSP,
};
use std::fs;
use std::path::Path;

// ---------- helpers ----------

fn err_code(msg: &str) -> &str {
    msg.split(':').next().unwrap_or("").trim()
}

/// Build a manifest JSON string, letting each test override just the fields it
/// cares about. Mirrors the exact shape of the sample in
/// `doc/design/pane-plugins.md`.
fn manifest_json(id: &str, entry: &str, api_version: i64, capabilities: &[&str], rootless: bool) -> String {
    let caps = capabilities
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
            "id": "{id}",
            "name": "Test plugin",
            "version": "1.0.0",
            "apiVersion": {api_version},
            "entry": "{entry}",
            "capabilities": [{caps}],
            "rootless": {rootless}
        }}"#
    )
}

/// Write a plugin folder at `root/folder_name`: `plugin.json` plus an entry
/// HTML file (and any extra files) so discovery/install/asset-serving tests
/// have something real to read.
fn write_plugin_folder(root: &Path, folder_name: &str, manifest: &str, extra_files: &[(&str, &str)]) {
    let dir = root.join(folder_name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("plugin.json"), manifest).unwrap();
    for (rel, body) in extra_files {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }
}

fn try_symlink(original: &Path, link: &Path, is_dir: bool) -> bool {
    #[cfg(windows)]
    {
        let r = if is_dir {
            std::os::windows::fs::symlink_dir(original, link)
        } else {
            std::os::windows::fs::symlink_file(original, link)
        };
        r.is_ok()
    }
    #[cfg(unix)]
    {
        let _ = is_dir;
        std::os::unix::fs::symlink(original, link).is_ok()
    }
}

// ---------- manifest parsing / validation ----------

#[test]
fn design_note_example_manifest_is_valid() {
    // Pinned verbatim from doc/design/pane-plugins.md's sample manifest — if
    // this ever fails, the implementation and the public contract have drifted.
    let raw = r#"{
      "id": "resource-monitor",
      "name": "Resource monitor",
      "version": "1.0.0",
      "apiVersion": 1,
      "entry": "index.html",
      "capabilities": ["panel", "metrics.system"],
      "rootless": true
    }"#;
    let m = parse_manifest(raw).expect("design note's own example must validate");
    assert_eq!(m.id, "resource-monitor");
    assert_eq!(m.name, "Resource monitor");
    assert_eq!(m.api_version, 1);
    assert_eq!(m.entry, "index.html");
    assert_eq!(m.capabilities, vec!["panel".to_string(), "metrics.system".to_string()]);
    assert!(m.rootless);
}

#[test]
fn empty_capabilities_array_is_allowed() {
    let raw = manifest_json("plain", "index.html", 1, &[], false);
    let m = parse_manifest(&raw).expect("capabilities: [] is explicitly allowed");
    assert!(m.capabilities.is_empty());
}

#[test]
fn missing_required_fields_are_rejected_with_reason() {
    for field in ["id", "name", "version", "apiVersion", "entry", "capabilities"] {
        let full = serde_json::json!({
            "id": "p",
            "name": "P",
            "version": "1.0.0",
            "apiVersion": 1,
            "entry": "index.html",
            "capabilities": [],
        });
        let mut v = full.as_object().unwrap().clone();
        v.remove(field);
        let raw = serde_json::Value::Object(v).to_string();
        let e = parse_manifest(&raw).unwrap_err();
        assert_eq!(
            err_code(&e),
            "invalid-manifest",
            "missing `{field}` should fail closed, got: {e}"
        );
    }
}

#[test]
fn unknown_capability_is_rejected() {
    let raw = manifest_json("p", "index.html", 1, &["fs.write"], false);
    let e = parse_manifest(&raw).unwrap_err();
    assert_eq!(err_code(&e), "unknown-capability", "got: {e}");
}

#[test]
fn api_version_above_current_is_rejected() {
    // CURRENT_API_VERSION is 1 today; a plugin declaring the future is a newer
    // plugin on an older loomux — refused, per the design note's Versioning section.
    let raw = manifest_json("p", "index.html", 999, &[], false);
    let e = parse_manifest(&raw).unwrap_err();
    assert_eq!(err_code(&e), "unsupported-api-version", "got: {e}");
}

#[test]
fn api_version_zero_is_rejected() {
    let raw = manifest_json("p", "index.html", 0, &[], false);
    let e = parse_manifest(&raw).unwrap_err();
    assert_eq!(err_code(&e), "invalid-manifest", "got: {e}");
}

#[test]
fn rootless_plugin_cannot_declare_fs_read() {
    let raw = manifest_json("p", "index.html", 1, &["fs.read"], true);
    let e = parse_manifest(&raw).unwrap_err();
    assert_eq!(
        err_code(&e),
        "invalid-combination",
        "rootless + fs.read has no root to jail to — must be rejected, got: {e}"
    );
}

#[test]
fn absolute_entry_is_rejected() {
    // Forward slashes only — this is embedded into a JSON string literal by
    // `manifest_json`, and a literal `\` would need JSON escaping, which isn't
    // what this test is about. Windows accepts `/` as a separator, so
    // `C:/evil.html` is still absolute per `Path::is_absolute`.
    let abs = if cfg!(windows) { "C:/evil.html" } else { "/evil.html" };
    let raw = manifest_json("p", abs, 1, &[], false);
    let e = parse_manifest(&raw).unwrap_err();
    assert_eq!(err_code(&e), "invalid-entry", "got: {e}");
}

#[test]
fn malformed_json_is_rejected_not_panicking() {
    let e = parse_manifest("{ not json").unwrap_err();
    assert_eq!(err_code(&e), "invalid-json", "got: {e}");
}

#[test]
fn oversized_manifest_string_fields_are_rejected() {
    // rev-60 finding C: an abusive manifest can't carry unbounded strings.
    let base = serde_json::json!({
        "id": "p",
        "name": "P",
        "version": "1.0.0",
        "apiVersion": 1,
        "entry": "index.html",
        "capabilities": [],
    });
    let oversized: Vec<(&str, String)> = vec![
        ("id", "a".repeat(129)),
        ("name", "a".repeat(201)),
        ("version", "a".repeat(65)),
        ("entry", format!("{}.html", "a".repeat(512))),
    ];
    for (field, value) in oversized {
        let mut v = base.as_object().unwrap().clone();
        v.insert(field.to_string(), serde_json::Value::String(value));
        let raw = serde_json::Value::Object(v).to_string();
        let e = parse_manifest(&raw).unwrap_err();
        assert_eq!(
            err_code(&e),
            "invalid-manifest",
            "an oversized `{field}` should fail closed, got: {e}"
        );
    }
}

// ---------- discovery ----------

#[test]
fn discovery_finds_valid_plugin_and_skips_invalid_sibling() {
    let root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        root.path(),
        "good",
        &manifest_json("good", "index.html", 1, &["panel"], false),
        &[("index.html", "<h1>good</h1>")],
    );
    // Invalid: unknown capability. One bad folder must not block discovery of
    // the rest (design note's "one bad entry doesn't take down the rest").
    write_plugin_folder(
        root.path(),
        "bad",
        &manifest_json("bad", "index.html", 1, &["fs.write"], false),
        &[("index.html", "<h1>bad</h1>")],
    );
    let found = discover_installed(root.path());
    assert_eq!(found.len(), 1, "expected only the valid plugin, got: {found:?}");
    assert_eq!(found[0].id, "good");
}

#[test]
fn discovery_skips_folder_whose_id_does_not_match_its_own_folder_name() {
    let root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        root.path(),
        "folder-name",
        &manifest_json("different-id", "index.html", 1, &[], false),
        &[("index.html", "hi")],
    );
    let found = discover_installed(root.path());
    assert!(found.is_empty(), "id/folder-name mismatch must be skipped, got: {found:?}");
}

#[test]
fn discovery_skips_plugin_whose_entry_escapes_its_own_folder() {
    let root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        root.path(),
        "escapee",
        &manifest_json("escapee", "../outside.html", 1, &[], false),
        &[],
    );
    fs::write(root.path().join("outside.html"), "leaked").unwrap();
    let found = discover_installed(root.path());
    assert!(found.is_empty(), "an entry resolving outside the plugin folder must be skipped, got: {found:?}");
}

#[test]
fn discovery_on_missing_directory_returns_empty_not_error() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("does-not-exist");
    assert!(discover_installed(&missing).is_empty());
}

// ---------- CSP — the network-egress-denial contract (rev-60 finding A) ----------

#[test]
fn plugin_csp_denies_network_egress_and_further_embedding_and_navigation() {
    // Pins the exact directives the design note's "Content-Security-Policy on
    // plugin content" section requires, plus the reviewer-requested hardening
    // (form-action/base-uri) — so a future edit that loosens any one of them
    // (e.g. `connect-src 'none'` -> `connect-src *`) fails this test directly,
    // not just "the response still has *a* CSP header".
    //
    // #360 Slice C reconcile note: split into individual `;`-separated
    // directives and require an EXACT match, not `str::contains` — a bare
    // `.contains("connect-src 'none'")` would still pass against a silently
    // *loosened* `connect-src 'none' https://evil.com`, since that whole
    // string still contains the substring being checked for. Exact-directive
    // equality closes that gap.
    let directives: Vec<&str> = PLUGIN_CSP.split(';').map(str::trim).collect();
    for expected in [
        "connect-src 'none'",
        "frame-src 'none'",
        "object-src 'none'",
        "form-action 'none'",
        "base-uri 'none'",
    ] {
        assert!(
            directives.contains(&expected),
            "PLUGIN_CSP must have the exact directive `{expected}` — not merely contain it as a \
             substring of a looser, appended one — got: {PLUGIN_CSP}"
        );
    }
}

#[test]
fn every_plugin_response_carries_the_csp_header_on_success_and_on_error() {
    // rev-60 finding A: nothing pinned that plugin_protocol_handler/
    // build_asset_response actually attach PLUGIN_CSP on every branch — a
    // future refactor could drop it on just the success (or just the error)
    // path with no red test to catch it.
    let root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        root.path(),
        "widget",
        &manifest_json("widget", "index.html", 1, &[], false),
        &[("index.html", "<h1>widget</h1>")],
    );

    let ok = build_asset_response(root.path(), "/widget/index.html");
    assert_eq!(ok.status, 200);
    assert_eq!(ok.csp, PLUGIN_CSP, "a successful asset response must still carry the CSP");
    assert_eq!(ok.body, b"<h1>widget</h1>");

    let missing = build_asset_response(root.path(), "/nope/index.html");
    assert_eq!(missing.status, 404);
    assert_eq!(
        missing.csp, PLUGIN_CSP,
        "an error response (404) must carry the CSP just as much as a success — omitting it \
         on any branch silently falsifies the 'cannot phone home' guarantee"
    );
}

// ---------- plugin:// asset resolution — the traversal-rejection contract ----------

#[test]
fn resolve_plugin_asset_serves_the_manifest_entry_for_a_bare_id_request() {
    let root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        root.path(),
        "widget",
        &manifest_json("widget", "index.html", 1, &[], false),
        &[("index.html", "<h1>widget</h1>")],
    );
    let (bytes, mime) = resolve_plugin_asset(root.path(), "/widget").expect("bare id should serve the entry");
    assert_eq!(bytes, b"<h1>widget</h1>");
    assert_eq!(mime, "text/html");
}

#[test]
fn resolve_plugin_asset_serves_a_named_sibling_asset() {
    let root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        root.path(),
        "widget",
        &manifest_json("widget", "index.html", 1, &[], false),
        &[("index.html", "<h1>widget</h1>"), ("style.css", "body{color:red}")],
    );
    let (bytes, mime) = resolve_plugin_asset(root.path(), "/widget/style.css").unwrap();
    assert_eq!(bytes, b"body{color:red}");
    assert_eq!(mime, "text/css");
}

#[test]
fn resolve_plugin_asset_rejects_dot_dot_traversal_out_of_the_plugin_folder() {
    let root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        root.path(),
        "widget",
        &manifest_json("widget", "index.html", 1, &[], false),
        &[("index.html", "<h1>widget</h1>")],
    );
    // A secret sibling of the plugins root, and another plugin's folder — a
    // traversing request must reach neither.
    fs::write(root.path().join("secret.txt"), "TOP SECRET").unwrap();
    write_plugin_folder(
        root.path(),
        "other",
        &manifest_json("other", "index.html", 1, &[], false),
        &[("index.html", "<h1>other</h1>")],
    );

    let e = resolve_plugin_asset(root.path(), "/widget/../../secret.txt").unwrap_err();
    assert_eq!(err_code(&e), "outside-root", "got: {e}");

    let e2 = resolve_plugin_asset(root.path(), "/widget/../other/index.html").unwrap_err();
    assert_eq!(
        err_code(&e2),
        "outside-root",
        "a widget request must never resolve into another plugin's folder, got: {e2}"
    );
}

#[test]
fn resolve_plugin_asset_rejects_traversal_via_the_id_segment_itself() {
    // The id comes straight off the request path before any folder is joined,
    // so a `..`-laced id is the other half of the traversal surface (the
    // `dot_dot_traversal` test above covers escaping via the asset path).
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("secret.txt"), "TOP SECRET").unwrap();
    let e = resolve_plugin_asset(root.path(), "/../secret.txt").unwrap_err();
    assert_eq!(err_code(&e), "bad-request", "got: {e}");
}

#[test]
fn resolve_plugin_asset_rejects_unknown_plugin_id() {
    let root = tempfile::tempdir().unwrap();
    let e = resolve_plugin_asset(root.path(), "/nope/index.html").unwrap_err();
    assert_eq!(err_code(&e), "not-found", "got: {e}");
}

#[test]
fn resolve_plugin_asset_does_not_follow_a_symlink_out_of_the_plugin_folder() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "TOP SECRET").unwrap();
    write_plugin_folder(
        root.path(),
        "widget",
        &manifest_json("widget", "index.html", 1, &[], false),
        &[("index.html", "<h1>widget</h1>")],
    );
    let link = root.path().join("widget").join("escape");
    if !try_symlink(outside.path(), &link, true) {
        eprintln!("skipping symlink test: platform/permissions don't allow creating one");
        return;
    }
    let e = resolve_plugin_asset(root.path(), "/widget/escape/secret.txt").unwrap_err();
    assert_eq!(err_code(&e), "symlink", "got: {e}");
}

// ---------- install ----------

#[test]
fn install_copies_a_valid_plugin_folder_and_discovery_then_finds_it() {
    let source = tempfile::tempdir().unwrap();
    let plugins_root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        source.path(),
        "unused-source-folder-name", // install keys the dest off manifest.id, not the source folder's name
        &manifest_json("installed-one", "index.html", 1, &["panel"], false),
        &[("index.html", "<h1>hi</h1>")],
    );
    let src_plugin_dir = source.path().join("unused-source-folder-name");

    let manifest = install_plugin_from(&src_plugin_dir, plugins_root.path()).expect("valid install");
    assert_eq!(manifest.id, "installed-one");
    assert!(plugins_root.path().join("installed-one").join("plugin.json").is_file());
    assert!(plugins_root.path().join("installed-one").join("index.html").is_file());

    let found = discover_installed(plugins_root.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "installed-one");
}

#[test]
fn install_rejects_invalid_manifest_and_copies_nothing() {
    let source = tempfile::tempdir().unwrap();
    let plugins_root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        source.path(),
        "src",
        &manifest_json("bad-one", "index.html", 1, &["not-a-capability"], false),
        &[("index.html", "hi")],
    );
    let src_plugin_dir = source.path().join("src");

    let e = install_plugin_from(&src_plugin_dir, plugins_root.path()).unwrap_err();
    assert_eq!(err_code(&e), "unknown-capability", "got: {e}");
    assert!(
        !plugins_root.path().join("bad-one").exists(),
        "a rejected install must not copy anything"
    );
}

#[test]
fn install_rejects_entry_escaping_the_source_folder() {
    let source = tempfile::tempdir().unwrap();
    let plugins_root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        source.path(),
        "src",
        &manifest_json("escapee", "../outside.html", 1, &[], false),
        &[],
    );
    fs::write(source.path().join("outside.html"), "leaked").unwrap();
    let src_plugin_dir = source.path().join("src");

    let e = install_plugin_from(&src_plugin_dir, plugins_root.path()).unwrap_err();
    assert_eq!(err_code(&e), "outside-root", "got: {e}");
    assert!(!plugins_root.path().join("escapee").exists());
}

#[test]
fn install_rejects_a_plugin_id_that_is_not_a_single_path_segment() {
    let source = tempfile::tempdir().unwrap();
    let plugins_root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        source.path(),
        "src",
        &manifest_json("../escape", "index.html", 1, &[], false),
        &[("index.html", "hi")],
    );
    let src_plugin_dir = source.path().join("src");

    let e = install_plugin_from(&src_plugin_dir, plugins_root.path()).unwrap_err();
    assert_eq!(err_code(&e), "invalid-manifest", "got: {e}");
    // Nothing must land outside plugins_root itself.
    assert!(plugins_root.path().read_dir().unwrap().next().is_none());
}

#[test]
fn install_does_not_follow_a_symlink_out_of_the_source_folder() {
    let source = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let plugins_root = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "TOP SECRET").unwrap();
    write_plugin_folder(
        source.path(),
        "src",
        &manifest_json("linked", "index.html", 1, &[], false),
        &[("index.html", "hi")],
    );
    let src_plugin_dir = source.path().join("src");
    let link = src_plugin_dir.join("escape");
    if !try_symlink(outside.path(), &link, true) {
        eprintln!("skipping symlink test: platform/permissions don't allow creating one");
        return;
    }

    install_plugin_from(&src_plugin_dir, plugins_root.path()).expect("install itself still succeeds");
    assert!(
        !plugins_root.path().join("linked").join("escape").exists(),
        "a symlink inside the source folder must not be followed into the installed copy"
    );
}

#[test]
fn install_missing_source_is_not_found() {
    let plugins_root = tempfile::tempdir().unwrap();
    let e = install_plugin_from(Path::new("this-path-does-not-exist-anywhere"), plugins_root.path()).unwrap_err();
    assert_eq!(err_code(&e), "not-found", "got: {e}");
}

// ---------- bundled example seeding (#360 Slice F) ----------

#[test]
fn seed_bundled_example_plugin_installs_on_first_boot() {
    let resource_dir = tempfile::tempdir().unwrap();
    let plugins_root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        &resource_dir.path().join("plugins"),
        BUNDLED_EXAMPLE_PLUGIN_ID,
        &manifest_json(BUNDLED_EXAMPLE_PLUGIN_ID, "index.html", 1, &["panel", "metrics.system"], true),
        &[("index.html", "<h1>resource monitor</h1>")],
    );

    seed_bundled_example_plugin(resource_dir.path(), plugins_root.path());

    let found = discover_installed(plugins_root.path());
    assert_eq!(found.len(), 1, "expected the bundled example to be installed, got: {found:?}");
    assert_eq!(found[0].id, BUNDLED_EXAMPLE_PLUGIN_ID);
}

#[test]
fn seed_bundled_example_plugin_never_overwrites_an_already_installed_copy() {
    // A human who customized (or is mid-uninstall of) the bundled example
    // must not have it silently reseeded/reset on the next boot.
    let resource_dir = tempfile::tempdir().unwrap();
    let plugins_root = tempfile::tempdir().unwrap();
    write_plugin_folder(
        &resource_dir.path().join("plugins"),
        BUNDLED_EXAMPLE_PLUGIN_ID,
        &manifest_json(BUNDLED_EXAMPLE_PLUGIN_ID, "index.html", 1, &["panel", "metrics.system"], true),
        &[("index.html", "<h1>bundled build</h1>")],
    );
    write_plugin_folder(
        plugins_root.path(),
        BUNDLED_EXAMPLE_PLUGIN_ID,
        &manifest_json(BUNDLED_EXAMPLE_PLUGIN_ID, "index.html", 1, &["panel"], true),
        &[("index.html", "<h1>human-customized</h1>"), ("marker.txt", "do not touch")],
    );

    seed_bundled_example_plugin(resource_dir.path(), plugins_root.path());

    let installed_entry = plugins_root.path().join(BUNDLED_EXAMPLE_PLUGIN_ID).join("index.html");
    assert_eq!(
        fs::read_to_string(installed_entry).unwrap(),
        "<h1>human-customized</h1>",
        "an already-installed copy must never be reseeded/overwritten"
    );
    assert!(plugins_root.path().join(BUNDLED_EXAMPLE_PLUGIN_ID).join("marker.txt").is_file());
}

#[test]
fn seed_bundled_example_plugin_is_best_effort_when_the_resource_dir_has_nothing_to_seed() {
    // A `cargo test` (or a build where the resource wasn't unpacked) must not
    // panic or otherwise disrupt startup — the app just runs without the
    // example pre-installed.
    let resource_dir = tempfile::tempdir().unwrap();
    let plugins_root = tempfile::tempdir().unwrap();
    seed_bundled_example_plugin(resource_dir.path(), plugins_root.path());
    assert!(!plugins_root.path().join(BUNDLED_EXAMPLE_PLUGIN_ID).exists());
}

// ---------- #377: install-time capability approval gate ----------
//
// The consent boundary `doc/design/pane-plugins.md` promises: a plugin's
// declared capabilities are NOT live until a human has seen and approved them.
// The store and the subset rule live in `plugingrants.rs`; the enforcement
// point is `pluginbroker::validate_open_request`, which `plugin_open_window`
// calls before it builds any child webview. Everything below drives those two
// against a tempdir — never the real user data dir, and never by writing
// `grants.json` by hand (CLAUDE.md constraint 9: an agent must not
// self-approve a security gate, and that includes forging its store).

fn caps(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// An open request for `plugin_id` asking for `capabilities`. `root` is the
/// rootless SIGNAL only (#377 NB3) — `Some(_)` means "not rootless"; what the
/// string says is deliberately irrelevant, which
/// `fs_read_jail_root_is_derived_server_side_not_taken_from_the_request`
/// below is the proof of.
fn open_req(plugin_id: &str, capabilities: &[&str], root: Option<&str>) -> OpenPluginWindowRequest {
    OpenPluginWindowRequest {
        plugin_id: plugin_id.to_string(),
        entry: "index.html".to_string(),
        root: root.map(String::from),
        capabilities: caps(capabilities),
        api_version: 1,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    }
}

/// Install a real plugin folder declaring `capabilities` into `plugins_root`,
/// exactly as a human dropping a folder in would — the state every test below
/// starts from, since install is what #377 says must grant nothing.
fn install_declaring(plugins_root: &Path, id: &str, version: &str, capabilities: &[&str], rootless: bool) {
    let src = tempfile::tempdir().unwrap();
    let caps_json = capabilities
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        r#"{{
            "id": "{id}",
            "name": "Test plugin",
            "version": "{version}",
            "apiVersion": 1,
            "entry": "index.html",
            "capabilities": [{caps_json}],
            "rootless": {rootless}
        }}"#
    );
    write_plugin_folder(src.path(), id, &manifest, &[("index.html", "<h1>hi</h1>")]);
    install_plugin_from(&src.path().join(id), plugins_root).expect("install should succeed");
}

#[test]
fn installing_a_plugin_grants_nothing_and_open_is_refused_until_a_human_approves() {
    // The whole of #377 in one test: before this gate, this install WAS the
    // grant — the manifest's `capabilities` array went live the moment the
    // folder copy finished, with no human ever shown what it asked for.
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "1.0.0", &["storage", "fs.read"], false);

    assert!(
        load_grants(plugins_root.path()).is_empty(),
        "install must not write a grant — a folder on disk grants nothing"
    );

    let req = open_req("demo", &["storage", "fs.read"], Some("ignored"));
    let refusal = validate_open_request(plugins_root.path(), &req)
        .expect_err("an unapproved plugin must not be able to open");
    assert!(
        refusal.starts_with(format!("{NOT_APPROVED_CODE}: ").as_str()),
        "the refusal must lead with the stable code the frontend's consent surface branches on \
         (src/pluginhost.ts's pluginErrorCode), got: {refusal}"
    );
}

#[test]
fn approving_the_declared_set_lets_the_same_open_through() {
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "1.0.0", &["storage", "fs.read"], false);
    let req = open_req("demo", &["storage", "fs.read"], Some("ignored"));
    assert!(validate_open_request(plugins_root.path(), &req).is_err());

    approve_capabilities(plugins_root.path(), "demo", &caps(&["fs.read", "storage"]), 1_700_000_000)
        .expect("approving exactly what the manifest declares must succeed");

    let validated = validate_open_request(plugins_root.path(), &req)
        .expect("an approved plugin opens with the capabilities the human approved");
    assert_eq!(validated.granted.len(), 2);
}

#[test]
fn a_widened_manifest_re_prompts_and_names_only_the_newly_added_capability() {
    // The upgrade case: a plugin the human approved for `storage` ships a new
    // version that also wants `fs.read`. The old grant must not cover it, and
    // the re-prompt must be able to say WHAT changed rather than making the
    // human re-read the whole list.
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "1.0.0", &["storage"], false);
    approve_capabilities(plugins_root.path(), "demo", &caps(&["storage"]), 1).unwrap();

    install_declaring(plugins_root.path(), "demo", "2.0.0", &["storage", "fs.read"], false);
    let req = open_req("demo", &["storage", "fs.read"], Some("ignored"));
    assert!(
        validate_open_request(plugins_root.path(), &req).is_err(),
        "a widened manifest must NOT ride the earlier, narrower approval"
    );
    assert_eq!(
        approval_status(&load_grants(plugins_root.path()), "demo", &caps(&["storage", "fs.read"])),
        ApprovalStatus::Widened {
            added: caps(&["fs.read"])
        }
    );

    approve_capabilities(plugins_root.path(), "demo", &caps(&["storage", "fs.read"]), 2).unwrap();
    assert!(validate_open_request(plugins_root.path(), &req).is_ok());
}

#[test]
fn narrowing_a_manifest_needs_no_second_approval() {
    // Subset, not equality: the human already consented to strictly more than
    // what is now being asked for, so there is nothing new to ask about.
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "1.0.0", &["storage", "fs.read"], false);
    approve_capabilities(plugins_root.path(), "demo", &caps(&["storage", "fs.read"]), 1).unwrap();

    let narrowed = open_req("demo", &["storage"], Some("ignored"));
    assert!(validate_open_request(plugins_root.path(), &narrowed).is_ok());
    assert_eq!(
        approval_status(&load_grants(plugins_root.path()), "demo", &caps(&["storage"])),
        ApprovalStatus::Approved
    );
}

#[test]
fn a_grant_is_per_plugin_id_and_never_covers_another_plugin() {
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "approved", "1.0.0", &["storage"], false);
    install_declaring(plugins_root.path(), "other", "1.0.0", &["storage"], false);
    approve_capabilities(plugins_root.path(), "approved", &caps(&["storage"]), 1).unwrap();

    assert!(validate_open_request(plugins_root.path(), &open_req("approved", &["storage"], None)).is_ok());
    assert!(
        validate_open_request(plugins_root.path(), &open_req("other", &["storage"], None)).is_err(),
        "approving one plugin must never approve another — the store is keyed by plugin id"
    );
}

#[test]
fn a_corrupt_grants_file_fails_closed_and_is_quarantined_not_trusted() {
    // Same discipline as tabs.json (uistate.rs): the corrupt file is renamed
    // aside for a human to inspect, and the store reads as EMPTY — which
    // denies every plugin. Losing the file costs one re-approval; the other
    // direction would cost the consent boundary itself.
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "1.0.0", &["storage"], false);
    approve_capabilities(plugins_root.path(), "demo", &caps(&["storage"]), 1).unwrap();
    fs::write(grants_path(plugins_root.path()), "{ this is not json").unwrap();

    assert!(!is_approved(plugins_root.path(), "demo", &caps(&["storage"])));
    assert!(validate_open_request(plugins_root.path(), &open_req("demo", &["storage"], None)).is_err());
    assert!(
        grants_path(plugins_root.path()).with_extension("corrupt.json").is_file(),
        "the corrupt store must be quarantined for inspection, not silently deleted"
    );
}

#[test]
fn an_approval_records_the_sorted_set_and_the_version_that_was_on_disk() {
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "3.1.4", &["storage", "fs.read", "panel"], false);

    let record = approve_capabilities(
        plugins_root.path(),
        "demo",
        // deliberately unsorted, as a manifest's own array order would be
        &caps(&["storage", "panel", "fs.read"]),
        1_700_000_042,
    )
    .unwrap();

    assert_eq!(record.capabilities, caps(&["fs.read", "panel", "storage"]));
    assert_eq!(record.approved_at_version, "3.1.4");
    assert_eq!(record.decided_at, 1_700_000_042);

    let reloaded = load_grants(plugins_root.path());
    assert_eq!(reloaded.get("demo"), Some(&record), "the record must survive a round trip to disk");
}

#[test]
fn approving_a_set_the_manifest_no_longer_declares_is_refused_and_persists_nothing() {
    // The TOCTOU the approve command closes: the consent surface read the
    // manifest (via list_plugins), then the folder was rewritten before the
    // human pressed Approve. A consent to ["storage"] must never become a
    // grant of whatever the manifest declares NOW.
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "1.0.0", &["storage", "fs.read"], false);

    let e = approve_capabilities(plugins_root.path(), "demo", &caps(&["storage"]), 1)
        .expect_err("approving a set that isn't what the manifest declares must be refused");
    assert_eq!(err_code(&e), "manifest-changed", "got: {e}");
    assert!(
        load_grants(plugins_root.path()).is_empty(),
        "a refused approval must write nothing at all"
    );
}

#[test]
fn approving_an_uninstalled_or_traversing_plugin_id_is_refused() {
    let plugins_root = tempfile::tempdir().unwrap();
    assert!(approve_capabilities(plugins_root.path(), "nope", &caps(&["storage"]), 1).is_err());
    assert!(approve_capabilities(plugins_root.path(), "../escape", &caps(&["storage"]), 1).is_err());
    assert!(manifest_for_installed(plugins_root.path(), "../escape").is_err());
    assert!(load_grants(plugins_root.path()).is_empty());
}

#[test]
fn the_bundled_example_prompts_like_any_other_plugin_by_default() {
    // Pins the SHIPPED default of the open human decision (#377, plan §3's
    // "Bundled resource-monitor" bullet): loomux prompts for the bundled
    // example too — no capability is live without a human decision. Flipping
    // that is a human's call, not an agent's (CLAUDE.md constraint 9), and
    // this test is what would go red if someone flipped it quietly.
    assert!(
        PRE_SEEDED_GRANT_PLUGIN_IDS.is_empty(),
        "pre-seeding any plugin id is an OPEN HUMAN DECISION — see plugingrants.rs"
    );
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), BUNDLED_EXAMPLE_PLUGIN_ID, "1.0.0", &["metrics.system"], true);

    seed_declared_grants(plugins_root.path(), PRE_SEEDED_GRANT_PLUGIN_IDS, 1);
    assert!(load_grants(plugins_root.path()).is_empty());
    assert!(validate_open_request(
        plugins_root.path(),
        &open_req(BUNDLED_EXAMPLE_PLUGIN_ID, &["metrics.system"], None)
    )
    .is_err());
}

#[test]
fn pre_seeding_a_grant_is_possible_if_the_human_chooses_it() {
    // The other branch of that same decision, proven reachable so the choice
    // is a one-line list edit rather than a redesign: seeding an id grants
    // exactly what its own installed manifest declares, and the plugin then
    // opens with no prompt.
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), BUNDLED_EXAMPLE_PLUGIN_ID, "1.0.0", &["metrics.system"], true);

    seed_declared_grants(plugins_root.path(), &[BUNDLED_EXAMPLE_PLUGIN_ID], 1_700_000_000);

    let record = load_grants(plugins_root.path())
        .remove(BUNDLED_EXAMPLE_PLUGIN_ID)
        .expect("seeding must record a grant for the id it was given");
    assert_eq!(record.capabilities, caps(&["metrics.system"]));
    assert!(validate_open_request(
        plugins_root.path(),
        &open_req(BUNDLED_EXAMPLE_PLUGIN_ID, &["metrics.system"], None)
    )
    .is_ok());
}

#[test]
fn seeding_never_overwrites_a_decision_a_human_already_made() {
    // A human who approved a narrower set (or revoked one by editing the
    // store) keeps that across every later boot — the same rule
    // seed_bundled_example_plugin follows for the folder itself.
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "2.0.0", &["storage", "fs.read"], false);
    record_grant(plugins_root.path(), "demo", &caps(&["storage"]), "1.0.0", 5).unwrap();

    seed_declared_grants(plugins_root.path(), &["demo"], 9);

    let record = load_grants(plugins_root.path()).remove("demo").unwrap();
    assert_eq!(record.capabilities, caps(&["storage"]));
    assert_eq!(record.approved_at_version, "1.0.0");
    assert_eq!(record.decided_at, 5);
}

#[test]
fn seeding_an_uninstalled_plugin_is_a_silent_no_op() {
    // This runs during startup (lib.rs's .setup()); a missing or unparseable
    // plugin must never block boot, and must certainly never mint a grant.
    let plugins_root = tempfile::tempdir().unwrap();
    seed_declared_grants(plugins_root.path(), &["not-installed"], 1);
    assert!(load_grants(plugins_root.path()).is_empty());
}

#[test]
fn fs_read_jail_root_is_derived_server_side_not_taken_from_the_request() {
    // #377 NB3: `req.root` is a rootless SIGNAL, never a path to trust. The
    // jail `fs.read` is confined to is rebuilt from plugins_root + plugin id —
    // the identical folder the plugin:// scheme handler serves for that id —
    // so a caller cannot widen a plugin's read jail by sending a different
    // string, and the jail can never drift from the served address space.
    let plugins_root = tempfile::tempdir().unwrap();
    install_declaring(plugins_root.path(), "demo", "1.0.0", &["fs.read"], false);
    approve_capabilities(plugins_root.path(), "demo", &caps(&["fs.read"]), 1).unwrap();

    let hostile = open_req("demo", &["fs.read"], Some("/etc"));
    let validated = validate_open_request(plugins_root.path(), &hostile).unwrap();
    assert_eq!(
        validated.root.as_deref(),
        Some(plugins_root.path().join("demo").to_string_lossy().as_ref()),
        "the session's fs.read jail must be the plugin's own installed folder, whatever the \
         request asked for"
    );

    // `None` keeps its one meaning: this plugin is rootless, so fs.read has no
    // jail to resolve against and stays unreachable at the broker.
    let rootless = open_req("demo", &["fs.read"], None);
    assert_eq!(validate_open_request(plugins_root.path(), &rootless).unwrap().root, None);
}
