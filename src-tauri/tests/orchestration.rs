//! Functional tests for the orchestration backend: guardrails, role authz,
//! group isolation, persistence, audit, and the MCP dispatch surface.
//!
//! These live as integration tests (not unit tests) because test executables
//! that link the full lib need the common-controls-v6 manifest embedded via
//! `rustc-link-arg-tests` (see build.rs / test.manifest), which cargo only
//! applies to integration-test targets.

use loomux_lib::orchestration::mcp::dispatch;
use loomux_lib::orchestration::{
    add_trusted_folder, bracketed_paste, cli_ready, create_orchestration_group, hold_until_quiet,
    idle_should_kill, max_agents_notice,
    normalize_remote_web_base, parse_audit_lines, parse_session_cost, prompt_wait_detected,
    resolve_ref_url, rotate_audit_if_needed, sanitize_attachment_ext, should_flush_before_paste,
    spawn_rate_exceeded, strip_ansi, submit_confirmed, submit_sequence, watchdog_should_notify,
    worktree_cleanup_targets, AttentionItem, Caller,
    Guardrails, NameSource, OrchRegistry, Role, TaskPatch, UsageSnapshot, MAX_ATTACHMENT_BYTES,
    PLANNER_READONLY_NOTE,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Wall-clock Unix-ms, mirroring the crate-private `now_ms` — the debounce
/// tests inject `now_ms() + window` into `flush_due_max_notices` to fire a
/// pending notice deterministically without sleeping out the real 3s window.
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

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
        planner_model: "opus".into(),
        auto_ops: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
        // Per-role CLIs default to inheriting `agent_cli`.
        ..Guardrails::default()
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
        planner_model: "".into(),
        auto_ops: true,
        idle_kill_minutes: 99999,
        max_spawns_per_hour: 9999,
        watchdog_stall_minutes: 99999,
        ..Guardrails::default()
    }
    .clamped();
    assert_eq!(g.max_agents, 12, "cap must clamp to the hard ceiling");
    assert_eq!(g.idle_kill_minutes, 1440, "idle-kill timeout clamps to 24h");
    assert_eq!(g.max_spawns_per_hour, 240, "spawn-rate cap clamps to the ceiling");
    assert_eq!(g.watchdog_stall_minutes, 1440, "watchdog stall timeout clamps to 24h");
    assert_eq!(g.agent_cli, "claude", "unknown group CLIs fall back to claude explicitly");
    assert_eq!(g.worker_model, "sonnetrm-rf", "shell metacharacters must be stripped");
    assert_eq!(g.reviewer_model, "sonnet", "empty model falls back to default");
    // Reasoning roles (orchestrator, planner) default to the strong tier on Claude.
    assert_eq!(g.planner_model, "opus", "empty planner model falls back to the reasoning tier");
    // Copilot's fallback model is "auto" (it picks the best itself), and a
    // per-role CLI overrides the group default (issue #4).
    let g = Guardrails {
        max_agents: 4,
        agent_cli: "copilot".into(),
        worker_model: "".into(),
        reviewer_model: "".into(),
        orchestrator_model: "".into(),
        planner_model: "".into(),
        auto_ops: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
        ..Guardrails::default()
    }
    .clamped();
    assert_eq!(g.worker_model, "auto");
    assert_eq!(g.orchestrator_model, "auto");
    assert_eq!(g.planner_model, "auto");
    // A per-role CLI is honored by `cli_for` (empty roles inherit agent_cli);
    // its model fallback follows the role's *effective* CLI.
    let g = Guardrails {
        max_agents: 4,
        agent_cli: "copilot".into(),
        worker_cli: "claude".into(),
        worker_model: "".into(),
        ..Guardrails::default()
    }
    .clamped();
    assert_eq!(g.cli_for(Role::Worker), "claude", "per-role CLI overrides the group default");
    assert_eq!(g.cli_for(Role::Reviewer), "copilot", "an empty per-role CLI inherits the group default");
    assert_eq!(g.worker_model, "sonnet", "worker model fallback follows the worker's claude CLI");
    assert_eq!(g.reviewer_model, "auto", "reviewer model fallback follows the inherited copilot CLI");
}

// ---------- #56: adjustable max_agents on the fly ----------

#[test]
fn set_max_agents_validates_bounds() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // Below the floor and above the ceiling are refused; the cap is unchanged.
    assert!(reg.set_max_agents(&g.id, 0, "human").is_err());
    assert!(reg.set_max_agents(&g.id, 13, "human").is_err());
    assert_eq!(reg.group(&g.id).unwrap().guardrails.max_agents, 2, "a rejected change must not mutate the cap");
    // The inclusive bounds 1..=12 are accepted.
    assert_eq!(reg.set_max_agents(&g.id, 1, "human").unwrap(), 1);
    assert_eq!(reg.set_max_agents(&g.id, 12, "human").unwrap(), 12);
    // An unknown group is an error, not a panic.
    assert!(reg.set_max_agents("no-such-group", 3, "human").is_err());
}

#[test]
fn set_max_agents_enforcement_reads_live_value() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap(); // cap 2
    reg.spawn_agent(&g.id, Role::Worker, "w1", "t", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w2", "t", false, None).unwrap();
    // At the cap: a third is refused.
    assert!(reg.spawn_agent(&g.id, Role::Worker, "w3", "t", false, None).is_err());
    // Raise the cap live → spawn_agent reads the new value, so the next spawn
    // succeeds (nothing cached the creation-time number).
    assert_eq!(reg.set_max_agents(&g.id, 3, "human").unwrap(), 3);
    reg.spawn_agent(&g.id, Role::Worker, "w3", "t", false, None).unwrap();
    // Lower it below the live count → new spawns blocked again immediately.
    assert_eq!(reg.set_max_agents(&g.id, 1, "human").unwrap(), 1);
    let err = reg.spawn_agent(&g.id, Role::Worker, "w4", "t", false, None).unwrap_err();
    assert!(err.contains("guardrail"), "a lowered cap must block new spawns, got: {err}");
}

#[test]
fn lowering_max_agents_kills_nobody() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap(); // cap 2
    let w1 = reg.spawn_agent(&g.id, Role::Worker, "w1", "t", false, None).unwrap();
    let w2 = reg.spawn_agent(&g.id, Role::Worker, "w2", "t", false, None).unwrap();
    // Drop the cap under the live count: attrition-only, no kills.
    reg.set_max_agents(&g.id, 1, "human").unwrap();
    let sum = reg.group_summary(&g.id);
    assert_eq!(sum["live_agents"].as_u64().unwrap(), 2, "both live workers survive a lowered cap");
    assert_eq!(sum["max_agents"].as_u64().unwrap(), 1, "summary reflects the new cap");
    assert_eq!(sum["live_delegates"].as_u64().unwrap(), 2, "summary exposes the count that would block spawns");
    // Still present and not dead in the roster.
    for w in [&w1, &w2] {
        let alive = reg.list_agents(&g.id).as_array().unwrap().iter().any(|a| {
            a["id"] == json!(w.id) && a["status"] != json!("dead")
        });
        assert!(alive, "worker {} must stay alive", w.id);
    }
}

#[test]
fn max_agents_change_survives_launcher_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let gid;
    let path;
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", rails()).unwrap(); // launcher cap 2
        gid = g.id.clone();
        path = reg.state_root().join(&g.id).join("group.json");
        reg.set_max_agents(&g.id, 9, "human").unwrap();
    }
    // group.json carries the new cap; unrelated fields are preserved (the
    // update patches the field in place rather than rewriting the file).
    let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["guardrails"]["max_agents"].as_u64().unwrap(), 9);
    assert!(v["created_ms"].as_u64().is_some(), "created_ms must survive the patch");
    assert_eq!(v["guardrails"]["worker_model"], json!("sonnet"), "other guardrails must survive the patch");
    // A fresh registry (app restart) + a real launcher relaunch on the same
    // repo: this drives create_group's actual resume path, not a hand-fed
    // group.json. The launcher hardcodes its default cap (rails() = 2), but the
    // persisted adjustment (9) must win — otherwise the relaunch silently
    // reverts 9→2. Other guardrails still come from the launch.
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    assert_eq!(g.id, gid, "restart resumes the same group");
    assert_eq!(g.guardrails.max_agents, 9, "the persisted cap wins over the launcher default on resume");
    // ...and it's the value the resumed group actually holds + re-persists.
    assert_eq!(reg.group(&gid).unwrap().guardrails.max_agents, 9);
    let v: Value =
        serde_json::from_str(&fs::read_to_string(reg.state_root().join(&gid).join("group.json")).unwrap()).unwrap();
    assert_eq!(v["guardrails"]["max_agents"].as_u64().unwrap(), 9, "resume re-persists the honored cap");
}

#[test]
fn new_group_still_honors_the_launcher_cap() {
    // The resume-prefers-persisted rule must not leak into genuinely new
    // groups: a first launch on a repo uses the caller's cap verbatim.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/fresh-repo", Guardrails { max_agents: 5, ..rails() }).unwrap();
    assert_eq!(g.guardrails.max_agents, 5, "a new group takes the launcher's cap");
}

#[test]
fn max_agents_change_audits_and_notifies_orchestrator() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap(); // cap 2
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    // Pause so the notice delivery is suppressed-but-audited (test mode has no
    // pane to type into) — this lets us observe the exact notice text.
    reg.pause_group(&g.id).unwrap();
    reg.set_max_agents(&g.id, 4, "human").unwrap();
    // The audit is immediate (per-click); the notice is debounced (#79), so it
    // is delivered only when its window has elapsed — drive the flush past the
    // 3s debounce deterministically (no sleep) to observe the notice text.
    reg.flush_due_max_notices(now_ms() + 4_000);
    let log = fs::read_to_string(reg.state_root().join(&g.id).join("audit.jsonl")).unwrap();
    let events: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(
        events.iter().any(|e|
            e["action"] == json!("max-agents-set")
            && e["detail"]["from"] == json!(2) && e["detail"]["to"] == json!(4)
            && e["actor"] == json!("human")),
        "the cap change must be audited with from/to and the actor"
    );
    assert!(
        events.iter().any(|e|
            e["action"] == json!("prompt-suppressed-paused")
            && e["detail"]["text"].as_str().unwrap_or("").contains("max live agents changed 2→4")),
        "the orchestrator must receive the cap-change re-plan notice"
    );
}

// Helper: read the audit log and count the coalesced re-plan notices (visible
// as `prompt-suppressed-paused` entries because the group is paused in tests).
fn replan_notices(reg: &OrchRegistry, group: &str) -> Vec<String> {
    let log = fs::read_to_string(reg.state_root().join(group).join("audit.jsonl")).unwrap();
    log.lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|e| e["action"] == json!("prompt-suppressed-paused"))
        .filter_map(|e| e["detail"]["text"].as_str().map(str::to_string))
        .filter(|t| t.contains("max live agents changed"))
        .collect()
}

