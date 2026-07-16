//! ACL coherence + denial tests (#363, extended by #360 Slice C).
//!
//! This is the CI guard the #363 plan calls "the single most important
//! deliverable": the app manifest flip (`build.rs`) makes every command
//! without an explicit grant silently unreachable for every window,
//! including `main`. These tests turn that silent failure into a red test:
//!
//!   - `generate_handler_matches_app_commands` / `app_commands_len_is_125`:
//!     `src/lib.rs`'s `generate_handler!` and `command_manifest::APP_COMMANDS`
//!     are the two hand-maintained lists this migration depends on staying
//!     identical; this diffs them directly out of the `lib.rs` source rather
//!     than trusting a hand count. The count was 120 at #363, grew to 122
//!     with #360 Slice B's `list_plugins`/`install_plugin`, and grew again to
//!     125 with #360 Slice C's own three broker commands (`plugin_open_window`,
//!     `plugin_broker_request`, `plugin_broker_open_channel`).
//!   - `main_has_all_125_and_zero_permission_denies_dangerous_spread`: builds
//!     a real (headless) `tauri::test` mock app using the app's *actual*
//!     `capabilities/`/`permissions/` on disk (via the same `generate_context!`
//!     `build.rs` already feeds — not a reimplementation of ACL resolution),
//!     invokes all 125 commands against the `main` window label, and invokes
//!     a representative dangerous spread + a benign control against the
//!     `plugin-zero-template` window label (see
//!     `capabilities/plugin-zero-template.json`). This is both the coherence
//!     test's A2 (every command reachable to main) and the plan's B
//!     (zero-permission denial), proven against the real resolver.
//!   - `plugin_capability_grants_only_broker_commands`: the #360 Slice C
//!     addition — proves a `plugin-*`-labeled window (bound to
//!     `capabilities/plugin.json`) can reach exactly the two broker commands
//!     and nothing else, including the dangerous spread and the benign
//!     control that `main` gets and the zero-permission template doesn't.
//!
//! Red-before-green (cited in the PR): dropping `orch_grant_merge` from
//! `permissions/sets/orch-control.toml` makes
//! `main_has_all_125_and_zero_permission_denies_dangerous_spread` fail with
//! `main is missing a grant for: ["orch_grant_merge"]`. Dropping
//! `allow-plugin-broker-request` from `permissions/sets/plugin-broker.toml`
//! makes `plugin_capability_grants_only_broker_commands` fail the same way.

// Stub commands: same bare identifiers as the real commands in
// `src/command_manifest.rs` / `src/lib.rs`'s `generate_handler!`, but
// zero-arg no-ops — this file never touches real state or has side effects
// (no PTYs spawned, no git/gh/orchestration calls). It only exercises ACL
// *resolution* for each command name, which depends solely on the name
// matching a grant, not on the real function's signature or body.
macro_rules! stub_commands {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tauri::command]
            fn $name() {}
        )+

        const STUB_COMMAND_NAMES: &[&str] = &[$(stringify!($name)),+];

        fn build_app() -> tauri::App<tauri::test::MockRuntime> {
            tauri::test::mock_builder()
                .invoke_handler(tauri::generate_handler![$($name),+])
                .build(tauri::generate_context!())
                .expect("failed to build mock app from the real capabilities/permissions")
        }
    };
}

stub_commands!(
    spawn_pty, pty_backend_info, write_pty, resize_pty, kill_pty, dir_info, change_dir, discover_git_bash,
    list_sessions,
    git_repo_root, git_log, git_status, git_diff, git_commit_files, git_stage, git_unstage, git_commit,
    git_checkout, git_discard, git_worktree_add, git_worktree_list, git_fetch, git_push, git_pull, git_tag,
    git_branch_create, git_cherry_pick, git_revert, git_merge, git_rebase, git_branches,
    gh_auth_status, gh_issue_list, gh_issue_create, gh_issue_set_labels, gh_issue_view, gh_issue_comment,
    gh_pr_list, gh_pr_view, gh_pr_comment,
    git_watch, git_unwatch,
    agent_autopilot_flags, create_orchestration, bind_agent, orch_agent_renamed, orch_session_roles,
    resume_orch_session, orch_tasks, orch_audit, orch_steer, orch_save_attachment, orch_upsert_task,
    orch_delete_task, orch_delete_done_tasks, orch_delete_tasks, orch_reorder_tasks, orch_open_ref,
    orch_approve_task, orch_grant_merge, orch_grant_release, orch_request_changes, orch_start_task,
    orch_proceed_task, orch_pause_group, orch_resume_group, orch_group_paused, orch_ack_attention,
    orch_ack_attention_pty, orch_notify_enabled, orch_set_notify, orch_spawn_expanded,
    orch_set_spawn_expanded, orch_set_max_agents, orch_set_autonomous, orch_set_auto_merge,
    orch_set_auto_release, orch_set_dangerous_mode, orch_set_autonomy_budget, orch_set_idle_tick_minutes,
    orch_set_idle_activity_floor, orch_autonomy, orch_group_usage, orch_group_summary,
    orch_workflow_preview, orch_group_watches, orch_end_group, orch_channel_connect,
    orch_channel_disconnect, orch_channel_list, orch_channel_for_pane, orch_channel_set_sender,
    orch_solo_prepare, orch_solo_bind, orch_solo_adopt,
    probe_agent_cli,
    open_in_editor,
    ft_list_dir, ft_read_file, ft_write_file, ft_search_start, ft_search_cancel, ft_files_start, ft_replace,
    fm_list, fm_new_folder, fm_new_file, fm_rename, fm_delete_start, fm_capabilities, fm_open, fm_open_with,
    fm_reveal,
    fm_hash_start,
    list_plugins, install_plugin,
    take_startup_notice,
    load_ui_tabs, save_ui_tabs,
    voice_start, voice_stop, voice_cancel,
    plugin_open_window, plugin_broker_request, plugin_broker_open_channel,
);

