//! The manager's LIFECYCLE (#1161 M3): who opens its pane, which guardrails
//! skip it, and every route by which something other than loomux could have
//! opened one.
//!
//! Its own integration-test binary rather than more of `orchestration.rs` for
//! two reasons. The mechanical one: three PRs touching that file were ahead of
//! this slice in the merge queue, and an end-of-file append conflicts on its
//! shared trailing tokens rather than on its content (CLAUDE.md's git section).
//! The real one: these tests are one subject — the lifecycle of a single
//! capability class — and the class is new enough that a reader should be able
//! to find all of it in one place.
//!
//! An integration test (not a unit test) because a test executable linking the
//! full lib needs the common-controls-v6 manifest `build.rs` embeds via
//! `rustc-link-arg-tests` — CLAUDE.md constraint 4.
//!
//! No test here spawns a real agent CLI (constraint 3). Command lines are built
//! and asserted; nothing is executed.

use loomux_lib::orchestration::mcp::dispatch;
use loomux_lib::orchestration::{
    create_orchestration_group, AgentRecord, Caller, Delivery, GroupId,
    Guardrails, NameSource, OrchRegistry, Role, SessionOrigin,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Build a registry against `dir` with every test-only directory override
/// applied. Duplicated from `orchestration.rs`/`workflow.rs` because these are
/// separate integration-test binaries — and it is a real requirement, not
/// ceremony: a registry built without these overrides writes a generated agent
/// file into the REAL `~/.claude`/`~/.copilot` agents dir on its first spawn
/// (#464). `no_registry_construction_bypasses_the_test_agent_dir_overrides` in
/// `orchestration.rs` enforces that this file has exactly one raw
/// `OrchRegistry::new`, here.
fn relaunch_registry(dir: &Path) -> OrchRegistry {
    let reg = OrchRegistry::new(dir.to_path_buf());
    reg.set_port(45999);
    reg.set_claude_agents_dir_override(dir.join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.join("copilot-hooks"));
    reg
}

fn test_registry() -> (Arc<OrchRegistry>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Arc::new(relaunch_registry(dir.path())), dir)
}

/// Guardrails for a group that RUNS the repo's workflow file — the manager
/// class is workflow-only, so every test here needs the advanced orchestrator
/// on. `idle_kill_minutes` is deliberately non-zero: a 0 disables the reaper
/// entirely, and a reaper test against a disabled reaper is the vacuity shape.
fn rails() -> Guardrails {
    Guardrails {
        max_agents: 6,
        agent_cli: "claude".into(),
        auto_ops: false,
        advanced_orchestrator: true,
        idle_kill_minutes: 5,
        watchdog_stall_minutes: 5,
        ..Guardrails::default()
    }
}

/// A throwaway repo one level below its own temp root — see `workflow.rs`'s
/// `Repo` for why the nesting matters (a worktree is cut SIBLING to the repo,
/// and a bare tempdir used as the repo root leaks it past `Drop`).
struct Repo {
    _root: tempfile::TempDir,
    repo: PathBuf,
}

impl Repo {
    fn new(yaml: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        if let Some(yaml) = yaml {
            let dir = repo.join(".loomux");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("workflow.yml"), yaml).unwrap();
        }
        Repo { _root: root, repo }
    }
    fn path(&self) -> String {
        self.repo.to_string_lossy().replace('\\', "/")
    }
}

/// A roster declaring a manager, plus the two delegate classes the controls in
/// this file need. Deliberately NOT the built-in four: a manager only ever
/// arrives from a repo's workflow file.
const WITH_MANAGER: &str = "version: 1\nblocks:\n\
     \x20 - id: manager\n    kind: manager\n\
     \x20 - id: worker\n    kind: worker\n\
     \x20 - id: rev\n    kind: reviewer\n";

/// The SAME roster with the manager block removed — every control in this file
/// runs against this, so a failure can never be "the workflow didn't load".
const WITHOUT_MANAGER: &str = "version: 1\nblocks:\n\
     \x20 - id: worker\n    kind: worker\n\
     \x20 - id: rev\n    kind: reviewer\n";