#[test]
fn rapid_max_agents_clicks_coalesce_to_one_notice() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap(); // cap 2
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    reg.pause_group(&g.id).unwrap();
    // A burst of stepper clicks 2→4→6→3, all within the debounce window (test
    // calls land in the same few ms). Each persists + enforces + audits per
    // click, but the notice is held.
    reg.set_max_agents(&g.id, 4, "human").unwrap();
    reg.set_max_agents(&g.id, 6, "human").unwrap();
    reg.set_max_agents(&g.id, 3, "human").unwrap();
    // Before the window elapses, nothing has been delivered.
    reg.flush_due_max_notices(now_ms());
    assert!(replan_notices(&reg, &g.id).is_empty(), "no notice fires mid-burst");
    // Every click is audited (enforcement/persist stay per-click).
    let sets = fs::read_to_string(reg.state_root().join(&g.id).join("audit.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| l.contains("max-agents-set"))
        .count();
    assert_eq!(sets, 3, "each click is audited immediately");
    // Once the window passes: exactly ONE notice, spanning the whole burst
    // (2→3), never the intermediate 2→4 / 4→6 values.
    reg.flush_due_max_notices(now_ms() + 4_000);
    let notices = replan_notices(&reg, &g.id);
    assert_eq!(notices.len(), 1, "a burst yields one coalesced notice, got: {notices:?}");
    assert!(notices[0].contains("2→3"), "notice spans the whole burst, got: {}", notices[0]);
}

#[test]
fn spaced_max_agents_changes_notify_separately() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap(); // cap 2
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    reg.pause_group(&g.id).unwrap();
    // First change flushes fully before the second is recorded → two notices.
    reg.set_max_agents(&g.id, 5, "human").unwrap();
    reg.flush_due_max_notices(now_ms() + 4_000);
    reg.set_max_agents(&g.id, 2, "human").unwrap();
    reg.flush_due_max_notices(now_ms() + 8_000);
    let notices = replan_notices(&reg, &g.id);
    assert_eq!(notices.len(), 2, "spaced changes stay separate, got: {notices:?}");
    assert!(notices[0].contains("2→5"), "first notice: {}", notices[0]);
    assert!(notices[1].contains("5→2"), "second notice: {}", notices[1]);
}

#[test]
fn planners_count_toward_live_delegates_summary() {
    // #47 makes planners count against the cap (live_delegate_count includes
    // them); the summary's live_delegates must agree so the UI's "cap below N
    // live" warning stays honest.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", Guardrails { max_agents: 5, ..rails() }).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w1", "t", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Reviewer, "r1", "t", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Planner, "p1", "t", false, None).unwrap();
    let sum = reg.group_summary(&g.id);
    assert_eq!(
        sum["live_delegates"].as_u64().unwrap(),
        3,
        "worker + reviewer + planner all count against the cap"
    );
}

#[test]
fn setting_same_max_agents_is_a_noop() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap(); // cap 2
    assert_eq!(reg.set_max_agents(&g.id, 2, "human").unwrap(), 2);
    let log = fs::read_to_string(reg.state_root().join(&g.id).join("audit.jsonl")).unwrap();
    assert!(!log.contains("max-agents-set"), "a no-op change must not audit or notify");
}

#[test]
fn set_max_agents_fails_soft_on_corrupt_group_file() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // A valid-JSON but non-object root (e.g. from corruption) must error rather
    // than panic on the in-place field assignment.
    fs::write(reg.state_root().join(&g.id).join("group.json"), "null").unwrap();
    let err = reg.set_max_agents(&g.id, 5, "human").unwrap_err();
    assert!(err.contains("not a JSON object"), "non-object root must fail soft, got: {err}");
}

#[test]
fn max_agents_notice_reads_naturally() {
    assert_eq!(
        max_agents_notice(4, 2),
        "[loomux] max live agents changed 4→2 — re-plan accordingly"
    );
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
fn submit_sequence_claude_is_a_bare_cr() {
    // Claude's submit must stay byte-identical: a single carriage return, no
    // focus prefix, no CRLF. Any drift here would regress the tuned TUI path.
    assert_eq!(submit_sequence("claude"), b"\r");
}

#[test]
fn submit_sequence_unknown_cli_falls_back_to_bare_cr() {
    // An unrecognized / future CLI gets the safe default (bare CR), never the
    // Copilot-specific focus prefix.
    assert_eq!(submit_sequence("aider"), b"\r");
    assert_eq!(submit_sequence(""), b"\r");
}

#[test]
fn submit_sequence_copilot_prefixes_focus_in_before_enter() {
    // #98: Copilot drops non-paste keystrokes on an unfocused pane, so a bare
    // CR after the paste never submits. The fix prefixes a focus-in report
    // (CSI I) that flips Copilot's focus flag true, so the CR that follows is
    // accepted. Order matters: focus-in MUST come before the CR.
    let seq = submit_sequence("copilot");
    assert_eq!(seq, b"\x1b[I\r");
    assert!(seq.starts_with(b"\x1b[I"), "focus-in must precede the Enter");
    assert!(seq.ends_with(b"\r"), "the Enter itself is still a bare CR");
    // The focus report is exactly CSI I (no params/intermediates) — that is
    // what Copilot's CSI parser maps to a focus event; a stray param would
    // parse as something else and not flip the flag.
    assert_eq!(&seq[..seq.len() - 1], b"\x1b[I");
}

#[test]
fn flush_reuses_the_per_cli_submit_sequence() {
    // The stranded-text flush presses submit once — it must use the SAME
    // per-CLI sequence as a normal submit, so Copilot's flush also carries the
    // focus-in prefix (a bare CR would be ignored on an unfocused pane, and the
    // stranded text would never clear).
    assert_eq!(submit_sequence("copilot"), b"\x1b[I\r");
    assert_eq!(submit_sequence("claude"), b"\r");
}

#[test]
fn should_flush_only_on_the_stranded_text_signature() {
    // First delivery to a pane (no prior outcome): never flush.
    assert!(!should_flush_before_paste(None, false));
    assert!(!should_flush_before_paste(None, true));
    // Previous delivery confirmed as submitted: box is empty, never flush.
    assert!(!should_flush_before_paste(Some(true), false));
    assert!(!should_flush_before_paste(Some(true), true));
    // Previous delivery unconfirmed BUT a human has typed since: their line may
    // be in the box — never blind-submit it.
    assert!(!should_flush_before_paste(Some(false), true));
    // The one case that flushes: previous unconfirmed, no human input since.
    assert!(should_flush_before_paste(Some(false), false));
}

#[test]
fn submit_confirmed_needs_a_real_output_burst() {
    // No / trivial growth after Enter -> not confirmed (an ignored key, or idle
    // cursor-blink noise, must not read as a landed submit).
    assert!(!submit_confirmed(1000, 1000));
    assert!(!submit_confirmed(1000, 1010));
    // A burst clearing the threshold -> confirmed.
    assert!(submit_confirmed(1000, 1024));
    assert!(submit_confirmed(1000, 100_000));
    // Totals never go backwards, but a wrapped/garbage reading must not panic
    // or false-confirm.
    assert!(!submit_confirmed(1000, 500));
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
    let cmd = reg.build_agent_command("claude", "sonnet", false, cfg, gdir, Path::new("C:/repo"), None, false, false);
    assert!(cmd.contains("--model sonnet"));
    assert!(cmd.contains("--permission-mode acceptEdits"));
    assert!(cmd.contains("--strict-mcp-config"), "workers must not see the user's other MCP servers");
    assert!(cmd.contains("--add-dir \"C:/data/group\""),
        "instructions dir must be a workspace so reading it never prompts");
    assert!(cmd.contains("--allowedTools mcp__loomux"),
        "loomux tools must be pre-approved so report/list never prompt");
    assert!(!cmd.contains("Bash(git"), "git is not pre-approved for a non-auto_ops worker");
    let cmd = reg.build_agent_command("claude", "sonnet", true, cfg, gdir, Path::new("C:/repo"), None, false, false);
    assert!(cmd.contains("--permission-mode auto"),
        "the Auto preset must use Claude Code's native auto permission mode");
    assert!(cmd.contains("\"Bash(git *)\"") && cmd.contains("\"Bash(gh *)\""),
        "auto_ops pre-approves git + gh so the branch→commit→PR flow runs unattended");
    assert!(
        !cmd.contains("--dangerously-skip-permissions"),
        "bypass mode must never be used: its confirm dialog defaults to exit and kills the pane"
    );
    // A worker (read_only=false) has no write/commit denials.
    assert!(!cmd.contains("--disallowedTools"), "non-planner agents get no tool denials");
    // A planner (read_only=true) is denied file writes + git commit/push at
    // the CLI level, even under Auto perms — but keeps gh for the plan comment.
    let plan = reg.build_agent_command("claude", "opus", true, cfg, gdir, Path::new("C:/repo"), None, false, true);
    assert!(plan.contains("--disallowedTools"), "planner must deny tools structurally");
    for denied in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
        assert!(plan.contains(denied), "planner must deny the {denied} tool");
    }
    assert!(plan.contains("\"Bash(git commit *)\"") && plan.contains("\"Bash(git push *)\""),
        "planner must deny git commit/push using the canonical (space-form) rule spelling");
    assert!(plan.contains("\"Bash(gh *)\""), "gh stays allowed so the planner can post its plan comment");
    assert!(!plan.contains("\"Bash(git checkout"),
        "read-only git usage (checkout/log/diff) must not be denied");
    // The colon-mid wildcard is malformed: Claude Code ignores the rule AND
    // prints a startup warning (the "auto deny rule" flash on planner boot).
    // No rule may use it — that regression is what this pins down.
    assert!(!plan.contains("commit:*") && !plan.contains("push:*"),
        "no colon-mid wildcard rule (`Bash(git commit:*)`) — it is malformed and triggers the startup warning flash");
}

#[test]
fn planner_runs_unattended_regardless_of_auto_ops() {
    // A planner has no human in its pane, so it must reach gh + the loomux MCP
    // and explore read-only WITHOUT prompting — even when the group is NOT in
    // auto_ops. Otherwise it deadlocks on the first approval no one can give,
    // which is exactly why claude's `plan` permission mode can't be used here.
    let (reg, _d) = test_registry();
    let cfg = Path::new("C:/x/cfg.json");
    let gdir = Path::new("C:/data/group");
    // auto_ops = FALSE, read_only = TRUE (a planner in a manual-ops group).
    let plan = reg.build_agent_command("claude", "opus", false, cfg, gdir, Path::new("C:/repo"), None, false, true);
    assert!(plan.contains("--permission-mode auto"),
        "a planner runs unattended (Auto perms) even when the group is not auto_ops — else it deadlocks");
    assert!(plan.contains("\"Bash(gh *)\""),
        "a non-auto_ops planner must still have gh pre-approved so `gh issue comment` (its plan) never prompts");
    assert!(plan.contains("\"Bash(git *)\""),
        "a non-auto_ops planner must still have read-only git pre-approved for exploration");
    assert!(plan.contains("--disallowedTools") && plan.contains("Write"),
        "writes/commit/push stay denied structurally — Auto perms don't loosen the read-only contract");
    // By contrast a non-auto_ops WORKER (read_only=false) is unchanged: it
    // stays in acceptEdits with no pre-approved git/gh (the human gates ops).
    let worker = reg.build_agent_command("claude", "sonnet", false, cfg, gdir, Path::new("C:/repo"), None, false, false);
    assert!(worker.contains("--permission-mode acceptEdits"),
        "a non-auto_ops worker is unaffected: it still gates ops through acceptEdits");
    assert!(!worker.contains("\"Bash(gh *)\""),
        "a non-auto_ops worker gets no pre-approved gh — only planners run unattended");
}

#[test]
fn copilot_command_uses_copilot_adapter_flags() {
    let (reg, _d) = test_registry();
    let cfg = Path::new("C:/x/cfg.json");
    let gdir = Path::new("C:/data/group");
    let cmd = reg.build_agent_command("copilot", "auto", true, cfg, gdir, Path::new("C:/repo"), None, false, false);
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
    let cmd = reg.build_agent_command("copilot", "auto", false, cfg, gdir, Path::new("C:/repo"), None, false, false);
    assert!(!cmd.contains("--autopilot") && !cmd.contains("--allow-all-tools"));
    assert!(cmd.contains("--allow-tool \"shell(git:*)\"") && cmd.contains("--allow-tool \"shell(gh:*)\""));
    // Resume reopens a tracked session via --resume; copilot has no
    // pre-assignable id, so a session without resume adds no session flag.
    let sid = "aabbccdd-1122-4334-8556-77889900aabb";
    let cmd = reg.build_agent_command("copilot", "auto", true, cfg, gdir, Path::new("C:/repo"), Some(sid), true, false);
    assert!(cmd.contains(&format!("--resume {sid}")), "copilot resume must pass --resume, got: {cmd}");
    let cmd = reg.build_agent_command("copilot", "auto", true, cfg, gdir, Path::new("C:/repo"), Some(sid), false, false);
    assert!(!cmd.contains("--resume") && !cmd.contains("--session-id"),
        "a fresh copilot spawn cannot pin a session id");
    // A non-planner copilot agent gets no deny-tool flags.
    assert!(!cmd.contains("--deny-tool"), "non-planner copilot agents get no tool denials");
    // A planner (read_only=true) denies writes + git commit/push even under
    // --allow-all-tools (deny wins in Copilot); gh stays reachable.
    let plan = reg.build_agent_command("copilot", "auto", true, cfg, gdir, Path::new("C:/repo"), None, false, true);
    assert!(plan.contains("--deny-tool \"write\"") && plan.contains("--deny-tool \"edit\""),
        "planner must deny copilot's write/edit tools, got: {plan}");
    assert!(plan.contains("--deny-tool \"shell(git commit)\"") && plan.contains("--deny-tool \"shell(git push)\""),
        "planner must deny git commit/push");
    assert!(!plan.contains("--deny-tool \"shell(gh"), "gh stays allowed for the plan comment");
}