// Tauri's "local origin" custom-protocol scheme differs by platform (WebView2
// needs an http(s) scheme, WKWebView/webkit2gtk accept a bare custom scheme):
// `http://tauri.localhost` on Windows/Android, `tauri://localhost` elsewhere
// (see `Webview::is_local_url` upstream). Using the Windows-only form here
// made every invoke resolve as a *remote* origin on Linux/macOS, which
// denies every command regardless of any grant — caught by CI (#363).
#[cfg(any(windows, target_os = "android"))]
const LOCAL_ORIGIN_URL: &str = "http://tauri.localhost";
#[cfg(not(any(windows, target_os = "android")))]
const LOCAL_ORIGIN_URL: &str = "tauri://localhost";

fn invoke(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
    tauri::test::get_ipc_response(
        webview,
        tauri::webview::InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: LOCAL_ORIGIN_URL.parse().unwrap(),
            body: tauri::ipc::InvokeBody::default(),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.to_string(),
        },
    )
}

/// Parses the bare command names out of `tauri::generate_handler![...]` in
/// `src/lib.rs` — the actual registration list, not a hand transcription of
/// it — so this test fails if a command is added/removed there without a
/// matching update to `command_manifest::APP_COMMANDS`.
fn parse_generate_handler_commands() -> Vec<String> {
    let lib_rs_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");
    let src = std::fs::read_to_string(lib_rs_path).expect("read src/lib.rs");
    let marker = "tauri::generate_handler![";
    let start = src
        .find(marker)
        .expect("tauri::generate_handler![ not found in src/lib.rs — did it move or get renamed?")
        + marker.len();
    let rest = &src[start..];
    let end = rest
        .find(']')
        .expect("no closing ] found for generate_handler! in src/lib.rs");
    rest[..end]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.rsplit("::").next().unwrap().to_string())
        .collect()
}

#[test]
fn generate_handler_matches_app_commands() {
    let mut from_lib = parse_generate_handler_commands();
    let mut from_const: Vec<String> = loomux_lib::command_manifest::APP_COMMANDS
        .iter()
        .map(|s| s.to_string())
        .collect();
    from_lib.sort();
    from_const.sort();
    assert_eq!(
        from_lib, from_const,
        "src/lib.rs's generate_handler! and command_manifest::APP_COMMANDS have diverged (#363) — \
         every command registered in one must be listed in the other, or it silently loses (or \
         never gets) its ACL grant"
    );
}

#[test]
fn app_commands_len_is_125() {
    assert_eq!(
        loomux_lib::command_manifest::APP_COMMANDS.len(),
        125,
        "APP_COMMANDS drifted from the audited count of 125 (120 at #363 + 2 for #360 Slice B's \
         list_plugins/install_plugin + 3 for #360 Slice C's pluginbroker commands) — if this is \
         an intentional addition/removal, update the count here (and the relevant issue's \
         inventory, if that's the drift)"
    );
}

