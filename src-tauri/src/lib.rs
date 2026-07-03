mod git;
mod pty;
mod sessions;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(pty::PtyManager::default())
        .invoke_handler(tauri::generate_handler![
            pty::spawn_pty,
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