#[test]
fn copilot_planner_runs_unattended_regardless_of_auto_ops() {
    // Mirror of the claude fix on the copilot adapter: a planner has no human
    // in its pane, so a NON-auto_ops copilot planner must still take copilot's
    // unattended (autopilot) preset — the conservative interactive preset
    // would stall it on approvals no one can give. Deny rules keep it
    // read-only (deny wins over --allow-all-tools in Copilot).
    let (reg, _d) = test_registry();
    let cfg = Path::new("C:/x/cfg.json");
    let gdir = Path::new("C:/data/group");
    // auto_ops = FALSE, read_only = TRUE (a planner in a manual-ops group).
    let plan = reg.build_agent_command("copilot", "auto", false, cfg, gdir, Path::new("C:/repo"), None, false, true);
    assert!(plan.contains("--autopilot") && plan.contains("--allow-all-tools") && plan.contains("--allow-all-paths"),
        "a non-auto_ops copilot planner must run unattended (autopilot), else it deadlocks: {plan}");
    assert!(plan.contains("--deny-tool \"write\"") && plan.contains("--deny-tool \"shell(git commit)\""),
        "writes/commit stay denied — the autopilot preset doesn't loosen the read-only contract");
    assert!(!plan.contains("--deny-tool \"shell(gh"),
        "gh stays allowed so the copilot planner can post its plan comment unattended");
    // A non-auto_ops copilot WORKER (read_only=false) is unchanged: it keeps
    // the conservative interactive preset, not autopilot.
    let worker = reg.build_agent_command("copilot", "auto", false, cfg, gdir, Path::new("C:/repo"), None, false, false);
    assert!(!worker.contains("--autopilot") && !worker.contains("--allow-all-tools"),
        "a non-auto_ops copilot worker stays interactive — only planners run unattended");
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
            None,
        )
        .unwrap();
    assert_eq!(w.session_id.as_deref(), Some(sid.as_str()));
    // A mangled id is rejected rather than silently resuming the wrong one.
    assert!(reg
        .spawn_agent_ex(
            &g.id, Role::Worker, "bad", "", false, None,
            Some("../../etc/passwd".into()),
            Some(dir.path().to_string_lossy().into_owned()),
            None,
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
fn planner_kickoff_references_planner_instructions() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let p = reg
        .spawn_agent(&g.id, Role::Planner, "plan-47", "Plan issue #47", false, None)
        .unwrap();
    let k = reg.kickoff_prompt(&p, &g, "note");
    assert!(k.contains("planner.md"), "planner kickoff must reference its instructions file");
    assert!(k.contains("a planner agent"), "kickoff must name the planner role");
    assert!(k.contains("Plan issue #47"));
}

#[test]
fn planner_explores_read_only_and_never_gets_a_worktree() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // Even with worktree requested, a planner runs in the repo itself — it
    // never branches, worktrees, commits, or PRs (#47).
    let p = reg
        .spawn_agent(&g.id, Role::Planner, "plan", "Plan it", true, Some("plan/x".into()))
        .unwrap();
    assert_eq!(p.cwd, "C:/tmp/repo", "a planner must not get a dedicated worktree");
    assert!(
        reg.list_agents(&g.id).as_array().unwrap().iter().any(|a| a["role"] == "planner"),
        "planner must appear in the roster with its role"
    );
    // The planner's spawn-time read-only note (PLANNER_READONLY_NOTE) is threaded
    // verbatim into its kickoff, communicating the no-code/branches/PRs contract.
    let k = reg.kickoff_prompt(&p, &g, PLANNER_READONLY_NOTE);
    assert!(
        k.contains("never create branches, worktrees, commits, or PRs"),
        "planner kickoff must carry the read-only containment note, got: {k}"
    );
    assert!(
        PLANNER_READONLY_NOTE.contains("read-only") && PLANNER_READONLY_NOTE.contains("issue comment"),
        "the containment note must state the read-only contract and the issue-comment deliverable"
    );
}

#[test]
fn planner_counts_toward_the_live_agent_cap() {
    let (reg, _d) = test_registry();
    // Cap is 2 (rails). A worker + a planner fill it; the next delegate is refused.
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w1", "t", false, None).unwrap();
    reg.spawn_agent(&g.id, Role::Planner, "p1", "plan", false, None).unwrap();
    let err = reg
        .spawn_agent(&g.id, Role::Reviewer, "rev1", "t", false, None)
        .unwrap_err();
    assert!(err.contains("guardrail"), "a planner must count against the delegate cap: {err}");
}

#[test]
fn per_role_cli_is_pinned_at_spawn_and_persisted() {
    let (reg, _d) = test_registry();
    // Group default is copilot, but the reviewer role overrides to claude (#4).
    let rails = Guardrails {
        agent_cli: "copilot".into(),
        reviewer_cli: "claude".into(),
        max_agents: 4,
        ..rails()
    };
    let g = reg.create_group("C:/tmp/mixed-repo", rails).unwrap();
    // Observable per-role effect: claude agents get a pre-assigned session id;
    // copilot agents mint their own later, so start without one.
    let worker = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    let reviewer = reg.spawn_agent(&g.id, Role::Reviewer, "rev", "t", false, None).unwrap();
    assert!(worker.session_id.is_none(), "worker inherits the copilot group default (no pre-assigned session)");
    assert!(reviewer.session_id.is_some(), "reviewer's per-role claude CLI pre-assigns a session id");
    // The per-role config is persisted additively to group.json.
    let gj = fs::read_to_string(reg.state_root().join(&g.id).join("group.json")).unwrap();
    let v: Value = serde_json::from_str(&gj).unwrap();
    assert_eq!(v["guardrails"]["reviewer_cli"], "claude");
    assert_eq!(v["guardrails"]["agent_cli"], "copilot");
    assert!(v["guardrails"].get("planner_cli").is_some(), "planner_cli must be persisted");
    assert!(v["guardrails"].get("planner_model").is_some(), "planner_model must be persisted");
}

#[test]
fn unknown_per_role_cli_is_rejected_at_spawn() {
    let (reg, _d) = test_registry();
    // A hand-edited group.json could pin an unsupported CLI to a role; the
    // spawn must reject it rather than silently downgrade (#4).
    let rails = Guardrails { worker_cli: "aider".into(), max_agents: 4, ..rails() };
    let g = reg.create_group("C:/tmp/bad-cli-repo", rails).unwrap();
    let err = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap_err();
    assert!(err.contains("unsupported agent CLI"), "unknown per-role CLI must be rejected: {err}");
    // Roles that inherit the (valid) group default still spawn fine.
    reg.spawn_agent(&g.id, Role::Reviewer, "rev", "t", false, None).unwrap();
}

#[test]
fn instruction_files_rendered_with_group_facts() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/myrepo", rails()).unwrap();
    let dir = reg.state_root().join(&g.id);
    let orch = fs::read_to_string(dir.join("orchestrator.md")).unwrap();
    assert!(orch.contains("C:/tmp/myrepo"));
    assert!(orch.contains("at most 2 live delegates"), "guardrails must be rendered into the doc");
    // The rename_agent tool and its delegation guidance are rendered (#95r).
    assert!(orch.contains("rename_agent(agent_id, name)"),
        "orchestrator instructions must document rename_agent");
    assert!(orch.contains("Name the pane for its work"),
        "orchestrator instructions must guide renaming a worker to its task");
    assert!(!orch.contains("{{"), "no unrendered placeholders");
    let worker = fs::read_to_string(dir.join("worker.md")).unwrap();
    assert!(worker.contains("Never merge"), "merge gatekeeping must be in worker instructions");
    // The planner instructions are rendered alongside the other roles (#47).
    let planner = fs::read_to_string(dir.join("planner.md")).unwrap();
    assert!(planner.contains("planner"), "planner instructions must be written to the group dir");
    assert!(
        planner.contains("never write code") || planner.contains("You never write code"),
        "planner instructions must forbid writing code"
    );
    assert!(!planner.contains("{{"), "no unrendered placeholders in planner.md");
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
    let cmd = reg.build_agent_command("claude", "sonnet", false, cfg, gdir, Path::new("C:/repo"), Some(&sid), false, false);
    assert!(cmd.contains(&format!("--session-id {sid}")));
    // Resume uses --resume instead.
    let cmd = reg.build_agent_command("claude", "sonnet", false, cfg, gdir, Path::new("C:/repo"), Some(&sid), true, false);
    assert!(cmd.contains(&format!("--resume {sid}")) && !cmd.contains("--session-id"));
}

#[test]
fn resume_spawn_requires_valid_session_and_existing_cwd() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let bad_session = reg.spawn_agent_ex(
        &g.id, Role::Worker, "w", "follow-up", false, None,
        Some("; rm -rf /".into()), None, None,
    );
    assert!(bad_session.is_err(), "shell-metachar session ids must be rejected");
    let bad_cwd = reg.spawn_agent_ex(
        &g.id, Role::Worker, "w", "follow-up", false, None,
        Some("abc-123".into()), Some("C:/definitely/not/a/dir".into()), None,
    );
    assert!(bad_cwd.unwrap_err().contains("cwd"), "resume cwd must exist");
    // Valid resume records the reused session on the agent.
    let dir = tempfile::tempdir().unwrap();
    let ok = reg
        .spawn_agent_ex(
            &g.id, Role::Worker, "w", "follow-up", false, None,
            Some("abc-123".into()), Some(dir.path().to_string_lossy().into_owned()), None,
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

#[test]
fn start_records_note_and_leaves_status_queued() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("Ship the parser"), None, None)).unwrap();
    // Starting is the human's nudge: a human-attributed note is recorded, but
    // the status deliberately stays queued — the orchestrator flips it to
    // in-progress when it actually assigns a worker.
    let after = reg.start_task(&g.id, &t.id).unwrap();
    assert_eq!(after.status, "queued", "start must not flip the status itself");
    let note = after.notes.last().unwrap();
    assert_eq!(note.author, "human");
    assert!(note.text.contains("Started"), "the nudge must be auditable on the board");
}

#[test]
fn start_is_guarded_to_queued_items() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("Ship it"), None, None)).unwrap();
    // An unknown id is an error, not a silent no-op.
    assert!(reg.start_task(&g.id, "t-999").is_err());
    // Every non-queued status must refuse, and refuse without mutating.
    for status in ["in-progress", "review", "pr", "human-testing", "done", "blocked"] {
        reg.upsert_task(&g.id, "orch-1", Some(&t.id), patch(None, Some(status), None)).unwrap();
        let before = reg.tasks(&g.id)[0].notes.len();
        assert!(reg.start_task(&g.id, &t.id).is_err(), "cannot start a {status} item");
        assert_eq!(reg.tasks(&g.id)[0].notes.len(), before, "a refused start must not leave a note");
    }
    // Back to queued, it's allowed again.
    reg.upsert_task(&g.id, "orch-1", Some(&t.id), patch(None, Some("queued"), None)).unwrap();
    assert!(reg.start_task(&g.id, &t.id).is_ok(), "queued is startable");
}

