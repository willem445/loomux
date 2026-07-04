//! Functional tests for the orchestration backend: guardrails, role authz,
//! group isolation, persistence, audit, and the MCP dispatch surface.
//!
//! These live as integration tests (not unit tests) because test executables
//! that link the full lib need the common-controls-v6 manifest embedded via
//! `rustc-link-arg-tests` (see build.rs / test.manifest), which cargo only
//! applies to integration-test targets.

use loomux_lib::orchestration::mcp::dispatch;
use loomux_lib::orchestration::{
    add_trusted_folder, bracketed_paste, cli_ready, create_orchestration_group, idle_should_kill,
    normalize_remote_web_base, parse_audit_lines, parse_session_cost, resolve_ref_url,
    rotate_audit_if_needed, spawn_rate_exceeded, strip_ansi, watchdog_should_notify,
    worktree_cleanup_targets, Caller, Guardrails, OrchRegistry, Role, TaskPatch,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

#[test]
fn kickoff_readiness_waits_for_painted_and_quiet_cli() {
    let s = Duration::from_secs;
    let ms = Duration::from_millis;
    // A slow-booting CLI (no output yet) is not ready no matter the elapsed
    // time inside the window — this is the race that ate a reviewer kickoff.
    assert!(!cli_ready(0, s(5), s(5)));
    // Output present but still actively painting (not quiet) → not ready.
    assert!(!cli_ready(4096, ms(100), s(5)));
    // Too early to judge, even if output looks settled.
    assert!(!cli_ready(4096, s(1), ms(800)));
    // Painted + quiet + past the minimum wait → ready.
    assert!(cli_ready(4096, s(2), s(3)));
}

fn rails() -> Guardrails {
    Guardrails {
        max_agents: 2,
        agent_cli: "claude".into(),
        worker_model: "sonnet".into(),
        reviewer_model: "sonnet".into(),
        orchestrator_model: "opus".into(),
        auto_ops: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
    }
}

fn test_registry() -> (OrchRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999); // fake port so config writing works
    (reg, dir)
}

// ---------- registry: guardrails, isolation, persistence, audit ----------

#[test]
fn guardrail_caps_live_agents() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w1", "t", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w2", "t", false, None).unwrap();
    let err = reg.spawn_agent(&g.id, Role::Worker, "w3", "t", false, None).unwrap_err();
    assert!(err.contains("guardrail"), "expected guardrail rejection, got: {err}");
    // A dead agent frees its slot.
    let id = reg.list_agents(&g.id)[0]["id"].as_str().unwrap().to_string();
    reg.mark_dead(&id, Some(0));
    reg.spawn_agent(&g.id, Role::Worker, "w3", "t", false, None).unwrap();
}

#[test]
fn guardrail_clamps_and_sanitizes() {
    let g = Guardrails {
        max_agents: 99,
        agent_cli: "definitely-not-a-cli".into(),
        worker_model: "sonnet; rm -rf /".into(),
        reviewer_model: "".into(),
        orchestrator_model: "opus".into(),
        auto_ops: true,
        idle_kill_minutes: 99999,
        max_spawns_per_hour: 9999,
        watchdog_stall_minutes: 99999,
    }
    .clamped();
    assert_eq!(g.max_agents, 12, "cap must clamp to the hard ceiling");
    assert_eq!(g.idle_kill_minutes, 1440, "idle-kill timeout clamps to 24h");
    assert_eq!(g.max_spawns_per_hour, 240, "spawn-rate cap clamps to the ceiling");
    assert_eq!(g.watchdog_stall_minutes, 1440, "watchdog stall timeout clamps to 24h");
    assert_eq!(g.agent_cli, "claude", "unknown CLIs fall back to claude explicitly");
    assert_eq!(g.worker_model, "sonnetrm-rf", "shell metacharacters must be stripped");
    assert_eq!(g.reviewer_model, "sonnet", "empty model falls back to default");
    // Copilot's fallback model is "auto" (it picks the best itself).
    let g = Guardrails {
        max_agents: 4,
        agent_cli: "copilot".into(),
        worker_model: "".into(),
        reviewer_model: "".into(),
        orchestrator_model: "".into(),
        auto_ops: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
    }
    .clamped();
    assert_eq!(g.worker_model, "auto");
    assert_eq!(g.orchestrator_model, "auto");
}

#[test]
fn agent_config_carries_token_and_server_url() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo2", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    let cfg = fs::read_to_string(
        reg.state_root().join(&g.id).join("configs").join(format!("{}.json", w.id)),
    )
    .unwrap();
    assert!(cfg.contains(&w.token), "config must carry the agent token");
    assert!(cfg.contains("127.0.0.1:45999/mcp"));
}

#[test]
fn token_resolution_and_group_isolation() {
    let (reg, _d) = test_registry();
    let ga = reg.create_group("C:/tmp/repo-a", rails()).unwrap();
    let gb = reg.create_group("C:/tmp/repo-b", rails()).unwrap();
    let wa = reg.spawn_agent(&ga.id, Role::Worker, "wa", "t", false, None).unwrap();
    let wb = reg.spawn_agent(&gb.id, Role::Worker, "wb", "t", false, None).unwrap();
    let ca = reg.resolve_token(&wa.token).unwrap();
    assert_eq!(ca.group, ga.id);
    // Group A's roster never shows group B's agents.
    let roster = reg.list_agents(&ga.id).to_string();
    assert!(roster.contains(&wa.id) && !roster.contains(&wb.id));
    // Dead agents lose their token entirely.
    reg.mark_dead(&wa.id, Some(1));
    assert!(reg.resolve_token(&wa.token).is_none());
    assert!(reg.resolve_token("no-such-token").is_none());
}

#[test]
fn state_persists_across_registry_instances() {
    let dir = tempfile::tempdir().unwrap();
    let gid;
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
        gid = g.id.clone();
        reg.set_state(&g.id, r#"{"queue":[12,13]}"#).unwrap();
    }
    // Fresh instance (app restart) + same repo → same group id and state.
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    assert_eq!(g.id, gid, "group id must be stable per repo for resume");
    assert_eq!(reg.get_state(&g.id), r#"{"queue":[12,13]}"#);
}

#[test]
fn group_id_normalizes_path_but_separates_repos() {
    let (reg, _d) = test_registry();
    let a = reg.create_group("C:\\Tmp\\Repo", rails()).unwrap();
    let b = reg.create_group("c:/tmp/repo/", rails()).unwrap();
    let c = reg.create_group("C:/tmp/other", rails()).unwrap();
    assert_eq!(a.id, b.id, "case/separator/trailing-slash variants are the same repo");
    assert_ne!(a.id, c.id, "different repos must not share state");
}

#[test]
fn state_rejects_invalid_input() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    assert!(reg.set_state(&g.id, "not json").is_err());
    let huge = format!("{{\"x\":\"{}\"}}", "a".repeat(512 * 1024));
    assert!(reg.set_state(&g.id, &huge).is_err());
    assert_eq!(reg.get_state(&g.id), "{}", "failed writes must not corrupt state");
}

#[test]
fn audit_log_records_lifecycle_as_json_lines() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w", "do a thing", false, None).unwrap();
    reg.set_state(&g.id, "{}").unwrap();
    let log = fs::read_to_string(reg.state_root().join(&g.id).join("audit.jsonl")).unwrap();
    let events: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(
        events.iter().any(|e| e["detail"]["task"] == "do a thing"),
        "spawn audit must capture the task brief"
    );
    let kinds: Vec<&str> = events.iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"group-create"));
    assert!(kinds.contains(&"agent-spawn"));
    assert!(kinds.contains(&"state-write"));
    assert!(events.iter().all(|e| e["ts_ms"].as_u64().is_some()));
}

#[test]
fn bracketed_paste_frames_text_and_normalizes_crlf() {
    let p = bracketed_paste("line1\r\nline2");
    let s = String::from_utf8(p).unwrap();
    assert!(s.starts_with("\x1b[200~") && s.ends_with("\x1b[201~"));
    assert!(
        s.contains("line1\nline2") && !s.contains('\r'),
        "CR must not leak inside a paste — it would submit early"
    );
}

#[test]
fn strip_ansi_removes_csi_osc_and_controls() {
    let raw = b"\x1b[31mred\x1b[0m and \x1b]0;title\x07plain\r\nnext";
    assert_eq!(strip_ansi(raw), "red and plain\nnext");
}

