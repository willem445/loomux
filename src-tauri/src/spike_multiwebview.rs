//! THROWAWAY spike for #360 — does NOT ship, never referenced outside this
//! file and `dev-harness/360-multiwebview-spike/`. Proves or refutes whether
//! Tauri v2's multiwebview API (`Window::add_child`, behind the `unstable`
//! feature) can host a pane plugin as a real embedded region of the MAIN
//! window instead of a separate, decorated `WebviewWindow` (the overlay
//! approach `pluginbroker::plugin_open_window` currently uses).
//!
//! Opens a child `Webview` labeled `spike-child`, embedded in the `main`
//! window, loading `child-webview-adversary.html` from the dev server. That
//! page attempts `invoke("pty_backend_info")` — an existing, already-shipped
//! command granted to `main` via `capabilities/default.json`'s
//! `"windows": ["main"]` grant — to test whether a `windows`-scoped capability
//! leaks to a child webview of that window (Tauri's own doc comment on
//! `Capability::windows` says it does: "the capability will be enabled on all
//! the webviews of that window, regardless of the value of `webviews`").
//!
//! See the findings comment on #360 for the verdict.
#[tauri::command]
pub async fn spike_open_child_webview(window: tauri::Window) -> Result<(), String> {
    let url = tauri::Url::parse(
        "http://localhost:1420/dev-harness/360-multiwebview-spike/child-webview-adversary.html",
    )
    .map_err(|e| e.to_string())?;
    let builder = tauri::webview::WebviewBuilder::new("spike-child", tauri::WebviewUrl::External(url))
        .on_navigation(|_url| true);
    window
        .add_child(
            builder,
            tauri::LogicalPosition::new(40.0, 40.0),
            tauri::LogicalSize::new(480.0, 360.0),
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}