#[test]
fn start_is_rejected_up_front_when_the_group_is_paused() {
    // A paused group suppresses delivery silently (deliver_prompt returns Ok
    // after auditing prompt-suppressed-paused), and unlike approve — which
    // leaves a durable `done` flip — a Start nudge leaves only a note and the
    // "begin work" signal is lost on resume. So Start rejects up front, like the
    // steering strip (#43): a clear error, NO note appended, and — the point —
    // it never reaches delivery, so no suppression audit is recorded.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // An orchestrator is present: if the guard were missing, start would reach
    // delivery and record a prompt-suppressed-paused audit — so asserting its
    // absence proves the guard fires *before* delivery, not that there was
    // simply no target.
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let t = reg.upsert_task(&g.id, "orch-1", None, patch(Some("Ship the parser"), None, None)).unwrap();

    reg.pause_group(&g.id).unwrap();
    let err = reg.start_task(&g.id, &t.id).unwrap_err();
    assert!(err.contains("paused"), "paused rejection must say so: {err}");

    assert!(reg.tasks(&g.id)[0].notes.is_empty(), "a rejected start must not leave a note");
    assert!(
        reg.audit_log(&g.id).iter().all(|e| e.action != "prompt-suppressed-paused"),
        "start must reject before delivery — no suppressed-prompt audit"
    );

    // Resuming lets it through again.
    reg.resume_group(&g.id).unwrap();
    let after = reg.start_task(&g.id, &t.id).unwrap();
    assert_eq!(after.status, "queued");
    assert!(after.notes.last().unwrap().text.contains("Started"), "resumed start records the nudge");
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
    assert!(orch.contains(&"rename_agent".to_string()), "orchestrator must see rename_agent");
    assert!(!work.contains(&"rename_agent".to_string()), "workers must not see rename_agent");
    assert!(!work.contains(&"spawn_agent".to_string()), "workers must not see spawn");
    assert!(!work.contains(&"set_state".to_string()), "workers must not see state writes");
    assert!(work.contains(&"report".to_string()));
}

#[test]
fn workers_cannot_use_privileged_tools_even_if_they_try() {
    let (reg, _d, _co, cw) = setup_mcp();
    for tool in ["spawn_agent", "send_prompt", "kill_agent", "rename_agent", "set_state", "get_output"] {
        let r = dispatch(&reg, &cw, "tools/call",
            &json!({ "name": tool, "arguments": { "task": "x", "agent_id": "w-1", "text": "x", "name": "x", "state": "{}" } }))
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
fn spawn_agent_mcp_accepts_planner_kind() {
    let (reg, _d, co, _cw) = setup_mcp();
    // The orchestrator can spawn a planner via the shared spawn_agent tool (#47).
    let r = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "spawn_agent", "arguments": { "kind": "planner", "task": "plan issue #47" } }))
        .unwrap();
    assert_eq!(r["isError"], false, "planner spawn must succeed");
    assert!(
        r["content"][0]["text"].as_str().unwrap().contains("Planner"),
        "spawn result must report the planner role"
    );
    // The planner is on the roster with the planner role.
    let planner = reg
        .list_agents(&co.group)
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["role"] == "planner");
    assert!(planner, "spawned planner must appear on the roster");
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

// ---------- pane naming & rename precedence (#95r) ----------

#[test]
fn default_pane_name_derives_from_minted_id() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // The orchestrator takes seq 1 (orch-1); the worker mints the next seq.
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    // No meaningful name → the title is derived from the minted id, so it
    // carries the SAME seq the "W <seq>" badge (#75) and the roster id show,
    // never the old per-launch "worker N" counter that drifted from the seq
    // (the "worker 1" pane wearing a "W 2" badge in the #95 screenshot).
    let w = reg.spawn_agent(&g.id, Role::Worker, "", "", false, None).unwrap();
    let seq = w.id.rsplit('-').next().unwrap();
    assert_eq!(w.id, format!("w-{seq}"));
    assert_eq!(
        w.name,
        format!("worker {seq}"),
        "default name must carry the id/badge seq, got id={} name={}",
        w.id, w.name
    );
}

#[test]
fn explicit_spawn_name_is_kept_verbatim() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // A meaningful name the orchestrator chose is not derived away.
    let w = reg.spawn_agent(&g.id, Role::Worker, "gitwatch fix", "t", false, None).unwrap();
    assert_eq!(w.name, "gitwatch fix");
}

#[test]
fn rename_agent_updates_roster_and_audits() {
    let (reg, _d, co, cw) = setup_mcp();
    let ok = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "rename_agent", "arguments": { "agent_id": cw.agent_id, "name": "w-2: gitwatch fix" } }))
        .unwrap();
    assert_eq!(ok["isError"], false, "orchestrator rename must succeed: {ok}");
    assert!(ok["content"][0]["text"].as_str().unwrap().contains("gitwatch fix"));
    // The durable roster reflects the new name (title ↔ roster agree, #95r).
    let roster = reg.list_agents(&co.group);
    let row = roster.as_array().unwrap().iter().find(|a| a["id"] == cw.agent_id.as_str()).unwrap();
    assert_eq!(row["name"], "w-2: gitwatch fix", "roster name must follow the rename");
    // And it is audited.
    let log = fs::read_to_string(reg.state_root().join(&co.group).join("audit.jsonl")).unwrap();
    assert!(log.contains("agent-rename"), "rename must be audited");
}

#[test]
fn rename_agent_is_orchestrator_only() {
    let (reg, _d, _co, cw) = setup_mcp();
    let denied = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "rename_agent", "arguments": { "agent_id": cw.agent_id, "name": "x" } }))
        .unwrap();
    assert_eq!(denied["isError"], true, "workers must not rename");
    assert!(denied["content"][0]["text"].as_str().unwrap().contains("orchestrator-only"));
}

#[test]
fn rename_agent_cannot_target_another_group() {
    let (reg, _d, co, _cw) = setup_mcp();
    let g2 = reg.create_group("C:/tmp/other-repo", rails()).unwrap();
    let foreign = reg.spawn_agent(&g2.id, Role::Worker, "fw", "t", false, None).unwrap();
    let r = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "rename_agent", "arguments": { "agent_id": foreign.id, "name": "x" } }))
        .unwrap();
    assert_eq!(r["isError"], true);
    // Cross-group is indistinguishable from a nonexistent agent (no id leak).
    assert!(r["content"][0]["text"].as_str().unwrap().contains("unknown agent"));
    assert_eq!(reg.agent(&foreign.id).unwrap().name, "fw", "foreign agent keeps its name");
}

#[test]
fn rename_agent_rejects_dead_target() {
    let (reg, _d, co, cw) = setup_mcp();
    reg.mark_dead(&cw.agent_id, None);
    let r = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "rename_agent", "arguments": { "agent_id": cw.agent_id, "name": "x" } }))
        .unwrap();
    assert_eq!(r["isError"], true, "a dead agent cannot be renamed");
    assert!(r["content"][0]["text"].as_str().unwrap().contains("not alive"));
}

#[test]
fn rename_agent_rejects_empty_name() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "", "t", false, None).unwrap();
    assert!(reg.rename_agent(&w.id, "   ", NameSource::Orchestrator).is_err());
}

#[test]
fn rename_precedence_human_beats_orchestrator_beats_default() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "", "t", false, None).unwrap();
    // Starts at the id-derived default.
    let seq = w.id.rsplit('-').next().unwrap().to_string();
    assert_eq!(w.name, format!("worker {seq}"));

    // Orchestrator outranks the default → applies.
    reg.rename_agent(&w.id, "w: parser", NameSource::Orchestrator).unwrap();
    assert_eq!(reg.agent(&w.id).unwrap().name, "w: parser");

    // Human outranks the orchestrator → applies and locks.
    reg.rename_agent(&w.id, "my parser work", NameSource::Human).unwrap();
    assert_eq!(reg.agent(&w.id).unwrap().name, "my parser work");

    // A later orchestrator rename must NOT override the human's title.
    let err = reg.rename_agent(&w.id, "w: something else", NameSource::Orchestrator).unwrap_err();
    assert!(err.contains("human"), "rejection must explain the precedence: {err}");
    assert_eq!(
        reg.agent(&w.id).unwrap().name,
        "my parser work",
        "human rename must survive a later orchestrator rename"
    );

    // The human can still re-rename their own pane (human ≥ human).
    reg.rename_agent(&w.id, "parser v2", NameSource::Human).unwrap();
    assert_eq!(reg.agent(&w.id).unwrap().name, "parser v2");
}

#[test]
fn rename_strips_control_characters() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "", "t", false, None).unwrap();
    // A pasted name can't smuggle newlines/escape codes into the title/roster.
    let applied = reg.rename_agent(&w.id, "w-2:\tgit\nfix\u{1b}[31m", NameSource::Orchestrator).unwrap();
    assert_eq!(applied, "w-2:gitfix[31m");
    assert!(!applied.chars().any(|c| c.is_control()));
    // An all-control name is rejected, not silently applied as empty.
    assert!(reg.rename_agent(&w.id, "\u{1b}\n\t", NameSource::Orchestrator).is_err());
}

#[test]
fn roster_persists_the_name_source_tier() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "", "t", false, None).unwrap();
    // A human rename must persist its tier, not just the text, so a later
    // rejoin can restore the "human wins" guarantee (#95r).
    reg.rename_agent(&w.id, "my parser work", NameSource::Human).unwrap();
    let roster: Value =
        serde_json::from_str(&fs::read_to_string(reg.state_root().join(&g.id).join("agents.json")).unwrap())
            .unwrap();
    let row = roster.as_array().unwrap().iter().find(|r| r["id"] == w.id.as_str()).unwrap();
    assert_eq!(row["name"], "my parser work");
    assert_eq!(row["name_source"], "human", "the tier must be durable, got: {row}");
}

#[test]
fn rejoined_session_restores_the_human_name_tier() {
    use loomux_lib::orchestration::resume_recorded_session;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let reg = Arc::new(OrchRegistry::new(dir.path().to_path_buf()));
    reg.set_port(45999);
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let orch = reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "", "t", false, None).unwrap();
    let sid = w.session_id.clone().unwrap();
    // Human renames the pane, then it dies (pre-"restart").
    reg.rename_agent(&w.id, "my parser work", NameSource::Human).unwrap();
    reg.mark_dead(&w.id, Some(0));

    // Rejoin (background spawn) must come back at the human tier, not demoted
    // to orchestrator — otherwise the "human wins" guarantee dies on restart.
    assert!(resume_recorded_session(&reg, &sid, None).unwrap().is_none());
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let rejoined_id = loop {
        let hit = reg
            .list_agents(&g.id)
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["session"] == sid.as_str() && a["status"] == "running")
            .map(|a| a["id"].as_str().unwrap().to_string());
        if let Some(id) = hit {
            break id;
        }
        assert!(std::time::Instant::now() < deadline, "rejoin did not complete");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_ne!(rejoined_id, w.id, "rejoin mints a fresh id");
    assert_eq!(reg.agent(&rejoined_id).unwrap().name, "my parser work", "name restored");

    // The orchestrator cannot clobber the restored human name.
    let co = reg.resolve_token(&orch.token).unwrap();
    let r = dispatch(&reg, &co, "tools/call",
        &json!({ "name": "rename_agent", "arguments": { "agent_id": rejoined_id, "name": "w: something else" } }))
        .unwrap();
    assert_eq!(r["isError"], true, "orchestrator must not override a restored human rename");
    assert!(r["content"][0]["text"].as_str().unwrap().contains("human"));
    assert_eq!(reg.agent(&rejoined_id).unwrap().name, "my parser work");
}

// ---------- cost containment (#7): pause, idle-kill, spawn-rate, usage ----------