#[test]
fn claude_command_minimizes_init_approvals_without_bypass() {
    let (reg, _d) = test_registry();
    let cfg = Path::new("C:/x/cfg.json");
    let gdir = Path::new("C:/data/group");
    let cmd = reg.build_agent_command("claude", "sonnet", false, cfg, gdir, Path::new("C:/repo"), None, false);
    assert!(cmd.contains("--model sonnet"));
    assert!(cmd.contains("--permission-mode acceptEdits"));
    assert!(cmd.contains("--strict-mcp-config"), "workers must not see the user's other MCP servers");
    assert!(cmd.contains("--add-dir \"C:/data/group\""),
        "instructions dir must be a workspace so reading it never prompts");
    assert!(cmd.contains("--allowedTools mcp__loomux"),
        "loomux tools must be pre-approved so report/list never prompt");
    assert!(!cmd.contains("Bash(git:*)"), "git is not pre-approved unless auto_ops");
    let cmd = reg.build_agent_command("claude", "sonnet", true, cfg, gdir, Path::new("C:/repo"), None, false);
    assert!(cmd.contains("--permission-mode auto"),
        "the Auto preset must use Claude Code's native auto permission mode");
    assert!(cmd.contains("\"Bash(git:*)\"") && cmd.contains("\"Bash(gh:*)\""));
    assert!(cmd.contains("\"Bash(git *)\""), "both allowlist rule spellings must be present");
    assert!(
        !cmd.contains("--dangerously-skip-permissions"),
        "bypass mode must never be used: its confirm dialog defaults to exit and kills the pane"
    );
}

#[test]
fn copilot_command_uses_copilot_adapter_flags() {
    let (reg, _d) = test_registry();
    let cfg = Path::new("C:/x/cfg.json");
    let gdir = Path::new("C:/data/group");
    let cmd = reg.build_agent_command("copilot", "auto", true, cfg, gdir, Path::new("C:/repo"), None, false);
    assert!(cmd.starts_with("copilot "), "selected CLI must actually be launched, not claude");
    assert!(
        cmd.contains("--additional-mcp-config \"@C:/x/cfg.json\""),
        "the @ file marker must be inside the quotes — a bare @\" opens a PowerShell here-string, got: {cmd}"
    );
    assert!(cmd.contains("--model auto"));
    assert!(cmd.contains("--allow-tool loomux"));
    assert!(cmd.contains("--add-dir \"C:/data/group\""));
    assert!(
        cmd.contains("--add-dir \"C:/repo\""),
        "the workspace must be pre-trusted so panes don't stall on a trust prompt"
    );
    assert!(
        cmd.contains("--no-auto-update"),
        "a mid-boot self-update restarts copilot and eats the kickoff"
    );
    // Auto preset = copilot's own unattended mode.
    assert!(cmd.contains("--autopilot") && cmd.contains("--allow-all-tools") && cmd.contains("--allow-all-paths"));
    // Conservative preset keeps the explicit allowlist instead.
    let cmd = reg.build_agent_command("copilot", "auto", false, cfg, gdir, Path::new("C:/repo"), None, false);
    assert!(!cmd.contains("--autopilot") && !cmd.contains("--allow-all-tools"));
    assert!(cmd.contains("--allow-tool \"shell(git:*)\"") && cmd.contains("--allow-tool \"shell(gh:*)\""));
    // Resume reopens a tracked session via --resume; copilot has no
    // pre-assignable id, so a session without resume adds no session flag.
    let sid = "aabbccdd-1122-4334-8556-77889900aabb";
    let cmd = reg.build_agent_command("copilot", "auto", true, cfg, gdir, Path::new("C:/repo"), Some(sid), true);
    assert!(cmd.contains(&format!("--resume {sid}")), "copilot resume must pass --resume, got: {cmd}");
    let cmd = reg.build_agent_command("copilot", "auto", true, cfg, gdir, Path::new("C:/repo"), Some(sid), false);
    assert!(!cmd.contains("--resume") && !cmd.contains("--session-id"),
        "a fresh copilot spawn cannot pin a session id");
}

#[test]
fn copilot_mcp_config_includes_tools_allowlist() {
    let (reg, _d) = test_registry();
    let mut rails = rails();
    rails.agent_cli = "copilot".into();
    let g = reg.create_group("C:/tmp/copilot-repo", rails).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    let cfg = fs::read_to_string(
        reg.state_root().join(&g.id).join("configs").join(format!("{}.json", w.id)),
    )
    .unwrap();
    assert!(cfg.contains("\"tools\""), "copilot expects a tools allowlist in the server entry");
    assert!(cfg.contains(&w.token));
}

fn copilot_rails() -> Guardrails {
    let mut r = rails();
    r.agent_cli = "copilot".into();
    r
}

#[test]
fn copilot_agents_spawn_without_a_preassigned_session() {
    // Copilot has no `--session-id`; a fresh copilot pane starts untracked
    // and is associated later once its session-state appears on disk.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/copilot-repo", copilot_rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    assert!(w.session_id.is_none(), "copilot cannot pre-assign a session id");
}

#[test]
fn associating_a_copilot_session_records_it_on_roster_and_task_board() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/copilot-repo", copilot_rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "builder", "do the thing", false, None).unwrap();
    // The orchestrator has put a task on the board assigned to this worker.
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("build feature"), None, None)).unwrap();
    reg.upsert_task(
        &g.id,
        "orch-1",
        Some(&t.id),
        TaskPatch { assignee: Some(w.id.clone()), ..Default::default() },
    )
    .unwrap();

    // The watcher discovered copilot's session id and binds it to the pane.
    let sid = "0f9e8d7c-1234-4abc-8def-0011223344ff";
    reg.associate_copilot_session(&g.id, &w.id, sid);

    // Agent map now carries the id (so list_agents/resume can use it).
    let agents = reg.list_agents(&g.id);
    let entry = agents.as_array().unwrap().iter().find(|a| a["id"] == w.id.as_str()).unwrap();
    assert_eq!(entry["session"], sid);

    // Durable roster records exactly one session row for this pane — the
    // placeholder was upgraded, not duplicated — and the session browser
    // surfaces it as a worker chip in this group.
    let roles: Vec<_> = reg.session_roles().into_iter().filter(|r| r.session_id == sid).collect();
    assert_eq!(roles.len(), 1, "one roster/session-browser entry per pane, got {}", roles.len());
    assert_eq!(roles[0].role, "worker");
    assert_eq!(roles[0].group_id, g.id);

    // Task board mirrors the session so the orchestrator can resume the task.
    let task = reg.tasks(&g.id).into_iter().find(|x| x.id == t.id).unwrap();
    assert_eq!(task.session.as_deref(), Some(sid));

    // Idempotent: a second (late) discovery must not clobber the bound id.
    reg.associate_copilot_session(&g.id, &w.id, "ffffffff-0000-4000-8000-000000000000");
    let agents = reg.list_agents(&g.id);
    let entry = agents.as_array().unwrap().iter().find(|a| a["id"] == w.id.as_str()).unwrap();
    assert_eq!(entry["session"], sid, "an already-tracked pane keeps its first session id");
}

#[test]
fn copilot_orchestration_session_gets_a_chip_and_restores() {
    use loomux_lib::orchestration::resume_recorded_session;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap(); // must exist for restore
    let repo_path = repo.path().to_string_lossy().into_owned();
    let sid = "0a1b2c3d-4e5f-4a6b-8c7d-8e9f00112233";
    let gid;
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group(&repo_path, copilot_rails()).unwrap();
        gid = g.id.clone();
        // A copilot orchestrator spawns untracked, then its session is bound
        // once it appears on disk (here, driven directly).
        let orch = reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
        assert!(orch.session_id.is_none());
        reg.associate_copilot_session(&g.id, &orch.id, sid);
        // Session browser now has an ORCH chip for this copilot session.
        let roles: Vec<_> = reg.session_roles().into_iter().filter(|r| r.session_id == sid).collect();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].role, "orchestrator");
    }
    // "App restart": a new registry restores the whole copilot orchestration
    // from the recorded session, resuming its conversation via `copilot
    // --resume`.
    let reg = Arc::new(OrchRegistry::new(dir.path().to_path_buf()));
    reg.set_port(45999);
    let req = resume_recorded_session(&reg, sid, None).unwrap().expect("orchestrator pane spec");
    assert_eq!(req.group_id, gid);
    assert!(req.command.starts_with("copilot "), "must relaunch copilot, got: {}", req.command);
    assert!(req.command.contains(&format!("--resume {sid}")), "must resume the recorded session");
}

#[test]
fn copilot_group_resumes_a_recorded_session() {
    // Resume parity: a copilot group accepts resume_session (its ids are
    // hex+dashes, so they pass sanitization) and reuses it on the pane.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/copilot-repo", copilot_rails()).unwrap();
    let dir = tempfile::tempdir().unwrap(); // an existing cwd for the resume
    let sid = "aabbccdd-1122-4334-8556-77889900aabb".to_string();
    let w = reg
        .spawn_agent_ex(
            &g.id,
            Role::Worker,
            "resumed",
            "follow-up",
            false,
            None,
            Some(sid.clone()),
            Some(dir.path().to_string_lossy().into_owned()),
        )
        .unwrap();
    assert_eq!(w.session_id.as_deref(), Some(sid.as_str()));
    // A mangled id is rejected rather than silently resuming the wrong one.
    assert!(reg
        .spawn_agent_ex(
            &g.id, Role::Worker, "bad", "", false, None,
            Some("../../etc/passwd".into()),
            Some(dir.path().to_string_lossy().into_owned()),
        )
        .is_err());
}

