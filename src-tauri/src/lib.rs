mod cliprobe;
mod editor;
mod git;
mod gitwatch;
mod winpath;
mod metrics;
mod obs;
pub mod orchestration; // pub: integration smoke test links through it
mod pty;
mod sessions;
pub mod usage; // pub: exercised by orchestration integration tests

use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Crash observability (issue #53): install the panic hook before anything
    // else so even a crash during setup leaves a log, then detect whether the
    // previous run exited uncleanly and arm this run's sentinel.
    obs::install_panic_hook();
    let startup = obs::check_and_arm();
    obs::breadcrumb(
        "startup",
        &format!(
            "v{} unclean_prev={}",
            env!("CARGO_PKG_VERSION"),
            startup.unclean
        ),
    );
    let startup_notice = obs::StartupNotice(std::sync::Mutex::new(startup.notice()));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(startup_notice)
        .manage(pty::PtyManager::default())
        .manage(Arc::new(gitwatch::GitWatcher::new()))
        .manage(Arc::new(orchestration::OrchRegistry::new(
            orchestration::OrchRegistry::default_root(),
        )))
        .setup(|app| {
            // Start streaming CPU/mem/GPU snapshots to the status bar.
            metrics::start(app.handle().clone());
            // Poll open panes' repos for external checkout/commit/stage (#36).
            let watcher = app.state::<Arc<gitwatch::GitWatcher>>().inner().clone();
            gitwatch::start(app.handle().clone(), watcher);
            // Orchestration MCP server: agents connect with per-pane tokens.
            let reg = app.state::<Arc<orchestration::OrchRegistry>>().inner().clone();
            reg.set_app(app.handle().clone());
            // Give the registry a handle to its own Arc so &self methods can
            // spawn background work (e.g. the copilot session watcher).
            reg.set_self_arc();
            orchestration::start_idle_reaper(reg.clone());
            orchestration::start_watchdog(reg.clone());
            orchestration::start_attention(reg.clone());
            orchestration::start_max_notice_flusher(reg.clone());
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
            git::git_fetch,
            git::git_push,
            git::git_pull,
            git::git_tag,
            git::git_branch_create,
            git::git_cherry_pick,
            git::git_revert,
            git::git_merge,
            git::git_rebase,
            git::git_branches,
            gitwatch::git_watch,
            gitwatch::git_unwatch,
            orchestration::create_orchestration,
            orchestration::bind_agent,
            orchestration::orch_agent_renamed,
            orchestration::orch_session_roles,
            orchestration::resume_orch_session,
            orchestration::orch_tasks,
            orchestration::orch_audit,
            orchestration::orch_steer,
            orchestration::orch_save_attachment,
            orchestration::orch_upsert_task,
            orchestration::orch_delete_task,
            orchestration::orch_reorder_tasks,
            orchestration::orch_open_ref,
            orchestration::orch_approve_task,
            orchestration::orch_request_changes,
            orchestration::orch_start_task,
            orchestration::orch_pause_group,
            orchestration::orch_resume_group,
            orchestration::orch_group_paused,
            orchestration::orch_ack_attention,
            orchestration::orch_ack_attention_pty,
            orchestration::orch_notify_enabled,
            orchestration::orch_set_notify,
            orchestration::orch_set_max_agents,
            orchestration::orch_group_usage,
            orchestration::orch_group_summary,
            orchestration::orch_end_group,
            cliprobe::probe_agent_cli,
            editor::open_in_editor,
            obs::take_startup_notice,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                obs::breadcrumb("shutdown", "window destroyed");
                let state: tauri::State<pty::PtyManager> = window.app_handle().state();
                state.kill_all();
                // Record a clean exit last, so a crash during teardown still
                // leaves the sentinel for the next launch to report.
                obs::mark_clean_exit();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running loomux");
}