/// Guardrails with the two cost knobs set; other fields mirror `rails()`
/// but with a roomier agent cap so the spawn-rate guardrail can be exercised
/// without tripping the live-agent cap first.
fn costed_rails(idle_kill_minutes: u32, max_spawns_per_hour: u32) -> Guardrails {
    Guardrails {
        max_agents: 6,
        idle_kill_minutes,
        max_spawns_per_hour,
        ..rails()
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

// ---------- attention routing: surface which pane needs the human (#6) ----------

/// Group with an orchestrator and one working (tasked) worker; the watchdog is
/// off (irrelevant here). Returns (reg, tempdir, group, worker id).
fn attention_setup() -> (OrchRegistry, tempfile::TempDir, String, String) {
    let (reg, dir) = test_registry();
    let g = reg.create_group("C:/tmp/repo", watchdog_rails(0)).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "do work", false, None).unwrap();
    (reg, dir, g.id, w.id)
}

fn no_tails() -> HashMap<String, String> {
    HashMap::new()
}

#[test]
fn prompt_wait_detected_spots_prompts_not_chatter() {
    // Claude Code permission menu.
    assert!(prompt_wait_detected(
        "Do you want to make this edit to lib.rs?\n❯ 1. Yes\n  2. No, tell Claude what to do"
    ));
    // Copilot-style allow prompt with a selection pointer.
    assert!(prompt_wait_detected("? Allow command? ›\n❯ Yes\n  No"));
    // A bare yes/no confirmation.
    assert!(prompt_wait_detected("Overwrite the file? (y/n)"));
    // Folder-trust dialog.
    assert!(prompt_wait_detected("Do you trust the files in this folder?"));
    // Normal streaming output is not a prompt.
    assert!(!prompt_wait_detected(
        "Running cargo test...\n   Compiling loomux v0.2.0\ntest result: ok. 42 passed"
    ));
    // A question merely mentioned mid-explanation is not enough on its own.
    assert!(!prompt_wait_detected(
        "I weighed whether to proceed with the refactor and decided it was fine."
    ));
    assert!(!prompt_wait_detected(""));
}

// Captured/synthetic terminal output (with real ANSI + box drawing) for the
// interactive-question repro from issue #40. Fed through `strip_ansi` exactly as
// the live attention scan does, so the fixtures exercise the whole detection
// pipeline, not a pre-cleaned string.
const FIX_CLAUDE_ASK: &str = include_str!("fixtures/attention/claude-askuserquestion.txt");
const FIX_COPILOT_ASK: &str = include_str!("fixtures/attention/copilot-question.txt");
const FIX_STREAMING: &str = include_str!("fixtures/attention/streaming-output.txt");
const FIX_IDLE_BOX: &str = include_str!("fixtures/attention/idle-input-box.txt");
// Finished-turn agent output that *mentions* interactive-UI cues but is not a
// live prompt (#40 review false-positive repro): keyboard-nav prose, a pasted
// shell prompt whose glyph is `❯`, and a `›` UI breadcrumb — each followed by
// the CLI's redrawn idle input box.
const FIX_FP_PROSE: &str = include_str!("fixtures/attention/fp-prose-arrow-keys.txt");
const FIX_FP_SHELL: &str = include_str!("fixtures/attention/fp-shell-prompt-glyph.txt");
const FIX_FP_BREADCRUMB: &str = include_str!("fixtures/attention/fp-breadcrumb-separator.txt");
// A leading ❯/›/→ in finished-turn prose above the idle box: repro steps that
// lead with a shell `❯` glyph, and a fenced ❯ command block. The pointer leads
// the line but is not in the last painted lines, so it must NOT flag (#40 review
// residual). Contrast with pos-pointer-last-line, where the pointer *is* the
// last thing painted — a genuine prompt-wait positive.
const FIX_FP_LEADING_PTR: &str = include_str!("fixtures/attention/fp-leading-pointer-prose.txt");
const FIX_FP_FENCED_PTR: &str = include_str!("fixtures/attention/fp-fenced-pointer-block.txt");
const FIX_POS_PTR_LAST: &str = include_str!("fixtures/attention/pos-pointer-last-line.txt");

#[test]
fn prompt_wait_detected_fires_on_interactive_question_fixtures() {
    // #40: a Claude Code AskUserQuestion menu highlights the active option with
    // reverse-video SGR (stripped away), leaving numbered options with arbitrary
    // labels and an "Enter to select" footer — no selection glyph survives.
    assert!(
        prompt_wait_detected(&strip_ansi(FIX_CLAUDE_ASK.as_bytes())),
        "AskUserQuestion menu must be recognized as needing the human"
    );
    // #40: a Copilot CLI question draws its `❯` pointer indented inside a box, so
    // the option line never *starts* with the pointer after trimming.
    assert!(
        prompt_wait_detected(&strip_ansi(FIX_COPILOT_ASK.as_bytes())),
        "Copilot boxed selection prompt must be recognized as needing the human"
    );
    // #40 review: a plain inquirer confirm whose `❯` pointer IS the last thing
    // painted (no footer / no y-n token) must still flag — proving the anchored
    // pointer signal fires when the menu really is on screen.
    assert!(
        prompt_wait_detected(&strip_ansi(FIX_POS_PTR_LAST.as_bytes())),
        "a pointer as the last painted line is a genuine prompt-wait"
    );
}

#[test]
fn prompt_wait_detected_ignores_quiet_non_prompts() {
    // Ordinary streaming output — even when it ends on a numbered summary list —
    // is not a selection menu, or we'd get a false-positive storm.
    assert!(
        !prompt_wait_detected(&strip_ansi(FIX_STREAMING.as_bytes())),
        "a quiet numbered summary must not be mistaken for a selection menu"
    );
    // A CLI idling at its empty input box (turn finished, no question asked) must
    // not be flagged — otherwise every parked pane lights up.
    assert!(
        !prompt_wait_detected(&strip_ansi(FIX_IDLE_BOX.as_bytes())),
        "an idle input box is not an interactive question"
    );
}

#[test]
fn prompt_wait_detected_ignores_finished_turn_prose_about_ui() {
    // #40 review: finished-turn agent output that *describes* keyboard UIs, pastes
    // a `❯` shell prompt, or echoes a `›` breadcrumb must NOT flag once the CLI's
    // idle input box has redrawn below it. These flagged before the fix — the new
    // signals are anchored (pointer must lead a line; footer read from the last
    // few lines only), so the phrases/glyphs now fall out of range.
    for (name, fixture) in [
        ("keyboard-nav prose", FIX_FP_PROSE),
        ("pasted ❯ shell prompt", FIX_FP_SHELL),
        ("› UI breadcrumb", FIX_FP_BREADCRUMB),
        ("leading ❯ repro steps", FIX_FP_LEADING_PTR),
        ("fenced ❯ command block", FIX_FP_FENCED_PTR),
    ] {
        assert!(
            !prompt_wait_detected(&strip_ansi(fixture.as_bytes())),
            "{name}: finished-turn prose must not be mistaken for a live prompt"
        );
    }
}

#[test]
fn attention_flags_a_pane_parked_on_a_question_fixture() {
    // End-to-end through attention_tick with a real captured Copilot question:
    // once the pane's output is quiet past the window and unattended, it must
    // surface as `waiting` — and carry the pty_id the pane header indicator and
    // the #26/#31 dock-tab dot both key off.
    let (reg, _d, _g, wid) = attention_setup();
    let now = 1_000_000_000_000u64;
    let out: HashMap<String, u64> = [(wid.clone(), 512u64)].into_iter().collect();
    let tail: HashMap<String, String> =
        [(wid.clone(), strip_ansi(FIX_COPILOT_ASK.as_bytes()))].into_iter().collect();
    let no_input = HashMap::new();

    // Debounced on first sighting (the menu may still be painting).
    let first = reg.attention_tick(now, &out, &tail, &no_input);
    assert!(first.iter().all(|i| i.reason != "waiting"), "first quiet tick is debounced");

    // Stable past the quiet window → the pane needs the human.
    let waited = reg.attention_tick(now + 5000, &out, &tail, &no_input);
    let item = waited
        .iter()
        .find(|i| i.agent_id == wid && i.reason == "waiting")
        .expect("a quiet pane parked on a question must be flagged");
    assert_eq!(
        item.pty_id,
        reg.agent(&wid).unwrap().pty_id,
        "the attention item must carry the pane's pty_id for the header + dock-tab dot"
    );
}

#[test]
fn attention_does_not_flag_streaming_or_idle_fixtures() {
    // The two negatives, driven all the way through attention_tick: neither a
    // quiet stream of ordinary output nor an idle input box may raise `waiting`,
    // even long after they've gone quiet.
    let (reg, _d, _g, wid) = attention_setup();
    let now = 1_000_000_000_000u64;
    let out: HashMap<String, u64> = [(wid.clone(), 256u64)].into_iter().collect();
    let no_input = HashMap::new();
    for fixture in [
        FIX_STREAMING,
        FIX_IDLE_BOX,
        FIX_FP_PROSE,
        FIX_FP_SHELL,
        FIX_FP_BREADCRUMB,
        FIX_FP_LEADING_PTR,
        FIX_FP_FENCED_PTR,
    ] {
        let tail: HashMap<String, String> =
            [(wid.clone(), strip_ansi(fixture.as_bytes()))].into_iter().collect();
        reg.attention_tick(now, &out, &tail, &no_input);
        let later = reg.attention_tick(now + 60_000, &out, &tail, &no_input);
        assert!(
            later.iter().all(|i| i.reason != "waiting"),
            "ordinary/idle/prose output must not raise attention, got: {later:?}"
        );
    }
}

#[test]
fn plain_pane_parked_on_a_question_flags_waiting_by_pty() {
    // #40 (human repro): a plain pane the human opened by hand — no orchestration
    // agent/group — running a CLI that's now parked on a question must flag
    // `waiting`, keyed only by its pty id (empty agent_id, no role).
    let (reg, _d, _g, _w) = attention_setup();
    let now = 1_000_000_000_000u64;
    let pty = 77u32;
    let out: HashMap<u32, u64> = [(pty, 512u64)].into_iter().collect();
    let tail: HashMap<u32, String> =
        [(pty, strip_ansi(FIX_CLAUDE_ASK.as_bytes()))].into_iter().collect();
    let no_input = HashMap::new();
    let no_agents = HashSet::new();

    // Debounced on first sighting, then flags once quiet past the window.
    let first = reg.plain_pane_attention(now, &out, &tail, &no_input, &no_agents);
    assert!(first.iter().all(|i| i.reason != "waiting"), "first quiet tick is debounced");
    let waited = reg.plain_pane_attention(now + 5000, &out, &tail, &no_input, &no_agents);
    let item = waited
        .iter()
        .find(|i| i.pty_id == Some(pty) && i.reason == "waiting")
        .expect("a plain pane parked on a question must be flagged");
    assert!(item.agent_id.is_empty(), "a plain pane has no agent identity");
    assert!(item.role.is_none(), "a plain pane has no orchestration role");
}

#[test]
fn plain_pane_scan_skips_agent_ptys_and_quiet_non_prompts() {
    // A pty that belongs to a registered agent is handled by attention_tick, so
    // the plain scan must skip it (no double-flag). And ordinary/idle/prose
    // output on a plain pane must not flag.
    let (reg, _d, _g, _w) = attention_setup();
    let now = 1_000_000_000_000u64;
    let agent_pty = 5u32;
    let out: HashMap<u32, u64> = [(agent_pty, 100u64)].into_iter().collect();
    let tail: HashMap<u32, String> =
        [(agent_pty, strip_ansi(FIX_CLAUDE_ASK.as_bytes()))].into_iter().collect();
    let agents: HashSet<u32> = [agent_pty].into_iter().collect();
    reg.plain_pane_attention(now, &out, &tail, &HashMap::new(), &agents);
    assert!(
        reg.plain_pane_attention(now + 60_000, &out, &tail, &HashMap::new(), &agents)
            .is_empty(),
        "an agent's pty must not be double-flagged by the plain scan"
    );

    let no_agents = HashSet::new();
    for fixture in [FIX_STREAMING, FIX_IDLE_BOX, FIX_FP_PROSE] {
        let pty = 9u32;
        let out: HashMap<u32, u64> = [(pty, 100u64)].into_iter().collect();
        let tail: HashMap<u32, String> =
            [(pty, strip_ansi(fixture.as_bytes()))].into_iter().collect();
        reg.plain_pane_attention(now, &out, &tail, &HashMap::new(), &no_agents);
        assert!(
            reg.plain_pane_attention(now + 60_000, &out, &tail, &HashMap::new(), &no_agents)
                .iter()
                .all(|i| i.reason != "waiting"),
            "ordinary/idle/prose on a plain pane must not flag"
        );
    }
}

#[test]
fn plain_pane_waiting_ack_by_pty_sticks_until_output_changes() {
    // Turning to a plain pane (ack by pty) must make the ack stick until the pane
    // repaints, mirroring the agent path.
    let (reg, _d, _g, _w) = attention_setup();
    let now = 1_000_000_000_000u64;
    let pty = 12u32;
    let out: HashMap<u32, u64> = [(pty, 100u64)].into_iter().collect();
    let tail: HashMap<u32, String> =
        [(pty, strip_ansi(FIX_CLAUDE_ASK.as_bytes()))].into_iter().collect();
    let no_input = HashMap::new();
    let no_agents = HashSet::new();

    reg.plain_pane_attention(now, &out, &tail, &no_input, &no_agents);
    assert_eq!(
        reg.plain_pane_attention(now + 5000, &out, &tail, &no_input, &no_agents)
            .iter()
            .filter(|i| i.pty_id == Some(pty) && i.reason == "waiting")
            .count(),
        1,
        "a quiet plain pane on a menu flags waiting"
    );

    reg.ack_attention_pty(pty);
    assert!(
        reg.plain_pane_attention(now + 8000, &out, &tail, &no_input, &no_agents)
            .iter()
            .all(|i| i.reason != "waiting"),
        "ack by pty must stick while the same menu is on screen"
    );

    // Repaint (menu answered / new prompt) re-arms.
    let grew: HashMap<u32, u64> = [(pty, 200u64)].into_iter().collect();
    reg.plain_pane_attention(now + 9000, &grew, &tail, &no_input, &no_agents);
    assert_eq!(
        reg.plain_pane_attention(now + 14_000, &grew, &tail, &no_input, &no_agents)
            .iter()
            .filter(|i| i.pty_id == Some(pty) && i.reason == "waiting")
            .count(),
        1,
        "a fresh prompt after the plain pane repainted flags again"
    );
}

#[test]
fn attention_scan_surfaces_plain_panes_without_double_covering_agents() {
    // #40 review: exercise run_attention's merge wiring (the layer where the
    // scope bug lived) — attention_scan combines the roster scan with the
    // plain-pane scan. A plain pty parked on a menu must surface (keyed only by
    // pty), and an agent-owned pty must NOT be double-covered by the plain pass.
    let (reg, _d, _g, _w) = attention_setup();
    let now = 1_000_000_000_000u64;
    let no_agent_in: HashMap<String, u64> = HashMap::new();
    let no_agent_tail: HashMap<String, String> = HashMap::new();
    // pty 7 = a plain hand-opened pane; pty 5 stands in for an agent's pty.
    let p_out: HashMap<u32, u64> = [(7u32, 10u64), (5u32, 10u64)].into_iter().collect();
    let p_tails: HashMap<u32, String> = [
        (7u32, strip_ansi(FIX_CLAUDE_ASK.as_bytes())),
        (5u32, strip_ansi(FIX_CLAUDE_ASK.as_bytes())),
    ]
    .into_iter()
    .collect();
    let p_ins = HashMap::new();
    let agent_ptys: HashSet<u32> = [5u32].into_iter().collect();

    let scan = |t: u64| {
        reg.attention_scan(
            t, &no_agent_in, &no_agent_tail, &HashMap::new(), &p_out, &p_tails, &p_ins, &agent_ptys,
        )
    };
    scan(now); // establish the quiet clock
    let items = scan(now + 5000);
    assert!(
        items.iter().any(|i| i.pty_id == Some(7) && i.reason == "waiting" && i.agent_id.is_empty()),
        "a plain pane parked on a question must surface through run_attention's merge"
    );
    assert!(
        items.iter().all(|i| i.pty_id != Some(5)),
        "an agent's pty must not be double-covered by the plain pass"
    );
}

#[test]
fn pane_attention_inputs_from_strips_ansi_and_skips_agent_ptys() {
    // #40 review: drive the gather wiring with a fake live-ids source. Raw bytes
    // carry ANSI + box drawing; the built tail must be stripped, agent ptys must
    // be skipped (not gathered), and the stripped tail must still detect a prompt.
    let (reg, _d, _g, _w) = attention_setup();
    let raw_menu = FIX_COPILOT_ASK.as_bytes().to_vec();
    let live = vec![
        (7u32, 42u64, raw_menu.clone(), 0u64),   // a plain pane
        (5u32, 99u64, raw_menu.clone(), 123u64), // an agent's pty — must be skipped
    ];
    let agent_ptys: HashSet<u32> = [5u32].into_iter().collect();
    let (outs, tails, ins) = reg.pane_attention_inputs_from(&live, &agent_ptys);

    assert!(!outs.contains_key(&5) && !tails.contains_key(&5), "agent pty must be skipped");
    assert_eq!(outs.get(&7), Some(&42u64));
    assert_eq!(ins.get(&7), Some(&0u64));
    let tail7 = tails.get(&7).expect("plain pty tail present");
    assert!(!tail7.contains('\u{1b}'), "ANSI escapes must be stripped from the gathered tail");
    assert!(prompt_wait_detected(tail7), "the stripped tail still detects the menu");
}

#[test]
fn attention_scan_flags_a_plain_pane_with_no_orchestration_group_at_all() {
    // #40 review: the human's repro had NO orchestration group — plain panes only.
    // A fresh registry (no group, no agents) must still flag a plain pane parked
    // on a question, confirming the scan doesn't depend on any group existing.
    let (reg, _d) = test_registry();
    let now = 1_000_000_000_000u64;
    let empty_in: HashMap<String, u64> = HashMap::new();
    let empty_tail: HashMap<String, String> = HashMap::new();
    let p_out: HashMap<u32, u64> = [(3u32, 10u64)].into_iter().collect();
    let p_tails: HashMap<u32, String> =
        [(3u32, strip_ansi(FIX_CLAUDE_ASK.as_bytes()))].into_iter().collect();
    let no_agents = HashSet::new();

    let scan = |t: u64| {
        reg.attention_scan(
            t, &empty_in, &empty_tail, &HashMap::new(), &p_out, &p_tails, &HashMap::new(), &no_agents,
        )
    };
    scan(now);
    assert!(
        scan(now + 5000).iter().any(|i| i.pty_id == Some(3) && i.reason == "waiting"),
        "attention must fire for a plain pane even with no orchestration group present"
    );
}

#[test]
fn attention_waiting_ack_sticks_until_the_prompt_changes() {
    // #40 review: focusing/acking a pane parked on a live menu must make the ack
    // *stick* — the next scan must not re-light `waiting` on the pane the human is
    // already on. The suppression lifts only when the pane's output changes.
    let (reg, _d, _g, wid) = attention_setup();
    let now = 1_000_000_000_000u64;
    let out: HashMap<String, u64> = [(wid.clone(), 100u64)].into_iter().collect();
    let tail: HashMap<String, String> =
        [(wid.clone(), strip_ansi(FIX_COPILOT_ASK.as_bytes()))].into_iter().collect();
    let no_input = HashMap::new();

    // Establish the quiet clock, then flag `waiting`.
    reg.attention_tick(now, &out, &tail, &no_input);
    assert_eq!(
        reg.attention_tick(now + 5000, &out, &tail, &no_input)
            .iter()
            .filter(|i| i.agent_id == wid && i.reason == "waiting")
            .count(),
        1,
        "a quiet pane parked on a menu must flag waiting"
    );

    // The human focuses the pane (auto-ack). Even with the menu still on screen
    // (output unchanged), the next scan must not re-raise waiting.
    reg.ack_attention(&wid);
    assert!(
        reg.attention_tick(now + 8000, &out, &tail, &no_input)
            .iter()
            .all(|i| !(i.agent_id == wid && i.reason == "waiting")),
        "ack must stick while the same menu is on screen"
    );
    assert!(
        reg.attention_tick(now + 11_000, &out, &tail, &no_input)
            .iter()
            .all(|i| !(i.agent_id == wid && i.reason == "waiting")),
        "ack stays sticky across further quiet scans"
    );

    // The pane repaints (menu answered → a *new* prompt appears): re-arm and flag.
    let grew: HashMap<String, u64> = [(wid.clone(), 200u64)].into_iter().collect();
    reg.attention_tick(now + 12_000, &grew, &tail, &no_input); // observe the change, reset quiet clock
    assert_eq!(
        reg.attention_tick(now + 17_000, &grew, &tail, &no_input)
            .iter()
            .filter(|i| i.agent_id == wid && i.reason == "waiting")
            .count(),
        1,
        "a fresh prompt after the pane repainted flags again"
    );
}

#[test]
fn attention_flags_idle_with_prompt_only_when_quiet_and_unattended() {
    let (reg, _d, _g, wid) = attention_setup();
    let now = 1_000_000_000_000u64;
    let out: HashMap<String, u64> = [(wid.clone(), 100u64)].into_iter().collect();
    let prompt: HashMap<String, String> =
        [(wid.clone(), "Do you want to proceed?\n❯ 1. Yes\n  2. No".to_string())]
            .into_iter()
            .collect();
    let no_input = HashMap::new();

    // First sighting starts the quiet clock — not yet flagged even though the
    // tail is prompt-shaped (could be a prompt that just appeared, still painting).
    let first = reg.attention_tick(now, &out, &prompt, &no_input);
    assert!(first.iter().all(|i| i.reason != "waiting"), "must debounce the first quiet tick");

    // Output stable past the quiet window → idle-with-prompt.
    let waited = reg.attention_tick(now + 5000, &out, &prompt, &no_input);
    assert_eq!(
        waited.iter().filter(|i| i.agent_id == wid && i.reason == "waiting").count(),
        1,
        "a quiet pane parked on a prompt needs the human"
    );

    // The human typing into the pane means they are already on it — suppressed.
    let typed: HashMap<String, u64> = [(wid.clone(), now + 5000)].into_iter().collect();
    let while_typing = reg.attention_tick(now + 5000, &out, &prompt, &typed);
    assert!(
        while_typing.iter().all(|i| i.reason != "waiting"),
        "a recent human keystroke suppresses the idle-with-prompt badge"
    );

    // Fresh output (the CLI is painting again) resets the quiet clock.
    let grew: HashMap<String, u64> = [(wid.clone(), 200u64)].into_iter().collect();
    let painting = reg.attention_tick(now + 6000, &grew, &prompt, &no_input);
    assert!(
        painting.iter().all(|i| i.reason != "waiting"),
        "new output is activity, not an idle prompt"
    );
}

#[test]
fn attention_does_not_flag_a_quiet_pane_without_a_prompt() {
    let (reg, _d, _g, wid) = attention_setup();
    let now = 1_000_000_000_000u64;
    let out: HashMap<String, u64> = [(wid.clone(), 100u64)].into_iter().collect();
    let busy_tail: HashMap<String, String> =
        [(wid.clone(), "   Compiling loomux\ntest result: ok".to_string())].into_iter().collect();
    let no_input = HashMap::new();
    reg.attention_tick(now, &out, &busy_tail, &no_input);
    let later = reg.attention_tick(now + 60_000, &out, &busy_tail, &no_input);
    assert!(
        later.is_empty(),
        "a quiet pane whose tail is not a prompt must not be flagged, got: {later:?}"
    );
}

#[test]
fn attention_latches_worker_reports_until_ack_or_progress() {
    let (reg, _d, _g, wid) = attention_setup();
    let w = reg.agent(&wid).unwrap();
    let cw = reg.resolve_token(&w.token).unwrap();
    let now = 1_000_000_000_000u64;
    let empty = HashMap::new();

    // A blocked report badges the pane "blocked".
    let _ = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "report", "arguments": { "status": "blocked", "summary": "stuck" } }));
    let flagged = reg.attention_tick(now, &empty, &no_tails(), &empty);
    assert_eq!(
        flagged.iter().filter(|i| i.agent_id == wid && i.reason == "blocked").count(),
        1,
        "a blocked report must badge the reporting pane"
    );

    // The human acks (focuses the pane): the latch clears.
    reg.ack_attention(&wid);
    assert!(
        reg.attention_tick(now, &empty, &no_tails(), &empty).iter().all(|i| i.agent_id != wid),
        "ack must clear the report badge"
    );

    // A done report badges "report"; a later progress report (worker resumed)
    // clears it.
    let _ = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "report", "arguments": { "status": "done", "summary": "pr up" } }));
    assert_eq!(
        reg.attention_tick(now, &empty, &no_tails(), &empty)
            .iter()
            .filter(|i| i.agent_id == wid && i.reason == "report")
            .count(),
        1,
        "a done report awaits the human's review"
    );
    let _ = dispatch(&reg, &cw, "tools/call",
        &json!({ "name": "report", "arguments": { "status": "progress", "summary": "back at it" } }));
    assert!(
        reg.attention_tick(now, &empty, &no_tails(), &empty).iter().all(|i| i.agent_id != wid),
        "a progress report means the worker is active again — no badge"
    );
}