#[test]
fn concurrent_groups_on_one_repo_stay_separate_but_resume_when_free() {
    let (reg, _d) = test_registry();
    // First orchestration on the repo.
    let g1 = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g1.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    reg.set_state(&g1.id, r#"{"queue":[1]}"#).unwrap();
    // Second concurrent orchestration on the SAME repo must get its own
    // group (otherwise its orchestrator would receive g1's worker reports)
    // and must not inherit g1's state.
    let g2 = reg.create_group("C:/tmp/repo", rails()).unwrap();
    assert_ne!(g1.id, g2.id);
    assert_eq!(reg.get_state(&g2.id), "{}");
    // Once g1 has no live agents, a new launch resumes g1's id and state.
    for a in reg.list_agents(&g1.id).as_array().unwrap() {
        reg.mark_dead(a["id"].as_str().unwrap(), Some(0));
    }
    let g3 = reg.create_group("C:/tmp/repo", rails()).unwrap();
    assert_eq!(g3.id, g1.id, "freed base group id must be reused for resume");
    assert_eq!(reg.get_state(&g3.id), r#"{"queue":[1]}"#);
}

#[test]
fn kickoff_prompt_references_instructions_and_task() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "fix-auth", "Fix issue #7", false, None).unwrap();
    let k = reg.kickoff_prompt(&w, &g, "note");
    assert!(k.contains("worker.md"));
    assert!(k.contains("Fix issue #7"));
    let idle = reg.spawn_agent(&g.id, Role::Worker, "idle", "", false, None).unwrap();
    let k = reg.kickoff_prompt(&idle, &g, "");
    assert!(k.contains("No task is assigned yet"));
}

#[test]
fn instruction_files_rendered_with_group_facts() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/myrepo", rails()).unwrap();
    let dir = reg.state_root().join(&g.id);
    let orch = fs::read_to_string(dir.join("orchestrator.md")).unwrap();
    assert!(orch.contains("C:/tmp/myrepo"));
    assert!(orch.contains("at most 2 live workers"), "guardrails must be rendered into the doc");
    assert!(!orch.contains("{{"), "no unrendered placeholders");
    let worker = fs::read_to_string(dir.join("worker.md")).unwrap();
    assert!(worker.contains("Never merge"), "merge gatekeeping must be in worker instructions");
}

// ---------- task board ----------

fn patch(title: Option<&str>, status: Option<&str>, note: Option<&str>) -> TaskPatch {
    TaskPatch {
        title: title.map(String::from),
        status: status.map(String::from),
        note: note.map(String::from),
        ..Default::default()
    }
}

#[test]
fn task_lifecycle_create_edit_note_delete() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("Add retry logic"), None, None)).unwrap();
    assert_eq!(t.status, "queued", "new tasks start queued");
    assert_eq!(t.id, "t-1");
    // Edit status + append a note; note carries author and timestamp.
    let t = reg
        .upsert_task(&g.id, "human", Some(&t.id), patch(None, Some("in-progress"), Some("looks good")))
        .unwrap();
    assert_eq!(t.status, "in-progress");
    assert_eq!(t.notes.len(), 1);
    assert_eq!(t.notes[0].author, "human");
    assert!(t.notes[0].ts_ms > 0);
    // Invalid status rejected; unknown id rejected; title required for new.
    assert!(reg.upsert_task(&g.id, "x", Some(&t.id), patch(None, Some("nope"), None)).is_err());
    assert!(reg.upsert_task(&g.id, "x", Some("t-999"), patch(None, Some("done"), None)).is_err());
    assert!(reg.upsert_task(&g.id, "x", None, patch(None, None, None)).is_err());
    // Delete.
    reg.delete_task(&g.id, "human", &t.id).unwrap();
    assert!(reg.tasks(&g.id).is_empty());
    assert!(reg.delete_task(&g.id, "human", &t.id).is_err());
}

#[test]
fn task_board_persists_and_reorders() {
    let dir = tempfile::tempdir().unwrap();
    let gid;
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
        gid = g.id.clone();
        for title in ["a", "b", "c"] {
            reg.upsert_task(&g.id, "orch-1", None, patch(Some(title), None, None)).unwrap();
        }
        // Move c first; unmentioned ids keep relative order behind it.
        reg.reorder_tasks(&g.id, "human", &["t-3".into()]).unwrap();
    }
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    let titles: Vec<String> = reg.tasks(&gid).iter().map(|t| t.title.clone()).collect();
    assert_eq!(titles, ["c", "a", "b"], "order must survive an app restart");
}

#[test]
fn task_tools_are_role_gated_but_listing_is_shared() {
    let (reg, _d, co, cw) = setup_mcp();
    let denied = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "upsert_task", "arguments": { "title": "sneaky" } })).unwrap();
    assert_eq!(denied["isError"], true, "workers must not edit the board");
    let ok = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "upsert_task", "arguments": { "title": "Fix parser", "status": "in-progress", "issue": "#7" } }))
        .unwrap();
    assert_eq!(ok["isError"], false);
    let listed = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "list_tasks", "arguments": {} })).unwrap();
    let text = listed["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Fix parser") && text.contains("#7"),
        "workers must be able to read the board");
}

#[test]
fn copilot_trust_config_edit_preserves_content_and_dedupes() {
    let existing = "// User settings belong in settings.json.\n// This file is managed automatically.\n{\n  \"firstLaunchAt\": \"2026-07-04\",\n  \"trustedFolders\": [\n    \"C:\\\\Projects\\\\cattle-worker\"\n  ]\n}\n";
    // New folder: appended, comments and existing fields intact.
    let updated = add_trusted_folder(existing, r"C:\Projects\other").unwrap();
    assert!(updated.starts_with("// User settings"), "comment header must survive");
    assert!(updated.contains("firstLaunchAt"), "unknown fields must survive");
    assert!(updated.contains(r"C:\\Projects\\cattle-worker") || updated.contains("cattle-worker"));
    assert!(updated.contains("other"));
    // Already trusted (case/separator variants): no rewrite at all.
    assert!(add_trusted_folder(existing, r"c:/projects/cattle-worker").is_none());
    // Empty/missing config: created from scratch.
    let fresh = add_trusted_folder("", r"C:\Projects\x").unwrap();
    assert!(fresh.contains("trustedFolders"));
    // Corrupt config must NOT be clobbered.
    assert!(add_trusted_folder("// c\n{ not json", r"C:\x").is_none());
}

// ---------- per-task sessions & resume ----------

#[test]
fn claude_agents_get_preassigned_resumable_sessions() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    let sid = w.session_id.expect("claude agents must get a session id at spawn");
    // Valid UUID shape (claude --session-id requires it).
    assert_eq!(sid.len(), 36);
    assert_eq!(sid.split('-').map(str::len).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
    // The roster exposes session + cwd so the orchestrator can record them.
    let roster = reg.list_agents(&g.id).to_string();
    assert!(roster.contains(&sid) && roster.contains("cwd"));
    // The launch command pins the id.
    let cfg = Path::new("C:/x/cfg.json");
    let gdir = Path::new("C:/x/g");
    let cmd = reg.build_agent_command("claude", "sonnet", false, cfg, gdir, Path::new("C:/repo"), Some(&sid), false);
    assert!(cmd.contains(&format!("--session-id {sid}")));
    // Resume uses --resume instead.
    let cmd = reg.build_agent_command("claude", "sonnet", false, cfg, gdir, Path::new("C:/repo"), Some(&sid), true);
    assert!(cmd.contains(&format!("--resume {sid}")) && !cmd.contains("--session-id"));
}

#[test]
fn resume_spawn_requires_valid_session_and_existing_cwd() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let bad_session = reg.spawn_agent_ex(
        &g.id, Role::Worker, "w", "follow-up", false, None,
        Some("; rm -rf /".into()), None,
    );
    assert!(bad_session.is_err(), "shell-metachar session ids must be rejected");
    let bad_cwd = reg.spawn_agent_ex(
        &g.id, Role::Worker, "w", "follow-up", false, None,
        Some("abc-123".into()), Some("C:/definitely/not/a/dir".into()),
    );
    assert!(bad_cwd.unwrap_err().contains("cwd"), "resume cwd must exist");
    // Valid resume records the reused session on the agent.
    let dir = tempfile::tempdir().unwrap();
    let ok = reg
        .spawn_agent_ex(
            &g.id, Role::Worker, "w", "follow-up", false, None,
            Some("abc-123".into()), Some(dir.path().to_string_lossy().into_owned()),
        )
        .unwrap();
    assert_eq!(ok.session_id.as_deref(), Some("abc-123"));
    assert_eq!(ok.cwd, dir.path().to_string_lossy());
}

