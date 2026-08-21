mod blocking; // the shared P1 delegation helper for the #746 gesture commands
mod cliprobe;
pub mod command_manifest; // pub: the ACL coherence integration test links APP_COMMANDS (#363)
mod editor;
pub mod fileedit; // pub: the file-editor integration test links its pure fns (#174)
pub mod filehash; // pub: the hashing integration test links its pure fns (#214)
pub mod filemgr; // pub: the file-manager integration test links its pure fns (#214)
mod gh;
mod git;
mod gitwatch;
// winpath (#888 slice A4 batch 13) moved whole into loomux-engine — std +
// winreg only, no Tauri surface left behind, so the re-export is the entire
// module rather than a local shim file.
pub use loomux_engine::winpath;
mod metrics;
mod modelwire; // the list-models control probe (#993)
mod obs;
pub mod opencodedb; // pub: the #722 usage-readback integration tests link its reader
pub mod orchestration; // pub: integration smoke test links through it
pub mod pty; // pub: Job-Object integration test links `assign_kill_on_close_job`
pub mod ptyout; // pub: the #712 output-coalescing integration test drives `pty_output_pump`
pub mod rootreg; // pub: the #1042 declared-root integration tests link its admit helpers
pub mod sessions; // pub: the #412 resume-hardening integration tests fixture its store-lookup test seams
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
    // The version is passed IN rather than read inside `obs`: that module lives
    // in `loomux-engine` now (#888 slice A3 batch 7), whose crate version is a
    // permanent `0.0.0` placeholder, and `env!("CARGO_PKG_VERSION")` names the
    // crate it is written in. Here it names the release, which is what a crash
    // log has to say.
    obs::install_panic_hook(env!("CARGO_PKG_VERSION"));
    let startup = obs::check_and_arm();
    obs::breadcrumb(
        "startup",
        &format!(
            "v{} unclean_prev={} data_root={}",
            env!("CARGO_PKG_VERSION"),
            startup.unclean,
            obs::data_root().display()
        ),
    );
    let startup_notice = obs::StartupNotice(std::sync::Mutex::new(startup.notice()));

    // #1042 slice B. The declared-root registry is process-wide state with
    // exactly two populators: `admit_root` (the trusted webview) and the engine
    // itself, as it creates or resumes a group and as it cuts a worktree. It is
    // therefore CONSTRUCTED BY the orchestration registry — the engine-side
    // populator — rather than here and handed over: there is no `set_roots` to
    // forget, no `Option` to be `None` in a shipped build, and every
    // `OrchRegistry` (including every integration test's) has a live one. The
    // same `Arc` is `manage`d below so `#[tauri::command]`s reach it as
    // `State<Arc<RootRegistry>>`.
    let orch = Arc::new(orchestration::OrchRegistry::new(
        orchestration::OrchRegistry::default_root(),
    ));
    let roots = orch.roots();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(startup_notice)
        .manage(pty::PtyManager::default())
        .manage(voice::VoiceState::default())
        .manage(Arc::new(gitwatch::GitWatcher::new()))
        .manage(Arc::new(fileedit::SearchRegistry::default()))
        .manage(roots)
        .manage(orch)
        .setup(|app| {
            // Start streaming CPU/mem/GPU snapshots to the status bar.
            metrics::start(app.handle().clone());
            // #1020: detect each supported CLI's models once, in the
            // background, so every model picker opens already knowing what
            // this machine offers instead of waiting for a human to ask. This
            // is the ONLY path in loomux that reaches an agent CLI unbidden —
            // see `modelwire.rs`'s header for the #1002 direction behind it and
            // the boundary that keeps it the only one.
            modelwire::start_startup_sweep(app.handle().clone());
            // Poll open panes' repos for external checkout/commit/stage (#36).
            let watcher = app.state::<Arc<gitwatch::GitWatcher>>().inner().clone();
            gitwatch::start(app.handle().clone(), watcher);
            // Orchestration MCP server: agents connect with per-pane tokens.
            let reg = app.state::<Arc<orchestration::OrchRegistry>>().inner().clone();
            reg.set_app(app.handle().clone());
            // Give the registry a handle to its own Arc so &self methods can
            // spawn background work (e.g. the copilot session watcher).
            reg.set_self_arc();
            // #464: reclaim generated custom-agent files a group left behind
            // in ~/.claude/agents or ~/.copilot/agents without ever reaching
            // `end_group` (crash, kill -9, an orchestration-suite test run).
            // One-shot and best-effort, like the rest of this setup block —
            // see `sweep_orphaned_agent_files`'s doc for why this is safe.
            //
            // #502 fixed the biggest SOURCE of what this reclaims (a test
            // registry could write into the user's real agent dirs at all);
            // the sweep stays, because a crash or kill still skips
            // `end_group` for a genuine group.
            reg.sweep_orphaned_agent_files();
            orchestration::start_idle_reaper(reg.clone());
            orchestration::start_watchdog(reg.clone());
            orchestration::start_attention(reg.clone());
            orchestration::start_max_notice_flusher(reg.clone());
            orchestration::start_idle_tick(reg.clone());
            orchestration::start_compact_nudge(reg.clone());
            orchestration::start_disk_monitor(reg.clone());
            // #406: ONE background loop makes `gh` calls per app instance —
            // the notification backend (#243) and the idle-tick intake gate
            // (#332) are both serviced by this tick, sharing its clock and
            // its GitHub API budget. Adding a second `gh`-polling thread here
            // is the thing that issue exists to prevent.
            orchestration::start_gh_poller(reg.clone());
            orchestration::start_workflow_gate_reload(reg.clone());
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
            pty::discover_ssh,
            sessions::list_sessions,
            sessions::record_copilot_launch_posture,
            sessions::record_claude_launch_posture,
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
            gh::gh_label_vocabulary,
            gh::gh_issue_list,
            gh::gh_issue_create,
            gh::gh_issue_set_labels,
            gh::gh_issue_view,
            gh::gh_issue_comment,
            gh::gh_pr_list,
            gh::gh_pr_view,
            gh::gh_pr_comment,
            gh::gh_activity,
            gitwatch::git_watch,
            gitwatch::git_unwatch,
            orchestration::agent_autopilot_flags,
            orchestration::agent_cli_knobs,
            orchestration::create_orchestration,
            orchestration::promote_to_orchestrator,
            orchestration::bind_agent,
            orchestration::orch_agent_renamed,
            orchestration::orch_session_roles,
            orchestration::resume_orch_session,
            orchestration::orch_tasks,
            orchestration::orch_audit,
            orchestration::orch_merge_queue,
            orchestration::orch_steer,
            orchestration::orch_save_attachment,
            orchestration::orch_upsert_task,
            orchestration::orch_delete_task,
            orchestration::orch_delete_done_tasks,
            orchestration::orch_clear_done_tasks,
            orchestration::orch_restore_cleared_tasks,
            orchestration::orch_delete_tasks,
            orchestration::orch_reorder_tasks,
            orchestration::orch_open_ref,
            orchestration::orch_approve_task,
            orchestration::orch_approve_tasks,
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
            orchestration::orch_dismiss_stranded,
            orchestration::orch_notify_enabled,
            orchestration::orch_set_notify,
            orchestration::orch_spawn_expanded,
            orchestration::orch_set_spawn_expanded,
            orchestration::orch_set_max_agents,
            orchestration::orch_set_autonomous,
            orchestration::orch_set_auto_merge,
            orchestration::orch_set_auto_release,
            orchestration::orch_set_full_autonomy,
            orchestration::orch_set_dangerous_mode,
            orchestration::orch_set_autonomy_budget,
            orchestration::orch_set_idle_tick_minutes,
            orchestration::orch_set_idle_activity_floor,
            orchestration::orch_set_compact_nudge_minutes,
            orchestration::orch_set_compact_nudge_roles,
            orchestration::orch_set_compact_nudge_min_context_percent,
            orchestration::orch_set_compact_context_threshold,
            orchestration::orch_autonomy,
            orchestration::orch_group_usage,
            orchestration::orch_group_summary,
            orchestration::orch_workflow_preview,
            orchestration::orch_set_advanced_orchestrator,
            orchestration::orch_workflow_status,
            orchestration::orch_group_watches,
            orchestration::orch_lock_state,
            orchestration::orch_questions_list,
            orchestration::orch_question_answer,
            orchestration::orch_needs_you_list,
            orchestration::orch_needs_you_resolve,
            orchestration::orch_needs_you_clear,
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
            modelwire::list_cli_models,
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
            rootreg::admit_root,
            obs::take_startup_notice,
            uistate::load_ui_tabs,
            uistate::save_ui_tabs,
            uistate::load_settings,
            uistate::save_settings,
            uistate::load_ssh_profiles,
            uistate::save_ssh_profiles,
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