#[test]
fn attention_flags_a_worker_whose_task_is_at_a_human_gate() {
    let (reg, _d, gid, wid) = attention_setup();
    let now = 1_000_000_000_000u64;
    let empty = HashMap::new();

    // A board task assigned to the worker, sitting at the PR merge gate.
    reg.upsert_task(&gid, "orch", None, TaskPatch {
        title: Some("ship it".into()),
        status: Some("pr".into()),
        assignee: Some(wid.clone()),
        ..Default::default()
    })
    .unwrap();
    let flagged = reg.attention_tick(now, &empty, &no_tails(), &empty);
    assert_eq!(
        flagged.iter().filter(|i| i.agent_id == wid && i.reason == "gate").count(),
        1,
        "an assigned task at a human gate must flag its worker's pane"
    );

    // Approving it (status → done) drops the gate.
    let tid = reg.tasks(&gid)[0].id.clone();
    reg.upsert_task(&gid, "orch", Some(&tid),
        TaskPatch { status: Some("done".into()), ..Default::default() }).unwrap();
    assert!(
        reg.attention_tick(now, &empty, &no_tails(), &empty).iter().all(|i| i.agent_id != wid),
        "an off-gate task no longer flags its worker"
    );
}

#[test]
fn attention_toasts_once_per_onset_only_for_optin_groups() {
    let (reg, _d, gid, wid) = attention_setup();
    let blocked = vec![AttentionItem {
        agent_id: wid.clone(),
        group: gid.clone(),
        name: "w".into(),
        role: Some(Role::Worker),
        pty_id: None,
        reason: "blocked",
        detail: "stuck".into(),
    }];

    // Notifications off by default → nothing toasts.
    assert!(reg.attention_toast_targets(&blocked).is_empty(), "no toasts until opted in");

    // Opt in → the blocked event toasts once, then dedups for the same stall.
    reg.set_notify(&gid, true).unwrap();
    assert_eq!(reg.attention_toast_targets(&blocked), vec![wid.clone()]);
    assert!(
        reg.attention_toast_targets(&blocked).is_empty(),
        "the same reason must not re-toast every scan"
    );

    // A persistent gate state never toasts — the board highlight covers it.
    let gate = vec![AttentionItem { reason: "gate", ..blocked[0].clone() }];
    assert!(reg.attention_toast_targets(&gate).is_empty(), "gate is not a toastable event");
}