#[test]
fn task_board_tracks_sessions_for_followups() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("Add retries"), None, None)).unwrap();
    let mut p = patch(None, Some("in-progress"), None);
    p.session = Some("11112222-3333-4444-8555-666677778888".into());
    p.assignee = Some("w-1".into());
    let t = reg.upsert_task(&g.id, "orch-1", Some(&t.id), p).unwrap();
    assert_eq!(t.session.as_deref(), Some("11112222-3333-4444-8555-666677778888"));
    // Survives the round-trip through disk.
    let stored = &reg.tasks(&g.id)[0];
    assert_eq!(stored.session, t.session);
    assert_eq!(stored.assignee.as_deref(), Some("w-1"));
}

// ---------- merge-gate actions (#9) ----------

#[test]
fn remote_web_base_normalizes_every_git_url_shape() {
    // scp-like, https (with/without .git), ssh with a port, trailing slash.
    let cases = [
        ("git@github.com:willem445/loomux.git", "https://github.com/willem445/loomux"),
        ("https://github.com/willem445/loomux.git", "https://github.com/willem445/loomux"),
        ("https://github.com/willem445/loomux", "https://github.com/willem445/loomux"),
        ("ssh://git@github.com:22/willem445/loomux.git", "https://github.com/willem445/loomux"),
        ("https://token@github.com/o/r/", "https://github.com/o/r"),
        // Self-hosted host survives (GitHub path scheme is assumed downstream).
        ("git@git.example.com:team/app.git", "https://git.example.com/team/app"),
    ];
    for (url, want) in cases {
        assert_eq!(normalize_remote_web_base(url).as_deref(), Some(want), "for {url}");
    }
    // Junk that can't be turned into a link.
    for bad in ["", "not-a-url", "https://", "git@github.com", "file:///tmp/x"] {
        assert!(normalize_remote_web_base(bad).is_none(), "{bad:?} must not resolve");
    }
}

#[test]
fn resolve_ref_url_handles_numbers_and_passthrough() {
    let base = Some("https://github.com/o/r");
    // Bare number and #-prefixed both resolve; issue vs pr picks the segment.
    assert_eq!(resolve_ref_url(base, "issue", "#9").as_deref(), Some("https://github.com/o/r/issues/9"));
    assert_eq!(resolve_ref_url(base, "pr", "42").as_deref(), Some("https://github.com/o/r/pull/42"));
    // A `GH-12`-style prefix resolves to its digit run (comment ↔ behavior).
    assert_eq!(resolve_ref_url(base, "issue", "GH-12").as_deref(), Some("https://github.com/o/r/issues/12"));
    // A full URL is used verbatim — even with no remote base available.
    let url = "https://github.com/o/r/pull/7";
    assert_eq!(resolve_ref_url(None, "pr", url).as_deref(), Some(url));
    // A bare number with no remote can't be resolved.
    assert!(resolve_ref_url(None, "issue", "9").is_none());
    // Non-numeric junk resolves to nothing.
    assert!(resolve_ref_url(base, "issue", "later").is_none());
}

#[test]
fn approve_marks_done_and_records_signoff() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("Ship the parser"), None, None)).unwrap();
    // Move it to the merge gate first (as the orchestrator would).
    let mut p = patch(None, Some("pr"), None);
    p.pr = Some("#12".into());
    reg.upsert_task(&g.id, "orch-1", Some(&t.id), p).unwrap();
    // Approving is the human's sign-off: status → done, note recorded, actor human.
    let done = reg.approve_task(&g.id, &t.id).unwrap();
    assert_eq!(done.status, "done");
    let note = done.notes.last().unwrap();
    assert_eq!(note.author, "human");
    assert!(note.text.contains("Approved"), "sign-off must be auditable on the board");
}

#[test]
fn request_changes_records_findings_but_not_done() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("Ship the parser"), Some("pr"), None)).unwrap();
    // Empty findings are rejected — the notice would be useless.
    assert!(reg.request_changes(&g.id, &t.id, "   ").is_err());
    let after = reg.request_changes(&g.id, &t.id, "retries still leak a handle").unwrap();
    // Status stays at the gate (orchestrator re-dispatches); findings recorded.
    assert_eq!(after.status, "pr", "request-changes must not silently complete the item");
    assert!(after.notes.last().unwrap().text.contains("retries still leak a handle"));
    // Unknown task id is an error, not a silent no-op.
    assert!(reg.request_changes(&g.id, "t-999", "x").is_err());
}

#[test]
fn merge_gate_actions_are_guarded_to_gate_statuses() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // A queued item is not at the merge gate — both actions must refuse, and
    // refuse without mutating (status unchanged, no note added).
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("Ship it"), None, None)).unwrap();
    assert!(reg.approve_task(&g.id, &t.id).is_err(), "cannot approve a queued item");
    assert!(reg.request_changes(&g.id, &t.id, "nope").is_err(), "cannot request changes off-gate");
    let stored = &reg.tasks(&g.id)[0];
    assert_eq!(stored.status, "queued", "a refused action must not change status");
    assert!(stored.notes.is_empty(), "a refused action must not leave a note");
    // Both gate statuses are allowed.
    for gate in ["pr", "human-testing"] {
        reg.upsert_task(&g.id, "orch-1", Some(&t.id), patch(None, Some(gate), None)).unwrap();
        assert!(reg.request_changes(&g.id, &t.id, "one more thing").is_ok(), "{gate} is a gate status");
    }
    // And once approved (→ done) it's off the gate again.
    reg.upsert_task(&g.id, "orch-1", Some(&t.id), patch(None, Some("pr"), None)).unwrap();
    reg.approve_task(&g.id, &t.id).unwrap();
    assert!(reg.approve_task(&g.id, &t.id).is_err(), "a done item is past the gate");
}

// ---------- review-round regression tests ----------

#[test]
fn concurrent_same_repo_launches_get_distinct_groups() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo_path = repo.path().to_string_lossy().into_owned();
    let reg = Arc::new(OrchRegistry::new(dir.path().to_path_buf()));
    reg.set_port(45999);
    // The id is chosen by liveness, but the orchestrator that makes a group
    // live registers after the choice — without the creation lock, two
    // simultaneous launches share an id and cross-deliver reports.
    let mut handles = vec![];
    for _ in 0..2 {
        let reg = reg.clone();
        let repo = repo_path.clone();
        handles.push(std::thread::spawn(move || {
            create_orchestration_group(&reg, &repo, rails(), None, None, 0).map(|r| r.group_id)
        }));
    }
    let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap().unwrap()).collect();
    assert_ne!(ids[0], ids[1], "concurrent launches on one repo must not share a group");
}

#[test]
fn repo_paths_with_quotes_are_rejected() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let reg = Arc::new(OrchRegistry::new(dir.path().to_path_buf()));
    reg.set_port(45999);
    let err = create_orchestration_group(&reg, "/tmp/evil\" ; rm -rf /", rails(), None, None, 0)
        .unwrap_err();
    assert!(err.contains("quote"), "the quote check must fire before anything else, got: {err}");
}

#[test]
fn roster_survives_agent_id_recycling_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let (s1, s2);
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
        s1 = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap().session_id.unwrap();
    }
    {
        // "Restart": agent ids start over at w-1, colliding with run 1.
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
        s2 = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap().session_id.unwrap();
    }
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    let sessions: Vec<String> = reg.session_roles().into_iter().map(|r| r.session_id).collect();
    assert!(sessions.contains(&s1), "run 1's session must survive id recycling");
    assert!(sessions.contains(&s2));
}

#[test]
fn audit_rotates_at_cap_and_backfill_reads_both_generations() {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    let gdir = reg.state_root().join(&g.id);
    // Force a rotation with a tiny cap: the spawn entry moves to audit.1.
    rotate_audit_if_needed(&gdir, 1);
    assert!(gdir.join("audit.1.jsonl").is_file(), "rotation must produce the old generation");
    reg.audit(&g.id, "loomux", "post-rotate", serde_json::json!({}));
    assert!(gdir.join("audit.jsonl").is_file());
    // Session mapping still resolves from the rotated generation.
    let sessions: Vec<String> = reg.session_roles().into_iter().map(|r| r.session_id).collect();
    assert!(
        sessions.contains(&w.session_id.unwrap()),
        "backfill must read rotated audit generations"
    );
}

