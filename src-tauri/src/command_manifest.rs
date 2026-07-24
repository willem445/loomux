// Single source of truth for the ACL manifest's command list (#363).
//
// `build.rs` cannot import compiled lib symbols, so this file is
// dependency-free and is pulled into both places with `include!`:
//   - `build.rs`, to build `tauri_build::AppManifest::new().commands(&APP_COMMANDS)`
//   - `lib.rs` (via `mod command_manifest;`), so `tests/acl_manifest.rs` can
//     diff this list against the `generate_handler!` block in `lib.rs` and
//     against the grants in `capabilities/default.json`.
//
// Names are the bare command identifiers Tauri's ACL codegen uses (the
// function name, not the `module::` path it's registered under in
// `generate_handler!`). Order mirrors `lib.rs`'s `generate_handler!` list so
// the two are easy to eyeball-diff; `tests/acl_manifest.rs` is what actually
// guarantees they agree.
//
// Adding a command to `generate_handler!`? Add its bare name here too, and
// grant it to `main` in `capabilities/default.json` (directly or via one of
// the permission sets under `permissions/`) — otherwise it silently breaks
// for every window, main included, per the #363 plan's all-or-nothing flip.
pub const APP_COMMANDS: &[&str] = &[
    // pty (8)
    "spawn_pty",
    "pty_backend_info",
    "write_pty",
    "resize_pty",
    "kill_pty",
    "dir_info",
    "change_dir",
    "discover_git_bash",
    // sessions (1)
    "list_sessions",
    // git (22)
    "git_repo_root",
    "git_log",
    "git_status",
    "git_diff",
    "git_commit_files",
    "git_stage",
    "git_unstage",
    "git_commit",
    "git_checkout",
    "git_discard",
    "git_worktree_add",
    "git_worktree_list",
    "git_fetch",
    "git_push",
    "git_pull",
    "git_tag",
    "git_branch_create",
    "git_cherry_pick",
    "git_revert",
    "git_merge",
    "git_rebase",
    "git_branches",
    // gh (9)
    "gh_auth_status",
    "gh_issue_list",
    "gh_issue_create",
    "gh_issue_set_labels",
    "gh_issue_view",
    "gh_issue_comment",
    "gh_pr_list",
    "gh_pr_view",
    "gh_pr_comment",
    // gitwatch (2)
    "git_watch",
    "git_unwatch",
    // orchestration (59)
    "agent_autopilot_flags",
    "create_orchestration",
    "bind_agent",
    "orch_agent_renamed",
    "orch_session_roles",
    "resume_orch_session",
    "orch_tasks",
    "orch_audit",
    "orch_steer",
    "orch_save_attachment",
    "orch_upsert_task",
    "orch_delete_task",
    "orch_delete_done_tasks",
    "orch_delete_tasks",
    "orch_reorder_tasks",
    "orch_open_ref",
    "orch_approve_task",
    "orch_grant_merge",
    "orch_grant_release",
    "orch_request_changes",
    "orch_start_task",
    "orch_proceed_task",
    "orch_pause_group",
    "orch_resume_group",
    "orch_group_paused",
    "orch_ack_attention",
    "orch_ack_attention_pty",
    "orch_notify_enabled",
    "orch_set_notify",
    "orch_spawn_expanded",
    "orch_set_spawn_expanded",
    "orch_set_max_agents",
    "orch_set_autonomous",
    "orch_set_auto_merge",
    "orch_set_auto_release",
    "orch_set_dangerous_mode",
    "orch_set_autonomy_budget",
    "orch_set_idle_tick_minutes",
    "orch_set_idle_activity_floor",
    "orch_set_compact_nudge_minutes",
    "orch_set_compact_nudge_roles",
    "orch_set_compact_context_threshold",
    "orch_autonomy",
    "orch_group_usage",
    "orch_group_summary",
    "orch_workflow_preview",
    "orch_set_advanced_orchestrator",
    "orch_workflow_status",
    "orch_group_watches",
    "orch_end_group",
    "orch_channel_connect",
    "orch_channel_disconnect",
    "orch_channel_list",
    "orch_channel_for_pane",
    "orch_channel_set_sender",
    "orch_solo_prepare",
    "orch_solo_bind",
    "orch_confirm_solo_copilot_autopilot",
    "orch_solo_adopt",
    // cliprobe (1)
    "probe_agent_cli",
    // editor (1)
    "open_in_editor",
    // fileedit (7)
    "ft_list_dir",
    "ft_read_file",
    "ft_write_file",
    "ft_search_start",
    "ft_search_cancel",
    "ft_files_start",
    "ft_replace",
    // filemgr (9)
    "fm_list",
    "fm_new_folder",
    "fm_new_file",
    "fm_rename",
    "fm_delete_start",
    "fm_capabilities",
    "fm_open",
    "fm_open_with",
    "fm_reveal",
    // filehash (1)
    "fm_hash_start",
    // obs (1)
    "take_startup_notice",
    // uistate (4)
    "load_ui_tabs",
    "save_ui_tabs",
    "load_settings",
    "save_settings",
    // voice (3)
    "voice_start",
    "voice_stop",
    "voice_cancel",
];
