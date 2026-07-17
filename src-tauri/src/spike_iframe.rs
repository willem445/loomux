//! SPIKE-ONLY (#360 iframe isolation proof, `spike/360-iframe-isolation-proof`).
//! Not product code. Proves/disproves whether an in-DOM iframe hosting a
//! `plugin://` pane plugin can be denied `window.__TAURI_INTERNALS__` on this
//! exact Tauri 2.11.5 / wry 0.55.1 / WebView2 / Windows 10 baseline, per
//! `doc/design/pane-plugins.md`'s "Rejected: sandboxed opaque-origin iframe"
//! section. Delete this module, its two commands' `command_manifest.rs`
//! entries, `capabilities/spike-iframe.json`, and `spike-iframe-test.html`
//! before any real feature work lands on top of this branch.
//!
//! Opt-in only: `maybe_open` no-ops unless `LOOMUX_SPIKE_IFRAME_TEST` is set,
//! so a normal `npm run tauri dev` is unaffected.

/// Mitigation 1 under test: does `.initialization_script` (Tauri's own
/// main-frame-only API, `for_main_frame_only: true`) actually keep this
/// marker out of a `plugin://` subframe? Both wry's own
/// `InitializationScript::for_main_frame_only` doc comment and Tauri's
/// `WebviewBuilder::initialization_script` doc comment say no ("Windows:
/// scripts are always added to subframes... regardless of the
/// `for_main_frame_only` option") — this is the live check. If
/// `window.__spike_m1_marker` is readable from inside the plugin iframe, the
/// mitigation does not hold on this baseline.
const M1_MARKER_SCRIPT: &str = r"
  window.__spike_m1_marker = { protocol: location.protocol, href: location.href };
";

/// Mitigation 2 under test: a document-start script guarded to `plugin:`
/// frames only, that (a) tries to delete `__TAURI_INTERNALS__` (and failing
/// that, just `.invoke`) before the plugin's own scripts run, and (b) — the
/// real bottom-line check — tries an actual `invoke()` of a denied command
/// from inside the frame, reporting the outcome to the host via
/// `postMessage` (the only channel a `sandbox="allow-scripts"` frame without
/// `allow-same-origin` has out). Added via `initialization_script_for_all_frames`
/// so it is unambiguously requesting subframe delivery — on this Windows
/// baseline that is behaviorally identical to `initialization_script` (both
/// always reach subframes), which is itself part of what this spike is
/// proving.
const M2_SCRUB_AND_PROBE_SCRIPT: &str = r#"
(function () {
  // The `plugin://` scheme is rewritten by wry to `http://plugin.localhost/…`
  // on Windows (pluginbroker.rs's `build_plugin_url` doc comment) — this is
  // the on-the-wire form an iframe `src` actually navigates to, so that is
  // what identifies the plugin frame here, not `location.protocol`.
  if (location.hostname !== 'plugin.localhost') { return; }
  var report = { protocol: location.protocol, hostname: location.hostname, href: location.href };
  report.internalsTypeBefore = typeof window.__TAURI_INTERNALS__;
  report.m1MarkerLeaked = (typeof window.__spike_m1_marker !== 'undefined');

  var scrub = {};
  try { scrub.deleteWholeReturned = delete window.__TAURI_INTERNALS__; }
  catch (e) { scrub.deleteWholeThrew = String(e); }
  scrub.internalsTypeAfterWholeDelete = typeof window.__TAURI_INTERNALS__;
  try {
    if (window.__TAURI_INTERNALS__) {
      scrub.deleteInvokeReturned = delete window.__TAURI_INTERNALS__.invoke;
    }
  } catch (e) { scrub.deleteInvokeThrew = String(e); }
  try {
    if (window.__TAURI_INTERNALS__) {
      scrub.invokeTypeAfterDelete = typeof window.__TAURI_INTERNALS__.invoke;
    }
  } catch (e) { scrub.invokeTypeAfterDeleteThrew = String(e); }
  report.scrub = scrub;

  function finish(invokeAttempt) {
    report.invokeAttempt = invokeAttempt;
    try {
      window.parent.postMessage({ kind: 'spike-iframe-probe', report: report }, '*');
    } catch (e) { /* nothing more we can do from in here */ }
  }

  try {
    if (window.__TAURI_INTERNALS__ && typeof window.__TAURI_INTERNALS__.invoke === 'function') {
      window.__TAURI_INTERNALS__.invoke('spike_probe_marker', { caller: 'plugin-frame' })
        .then(function (r) { finish({ ok: true, result: r }); })
        .catch(function (e) { finish({ ok: false, error: String(e) }); });
    } else {
      finish({ ok: false, error: 'invoke not callable, typeof=' + typeof (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) });
    }
  } catch (e) {
    finish({ ok: false, error: 'threw before invoke: ' + String(e) });
  }
})();
"#;

/// Called from wherever still has a working `invoke` — the plugin frame (if
/// the leak/scrub-failure hypothesis holds) and the host page (as the
/// control: main-frame IPC must keep working regardless). `caller` says
/// which. Breadcrumbed rather than returned-and-trusted alone, so the
/// backend's own view of "who reached me" doesn't depend on the frame's
/// self-report being honest.
#[tauri::command]
pub fn spike_probe_marker(caller: String) -> String {
    crate::obs::breadcrumb("spike-iframe-marker-hit", &caller);
    format!("marker-reached-from:{caller}")
}

/// The host page's combined report (its own frame-probe relay plus its own
/// `invoke` check), written to the existing breadcrumb log
/// (`<data>/loomux/logs/breadcrumbs.log`) so the result can be read without
/// ever looking at the GUI.
#[tauri::command]
pub fn spike_report_probe(payload: serde_json::Value) {
    crate::obs::breadcrumb("spike-iframe-report", &payload.to_string());
}

/// Opens the throwaway harness window when `LOOMUX_SPIKE_IFRAME_TEST` is
/// set. Never wired to any menu, shortcut, or default codepath — dev-only,
/// opt-in, spike-only.
pub fn maybe_open<R: tauri::Runtime, M: tauri::Manager<R>>(app: &M) -> tauri::Result<()> {
    if std::env::var_os("LOOMUX_SPIKE_IFRAME_TEST").is_none() {
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(
        app,
        "spike-iframe-test",
        tauri::WebviewUrl::App("spike-iframe-test.html".into()),
    )
    .title("SPIKE: iframe isolation proof (#360)")
    .inner_size(900.0, 750.0)
    .initialization_script(M1_MARKER_SCRIPT)
    .initialization_script_for_all_frames(M2_SCRUB_AND_PROBE_SCRIPT)
    .build()?;
    Ok(())
}