#[test]
fn parse_audit_lines_is_ordered_and_skips_malformed() {
    let text = "\
{\"ts_ms\":1,\"actor\":\"loomux\",\"action\":\"group-create\",\"detail\":{\"repo\":\"r\"}}
not json at all
{\"ts_ms\":2,\"actor\":\"human\",\"action\":\"prompt\",\"detail\":{\"to\":\"w-1\",\"text\":\"hi\\nthere\"}}

{\"ts_ms\":3,\"actor\":\"loomux\",\"action\":\"agent-spawn\"}";
    let entries = parse_audit_lines(text);
    // Malformed line and the blank line are skipped; the three valid ones
    // survive in file order.
    assert_eq!(entries.len(), 3, "malformed and blank lines must be skipped");
    assert_eq!(entries[0].action, "group-create");
    assert_eq!(entries[1].actor, "human");
    assert_eq!(entries[2].ts_ms, 3);
    // A line missing `detail` still parses (detail becomes null), not dropped.
    assert!(entries[2].detail.is_null());
    // Full prompt text is preserved for in-app expansion.
    assert_eq!(entries[1].detail["text"], "hi\nthere");
}

#[test]
fn audit_log_reads_both_generations_oldest_first() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let gdir = reg.state_root().join(&g.id);
    // Seed a group-create in the current log, then rotate it into audit.1 so
    // the two-generation read path is exercised.
    rotate_audit_if_needed(&gdir, 1);
    assert!(gdir.join("audit.1.jsonl").is_file());
    // Append a fresh entry to the new current log.
    reg.audit(&g.id, "human", "prompt", serde_json::json!({ "to": "w-1", "text": "hello" }));

    let entries = reg.audit_log(&g.id);
    let actions: Vec<&str> = entries.iter().map(|e| e.action.as_str()).collect();
    // Rotated (older) generation first, then the current one.
    assert!(actions.contains(&"group-create"), "rotated generation must be included");
    assert_eq!(actions.last(), Some(&"prompt"), "current generation appends after the rotated one");
    let prompt = entries.iter().find(|e| e.action == "prompt").unwrap();
    assert_eq!(prompt.detail["text"], "hello");
}

#[test]
fn audit_log_of_unknown_group_is_empty() {
    let (reg, _d) = test_registry();
    assert!(reg.audit_log("no-such-group").is_empty());
}

// ---------- durable roster & orchestration restore ----------

#[test]
fn roster_records_sessions_roles_and_liveness() {
    let dir = tempfile::tempdir().unwrap();
    let orch_sid;
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
        let orch = reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
        reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
        orch_sid = orch.session_id.unwrap();
        let roles = reg.session_roles();
        assert_eq!(roles.len(), 2);
        let o = roles.iter().find(|r| r.role == "orchestrator").unwrap();
        assert_eq!(o.session_id, orch_sid);
        assert!(o.group_live, "group with running agents must read as live");
    }
    // Fresh instance (app restart): roster survives, group reads dead.
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    let roles = reg.session_roles();
    assert!(roles.iter().any(|r| r.session_id == orch_sid && !r.group_live),
        "roster must survive restarts and report the group as not live");
}

#[test]
fn sessions_backfill_from_audit_when_roster_predates_it() {
    // Groups created before agents.json existed still have every spawn in
    // the audit log — their sessions must be markable and restorable too.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let orch = reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let sid = orch.session_id.unwrap();
    // Simulate the pre-roster era: the roster file never existed.
    fs::remove_file(reg.state_root().join(&g.id).join("agents.json")).unwrap();
    let roles = reg.session_roles();
    let o = roles
        .iter()
        .find(|r| r.session_id == sid)
        .expect("session must be discoverable via audit backfill");
    assert_eq!(o.role, "orchestrator");
}

#[test]
fn hint_restores_sessions_unknown_to_roster_and_audit() {
    // Pre-session-tracking orchestrators left no session id anywhere on
    // disk; the session browser identifies them from transcript signatures
    // and passes (group, role) hints. Restore must honor them — but only
    // for groups that actually exist.
    use loomux_lib::orchestration::resume_recorded_session;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo_path = repo.path().to_string_lossy().into_owned();
    let reg = Arc::new(OrchRegistry::new(dir.path().to_path_buf()));
    reg.set_port(45999);
    let g = reg.create_group(&repo_path, rails()).unwrap();
    let gid = g.id.clone();
    drop(g);
    let sid = "11112222-3333-4444-8555-666677778888";
    let hint = Some((gid.clone(), "orchestrator".to_string()));
    let req = resume_recorded_session(&reg, sid, hint).unwrap().expect("pane spec");
    assert_eq!(req.group_id, gid);
    assert!(req.command.contains(&format!("--resume {sid}")));
    // A hint pointing at a nonexistent group is rejected, not trusted.
    let reg2 = Arc::new(OrchRegistry::new(tempfile::tempdir().unwrap().path().to_path_buf()));
    reg2.set_port(45999);
    let bad = resume_recorded_session(&reg2, sid, Some(("ghost-1".into(), "orchestrator".into())));
    assert!(bad.is_err());
}

#[test]
fn orchestrator_session_restores_full_group_with_fresh_mcp_identity() {
    use loomux_lib::orchestration::resume_recorded_session;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap(); // must exist for restore
    let repo_path = repo.path().to_string_lossy().into_owned();
    let (gid, orch_sid);
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group(&repo_path, rails()).unwrap();
        gid = g.id.clone();
        let orch = reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
        orch_sid = orch.session_id.unwrap();
    }
    // "App restart": new registry, nothing live.
    let reg = Arc::new(OrchRegistry::new(dir.path().to_path_buf()));
    reg.set_port(45999);
    let req = resume_recorded_session(&reg, &orch_sid, None).unwrap().expect("orchestrator returns a pane spec");
    assert_eq!(req.group_id, gid, "restore must reattach to the recorded group (state/tasks/audit)");
    assert!(req.command.contains(&format!("--resume {orch_sid}")),
        "the orchestrator's conversation must be resumed, not cold-started");
    assert!(req.command.contains("--mcp-config"),
        "restore must re-wire MCP identity — the whole point");
    let g = reg.group(&gid).expect("group re-registered in memory");
    assert_eq!(g.guardrails.worker_model, "sonnet", "guardrails must be restored from group.json");
    // A second restore while live is refused.
    let err = resume_recorded_session(&reg, &orch_sid, None).unwrap_err();
    assert!(err.contains("already"), "got: {err}");
}

#[test]
fn worker_session_rejoin_requires_live_group_then_reuses_session() {
    use loomux_lib::orchestration::resume_recorded_session;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let reg = Arc::new(OrchRegistry::new(dir.path().to_path_buf()));
    reg.set_port(45999);
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    let sid = w.session_id.clone().unwrap();
    reg.mark_dead(&w.id, Some(0));
    // Group has no live agents → rejoin refused with guidance.
    let err = resume_recorded_session(&reg, &sid, None).unwrap_err();
    assert!(err.contains("orchestrator"), "must point at restarting the orchestrator, got: {err}");
    // With a live orchestrator, the rejoin spawns (background) reusing the session.
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    assert!(resume_recorded_session(&reg, &sid, None).unwrap().is_none(),
        "worker rejoin panes arrive via the spawn event, not the return value");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let roster = reg.list_agents(&g.id).to_string();
        let rejoined = roster.matches(&sid).count() >= 1
            && reg.session_roles().iter().filter(|r| r.session_id == sid).count() >= 1;
        // The new agent entry must carry the SAME session id as the old one.
        if rejoined && roster.matches("\"status\":\"running\"").count() >= 2 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "rejoin did not complete: {roster}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(resume_recorded_session(&reg, "0000-not-recorded", None).is_err());
}

// ---------- MCP dispatch: protocol, role filtering, cross-group access ----------

fn setup_mcp() -> (OrchRegistry, tempfile::TempDir, Caller, Caller) {
    let (reg, dir) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let orch = reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let worker = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let co = reg.resolve_token(&orch.token).unwrap();
    let cw = reg.resolve_token(&worker.token).unwrap();
    (reg, dir, co, cw)
}

#[test]
fn initialize_echoes_protocol_version() {
    let (reg, _d, co, _cw) = setup_mcp();
    let r = dispatch(&reg, &co, "initialize", &json!({ "protocolVersion": "2025-03-26" })).unwrap();
    assert_eq!(r["protocolVersion"], "2025-03-26");
    assert!(r["capabilities"]["tools"].is_object());
}

#[test]
fn tool_listing_is_role_filtered() {
    let (reg, _d, co, cw) = setup_mcp();
    let names = |c: &Caller| -> Vec<String> {
        dispatch(&reg, c, "tools/list", &Value::Null).unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    let orch = names(&co);
    let work = names(&cw);
    assert!(orch.contains(&"spawn_agent".to_string()));
    assert!(orch.contains(&"set_state".to_string()));
    assert!(!work.contains(&"spawn_agent".to_string()), "workers must not see spawn");
    assert!(!work.contains(&"set_state".to_string()), "workers must not see state writes");
    assert!(work.contains(&"report".to_string()));
}

#[test]
fn workers_cannot_use_privileged_tools_even_if_they_try() {
    let (reg, _d, _co, cw) = setup_mcp();
    for tool in ["spawn_agent", "send_prompt", "kill_agent", "set_state", "get_output"] {
        let r = dispatch(&reg, &cw, "tools/call",
            &json!({ "name": tool, "arguments": { "task": "x", "agent_id": "w-1", "text": "x", "state": "{}" } }))
            .unwrap();
        assert_eq!(r["isError"], true, "{tool} must be denied for workers");
        assert!(
            r["content"][0]["text"].as_str().unwrap().contains("orchestrator-only"),
            "{tool} denial must say why"
        );
    }
}

#[test]
fn spawn_respects_guardrail_cap_via_mcp() {
    let (reg, _d, co, _cw) = setup_mcp();
    // One worker exists (cap 2): one more fits, the next is refused.
    let ok = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "spawn_agent", "arguments": { "task": "b" } })).unwrap();
    assert_eq!(ok["isError"], false);
    let over = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "spawn_agent", "arguments": { "task": "c" } })).unwrap();
    assert_eq!(over["isError"], true);
    assert!(over["content"][0]["text"].as_str().unwrap().contains("guardrail"));
}