/// Launch a group the way the app does — `create_orchestration_group`, the one
/// entry point `register_orchestrator_pane` (and therefore
/// `open_manager_pane_at_launch`) sits behind.
fn launch(yaml: &str, rails: Guardrails) -> (Arc<OrchRegistry>, tempfile::TempDir, Repo, GroupId) {
    let (reg, dir) = test_registry();
    let repo = Repo::new(Some(yaml));
    let req =
        create_orchestration_group(&reg, &repo.path(), rails, SessionOrigin::Fresh, None, None)
            .expect("the group must launch");
    let gid = req.group_id.clone();
    (reg, dir, repo, gid)
}

/// The group's live roster rows of one wire role.
fn rows_of(reg: &OrchRegistry, g: &GroupId, role: &str) -> Vec<Value> {
    reg.list_agents(g)
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["role"] == json!(role))
        .cloned()
        .collect()
}

fn the_manager(reg: &OrchRegistry, g: &GroupId) -> Value {
    let rows = rows_of(reg, g, "manager");
    assert_eq!(rows.len(), 1, "expected exactly one manager row, got {rows:?}");
    rows[0].clone()
}

/// The group's raw audit log, as text — `audit_log` returns parsed rows, and
/// these assertions are about what was WRITTEN.
fn audit_text(reg: &OrchRegistry, g: &GroupId) -> String {
    fs::read_to_string(reg.state_root().join(g.as_str()).join("audit.jsonl")).unwrap_or_default()
}

/// RED RUN ONLY (round 2). Base has no launch-time open, so in round 1 seven of
/// these tests died at `the_manager`'s "expected exactly one manager row"
/// BEFORE reaching the assertion they exist for — a red that evidences only the
/// missing launch, not each guard. This hand-registers the pane exactly the way
/// loomux's own opener does (`block: None`, resolved by class) so every
/// assertion below the launch one runs and reddens on its own logic.
fn ensure_manager(reg: &OrchRegistry, g: &GroupId) {
    if rows_of(reg, g, "manager").is_empty() {
        reg.spawn_agent_ex(g, Role::Manager, None, "", "", false, None, None, None, None, None)
            .expect("hand-registered manager stand-in for the launch open");
    }
}

fn orch_caller(reg: &OrchRegistry, g: &GroupId) -> Caller {
    let id = rows_of(reg, g, "orchestrator")
        .first()
        .expect("a launched group has an orchestrator")["id"]
        .as_str()
        .unwrap()
        .to_string();
    Caller { agent_id: id, group: g.clone(), role: Role::Orchestrator, role_hint: None }
}

// ---------------------------------------------------------------- launch ----

#[test]
fn a_declared_manager_pane_opens_at_group_launch() {
    // The headline of the slice. Before M3 the launch opened an orchestrator
    // and nothing else, so a declared manager was a block nothing ever
    // instantiated — every M2 behaviour was reachable only through a
    // hand-registered agent.
    let (reg, _d, repo, gid) = launch(WITH_MANAGER, rails());

    let m = the_manager(&reg, &gid);
    assert_eq!(m["block"], json!("manager"), "it must be the DECLARED block, not a class default");
    assert_eq!(m["status"], json!("running"));
    assert_eq!(
        m["cwd"].as_str().unwrap().replace('\\', "/"),
        repo.path(),
        "a manager works in the repo the human is talking about — no worktree"
    );
    assert!(
        m["session"].as_str().is_some(),
        "a claude manager gets a pre-assigned session id, so its conversation is resumable later"
    );

    // The pane is expanded, not docked (#260's own argument, applied in M1) —
    // asserted at the request the frontend actually receives.
    let req = reg
        .spawn_request_for_test(m["id"].as_str().unwrap())
        .expect("the launch must have emitted a spawn request for the manager");
    assert_eq!(req.role, Role::Manager);
    assert!(!req.minimized, "a docked manager is a conversation the human cannot see");

    // The open is on the record, so an operator reading `audit.jsonl` can see
    // WHY a pane they never asked for exists.
    assert!(
        audit_text(&reg, &gid).contains("manager-opened"),
        "the launch-time open must be audited: {}",
        audit_text(&reg, &gid)
    );

    // THE CONTROL, and it is the compatibility promise of the whole feature:
    // the same launch on a roster with no manager block opens no manager, and
    // records nothing about one.
    let (reg2, _d2, _repo2, gid2) = launch(WITHOUT_MANAGER, rails());
    assert!(
        rows_of(&reg2, &gid2, "manager").is_empty(),
        "a roster that declares no manager must open none: {}",
        reg2.list_agents(&gid2)
    );
    let log = audit_text(&reg2, &gid2);
    assert!(
        !log.contains("manager-opened"),
        "...and must not record an open it did not perform: {log}"
    );
}

