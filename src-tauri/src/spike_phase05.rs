// Phase-0.5 spike for #360 — throwaway, spike-branch-only code that proves or
// refutes Option B (child WebviewWindow + dedicated zero-permission
// capability) as the pane-plugin trust-core fallback. Does not ship; never
// merged past spike/360-sandbox-proof.
//
// Opens a second top-level WebviewWindow labeled "spike-plugin", bound (via
// capabilities/spike-plugin-zero.json) to a capability with an empty
// permissions array. Loads an adversarial page from the same dev server that
// attempts every escape route from the Phase-0 harness plus real invoke()
// calls against a representative spread of the 117 app commands.
#[tauri::command]
pub async fn spike_open_plugin_window(app: tauri::AppHandle) -> Result<(), String> {
    let url = tauri::Url::parse(
        "http://localhost:1420/dev-harness/360-sandbox-spike/plugin-window-adversary.html",
    )
    .map_err(|e| e.to_string())?;
    tauri::WebviewWindowBuilder::new(&app, "spike-plugin", tauri::WebviewUrl::External(url))
        .title("Phase-0.5 spike: plugin window (zero-permission capability)")
        .inner_size(480.0, 420.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}