#[test]
fn cross_group_targets_are_invisible() {
    let (reg, _d, co, _cw) = setup_mcp();
    let g2 = reg.create_group("C:/tmp/other-repo", rails()).unwrap();
    let foreign = reg.spawn_agent(&g2.id, Role::Worker, "fw", "t", false, None).unwrap();
    let r = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "kill_agent", "arguments": { "agent_id": foreign.id } })).unwrap();
    assert_eq!(r["isError"], true);
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("unknown agent"),
        "cross-group access must be indistinguishable from a nonexistent agent, got: {text}"
    );
    // And the foreign agent never appears in this group's roster.
    let roster = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "list_agents", "arguments": {} })).unwrap();
    assert!(!roster["content"][0]["text"].as_str().unwrap().contains(&foreign.id));
}

#[test]
fn unknown_method_and_tool_are_rejected() {
    let (reg, _d, co, _cw) = setup_mcp();
    let err = dispatch(&reg, &co, "resources/list", &Value::Null).unwrap_err();
    assert_eq!(err.0, -32601);
    let r = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "no_such_tool", "arguments": {} })).unwrap();
    assert_eq!(r["isError"], true);
}

#[test]
fn report_validates_status_and_role() {
    let (reg, _d, co, cw) = setup_mcp();
    let bad = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "report", "arguments": { "status": "finished", "summary": "x" } })).unwrap();
    assert_eq!(bad["isError"], true, "invalid status must be rejected");
    let from_orch = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "report", "arguments": { "status": "done", "summary": "x" } })).unwrap();
    assert_eq!(from_orch["isError"], true, "orchestrator has no one to report to");
    // A valid worker report fails only at PTY delivery in test mode (no
    // panes), which proves routing reached the orchestrator lookup.
    let ok_path = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "report", "arguments": { "status": "done", "summary": "PR #1 open" } })).unwrap();
    let text = ok_path["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("terminal") || text.contains("reported"),
        "report must route to the orchestrator's pane, got: {text}"
    );
}

#[test]
fn every_tool_call_is_audited() {
    let (reg, _d, co, _cw) = setup_mcp();
    dispatch(&reg, &co, "tools/call", &json!({ "name": "list_agents", "arguments": {} })).unwrap();
    let log = fs::read_to_string(reg.state_root().join(&co.group).join("audit.jsonl")).unwrap();
    let lines: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let call = lines
        .iter()
        .find(|e| e["action"] == "tool-call" && e["detail"]["tool"] == "list_agents")
        .expect("tool-call audit entry");
    assert_eq!(call["actor"], co.agent_id.as_str());
    assert!(lines.iter().any(|e| e["action"] == "tool-result" && e["detail"]["tool"] == "list_agents"));
}

// ---------- cost containment (#7): pause, idle-kill, spawn-rate, usage ----------

/// Guardrails with the two cost knobs set; other fields mirror `rails()`
/// but with a roomier agent cap so the spawn-rate guardrail can be exercised
/// without tripping the live-agent cap first.
fn costed_rails(idle_kill_minutes: u32, max_spawns_per_hour: u32) -> Guardrails {
    Guardrails {
        max_agents: 6,
        agent_cli: "claude".into(),
        worker_model: "sonnet".into(),
        reviewer_model: "sonnet".into(),
        orchestrator_model: "opus".into(),
        auto_ops: false,
        idle_kill_minutes,
        max_spawns_per_hour,
        watchdog_stall_minutes: 0,
    }
}

/// Guardrails with a watchdog stall window set (other fields mirror
/// `costed_rails`); a roomy agent cap so several workers can be watched.
fn watchdog_rails(watchdog_stall_minutes: u32) -> Guardrails {
    Guardrails { watchdog_stall_minutes, ..costed_rails(0, 0) }
}

#[test]
fn idle_should_kill_respects_threshold_and_disable() {
    let min = 60_000u64;
    // Disabled (0) never kills, no matter how long idle.
    assert!(!idle_should_kill(Some(0), 100 * min, 0));
    // An agent with work (None) is never idle-killed.
    assert!(!idle_should_kill(None, 100 * min, 5));
    // Under the threshold: safe. At/over: kill.
    assert!(!idle_should_kill(Some(0), 4 * min, 5));
    assert!(!idle_should_kill(Some(min), 5 * min, 5)); // exactly 4 min idle < 5
    assert!(idle_should_kill(Some(0), 5 * min, 5)); // exactly at threshold
    assert!(idle_should_kill(Some(0), 10 * min, 5));
}

#[test]
fn spawn_rate_exceeded_counts_only_the_trailing_window() {
    let window = 60 * 60 * 1000u64;
    let now = 10 * window; // 10h in
    // Unlimited (0) never trips.
    assert!(!spawn_rate_exceeded(&[now, now, now, now], now, 0, window));
    // Three within the last hour, limit 3 → next is refused.
    let recent = [now - 1000, now - 2000, now - 3000];
    assert!(spawn_rate_exceeded(&recent, now, 3, window));
    // The same three but limit 4 → still room.
    assert!(!spawn_rate_exceeded(&recent, now, 4, window));
    // Old spawns (outside the window) don't count toward the cap.
    let stale = [now - window - 1, now - 2 * window, now - 500];
    assert!(!spawn_rate_exceeded(&stale, now, 2, window));
}

#[test]
fn parse_session_cost_reads_the_lowest_statusline_dollar() {
    // Typical Claude statusline at the bottom of the pane.
    let pane = "some agent output\n$ ran a command\nmodel: sonnet · $0.42 · 12k tokens";
    assert_eq!(parse_session_cost(pane), Some(0.42));
    // Thousands separators tolerated; bottom-most render wins.
    assert_eq!(parse_session_cost("cost $1.00\ntotal $1,234.56 session"), Some(1234.56));
    // A bare "$" or "$." with no digits is not a cost.
    assert_eq!(parse_session_cost("price: $ TBD\nsee $.foo"), None);
    // No dollar figure at all.
    assert_eq!(parse_session_cost("just some\noutput lines"), None);
    // Whole-dollar amount.
    assert_eq!(parse_session_cost("session cost $3"), Some(3.0));
}

#[test]
fn pause_suppresses_delivery_and_persists_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let gid;
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
        gid = g.id.clone();
        let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
        // Not paused: delivery proceeds past the pause gate and only fails
        // because test mode has no real terminal.
        let err = reg.deliver_prompt(&w.id, "hello", "loomux", false).unwrap_err();
        assert!(err.contains("terminal"), "unpaused delivery must reach the pty step, got: {err}");
        // Paused: delivery is suppressed (Ok, no error) and audited.
        reg.pause_group(&g.id).unwrap();
        assert!(reg.is_paused(&g.id));
        reg.deliver_prompt(&w.id, "hello again", "loomux", false).unwrap();
        let log = fs::read_to_string(reg.state_root().join(&g.id).join("audit.jsonl")).unwrap();
        assert!(log.contains("prompt-suppressed-paused"), "suppression must be audited");
        assert!(reg.state_root().join(&g.id).join("paused").is_file(), "pause marker must be written");
    }
    // Restart: the pause survives (marker re-seeds the in-memory flag).
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    assert_eq!(g.id, gid);
    assert!(reg.is_paused(&g.id), "a paused group must stay paused across restarts");
    // Resume clears the flag and the marker.
    reg.resume_group(&g.id).unwrap();
    assert!(!reg.is_paused(&g.id));
    assert!(!reg.state_root().join(&g.id).join("paused").is_file(), "resume must remove the marker");
}

