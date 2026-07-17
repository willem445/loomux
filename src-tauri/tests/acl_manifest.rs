//! ACL coherence + denial tests (#363, extended by #360 Slice C/D).
//!
//! This is the CI guard the #363 plan calls "the single most important
//! deliverable": the app manifest flip (`build.rs`) makes every command
//! without an explicit grant silently unreachable for every window,
//! including `main`. These tests turn that silent failure into a red test:
//!
//!   - `generate_handler_matches_app_commands` / `app_commands_len_is_128`:
//!     `src/lib.rs`'s `generate_handler!` and `command_manifest::APP_COMMANDS`
//!     are the two hand-maintained lists this migration depends on staying
//!     identical; this diffs them directly out of the `lib.rs` source rather
//!     than trusting a hand count. The count was 120 at #363, grew to 121
//!     with `orch_confirm_solo_copilot_autopilot` (#364/#365), grew to 123
//!     with #360 Slice B's `list_plugins`/`install_plugin`, grew to 126 with
//!     #360 Slice C's three broker commands (`plugin_open_window`,
//!     `plugin_broker_request`, `plugin_broker_open_channel`), grew to 127
//!     with #360 Slice D's `plugin_close_window` (the child-webview
//!     embedding's explicit-close command — see `pluginbroker.rs`'s module
//!     doc comment on why a child webview has no `WindowEvent::Destroyed` to
//!     hook cleanup onto instead), and grew again to 128 with #391's
//!     `plugin_set_occlusion` (folded into #380 — see `pluginregion.rs`'s
//!     module doc comment for the native z-order fix this command drives).
//!   - `main_has_all_128_and_zero_permission_denies_dangerous_spread`: builds
//!     a real (headless) `tauri::test` mock app using the app's *actual*
//!     `capabilities/`/`permissions/` on disk (via the same `generate_context!`
//!     `build.rs` already feeds — not a reimplementation of ACL resolution),
//!     invokes all 128 commands against the `main` webview label, and invokes
//!     a representative dangerous spread + a benign control against the
//!     `untrusted-probe-0` window label (see
//!     `capabilities/plugin-zero-template.json` — the label is deliberately
//!     NOT `plugin-*`-shaped, rev-65 NB-1, so it can't pick up
//!     `capabilities/plugin.json`'s broker grant and stays genuinely
//!     zero-permission). This is both the coherence test's A2 (every command
//!     reachable to main) and the plan's B (zero-permission denial), proven
//!     against the real resolver.
//!   - `plugin_capability_grants_only_broker_commands`: the #360 Slice C
//!     addition — proves a `plugin-*`-labeled webview (bound to
//!     `capabilities/plugin.json`) can reach exactly the two broker commands
//!     and nothing else, including the dangerous spread and the benign
//!     control that `main` gets and the zero-permission template doesn't.
//!   - `webview_scope_guard_denies_windows_scoped_leak_to_child_webview`: the
//!     #360 Slice D addition — the CI guard for the isolation prerequisite
//!     the multiwebview-embedding spike found (findings comment on #360,
//!     `fix/360-plugin-embed` commit e337c95): a `windows`-scoped grant on
//!     `main` (Tauri's own documented behavior — `Capability::windows`'s doc
//!     comment says it applies to "all the webviews of that window,
//!     regardless of the value of `webviews`") would silently hand a plugin's
//!     embedded child webview `main`'s ENTIRE command surface, with no test
//!     failure at the moment of the mistake, only a working exploit later.
//!     This test builds a real `main` window, `add_child`s a second webview
//!     labeled like a real plugin (`plugin-*`), and proves against the app's
//!     *actual* on-disk capabilities that the child is denied EVERY ONE of
//!     the 128 app commands except its own curated plugin-broker grant, while
//!     `main`'s own webview keeps every command — i.e. it tests the real
//!     shipped `default.json`/`plugin.json`, not a simulated ACL config, and
//!     it is deliberately a comprehensive sweep rather than a single-command
//!     canary (rev-89 NB-2): a canary on one command (the test's original
//!     shape, `pty_backend_info` only) only catches a `windows`-scoped leak
//!     on THAT command — a future capability that windows-scopes some OTHER
//!     app command would slip past a canary silently. Looping every command
//!     makes this guard catch the whole CLASS of mistake, not just the one
//!     instance of it the spike happened to find.
//!
//! Red-before-green (cited in the PR): dropping `orch_grant_merge` from
//! `permissions/sets/orch-control.toml` makes
//! `main_has_all_128_and_zero_permission_denies_dangerous_spread` fail with
//! `main is missing a grant for: ["orch_grant_merge"]`. Dropping
//! `allow-plugin-broker-request` from `permissions/sets/plugin-broker.toml`
//! makes `plugin_capability_grants_only_broker_commands` fail the same way.
//! Reverting `capabilities/default.json`'s `"webviews": ["main"]` back to
//! `"windows": ["main"]` makes
//! `webview_scope_guard_denies_windows_scoped_leak_to_child_webview` fail
//! with `child webview embedded in main leaked: [...128 commands...] — some
//! capability is granting an app command via windows: scope`. Adding a NEW,
//! otherwise-unrelated `windows`-scoped grant of a single different command
//! (e.g. a throwaway capability granting `orch_grant_merge` via
//! `windows: ["main"]`) makes the same test fail the same way, listing just
//! that one command — proving the guard catches the mistake class, not only
//! the specific `default.json` leak the spike originally found.

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
    orch_solo_prepare, orch_solo_bind, orch_confirm_solo_copilot_autopilot, orch_solo_adopt,
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
    plugin_open_window, plugin_close_window, plugin_broker_request, plugin_broker_open_channel,
    plugin_set_occlusion,
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