#[test]
fn a_launch_that_resumes_reopens_the_managers_own_session_with_no_task() {
    // "Including the resume path." A human's conversation with their manager
    // must survive an app restart: this pane's transcript IS the record of what
    // they said, so losing it is worse here than for any other pane.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::new(Some(WITH_MANAGER));
    let (gid, first_session, orch_session);
    {
        let reg = Arc::new(relaunch_registry(dir.path()));
        let req = create_orchestration_group(
            &reg,
            &repo.path(),
            rails(),
            SessionOrigin::Fresh,
            None,
            None,
        )
        .unwrap();
        gid = req.group_id.clone();
        orch_session = reg
            .agent(&req.agent_id)
            .expect("the orchestrator entry")
            .session_id
            .expect("a claude orchestrator has a session");
        first_session = the_manager(&reg, &gid)["session"].as_str().unwrap().to_string();
    }

    // "App restart": a new registry over the same state dir, relaunching the
    // group by resuming its orchestrator conversation.
    let reg = Arc::new(relaunch_registry(dir.path()));
    create_orchestration_group(
        &reg,
        &repo.path(),
        rails(),
        SessionOrigin::Resume(orch_session),
        Some(gid.as_str()),
        None,
    )
    .expect("the group must relaunch");

    let m = the_manager(&reg, &gid);
    assert_eq!(
        m["session"].as_str().unwrap(),
        first_session,
        "the manager must come back on ITS OWN conversation, not a new one"
    );
    let req = reg.spawn_request_for_test(m["id"].as_str().unwrap()).expect("a spawn request");
    assert!(
        req.command.contains(&format!("--resume {first_session}")),
        "the CLI must actually be told to reopen it: {}",
        req.command
    );
    // And it is typed NOTHING. `spawn_agent_bound`'s resume arm delivers a
    // follow-up only when the spawn carries a task; the launch path never gives
    // one, which is what makes "a resumed manager pane is a conversation, not a
    // pane waiting to be told what it is" true rather than merely intended.
    // `Delivery::ResumeKickoff` is in the permitted set and this path declines
    // to use it — see `doc/design/manager.md`.
    assert_eq!(m["task"], json!(""), "a resumed manager carries no task to be delivered");

    // THE CONTROL for the resume half: a launch that does NOT resume
    // cold-starts the manager even with that same record sitting on disk.
    let reg3 = Arc::new(relaunch_registry(dir.path()));
    create_orchestration_group(
        &reg3,
        &repo.path(),
        rails(),
        SessionOrigin::StartFresh,
        Some(gid.as_str()),
        None,
    )
    .expect("the group must relaunch fresh");
    let cold = the_manager(&reg3, &gid);
    assert_ne!(
        cold["session"].as_str().unwrap(),
        first_session,
        "start-fresh means start fresh — the prior conversation must not be reopened"
    );
}

