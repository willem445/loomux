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

use loomux_lib::plugins::{
    build_asset_response, discover_installed, install_plugin_from, parse_manifest, resolve_plugin_asset,
    seed_bundled_example_plugin, BUNDLED_EXAMPLE_PLUGIN_ID, PLUGIN_CSP,
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