#[test]
fn notify_optin_is_durable_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let gid;
    {
        let reg = OrchRegistry::new(dir.path().to_path_buf());
        reg.set_port(45999);
        let g = reg.create_group("C:/tmp/repo", watchdog_rails(0)).unwrap();
        gid = g.id.clone();
        assert!(!reg.notify_enabled(&gid), "off by default");
        reg.set_notify(&gid, true).unwrap();
        assert!(reg.notify_enabled(&gid));
    }
    // A fresh registry over the same root re-seeds the opt-in from the marker.
    let reg2 = OrchRegistry::new(dir.path().to_path_buf());
    reg2.set_port(45999);
    let g2 = reg2.create_group("C:/tmp/repo", watchdog_rails(0)).unwrap();
    assert_eq!(g2.id, gid);
    assert!(reg2.notify_enabled(&gid), "notification opt-in must survive a restart");

    // Turning it off removes the marker.
    reg2.set_notify(&gid, false).unwrap();
    assert!(!reg2.notify_enabled(&gid));
}

#[test]
fn group_usage_summarizes_agents_with_null_cost_without_panes() {
    let (reg, _d, _co, _cw) = setup_mcp();
    let usage = reg.group_usage(&_co.group);
    // No live panes and no transcripts in test mode → no dollar figures.
    assert!(usage["live_cost_usd"].is_null());
    assert!(usage["lifetime_cost_usd"].is_null());
    assert_eq!(usage["live_tokens"].as_u64(), Some(0));
    let agents = usage["agents"].as_array().unwrap();
    assert!(agents.iter().any(|a| a["id"] == "orch-1"), "orchestrator must appear");
    assert!(agents.iter().all(|a| a["cost_usd"].is_null()));
    // Exposed to the orchestrator over MCP too.
    let via_mcp = dispatch(&reg, &_co, "tools/call",
        &json!({ "name": "group_usage", "arguments": {} })).unwrap();
    assert_eq!(via_mcp["isError"], false);
    assert!(via_mcp["content"][0]["text"].as_str().unwrap().contains("lifetime_cost_usd"));
    // Workers cannot pull the group-wide usage summary.
    let denied = dispatch(&reg, &_cw, "tools/call",
        &json!({ "name": "group_usage", "arguments": {} })).unwrap();
    assert_eq!(denied["isError"], true, "usage aggregation is orchestrator-only");
}

/// Build a durable usage snapshot for a session, as a fresh transcript read
/// would produce.
fn usage_snap(key: &str, agent_id: &str, cost: f64, input: u64, output: u64) -> UsageSnapshot {
    UsageSnapshot {
        key: key.to_string(),
        agent_id: agent_id.to_string(),
        name: agent_id.to_string(),
        role: "worker".to_string(),
        source: "transcript".to_string(),
        input_tokens: input,
        output_tokens: output,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        cost_usd: Some(cost),
        estimated: true,
        model: Some("claude-opus-4-8".to_string()),
        updated_ms: 0,
    }
}

#[test]
fn killed_agent_stays_in_lifetime_total_but_not_live() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let sid = w.session_id.clone().expect("claude worker gets a session id");

    // Simulate a transcript read having captured this session's spend.
    reg.upsert_usage_snapshot(&g.id, usage_snap(&sid, &w.id, 1.50, 1000, 2000));

    let before = reg.group_usage(&g.id);
    let row = before["agents"].as_array().unwrap().iter()
        .find(|a| a["id"] == w.id.as_str()).expect("worker row present");
    assert_eq!(row["live"], true);
    assert_eq!(row["tokens"]["total"].as_u64(), Some(3000));
    assert!((before["live_cost_usd"].as_f64().unwrap() - 1.50).abs() < 1e-9);
    assert!((before["lifetime_cost_usd"].as_f64().unwrap() - 1.50).abs() < 1e-9);

    // Kill the worker. mark_dead re-reads usage (no transcript in test mode →
    // empty), but the merge must keep the captured spend rather than zero it.
    reg.mark_dead(&w.id, Some(0));

    let after = reg.group_usage(&g.id);
    let row = after["agents"].as_array().unwrap().iter()
        .find(|a| a["id"] == w.id.as_str()).expect("dead worker still listed");
    assert_eq!(row["live"], false, "killed agent is no longer live");
    // Lifetime keeps the spend; live no longer counts the dead worker.
    assert!((after["lifetime_cost_usd"].as_f64().unwrap() - 1.50).abs() < 1e-9,
        "lifetime total must survive the kill");
    assert!(after["live_cost_usd"].is_null(), "no live agents contribute cost now");
    assert_eq!(after["lifetime_tokens"].as_u64(), Some(3000));
    assert_eq!(after["live_tokens"].as_u64(), Some(0));
}

#[test]
fn mark_dead_captures_usage_from_transcript() {
    // Point the usage reader at a fixture transcript tree instead of ~/.claude,
    // via a per-registry override (no global env — safe under parallel runs).
    let proj = tempfile::tempdir().unwrap();
    let (reg, _d) = test_registry();
    reg.set_claude_projects_dir(proj.path().to_path_buf());
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "task", false, None).unwrap();
    let sid = w.session_id.clone().unwrap();

    // Write a synthetic Claude transcript for this session under an
    // encoded-cwd folder (any name — the reader scans all of them).
    let encoded = proj.path().join("C--tmp-repo");
    fs::create_dir_all(&encoded).unwrap();
    let transcript = format!(
        "{}\n{}\n",
        json!({"type":"user","message":{"content":"hi"}}),
        json!({"type":"assistant","message":{"id":"m1","model":"claude-opus-4-8",
            "usage":{"input_tokens":1000,"output_tokens":500,
                     "cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}),
    );
    fs::write(encoded.join(format!("{sid}.jsonl")), transcript).unwrap();

    // Kill without ever calling group_usage first: mark_dead must snapshot it.
    reg.mark_dead(&w.id, Some(0));

    let usage = reg.group_usage(&g.id);
    let row = usage["agents"].as_array().unwrap().iter()
        .find(|a| a["id"] == w.id.as_str()).expect("dead worker captured");
    assert_eq!(row["live"], false);
    assert_eq!(row["source"], "transcript");
    assert_eq!(row["estimated"], true);
    assert_eq!(row["tokens"]["total"].as_u64(), Some(1500));
    // Opus: (1000*5 + 500*25) / 1e6 = 0.0175
    let expect = (1000.0 * 5.0 + 500.0 * 25.0) / 1_000_000.0;
    assert!((usage["lifetime_cost_usd"].as_f64().unwrap() - expect).abs() < 1e-9);
    // Token-derived → labelled estimated, not reported.
    assert_eq!(usage["lifetime_cost_basis"], "estimated");
}

#[test]
fn usage_json_write_is_atomic_and_leaves_no_temp() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.upsert_usage_snapshot(&g.id, usage_snap("sess-a", "w-1", 0.50, 100, 200));

    let gdir = reg.state_root().join(&g.id);
    let path = gdir.join("usage.json");
    assert!(path.is_file(), "usage.json must exist after upsert");
    assert!(!gdir.join("usage.json.tmp").is_file(), "temp file must be cleaned up");
    // Round-trips as valid JSON with the snapshot.
    let list: Vec<UsageSnapshot> =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].key, "sess-a");
}

#[test]
fn corrupt_usage_json_is_preserved_not_silently_wiped() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let gdir = reg.state_root().join(&g.id);
    fs::create_dir_all(&gdir).unwrap();
    let path = gdir.join("usage.json");
    // Simulate a half-written / hand-mangled file.
    fs::write(&path, "{ this is not valid json ").unwrap();

    // The next upsert must not treat corruption as "empty and overwrite" — it
    // preserves the bad file so no killed-agent history is silently lost.
    reg.upsert_usage_snapshot(&g.id, usage_snap("sess-b", "w-2", 1.25, 300, 400));

    let bad = gdir.join("usage.json.bad");
    assert!(bad.is_file(), "corrupt file must be preserved as usage.json.bad");
    assert_eq!(fs::read_to_string(&bad).unwrap(), "{ this is not valid json ");
    // usage.json is now valid and holds the new snapshot.
    let list: Vec<UsageSnapshot> =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].key, "sess-b");
}