fn invoke_request(cmd: &str) -> tauri::webview::InvokeRequest {
    tauri::webview::InvokeRequest {
        cmd: cmd.into(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        url: LOCAL_ORIGIN_URL.parse().unwrap(),
        body: tauri::ipc::InvokeBody::default(),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_string(),
    }
}

fn invoke(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
    tauri::test::get_ipc_response(webview, invoke_request(cmd))
}

/// Same as [`invoke`], but for a plain `Webview` that ISN'T a `WebviewWindow`
/// — a real `add_child`-embedded plugin child webview, per #360 Slice D
/// (`webview_scope_guard_denies_windows_scoped_leak_to_child_webview`, below).
/// `Webview<R>` has no `AsRef<Webview<R>>` impl for `tauri::test::get_ipc_response`'s
/// generic bound to land on (only `WebviewWindow` does), so this calls the
/// same underlying `Webview::on_message` directly instead of going through
/// that helper.
fn invoke_webview(
    webview: &tauri::Webview<tauri::test::MockRuntime>,
    cmd: &str,
) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    webview.clone().on_message(
        invoke_request(cmd),
        Box::new(move |_window, _cmd, response, _callback, _error| {
            tx.send(response).unwrap();
        }),
    );
    match rx.recv().expect("failed to receive result from command") {
        tauri::ipc::InvokeResponse::Ok(b) => Ok(b),
        tauri::ipc::InvokeResponse::Err(tauri::ipc::InvokeError(v)) => Err(v),
    }
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
fn app_commands_len_is_128() {
    assert_eq!(
        loomux_lib::command_manifest::APP_COMMANDS.len(),
        128,
        "APP_COMMANDS drifted from the audited count of 128 (120 at #363 + 1 for \
         orch_confirm_solo_copilot_autopilot (#364/#365) + 2 for #360 Slice B's \
         list_plugins/install_plugin + 3 for #360 Slice C's pluginbroker commands + 1 for #360 \
         Slice D's plugin_close_window + 1 for #391's plugin_set_occlusion, folded into #380) — \
         if this is an intentional addition/removal, update the count here (and the relevant \
         issue's inventory, if that's the drift)"
    );
}

#[test]
fn main_has_all_128_and_zero_permission_denies_dangerous_spread() {
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
    // the #360 Slice C template; see that file's doc comment. Label is
    // deliberately NOT "plugin-*"-shaped (rev-65 NB-1): capabilities/plugin.json
    // binds that glob to the plugin-broker grant, so a label matching it would
    // no longer be a genuinely zero-grant probe — it would silently pick up
    // the two broker commands, diluting exactly the proof this test exists for.
    let zero_probe = tauri::WebviewWindowBuilder::new(&app, "untrusted-probe-0", Default::default())
        .build()
        .expect("failed to build the 'untrusted-probe-0' mock webview");

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
        let res = invoke(&zero_probe, cmd);
        assert!(
            res.is_err(),
            "zero-permission window ('untrusted-probe-0') should be DENIED {cmd}, got {res:?} — \
             the #360 Slice C zero-grant template must not leak a dangerous command"
        );
    }

    // The benign control is denied for the zero-permission window too (zero
    // permissions means zero) but allowed for main — proving this is a real
    // per-label ACL check, not a broken IPC pipe that would deny everything
    // globally (which would make the dangerous-spread denials meaningless).
    assert!(
        invoke(&zero_probe, BENIGN_CONTROL).is_err(),
        "zero-permission window should deny the benign control {BENIGN_CONTROL} too"
    );
    assert!(
        invoke(&main, BENIGN_CONTROL).is_ok(),
        "main should still allow the benign control {BENIGN_CONTROL} — if this fails, the deny \
         above is not per-label, it's global, and proves nothing"
    );
}