#[test]
fn main_has_all_125_and_zero_permission_denies_dangerous_spread() {
    // Catches drift in *this test file* before it can mask a real gap: the
    // stub list above must match APP_COMMANDS exactly.
    let mut stub_names: Vec<&str> = STUB_COMMAND_NAMES.to_vec();
    let mut app_commands: Vec<&str> = loomux_lib::command_manifest::APP_COMMANDS.to_vec();
    stub_names.sort();
    app_commands.sort();
    assert_eq!(
        stub_names, app_commands,
        "tests/acl_manifest.rs's stub_commands! list has drifted from command_manifest::APP_COMMANDS \
         — update the stub_commands! invocation in this file to match"
    );

    let app = build_app();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build the 'main' mock webview");
    // Bound to capabilities/plugin-zero-template.json ("permissions": []) —
    // the #360 Slice C template; see that file's doc comment.
    let plugin = tauri::WebviewWindowBuilder::new(&app, "plugin-zero-template", Default::default())
        .build()
        .expect("failed to build the 'plugin-zero-template' mock webview");

    // --- Coherence (plan §5A2): every registered command must reach main. ---
    let denied_for_main: Vec<&str> = STUB_COMMAND_NAMES
        .iter()
        .filter(|&&cmd| invoke(&main, cmd).is_err())
        .copied()
        .collect();
    assert!(
        denied_for_main.is_empty(),
        "main is missing a grant for: {denied_for_main:?} — the #363 flip is all-or-nothing, so \
         an ungranted command silently breaks main. Grant it via capabilities/default.json or one \
         of the permissions/sets/*.toml sets aggregated into \"main-ui\"."
    );

    // --- Denial (plan §5B): representative dangerous spread + benign control,
    // proven against the zero-permission template, exactly the spread the
    // #360 Phase-0.5 spike's Check 1 table validated. `list_plugins`/
    // `install_plugin` (#360 Slice B) are appended per rev-60 finding B: they
    // were already denied here by construction (zero grants = zero commands,
    // same as the benign control), but pinning them explicitly guards against
    // a future curated-subset plugin capability (Slice C) accidentally
    // widening to include plugin management itself — a plugin pane must
    // never be able to enumerate or install plugins. ---
    const DANGEROUS_SPREAD: &[&str] = &[
        "orch_grant_merge",
        "git_push",
        "ft_write_file",
        "spawn_pty",
        "open_in_editor",
        "list_plugins",
        "install_plugin",
    ];
    const BENIGN_CONTROL: &str = "pty_backend_info";

    for &cmd in DANGEROUS_SPREAD {
        let res = invoke(&plugin, cmd);
        assert!(
            res.is_err(),
            "zero-permission window ('plugin-zero-template') should be DENIED {cmd}, got {res:?} — \
             the #360 Slice C zero-grant template must not leak a dangerous command"
        );
    }

    // The benign control is denied for the zero-permission window too (zero
    // permissions means zero) but allowed for main — proving this is a real
    // per-label ACL check, not a broken IPC pipe that would deny everything
    // globally (which would make the dangerous-spread denials meaningless).
    assert!(
        invoke(&plugin, BENIGN_CONTROL).is_err(),
        "zero-permission window should deny the benign control {BENIGN_CONTROL} too"
    );
    assert!(
        invoke(&main, BENIGN_CONTROL).is_ok(),
        "main should still allow the benign control {BENIGN_CONTROL} — if this fails, the deny \
         above is not per-label, it's global, and proves nothing"
    );
}

/// #360 Slice C: a real plugin window's capability (`capabilities/plugin.json`,
/// `windows: ["plugin-*"]`) grants exactly the two broker commands and nothing
/// else — proven against the same real resolver as the test above, against a
/// window label a real plugin window would actually get
/// (`pluginbroker::next_window_label` produces `plugin-<id>-<seq>`).
#[test]
fn plugin_capability_grants_only_broker_commands() {
    let app = build_app();
    let plugin_window = tauri::WebviewWindowBuilder::new(&app, "plugin-demo-0", Default::default())
        .build()
        .expect("failed to build the 'plugin-demo-0' mock webview");

    for &cmd in &["plugin_broker_request", "plugin_broker_open_channel"] {
        assert!(
            invoke(&plugin_window, cmd).is_ok(),
            "a plugin-* window must be granted {cmd} — check capabilities/plugin.json and \
             permissions/sets/plugin-broker.toml"
        );
    }

    // A plugin cannot open another plugin window (main-only), cannot reach
    // the dangerous spread, and doesn't even get the benign control — a
    // curated two-command grant, not main-ui, not zero-permission.
    const MUST_BE_DENIED: &[&str] = &[
        "plugin_open_window",
        "orch_grant_merge",
        "git_push",
        "ft_write_file",
        "spawn_pty",
        "open_in_editor",
        "pty_backend_info",
    ];
    for &cmd in MUST_BE_DENIED {
        assert!(
            invoke(&plugin_window, cmd).is_err(),
            "a plugin-* window should be DENIED {cmd}, but it was allowed — the plugin capability \
             must never widen past the two broker commands"
        );
    }
}