#[test]
fn nothing_may_be_typed_into_the_manager_pane_the_launch_opened() {
    // M2's structural no-injection guarantee, asserted against a manager pane
    // that PRODUCTION opened — until M3 the only manager an assertion could be
    // made about was hand-registered, so the guarantee had never been exercised
    // on the path that now creates one.
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails());
    ensure_manager(&reg, &gid); // RED RUN ONLY
    let m = the_manager(&reg, &gid);
    let mid = m["id"].as_str().unwrap();

    let err = reg
        .deliver_prompt(mid, "status update", "orch-1", Delivery::MidSession)
        .expect_err("a mid-session delivery into a manager pane must be refused");
    assert!(
        err.contains("manager") && err.contains("message_manager"),
        "the refusal must redirect to the mailbox rather than only saying no: {err}"
    );

    // THE POSITIVE CONTROL, and it is what stops this passing on a pane that is
    // simply unreachable. A KICKOFF is permitted into a manager pane, so it gets
    // PAST this gate and fails on the next one instead — no terminal, because
    // this registry has no frontend. Two different refusals, which is the whole
    // claim: the manager gate is keyed on the DELIVERY KIND, not on the pane
    // being dead.
    let kick = reg
        .deliver_prompt(mid, "your kickoff", "loomux", Delivery::FreshKickoff)
        .expect_err("no frontend in a test registry, so it cannot actually land");
    assert!(
        kick.contains("terminal") && !kick.contains("message_manager"),
        "a kickoff must clear the manager gate and fail on the missing pty instead: {kick}"
    );

    // ...and the refusal is on the record, not merely returned to the caller.
    // `manager-pane` is `RefusalReason::ManagerPane`'s wire name.
    let log = audit_text(&reg, &gid);
    assert!(log.contains("manager-pane"), "the refusal must be audited: {log}");

    // The other producer the orchestrator would actually reach for.
    let caller = orch_caller(&reg, &gid);
    let out = dispatch(
        &reg,
        &caller,
        "tools/call",
        &json!({ "name": "send_prompt", "arguments": { "agent_id": mid, "text": "hi" } }),
    )
    .unwrap();
    assert_eq!(out["isError"], json!(true), "send_prompt to a manager must be refused: {out}");
}

// ------------------------------------------------------------ exemptions ----

#[test]
fn the_idle_reaper_takes_a_worker_beside_the_manager_and_never_the_manager() {
    // THE defect this exemption exists for, and it is not hypothetical: a
    // manager opens with no task (the human's first message IS the task), so
    // `idle_since_ms` stamps at BIRTH. Unguarded, the very first sweep past
    // `idle_kill_minutes` takes it — before the human has typed a word — and
    // the notice goes to the orchestrator's pane, not to the human sitting in
    // front of the one that just vanished.
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails());
    ensure_manager(&reg, &gid); // RED RUN ONLY
    let m = the_manager(&reg, &gid);
    let mid = m["id"].as_str().unwrap().to_string();

    // The idle clock really is stamped — without this the exemption below would
    // be asserted against an agent the reaper was never going to consider.
    assert!(
        m["idle_since_ms"].as_u64().is_some(),
        "a manager IS idle by the reaper's own measure — that is the trap: {m}"
    );

    // The non-vacuity control, spawned the same way and equally idle.
    let w = reg.spawn_agent(&gid, Role::Worker, "w", "", false, None).unwrap();
    assert!(reg.agent(&w.id).unwrap().idle_since_ms.is_some(), "the control worker is idle too");

    let far_future = u64::MAX / 2;
    assert_eq!(
        reg.idle_reap_candidates(far_future),
        vec![w.id.clone()],
        "the worker is reclaimable and the manager is not — an idle manager is a manager \
         whose human is away, which is its normal state"
    );
    // The reaper itself, not only its selection.
    assert_eq!(reg.reap_idle_agents(far_future), vec![w.id.clone()]);
    assert_eq!(
        reg.agent(&mid).map(|a| a.status),
        Some(loomux_lib::orchestration::AgentStatus::Running),
        "the manager pane must still be there"
    );
}