/// #360 Slice C: a real plugin window's capability (`capabilities/plugin.json`,
/// `webviews: ["plugin-*"]`) grants exactly the two broker commands and
/// nothing else — proven against the same real resolver as the test above,
/// against a webview label a real plugin child webview would actually get
/// (`pluginbroker::next_window_label` produces `plugin-<id>-<seq>`). Built as
/// a `WebviewWindow` (window label == webview label == "plugin-demo-0") for
/// simplicity — since `plugin.json` is `webviews`-scoped, the check is on the
/// webview label regardless of whether that label's window is a real
/// top-level window (as here) or a child of `main` (the real #360 Slice D
/// shape, exercised by `webview_scope_guard_denies_windows_scoped_leak_to_child_webview`
/// below).
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

    // A plugin cannot open or close another plugin webview (main-only),
    // cannot reach the dangerous spread, and doesn't even get the benign
    // control — a curated two-command grant, not main-ui, not zero-permission.
    const MUST_BE_DENIED: &[&str] = &[
        "plugin_open_window",
        "plugin_close_window",
        "plugin_set_occlusion",
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

/// #360 Slice D: the isolation prerequisite the multiwebview-embedding spike
/// found (findings comment on #360, `fix/360-plugin-embed` commit e337c95) —
/// see this file's module doc comment for the full rationale. Builds a real
/// `main` window, embeds a SECOND webview into it via `Window::add_child`
/// (exactly `pluginbroker::plugin_open_window`'s own shape) labeled like a
/// real plugin (`plugin-demo-0`), and proves — against the app's *actual*
/// on-disk `capabilities/`, not a simulated config — that the embedded child
/// is denied a `main-ui` command while `main`'s own webview keeps it. This is
/// the CI guard: reverting `capabilities/default.json`'s `webviews: ["main"]`
/// back to `windows: ["main"]` makes this test fail (see this file's module
/// doc comment for the exact failure message — verified red-before-green).
#[test]
fn webview_scope_guard_denies_windows_scoped_leak_to_child_webview() {
    let app = build_app();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build the 'main' mock webview");

    // The same shape `plugin_open_window` itself uses: `add_child` on main's
    // own `Window`, obtained via `AsRef<Webview<_>>::as_ref` + `Webview::window()`
    // — no separate top-level window is created, so this is a genuine
    // child-of-main scenario, not a relabeled WebviewWindow.
    let main_window =
        AsRef::<tauri::Webview<tauri::test::MockRuntime>>::as_ref(&main).window();
    let child = main_window
        .add_child(
            tauri::webview::WebviewBuilder::new("plugin-demo-0", Default::default()),
            tauri::LogicalPosition::new(0.0, 0.0),
            tauri::LogicalSize::new(1.0, 1.0),
        )
        .expect("failed to add the 'plugin-demo-0' child webview to main");

    assert_eq!(
        child.window().label(),
        "main",
        "sanity check: a child webview's WINDOW label must be its parent's (\"main\"), never its \
         own — this is exactly the ambiguity `windows`-scoped grants exploit"
    );

    // Comprehensive, not a single canary: every one of the 127 app commands must
    // be denied to the child webview EXCEPT the two the plugin capability
    // legitimately grants. A single-command probe (the original shape of this
    // test, `pty_backend_info` only) only catches a `windows`-scoped leak on
    // THAT one command — a future capability that windows-scopes some OTHER
    // command (say, a new main-only command added without checking this file)
    // would slip past it entirely. Looping every command instead makes this
    // guard catch the whole CLASS of mistake ("any windows-scoped app-command
    // grant leaks to a child webview"), proven against the real resolved ACL
    // (not a re-implementation of set-expansion logic), so it's blind to WHICH
    // capability file or WHICH command the mistake shows up in.
    const PLUGIN_ALLOWED: &[&str] = &["plugin_broker_request", "plugin_broker_open_channel"];
    let leaked: Vec<&str> = loomux_lib::command_manifest::APP_COMMANDS
        .iter()
        .copied()
        .filter(|cmd| !PLUGIN_ALLOWED.contains(cmd))
        .filter(|&cmd| invoke_webview(&child, cmd).is_ok())
        .collect();
    assert!(
        leaked.is_empty(),
        "child webview embedded in main leaked: {leaked:?} — some capability is granting an \
         app command via `windows:` scope instead of `webviews:` scope, which (per \
         Capability::windows's own doc comment) leaks to EVERY child webview of that window \
         regardless of the child's own label. The #360 multiwebview spike found this leak on \
         `capabilities/default.json`'s main grant specifically, but this guard is deliberately \
         not scoped to that one file or that one command."
    );

    // main's own webview must be UNAFFECTED by the webviews-scoping fix — every
    // app command still reachable. If this fails, the leak-denial above isn't a
    // real per-webview boundary, it's a broken pipe, and proves nothing.
    let denied_for_main: Vec<&str> = loomux_lib::command_manifest::APP_COMMANDS
        .iter()
        .filter(|&&cmd| invoke(&main, cmd).is_err())
        .copied()
        .collect();
    assert!(
        denied_for_main.is_empty(),
        "main is missing a grant for: {denied_for_main:?} — the webviews-scoping fix must not \
         narrow main's own coverage, only stop it leaking to embedded children"
    );

    // The child still gets exactly its curated plugin-broker grant — the fix
    // (`webviews`-scoping) isn't a blanket deny, it's a real per-label check.
    for &cmd in PLUGIN_ALLOWED {
        assert!(
            invoke_webview(&child, cmd).is_ok(),
            "the child webview's own plugin-* capability grant should still work after the \
             webviews-scoping fix ({cmd})"
        );
    }
}
