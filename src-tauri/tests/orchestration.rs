//! Functional tests for the orchestration backend: guardrails, role authz,
//! group isolation, persistence, audit, and the MCP dispatch surface.
//!
//! These live as integration tests (not unit tests) because test executables
//! that link the full lib need the common-controls-v6 manifest embedded via
//! `rustc-link-arg-tests` (see build.rs / test.manifest), which cargo only
//! applies to integration-test targets.

use loomux_lib::orchestration::mcp::dispatch;
use loomux_lib::orchestration::{bracketed_paste, strip_ansi, Caller, Guardrails, OrchRegistry, Role};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn rails() -> Guardrails {
    Guardrails {
        max_agents: 2,
        worker_model: "sonnet".into(),
        reviewer_model: "sonnet".into(),
        orchestrator_model: "opus".into(),
        full_auto: false,
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
        worker_model: "sonnet; rm -rf /".into(),
        reviewer_model: "".into(),
        orchestrator_model: "opus".into(),
        full_auto: true,
    }
    .clamped();
    assert_eq!(g.max_agents, 12, "cap must clamp to the hard ceiling");
    assert_eq!(g.worker_model, "sonnetrm-rf", "shell metacharacters must be stripped");
    assert_eq!(g.reviewer_model, "sonnet", "empty model falls back to default");
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
fn command_line_carries_guardrail_model_and_permissions() {
    let (reg, _d) = test_registry();
    let cmd = reg.build_claude_command("sonnet", false, Path::new("C:/x/cfg.json"));
    assert!(cmd.contains("--model sonnet"));
    assert!(cmd.contains("--permission-mode acceptEdits"));
    assert!(cmd.contains("--strict-mcp-config"), "workers must not see the user's other MCP servers");
    let cmd = reg.build_claude_command("sonnet", true, Path::new("C:/x/cfg.json"));
    assert!(cmd.contains("--dangerously-skip-permissions"));
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