#[test]
fn the_stall_watchdog_never_notifies_about_a_manager() {
    // A stall notice about a manager is a false report in both directions: its
    // silence means the human is READING, and the notice lands in a pane the
    // human is not looking at, naming the pane they are.
    //
    // The manager here is spawned WITH a task, which is the one way to construct
    // a manager whose silence clock is running — the launch path never gives one
    // and no agent-reachable path can (`send_prompt` into a manager is refused,
    // so `set_agent_idle(false)` is unreachable for this class). That is
    // deliberate, and it is why the guard is keyed on the ROLE rather than left
    // to the idle-clock clause it currently agrees with: "nothing is assigned"
    // is a fact about which paths exist today, and "loomux never nags the
    // orchestrator about the human's own pane" is the rule.
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails());
    // The launch already opened one, so this second group is built without the
    // launch path — `spawn_agent_ex` with `block: None`, which is how loomux's
    // own openers resolve a manager.
    let (reg2, _d2) = test_registry();
    let repo2 = Repo::new(Some(WITH_MANAGER));
    let g2 = reg2.create_group(&repo2.path(), rails()).unwrap();
    reg2.spawn_agent(&g2.id, Role::Orchestrator, "orch", "", false, None).unwrap();
    let tasked_manager = reg2
        .spawn_agent_ex(&g2.id, Role::Manager, None, "mgr", "a task", false, None, None, None, None, None)
        .expect("loomux's own opener resolves the manager block by class");
    assert!(
        reg2.agent(&tasked_manager.id).unwrap().idle_since_ms.is_none(),
        "sanity: this manager's silence clock IS running, or the guard below is vacuous"
    );
    // The non-vacuity control: a worker in the same group, equally silent.
    let w = reg2.spawn_agent(&g2.id, Role::Worker, "w", "do work", false, None).unwrap();

    // A time far past any real `now_ms()`, so both clocks are unambiguously
    // past the stall window.
    const FAR: u64 = 1_000_000_000_000_000;
    assert_eq!(
        reg2.watchdog_tick(FAR, &HashMap::new(), &HashMap::new()),
        vec![w.id.clone()],
        "the worker stalls and the manager does not"
    );
    let log = audit_text(&reg2, &g2.id);
    assert!(log.contains("watchdog-stall"), "the control's stall must be audited: {log}");
    assert!(
        !log.contains(&tasked_manager.id),
        "...and nothing may be audited against the manager: {log}"
    );

    // The launch-opened manager (idle, the ordinary shape) is not flagged either.
    assert!(reg.watchdog_tick(FAR, &HashMap::new(), &HashMap::new()).is_empty());
    assert!(rows_of(&reg, &gid, "manager").len() == 1, "and it is still there");
}

#[test]
fn a_manager_does_not_spend_a_max_agents_slot() {
    // Decision D3. The cap contains DELEGATE fan-out — the axis an orchestrator
    // controls — and the manager is not something it opens at all. Counting it
    // would make the human's interface competable with a worker slot, and on a
    // cap of 1 (below) would leave a group with a manager unable to spawn any
    // worker whatsoever.
    // RED RUN ONLY: the three `counts_against_max_agents` predicate asserts are
    // removed here because that function does not exist on base — a compile
    // error proves nothing about behaviour. Every assertion below is verbatim.

    let rails = Guardrails { max_agents: 1, ..rails() };
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails);
    ensure_manager(&reg, &gid); // RED RUN ONLY
    let _ = the_manager(&reg, &gid); // it is live

    // With the manager counted, this spawn is refused. It must not be.
    let w = reg
        .spawn_agent(&gid, Role::Worker, "w", "", false, None)
        .expect("a live manager must not consume the group's only delegate slot");

    // THE CONTROL: the cap is real, and it is the WORKER that trips it.
    let err = reg
        .spawn_agent(&gid, Role::Worker, "w2", "", false, None)
        .expect_err("the cap must still bite at 1 live delegate");
    assert!(err.contains("guardrail"), "{err}");
    assert!(
        err.contains(&w.id),
        "the refusal names the pane actually holding the slot, so the orchestrator can reuse it: {err}"
    );
    // ...and does NOT name the manager. A pane the cap does not count is not
    // holding a slot, and pointing a refused orchestrator at it would be
    // pointing it at the one pane it must not reuse or kill.
    let mid = the_manager(&reg, &gid)["id"].as_str().unwrap().to_string();
    assert!(!err.contains(&mid), "the cap refusal must not name the manager: {err}");

    // The panel's own number moves with the guardrail rather than being
    // computed beside it.
    let summary = reg.group_summary(&gid);
    assert_eq!(summary["live_delegates"], json!(1), "one worker, and only the worker: {summary}");
    assert_eq!(summary["roles"]["manager"], json!(1), "the manager is still SHOWN, just not counted");
}

// ------------------------------------------------------- the spawn routes ----

