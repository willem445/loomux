mod cliprobe;
pub mod command_manifest; // pub: the ACL coherence integration test links APP_COMMANDS (#363)
mod editor;
pub mod fileedit; // pub: the file-editor integration test links its pure fns (#174)
pub mod filehash; // pub: the hashing integration test links its pure fns (#214)
pub mod filemgr; // pub: the file-manager integration test links its pure fns (#214)
mod gh;
mod git;
mod gitwatch;
mod winpath;
mod metrics;
mod obs;
pub mod orchestration; // pub: integration smoke test links through it
pub mod plugins; // pub: the pane-plugins integration test links its pure fns (#360 Slice B)
pub mod pty; // pub: Job-Object integration test links `assign_kill_on_close_job`
mod sessions;
mod uistate; // durable UI state (project tabs, #63) — atomic tabs.json store
pub mod usage; // pub: exercised by orchestration integration tests
pub mod voice; // voice-prompt prototype (#58); pub: pure helpers are unit-tested

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
        // Pane plugins (#360 Slice B): serves each installed plugin's own
        // assets, jailed to its folder, with the CSP header the design note
        // requires on every response — see plugins::plugin_protocol_handler.
        .register_uri_scheme_protocol("plugin", plugins::plugin_protocol_handler)
        .manage(startup_notice)
        .manage(pty::PtyManager::default())
        .manage(voice::VoiceState::default())
        .manage(Arc::new(gitwatch::GitWatcher::new()))
        .manage(Arc::new(fileedit::SearchRegistry::default()))
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
            orchestration::start_idle_tick(reg.clone());
            orchestration::start_disk_monitor(reg.clone());
            orchestration::start_notify_poller(reg.clone());
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
            pty::discover_git_bash,
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
            git::git_worktree_list,
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
            gh::gh_auth_status,
            gh::gh_issue_list,
            gh::gh_issue_create,
            gh::gh_issue_set_labels,
            gh::gh_issue_view,
            gh::gh_issue_comment,
            gh::gh_pr_list,
            gh::gh_pr_view,
            gh::gh_pr_comment,
            gitwatch::git_watch,
            gitwatch::git_unwatch,
            orchestration::agent_autopilot_flags,
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
            orchestration::orch_delete_done_tasks,
            orchestration::orch_delete_tasks,
            orchestration::orch_reorder_tasks,
            orchestration::orch_open_ref,
            orchestration::orch_approve_task,
            orchestration::orch_grant_merge,
            orchestration::orch_grant_release,
            orchestration::orch_request_changes,
            orchestration::orch_start_task,
            orchestration::orch_proceed_task,
            orchestration::orch_pause_group,
            orchestration::orch_resume_group,
            orchestration::orch_group_paused,
            orchestration::orch_ack_attention,
            orchestration::orch_ack_attention_pty,
            orchestration::orch_notify_enabled,
            orchestration::orch_set_notify,
            orchestration::orch_spawn_expanded,
            orchestration::orch_set_spawn_expanded,
            orchestration::orch_set_max_agents,
            orchestration::orch_set_autonomous,
            orchestration::orch_set_auto_merge,
            orchestration::orch_set_auto_release,
            orchestration::orch_set_dangerous_mode,
            orchestration::orch_set_autonomy_budget,
            orchestration::orch_set_idle_tick_minutes,
            orchestration::orch_set_idle_activity_floor,
            orchestration::orch_autonomy,
            orchestration::orch_group_usage,
            orchestration::orch_group_summary,
            orchestration::orch_workflow_preview,
            orchestration::orch_group_watches,
            orchestration::orch_end_group,
            orchestration::orch_channel_connect,
            orchestration::orch_channel_disconnect,
            orchestration::orch_channel_list,
            orchestration::orch_channel_for_pane,
            orchestration::orch_channel_set_sender,
            orchestration::orch_solo_prepare,
            orchestration::orch_solo_bind,
            orchestration::orch_confirm_solo_copilot_autopilot,
            orchestration::orch_solo_adopt,
            cliprobe::probe_agent_cli,
            editor::open_in_editor,
            fileedit::ft_list_dir,
            fileedit::ft_read_file,
            fileedit::ft_write_file,
            fileedit::ft_search_start,
            fileedit::ft_search_cancel,
            fileedit::ft_files_start,
            fileedit::ft_replace,
            filemgr::fm_list,
            filemgr::fm_new_folder,
            filemgr::fm_new_file,
            filemgr::fm_rename,
            filemgr::fm_delete_start,
            filemgr::fm_capabilities,
            filemgr::fm_open,
            filemgr::fm_open_with,
            filemgr::fm_reveal,
            filehash::fm_hash_start,
            plugins::list_plugins,
            plugins::install_plugin,
            obs::take_startup_notice,
            uistate::load_ui_tabs,
            uistate::save_ui_tabs,
            voice::voice_start,
            voice::voice_stop,
            voice::voice_cancel,
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