#[test]
fn mixed_estimated_and_reported_totals_are_labelled_mixed() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // One transcript-estimated snapshot and one statusline-reported one.
    reg.upsert_usage_snapshot(&g.id, usage_snap("sess-est", "w-1", 1.00, 100, 100));
    let mut reported = usage_snap("sess-rep", "w-2", 2.00, 0, 0);
    reported.source = "statusline".to_string();
    reported.estimated = false;
    reg.upsert_usage_snapshot(&g.id, reported);

    let usage = reg.group_usage(&g.id);
    // Neither agent is live (no panes), so this exercises the lifetime total.
    assert!((usage["lifetime_cost_usd"].as_f64().unwrap() - 3.00).abs() < 1e-9);
    assert_eq!(usage["lifetime_cost_basis"], "mixed",
        "estimated + reported dollars must not hide under one label");
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

// ---------- #43: compose-strip steering + human-typing hold backstop ----------

#[test]
fn steer_rejects_empty_text() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    // Whitespace-only is also empty — the strip must not enqueue a blank line.
    let err = reg.steer_orchestrator(&g.id, "   ").unwrap_err();
    assert!(err.contains("empty"), "got: {err}");
}

#[test]
fn steer_rejects_paused_group_so_the_human_gets_feedback() {
    // A paused group suppresses delivery silently; steering must surface that
    // as an error the strip shows, not vanish (the whole point of the strip is
    // that the human's message is never silently lost).
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    reg.pause_group(&g.id).unwrap();
    let err = reg.steer_orchestrator(&g.id, "do the thing").unwrap_err();
    assert!(err.contains("paused"), "got: {err}");
}

#[test]
fn steer_without_a_live_orchestrator_errors() {
    // A group with only workers (no orchestrator) must NOT fall through to a
    // worker — steering targets the orchestrator or nothing.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    let err = reg.steer_orchestrator(&g.id, "steer me").unwrap_err();
    assert!(err.contains("no live orchestrator"), "got: {err}");
    // An unknown group is likewise not steerable.
    let err = reg.steer_orchestrator("no-such-group", "steer me").unwrap_err();
    assert!(err.contains("no live orchestrator"), "got: {err}");
}

#[test]
fn steer_of_a_healthy_group_reaches_delivery() {
    // Empty/paused/no-orch guards all pass → steering delegates to the
    // serialized delivery path, which in test mode (no real PTY) fails at the
    // terminal step. That error proves the guards let a healthy steer through.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let err = reg.steer_orchestrator(&g.id, "steer me").unwrap_err();
    assert!(err.contains("terminal"), "steer must reach the pty step, got: {err}");
}

#[test]
fn steering_targets_the_orchestrator_and_is_audited_under_its_group() {
    // Resolution + isolation + audit attribution in one: a paused group makes
    // delivery record a suppression audit (reachable without a real PTY) whose
    // `to` names the resolved target. It must be the ORCHESTRATOR (not the
    // worker in the same group), attributed to `human`, and written only under
    // this group's log.
    let (reg, _d) = test_registry();
    let a = reg.create_group("C:/tmp/repo-a", rails()).unwrap();
    let orch = reg.spawn_agent(&a.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    reg.spawn_agent(&a.id, Role::Worker, "w", "t", false, None).unwrap();
    let b = reg.create_group("C:/tmp/repo-b", rails()).unwrap();
    reg.spawn_agent(&b.id, Role::Orchestrator, "orch-b", "", false, None).unwrap();

    reg.pause_group(&a.id).unwrap();
    // Go through deliver_to_orchestrator directly (steer_orchestrator's own
    // paused-guard would short-circuit before delivery) to observe resolution.
    reg.deliver_to_orchestrator(&a.id, "hello orchestrator", "human").unwrap();

    let entries = reg.audit_log(&a.id);
    let sup = entries
        .iter()
        .find(|e| e.action == "prompt-suppressed-paused")
        .expect("suppressed steer must be audited");
    assert_eq!(sup.actor, "human", "steer must be attributed to the human");
    assert_eq!(sup.detail["to"], orch.id, "steer must resolve to the orchestrator, not the worker");
    assert_eq!(sup.detail["text"], "hello orchestrator");
    // Group isolation: nothing landed in group B's log.
    assert!(
        reg.audit_log(&b.id).iter().all(|e| e.action != "prompt-suppressed-paused"),
        "a steer to group A must not touch group B"
    );
}

// ---------- #72: steering-strip image attachments ----------

#[test]
fn sanitize_attachment_ext_allows_only_vetted_image_types() {
    // Case- and dot-insensitive; jpeg folds to jpg; everything else is refused.
    assert_eq!(sanitize_attachment_ext("png"), Some("png"));
    assert_eq!(sanitize_attachment_ext(".PNG"), Some("png"));
    assert_eq!(sanitize_attachment_ext("JPEG"), Some("jpg"));
    assert_eq!(sanitize_attachment_ext("jpg"), Some("jpg"));
    assert_eq!(sanitize_attachment_ext("webp"), Some("webp"));
    assert_eq!(sanitize_attachment_ext("gif"), Some("gif"));
    assert_eq!(sanitize_attachment_ext("bmp"), Some("bmp"));
    // Path-traversal / executable / script extensions are rejected outright.
    assert_eq!(sanitize_attachment_ext("exe"), None);
    assert_eq!(sanitize_attachment_ext("svg"), None);
    assert_eq!(sanitize_attachment_ext("../etc/passwd"), None);
    assert_eq!(sanitize_attachment_ext(""), None);
}

#[test]
fn save_attachment_writes_bytes_verbatim_under_the_group_dir() {
    let (reg, dir) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // A tiny "PNG" — bytes are written as-is, never decoded, so any payload works.
    let bytes = [0x89u8, b'P', b'N', b'G', 1, 2, 3, 0, 255];
    let path = reg.save_attachment(&g.id, "png", &bytes).unwrap();
    let p = Path::new(&path);
    assert!(p.is_file(), "the attachment file must exist");
    assert_eq!(fs::read(p).unwrap(), bytes, "bytes must be stored verbatim");
    // It lives under <root>/<group>/attachments/ and carries the .png extension.
    assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
    let attach_dir = dir.path().join(&g.id).join("attachments");
    assert!(p.starts_with(&attach_dir), "path {p:?} must be under {attach_dir:?}");
    // The save is audited so there's a human-attributed trail of what was sent.
    assert!(
        reg.audit_log(&g.id).iter().any(|e| e.action == "attachment-save" && e.actor == "human"),
        "the save must be audited under the human actor",
    );
}

#[test]
fn save_attachment_rejects_bad_type_empty_and_oversize() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    // Unsupported extension.
    assert!(reg.save_attachment(&g.id, "exe", &[1, 2, 3]).unwrap_err().contains("unsupported"));
    // Empty payload.
    assert!(reg.save_attachment(&g.id, "png", &[]).unwrap_err().contains("empty"));
    // One byte past the cap is refused (and nothing is written).
    let huge = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
    assert!(reg.save_attachment(&g.id, "png", &huge).unwrap_err().contains("too large"));
    // Exactly at the cap is accepted.
    let at_cap = vec![0u8; MAX_ATTACHMENT_BYTES];
    assert!(reg.save_attachment(&g.id, "png", &at_cap).is_ok());
}

#[test]
fn save_attachment_gives_each_image_a_distinct_path_in_a_burst() {
    // A multi-image paste saves several files back-to-back (possibly within one
    // millisecond); the per-process sequence must keep their names unique so one
    // image never clobbers another.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let mut paths = std::collections::HashSet::new();
    for _ in 0..20 {
        let p = reg.save_attachment(&g.id, "png", &[7u8]).unwrap();
        assert!(paths.insert(p.clone()), "duplicate attachment path: {p:?}");
    }
    assert_eq!(paths.len(), 20);
}

#[test]
fn save_attachment_rejects_an_unknown_group() {
    // Membership guard (#72 review): the dir is root.join(group), so a group id
    // that was never created — including a traversal attempt — must be refused
    // before anything is written.
    let (reg, dir) = test_registry();
    assert!(reg.save_attachment("never-made", "png", &[1, 2, 3]).unwrap_err().contains("unknown"));
    assert!(reg.save_attachment("../escape", "png", &[1, 2, 3]).unwrap_err().contains("unknown"));
    // Nothing was written anywhere under the root.
    assert!(!dir.path().join("never-made").exists());
    assert!(!dir.path().join("attachments").exists());
}

#[test]
fn orchestrator_cli_resolves_the_groups_cli_for_reference_formatting() {
    // The save command returns this so the frontend formats image references the
    // way the orchestrator's CLI reads them (#72 review note 3).
    let (reg, _d) = test_registry();
    let claude = reg.create_group("C:/tmp/claude-repo", rails()).unwrap();
    let copilot = reg.create_group("C:/tmp/copilot-repo", copilot_rails()).unwrap();
    assert_eq!(reg.orchestrator_cli(&claude.id), "claude");
    assert_eq!(reg.orchestrator_cli(&copilot.id), "copilot");
    // Unknown group → the safe default wording, never a panic.
    assert_eq!(reg.orchestrator_cli("nope"), "claude");
}

#[test]
fn end_group_sweeps_the_attachments_scratch_dir() {
    let (reg, dir) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    // A durable file (state) and an attachment: teardown reclaims the scratch
    // dir but leaves the rest of the group state alone.
    reg.set_state(&g.id, "{\"k\":1}").unwrap();
    let att = reg.save_attachment(&g.id, "png", &[1, 2, 3]).unwrap();
    let attach_dir = dir.path().join(&g.id).join("attachments");
    assert!(Path::new(&att).is_file() && attach_dir.is_dir());

    reg.end_group(&g.id, false).unwrap();
    assert!(!attach_dir.exists(), "attachments dir must be swept on group end");
    assert!(
        dir.path().join(&g.id).join("state.json").is_file(),
        "non-attachment group state must survive teardown",
    );
}

#[test]
fn hold_guard_proceeds_immediately_when_quiet() {
    // No keystrokes recorded (0) → the loop never holds and never reports.
    let quiet = Duration::from_millis(50);
    let cap = Duration::from_secs(5);
    let poll = Duration::from_millis(5);
    assert_eq!(hold_until_quiet(|| 0, quiet, cap, poll), None);
}

#[test]
fn hold_guard_caps_so_reports_are_not_starved() {
    // u64::MAX = "typed in the future" → always inside the quiet window, so the
    // ONLY way the loop can exit is the max-hold cap. That it returns at all
    // proves the starvation backstop fires; the value proves it held ~the cap.
    let cap = Duration::from_millis(40);
    let held = hold_until_quiet(|| u64::MAX, Duration::from_millis(50), cap, Duration::from_millis(5))
        .expect("a capped hold must report its held duration");
    assert!(held >= 30, "must have held near the cap before delivering, got {held}ms");
    assert!(held < 2000, "cap must bound the hold, got {held}ms");
}

#[test]
fn hold_guard_releases_once_the_human_goes_quiet() {
    use std::sync::atomic::{AtomicU64, Ordering};
    // "Typing" (future stamp) for the first few polls, then an ancient stamp
    // (quiet). The loop must hold while typing, then release well before the
    // cap — exercising the poll loop that consults should_hold_for_user, not
    // just the pure decision (the #40 wiring lesson).
    let calls = AtomicU64::new(0);
    let source = move || {
        if calls.fetch_add(1, Ordering::Relaxed) < 3 { u64::MAX } else { 1 }
    };
    let held = hold_until_quiet(source, Duration::from_millis(50), Duration::from_secs(5), Duration::from_millis(5))
        .expect("it must report having held while the human was typing");
    assert!(held < 4000, "must release on quiet, not ride the cap, got {held}ms");
}