/// Every way an agent could ask for a manager pane, and the answer to each.
///
/// The two `kind`/`block` refusals landed in M1 and are re-asserted here beside
/// the two M3 closes: they are four spellings of one rule, and a suite that
/// pins only the new ones would not notice an old one being lost.
#[test]
fn no_agent_reachable_route_can_open_a_manager_pane() {
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails());
    ensure_manager(&reg, &gid); // RED RUN ONLY
    let caller = orch_caller(&reg, &gid);
    let manager_session = the_manager(&reg, &gid)["session"].as_str().unwrap().to_string();

    let refused = |args: Value| -> String {
        let before = reg.list_agents(&gid).as_array().unwrap().len();
        let out =
            dispatch(&reg, &caller, "tools/call", &json!({ "name": "spawn_agent", "arguments": args }))
                .unwrap();
        assert_eq!(out["isError"], json!(true), "{args} must be refused, got {out}");
        assert_eq!(
            reg.list_agents(&gid).as_array().unwrap().len(),
            before,
            "no second manager pane may exist, not even briefly"
        );
        out["content"][0]["text"].as_str().unwrap().to_string()
    };

    // M1's two, by argument.
    let by_kind = refused(json!({ "kind": "manager", "task": "t" }));
    assert!(by_kind.contains("manager") && by_kind.contains("human"), "{by_kind}");
    let by_block = refused(json!({ "block": "manager", "task": "t" }));
    assert!(by_block.contains("manager") && by_block.contains("human"), "{by_block}");

    // M3's first: a BARE resume of the manager's own session. It names neither
    // `kind` nor `block`, so both checks above wave it through, and the block
    // inheritance that runs after them (#254) then hands it the recorded manager
    // block id.
    let bare = refused(json!({ "resume_session": manager_session, "task": "follow up" }));
    assert!(
        bare.contains("manager") && bare.contains("ask_human"),
        "the refusal must name what it is and where to go instead: {bare}"
    );

    // The control for THAT: the same bare-resume shape on a WORKER session is
    // exactly the documented follow-up contract and still works.
    let w = reg.spawn_agent(&gid, Role::Worker, "w", "first task", false, None).unwrap();
    let wsid = w.session_id.clone().expect("a claude worker has a session");
    reg.mark_dead(&w.id, Some(0));
    let out = dispatch(
        &reg,
        &caller,
        "tools/call",
        &json!({ "name": "spawn_agent", "arguments": {
            "resume_session": wsid, "task": "follow up", "cwd": _repo.path(),
        }}),
    )
    .unwrap();
    assert_eq!(
        out["isError"],
        json!(false),
        "control: a bare resume of a delegate session is the documented follow-up: {out}"
    );
}