#[test]
fn idle_workers_are_reap_candidates_but_busy_ones_and_orchestrator_are_not() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", costed_rails(5, 0)).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let idle = reg.spawn_agent(&g.id, Role::Worker, "idle", "", false, None).unwrap();
    let busy = reg.spawn_agent(&g.id, Role::Worker, "busy", "do work", false, None).unwrap();
    // Read the idle worker's stamped idle-since so the test is time-relative.
    let roster = reg.list_agents(&g.id);
    let idle_since = roster
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == idle.id.as_str())
        .and_then(|a| a["idle_since_ms"].as_u64())
        .expect("an idle-spawned worker must carry idle_since_ms");
    // The busy worker (spawned with a task) has no idle clock.
    let busy_idle = roster
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == busy.id.as_str())
        .unwrap()["idle_since_ms"]
        .clone();
    assert!(busy_idle.is_null(), "a worker given a task must not start the idle clock");
    let threshold_ms = 5 * 60_000u64;
    // Just before the threshold: nobody is reaped.
    let before = reg.idle_reap_candidates(idle_since + threshold_ms - 1);
    assert!(before.is_empty(), "must not reap before the timeout, got: {before:?}");
    // At/after the threshold: only the idle worker (never the orchestrator or
    // the busy worker).
    let after = reg.idle_reap_candidates(idle_since + threshold_ms);
    assert_eq!(after, vec![idle.id.clone()], "only the idle worker crosses the timeout");
}

#[test]
fn idle_kill_disabled_reaps_nothing() {
    let (reg, _d) = test_registry();
    // idle_kill_minutes = 0 → the guardrail is off.
    let g = reg.create_group("C:/tmp/repo", costed_rails(0, 0)).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "idle", "", false, None).unwrap();
    // Even absurdly far in the future, nothing is a candidate.
    assert!(reg.idle_reap_candidates(u64::MAX / 2).is_empty());
}

#[test]
fn reaper_spares_a_worker_reactivated_before_the_kill() {
    // Selection and kill happen under separate locks; a worker prompted in
    // that window (idle clock cleared) must not be reaped. reap_idle_agents
    // re-checks idle_should_kill immediately before killing.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", costed_rails(5, 0)).unwrap();
    let orch = reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let idle = reg.spawn_agent(&g.id, Role::Worker, "idle", "", false, None).unwrap();
    let co = reg.resolve_token(&orch.token).unwrap();
    let far_future = u64::MAX / 2;
    // The idle worker is a genuine candidate at that time.
    assert_eq!(reg.idle_reap_candidates(far_future), vec![idle.id.clone()]);
    // The orchestrator hands it work — send_prompt clears its idle clock
    // (delivery then fails in test mode with no pane, which is fine).
    let _ = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "send_prompt", "arguments": { "agent_id": idle.id, "text": "here is a task" } }));
    // Now it is no longer idle, so the reaper kills nothing.
    assert!(reg.reap_idle_agents(far_future).is_empty(),
        "a re-activated worker must not be reaped");
    // And it is still alive in the roster.
    let roster = reg.list_agents(&g.id).to_string();
    assert!(roster.contains(&idle.id));
}

#[test]
fn spawn_rate_guardrail_backstops_a_burst() {
    let (reg, _d) = test_registry();
    // Cap 2 spawns/hour, roomy agent cap so the rate limit is what bites.
    let g = reg.create_group("C:/tmp/repo", costed_rails(0, 2)).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w1", "t", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w2", "t", false, None).unwrap();
    let err = reg.spawn_agent(&g.id, Role::Worker, "w3", "t", false, None).unwrap_err();
    assert!(err.contains("spawn-rate"), "third spawn within the hour must be refused, got: {err}");
    // The orchestrator is exempt from the spawn-rate backstop.
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
}

#[test]
fn report_completion_reidles_worker_and_send_prompt_reactivates() {
    let (reg, _d, co, cw) = setup_mcp();
    // The worker from setup_mcp was spawned with a task → not idle.
    let idle_of = |id: &str| -> Value {
        reg.list_agents(&cw.group)
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap()["idle_since_ms"]
            .clone()
    };
    assert!(idle_of(&cw.agent_id).is_null(), "a tasked worker is not idle");
    // Reporting done re-idles it (delivery to the orchestrator fails in test
    // mode with no pane, but the idle transition happens first).
    let _ = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "report", "arguments": { "status": "done", "summary": "PR up" } }));
    assert!(!idle_of(&cw.agent_id).is_null(), "a worker that reported done becomes idle again");
    // The orchestrator sending it a fresh prompt clears the idle clock.
    let _ = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "send_prompt", "arguments": { "agent_id": cw.agent_id, "text": "next" } }));
    assert!(idle_of(&cw.agent_id).is_null(), "send_prompt must re-activate an idle worker");
}

// ---------- watchdog: stalled-agent detection (#10) ----------

/// A time far past any real `now_ms()` (year ~33658), so a `watchdog_tick` at
/// this instant is unambiguously past the stall window for an agent whose
/// clock was stamped at spawn/report with the real wall clock.
const FAR: u64 = 1_000_000_000_000_000;

/// Group with an orchestrator and one working (tasked) worker under a watchdog
/// with the given stall window (minutes). Returns (reg, tempdir, group, worker).
fn watchdog_setup(stall_min: u32) -> (OrchRegistry, tempfile::TempDir, String, String) {
    let (reg, dir) = test_registry();
    let g = reg.create_group("C:/tmp/repo", watchdog_rails(stall_min)).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "do work", false, None).unwrap();
    (reg, dir, g.id, w.id)
}

#[test]
fn watchdog_should_notify_respects_threshold_anti_nag_and_disable() {
    let min = 60_000u64;
    // A 0 window disables the guardrail entirely.
    assert!(!watchdog_should_notify(0, 100 * min, 0, false));
    // Inside the window: not yet.
    assert!(!watchdog_should_notify(0, 4 * min, 5, false));
    // At and past the window: notify.
    assert!(watchdog_should_notify(0, 5 * min, 5, false), "exactly at the window notifies");
    assert!(watchdog_should_notify(0, 10 * min, 5, false));
    // Past the window but already notified: the anti-nag latch suppresses it.
    assert!(!watchdog_should_notify(0, 10 * min, 5, true), "one notice per stall");
}

#[test]
fn watchdog_flags_a_silent_worker_once_per_stall() {
    let (reg, _d, gid, wid) = watchdog_setup(5);
    let no_output = HashMap::new();
    // Long past the stall window with no output and no report → one notice.
    assert_eq!(reg.watchdog_tick(FAR, &no_output), vec![wid.clone()],
        "a silent working agent must be flagged");
    let log = fs::read_to_string(reg.state_root().join(&gid).join("audit.jsonl")).unwrap();
    assert!(log.contains("watchdog-stall"), "the stall must be audited, got: {log}");
    // Anti-nag: still silent, but already notified for this same stall.
    assert!(reg.watchdog_tick(FAR + 60_000, &no_output).is_empty(),
        "must not nag twice for one uninterrupted stall");
}

#[test]
fn watchdog_stall_resets_when_the_agent_produces_output() {
    let (reg, _d, _gid, wid) = watchdog_setup(5);
    let empty = HashMap::new();
    assert_eq!(reg.watchdog_tick(FAR, &empty), vec![wid.clone()]);
    // The CLI emits output: a grown pty counter is activity — clock and latch
    // both reset, and this very tick must not also flag a stall.
    let grew: HashMap<String, u64> = [(wid.clone(), 1024u64)].into_iter().collect();
    assert!(reg.watchdog_tick(FAR, &grew).is_empty(), "output growth is activity, not a stall");
    // No further growth; a whole fresh window elapses → a brand-new notice.
    let later = FAR + 5 * 60_000 + 1;
    assert_eq!(reg.watchdog_tick(later, &grew), vec![wid.clone()],
        "a new stall after activity earns a new notice");
}

#[test]
fn watchdog_ignores_idle_dead_and_disabled_agents() {
    // A 0 stall window disables the watchdog for the whole group.
    let (off, _d0, _g0, _w0) = watchdog_setup(0);
    assert!(off.watchdog_tick(FAR, &HashMap::new()).is_empty(),
        "stall window 0 disables the watchdog");
    // With the guardrail on, idle and dead agents are still out of scope: idle
    // is the reaper's concern, and a dead/reaped pane must never be nudged.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo2", watchdog_rails(5)).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "idle", "", false, None).unwrap();
    let dead = reg.spawn_agent(&g.id, Role::Worker, "dead", "work", false, None).unwrap();
    reg.mark_dead(&dead.id, Some(1));
    let flagged = reg.watchdog_tick(FAR, &HashMap::new());
    assert!(flagged.is_empty(),
        "neither an idle nor a dead agent may be watchdog-flagged, got: {flagged:?}");
}

#[test]
fn watchdog_stays_quiet_for_a_paused_group() {
    let (reg, _d, gid, wid) = watchdog_setup(5);
    reg.pause_group(&gid).unwrap();
    assert!(reg.watchdog_tick(FAR, &HashMap::new()).is_empty(),
        "a paused group's agents idle out on purpose — no watchdog notices");
    // Crucially, the one-notice budget must be intact: pausing must not have
    // burned the latch, so on resume the outstanding stall still earns its
    // first notice.
    reg.resume_group(&gid).unwrap();
    assert_eq!(reg.watchdog_tick(FAR, &HashMap::new()), vec![wid.clone()],
        "resuming an unattended stall must still earn its first notice");
}

