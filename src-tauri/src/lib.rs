mod git;
mod metrics;
pub mod orchestration; // pub: integration smoke test links through it
mod pty;
mod sessions;

use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(pty::PtyManager::default())
        .manage(Arc::new(orchestration::OrchRegistry::new(
            orchestration::OrchRegistry::default_root(),
        )))
        .setup(|app| {
            // Start streaming CPU/mem/GPU snapshots to the status bar.
            metrics::start(app.handle().clone());
            // Orchestration MCP server: agents connect with per-pane tokens.
            let reg = app.state::<Arc<orchestration::OrchRegistry>>().inner().clone();
            reg.set_app(app.handle().clone());
            std::thread::spawn(move || orchestration::mcp::serve(reg));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            pty::spawn_pty,
            pty::pty_backend_info,
            pty::write_pty,
            pty::resize_pty,
            pty::kill_pty,
            pty::dir_info,
            pty::change_dir,
            sessions::list_sessions,
            git::git_repo_root,
            git::git_log,
            git::git_status,
            git::git_diff,
            git::git_commit_files,
            git::git_stage,
            git::git_unstage,
            git::git_commit,
            git::git_checkout,
            git::git_discard,
            git::git_worktree_add,
            orchestration::create_orchestration,
            orchestration::bind_agent,
            orchestration::orch_tasks,
            orchestration::orch_upsert_task,
            orchestration::orch_delete_task,
            orchestration::orch_reorder_tasks,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let state: tauri::State<pty::PtyManager> = window.app_handle().state();
                state.kill_all();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running loomux");
}