#[test]
fn a_bare_resume_of_a_pre_222_manager_row_is_refused() {
    // The SECOND bare-resume route, and it does not go through block
    // inheritance at all. A roster row written before blocks existed records a
    // ROLE STRING and no block id, so a bare resume of one derives the class
    // from that string — `kind_from_str("manager")` — and then takes that
    // class's DEFAULT block. Nothing the caller passed ever said "manager", and
    // the `role` argument reaching `spawn_agent_ex` is `Role::Planner`.
    //
    // Deleting the `block.kind == Role::Manager` guard leaves every other test
    // in this file green and reopens exactly this door.
    let (reg, _d, repo, gid) = launch(WITH_MANAGER, rails());
    let caller = orch_caller(&reg, &gid);

    let session = "7c9f2b10-1161-4161-8161-333333333333";
    let record = AgentRecord {
        id: "mgr-0".into(),
        role: "manager".into(),
        block: String::new(), // pre-#222: a role, no block identity
        name: "a manager from a build that predates blocks".into(),
        name_source: NameSource::default(),
        session: Some(session.to_string()),
        cwd: repo.path(),
        status: "dead".into(),
        updated_ms: 0,
        task: String::new(),
        branch: None,
    };
    // Written alongside the live rows rather than over them: the launch's own
    // manager row must survive, or the singleton check would be what refuses
    // this and the guard under test would go unexercised.
    let path = reg.state_root().join(gid.as_str()).join("agents.json");
    let mut rows: Vec<AgentRecord> =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    rows.push(record);
    fs::write(&path, serde_json::to_string(&rows).unwrap()).unwrap();

    let out = dispatch(
        &reg,
        &caller,
        "tools/call",
        &json!({ "name": "spawn_agent", "arguments": {
            "resume_session": session, "cwd": repo.path(), "task": "follow up",
        }}),
    )
    .unwrap();
    assert_eq!(out["isError"], json!(true), "a recorded `manager` role must not be resumable: {out}");
    let text = out["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("manager"), "{text}");
    assert_eq!(rows_of(&reg, &gid, "manager").len(), 1, "and no second pane was opened");
}

#[test]
fn spawn_agent_ex_refuses_a_named_manager_block_and_admits_the_class_default() {
    // The ENFORCEMENT layer, under the MCP surface — the check that makes the
    // three refusals above sentences rather than the only thing standing in the
    // way. It is the twin of the orchestrator-block guard, and the shape is the
    // load-bearing part: `named.is_some()` is true on every agent-reachable
    // route and false on exactly the two openers loomux owns, which resolve the
    // block BY CLASS.
    let (reg, _d) = test_registry();
    let repo = Repo::new(Some(WITH_MANAGER));
    let g = reg.create_group(&repo.path(), rails()).unwrap();

    let err = reg
        .spawn_agent_ex(&g.id, Role::Worker, Some("manager".into()), "m", "", false, None, None, None, None, None)
        .expect_err("a NAMED manager block must be refused however it is reached");
    assert!(err.contains("manager") && err.contains("session browser"), "{err}");
    // The same refusal when the caller's `role` argument already says manager —
    // it is `block.kind` that decides, so neither spelling can slip past.
    assert!(reg
        .spawn_agent_ex(&g.id, Role::Manager, Some("manager".into()), "m", "", false, None, None, None, None, None)
        .is_err());
    assert!(rows_of(&reg, &g.id, "manager").is_empty(), "nothing was opened");

    // THE POSITIVE CONTROL, and without it the guard could simply be "no
    // manager may ever be spawned", which would break the launch: the class
    // default (`block: None`) is admitted, and it resolves to the same block the
    // named form just named.
    let m = reg
        .spawn_agent_ex(&g.id, Role::Manager, None, "m", "", false, None, None, None, None, None)
        .expect("loomux's own opener must succeed");
    assert_eq!(m.block, "manager", "the class default IS the declared block — MANAGER_MAX is 1");

    // And the orchestrator's own twin still refuses, so this edit did not
    // rewrite the guard it was modelled on.
    assert!(reg
        .spawn_agent_ex(&g.id, Role::Worker, Some("orchestrator".into()), "o", "", false, None, None, None, None, None)
        .is_err());
}

#[test]
fn a_group_may_not_hold_two_live_managers() {
    // `MANAGER_MAX` bounds what a workflow file may DECLARE, not how many panes
    // one declaration opens — and loomux's two openers can genuinely race (a
    // human clicking Resume on a dead manager session while a relaunch brings
    // the group's own one up). Two panes would be two conversations the human
    // has to notice are different, and one mailbox drained by whichever read it
    // first.
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails());
    ensure_manager(&reg, &gid); // RED RUN ONLY
    let m = the_manager(&reg, &gid);
    let mid = m["id"].as_str().unwrap().to_string();

    let err = reg
        .spawn_agent_ex(&gid, Role::Manager, None, "second", "", false, None, None, None, None, None)
        .expect_err("a second live manager must be refused");
    assert!(err.contains("singleton") || err.contains("already"), "{err}");
    assert_eq!(rows_of(&reg, &gid, "manager").len(), 1);

    // THE CONTROL: it is LIVENESS that refuses, not the class. Kill the pane and
    // the replacement opens — which is the respawn-after-death path.
    reg.mark_dead(&mid, Some(0));
    let replacement = reg
        .spawn_agent_ex(&gid, Role::Manager, None, "second", "", false, None, None, None, None, None)
        .expect("a dead manager may be replaced");
    assert_ne!(replacement.id, mid, "a genuinely new pane");
}