#[test]
fn watchdog_stall_resets_when_the_agent_reports_or_messages() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", watchdog_rails(5)).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "work", false, None).unwrap();
    let cw = reg.resolve_token(&w.token).unwrap();
    // Stalled and flagged (anti-nag latch now set).
    assert_eq!(reg.watchdog_tick(FAR, &HashMap::new()), vec![w.id.clone()]);
    // A progress report is a sign of life: it clears the latch (via re-idle
    // bookkeeping), so a later silence re-notifies. If the latch had NOT been
    // cleared this tick would be empty — that's the discriminator.
    let _ = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "report", "arguments": { "status": "progress", "summary": "still going" } }));
    assert_eq!(reg.watchdog_tick(FAR + 60_000, &HashMap::new()), vec![w.id.clone()],
        "a report must reset the stall, then a later silence re-notifies");
    // A free-form message likewise counts as activity and clears the latch.
    let _ = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "message_orchestrator", "arguments": { "text": "checking in" } }));
    assert_eq!(reg.watchdog_tick(FAR + 120_000, &HashMap::new()), vec![w.id.clone()],
        "a message must also reset the stall, then a later silence re-notifies");
}

#[test]
fn group_usage_summarizes_agents_with_null_cost_without_panes() {
    let (reg, _d, _co, _cw) = setup_mcp();
    let usage = reg.group_usage(&_co.group);
    // No live panes in test mode → every cost is null and there is no total.
    assert!(usage["total_cost_usd"].is_null());
    let agents = usage["agents"].as_array().unwrap();
    assert!(agents.iter().any(|a| a["id"] == "orch-1"), "orchestrator must appear");
    assert!(agents.iter().all(|a| a["cost_usd"].is_null()));
    // Exposed to the orchestrator over MCP too.
    let via_mcp = dispatch(&reg, &_co, "tools/call",
        &json!({ "name": "group_usage", "arguments": {} })).unwrap();
    assert_eq!(via_mcp["isError"], false);
    assert!(via_mcp["content"][0]["text"].as_str().unwrap().contains("total_cost_usd"));
    // Workers cannot pull the group-wide usage summary.
    let denied = dispatch(&reg, &_cw, "tools/call",
        &json!({ "name": "group_usage", "arguments": {} })).unwrap();
    assert_eq!(denied["isError"], true, "usage aggregation is orchestrator-only");
}

#[test]
fn group_json_records_cost_guardrails() {
    let (reg, _d) = test_registry();
    let mut rails = costed_rails(15, 30);
    rails.watchdog_stall_minutes = 12;
    let g = reg.create_group("C:/tmp/repo", rails).unwrap();
    let gj: Value = serde_json::from_str(
        &fs::read_to_string(reg.state_root().join(&g.id).join("group.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(gj["guardrails"]["idle_kill_minutes"], 15);
    assert_eq!(gj["guardrails"]["max_spawns_per_hour"], 30);
    assert_eq!(gj["guardrails"]["watchdog_stall_minutes"], 12);
}

// ---------- group lifecycle: summary & end-orchestration (#8) ----------

#[test]
fn worktree_cleanup_targets_dedupes_and_spares_the_repo_root() {
    let repo = "C:/Projects/loomux";
    let cwds = vec![
        // The orchestrator's cwd == the repo root, in a different spelling —
        // must never be a removal target (it's the user's real checkout).
        r"C:\Projects\loomux".to_string(),
        // Two workers sharing one worktree (resume reuses cwd) → one target.
        "C:/Projects/loomux-worktrees/a".to_string(),
        "C:/Projects/loomux-worktrees/a/".to_string(),
        "C:/Projects/loomux-worktrees/b".to_string(),
        "".to_string(), // an unbound agent with no cwd is skipped
    ];
    let targets = worktree_cleanup_targets(repo, &cwds);
    assert_eq!(
        targets,
        vec![
            "C:/Projects/loomux-worktrees/a".to_string(),
            "C:/Projects/loomux-worktrees/b".to_string(),
        ],
        "repo root excluded, case/separator/trailing-slash duplicates collapsed"
    );
}

#[test]
fn group_summary_counts_live_agents_roles_and_uptime() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w1", "do a thing", false, None).unwrap();
    let dead = reg.spawn_agent(&g.id, Role::Worker, "w2", "", false, None).unwrap();
    // A dead agent must drop out of the live count and role breakdown.
    reg.mark_dead(&dead.id, Some(0));

    let s = reg.group_summary(&g.id);
    assert_eq!(s["live_agents"], 2);
    assert_eq!(s["roles"]["orchestrator"], 1);
    assert_eq!(s["roles"]["worker"], 1);
    assert_eq!(s["roles"]["reviewer"], 0);
    assert_eq!(s["paused"], false);
    // Uptime is present (measured from the earliest live agent) and every live
    // agent carries its own uptime; the dead one is gone.
    assert!(s["uptime_ms"].as_u64().is_some(), "group uptime must be reported");
    let agents = s["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
    assert!(agents.iter().all(|a| a["uptime_ms"].as_u64().is_some()));
    assert!(!agents.iter().any(|a| a["id"] == dead.id.as_str()));
    // Pausing the group is reflected so the panel can compose the two actions.
    reg.pause_group(&g.id).unwrap();
    assert_eq!(reg.group_summary(&g.id)["paused"], true);
}

#[test]
fn end_group_kills_everyone_including_the_orchestrator() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w1", "t", false, None).unwrap();
    // kill_agent refuses the orchestrator; end_group must not.
    let result = reg.end_group(&g.id, false).unwrap();
    assert_eq!(result["killed"].as_array().unwrap().len(), 2);
    // Every agent is now dead — the group reads as fully torn down.
    for a in reg.list_agents(&g.id).as_array().unwrap() {
        assert_eq!(a["status"], "dead", "end must kill every role");
    }
    assert_eq!(reg.group_summary(&g.id)["live_agents"], 0);
    // The teardown is audited as a human action.
    let log = fs::read_to_string(reg.state_root().join(&g.id).join("audit.jsonl")).unwrap();
    let end = log
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|e| e["action"] == "group-end")
        .expect("end must be audited");
    assert_eq!(end["actor"], "human");
    // Unknown group: an error, not a silent success.
    assert!(reg.end_group("ghost-group", false).is_err());
}

#[test]
fn end_group_clears_pause_so_relaunch_starts_clean() {
    let dir = tempfile::tempdir().unwrap();
    let gid;
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
        gid = g.id.clone();
        reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
        // A paused group that gets ended: the pause marker must not outlive it,
        // or a relaunch on the same repo id would silently resume paused.
        reg.pause_group(&g.id).unwrap();
        assert!(reg.state_root().join(&g.id).join("paused").is_file());
        reg.end_group(&g.id, false).unwrap();
        assert!(!reg.is_paused(&g.id), "ending must drop the in-memory pause");
        assert!(
            !reg.state_root().join(&g.id).join("paused").is_file(),
            "ending must remove the pause marker"
        );
    }
    // Relaunch on the same repo → same id, not paused.
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    assert_eq!(g.id, gid);
    assert!(!reg.is_paused(&g.id), "a relaunched group must not inherit the old pause");
}

#[test]
fn end_group_removes_worktrees_of_dead_and_live_agents() {
    // A real git repo with two worktrees; end_group(cleanup=true) must reclaim
    // both — including the one whose worker already exited — while leaving the
    // main checkout intact.
    let repo = tempfile::tempdir().unwrap();
    let repo_path = repo.path().to_string_lossy().replace('\\', "/");
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(args)
            .output()
            .expect("git must be installed for this test");
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    fs::write(repo.path().join("f.txt"), "hi").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);

    let (reg, _d) = test_registry();
    let g = reg.create_group(&repo_path, rails()).unwrap();
    // Two worktree-backed workers (spawn creates the worktree via git).
    let live = reg
        .spawn_agent(&g.id, Role::Worker, "live", "t", true, Some("wt-live".into()))
        .unwrap();
    let dead = reg
        .spawn_agent(&g.id, Role::Worker, "dead", "t", true, Some("wt-dead".into()))
        .unwrap();
    assert!(Path::new(&live.cwd).is_dir() && Path::new(&dead.cwd).is_dir());
    // One worker has already exited — its worktree must still be reclaimed.
    reg.mark_dead(&dead.id, Some(0));

    let result = reg.end_group(&g.id, true).unwrap();
    assert!(result["worktree_errors"].as_array().unwrap().is_empty(), "got: {result}");
    assert_eq!(result["worktrees_removed"].as_array().unwrap().len(), 2);
    assert!(!Path::new(&live.cwd).exists(), "live agent's worktree must be gone");
    assert!(!Path::new(&dead.cwd).exists(), "exited agent's worktree must be gone");
    // The main checkout is untouched.
    assert!(repo.path().join("f.txt").is_file(), "the repo root must survive teardown");
}
