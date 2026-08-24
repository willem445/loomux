//! The manager's LIFECYCLE (#1161 M3): who opens its pane, which guardrails
//! skip it, and every route by which something other than loomux could have
//! opened one.
//!
//! Its own integration-test binary rather than more of `orchestration.rs` for
//! two reasons. The mechanical one: that file is a permanent contention point —
//! an end-of-file append there conflicts on its shared trailing tokens rather
//! than on its content (CLAUDE.md's git section), so concurrent slices splice
//! into each other's final assertion. This file adds exactly one line to it (the
//! `sanctioned` row the #464 guard requires) and keeps everything else here.
//! The real one: these tests are one subject — the lifecycle of a single
//! capability class — and belong together for a reader regardless of what the
//! merge queue happens to hold.
//!
//! An integration test (not a unit test) because a test executable linking the
//! full lib needs the common-controls-v6 manifest `build.rs` embeds via
//! `rustc-link-arg-tests` — CLAUDE.md constraint 4.
//!
//! No test here spawns a real agent CLI (constraint 3). Command lines are built
//! and asserted; nothing is executed.

use loomux_lib::orchestration::mcp::dispatch;
use loomux_lib::orchestration::workflow;
use loomux_lib::orchestration::{
    create_orchestration_group, counts_against_max_agents, AgentRecord, Caller, Delivery, GroupId,
    Guardrails, NameSource, OrchRegistry, Role, SessionOrigin, ORCHESTRATOR_TPL,
};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
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
    // The audit log rides the failure message: `open_manager_pane_at_launch`
    // DEGRADES rather than failing the launch, so "no manager row" is the
    // symptom of a refusal recorded over there, and a bare count would send a
    // reader hunting for it.
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one manager row, got {rows:?}
roster: {}
audit:
{}",
        reg.list_agents(g),
        audit_text(reg, g)
    );
    rows[0].clone()
}

/// The group's raw audit log, as text — `audit_log` returns parsed rows, and
/// these assertions are about what was WRITTEN.
fn audit_text(reg: &OrchRegistry, g: &GroupId) -> String {
    fs::read_to_string(reg.state_root().join(g.as_str()).join("audit.jsonl")).unwrap_or_default()
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
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails());

    let m = the_manager(&reg, &gid);
    assert_eq!(m["block"], json!("manager"), "it must be the DECLARED block, not a class default");
    assert_eq!(m["status"], json!("running"));
    // The group's OWN repo, read back from the registry rather than compared
    // against the string this test passed in: what is pinned is "the manager
    // sits in the repo, not in a cut worktree", and re-deriving the left side
    // from the launch input would pass under any normalisation the group
    // applied to both sides equally.
    assert_eq!(
        m["cwd"].as_str().unwrap(),
        reg.group(&gid).expect("the launched group").repo,
        "a manager works in the repo the human is talking about — no worktree"
    );
    assert!(
        !m["cwd"].as_str().unwrap().contains("-worktrees"),
        "...and specifically not the sibling worktree dir a delegate would get: {m}"
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
    //
    // The guardrails come from `load_group_file`, exactly as the production
    // caller builds them (`resume_recorded_session`'s orchestrator branch), and
    // that is load-bearing rather than tidiness: `create_group_ex` sets
    // `reads_workflow_file = false` for `Launch::Resume` (#459/#255 — a resumed
    // roster is PINNED, never re-read from `.loomux/workflow.yml`), so a resume
    // handed a blockless `Guardrails` runs the BUILT-IN four and genuinely
    // declares no manager. Passing `rails()` here would test a group that is
    // not running the workflow this test is about.
    let reg = Arc::new(relaunch_registry(dir.path()));
    let (_, persisted) = reg.load_group_file(&gid).expect("group.json from the first launch");
    assert!(
        persisted.block_for(Role::Manager).is_some(),
        "sanity: the persisted roster is the one carrying the manager block"
    );
    create_orchestration_group(
        &reg,
        &repo.path(),
        persisted.clone(),
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
    //
    // **What this assertion pins, exactly** (#1161 M3 review N5): the INPUT that
    // makes the resume arm deliver nothing, not the absence of a delivery. A
    // no-frontend registry cannot type into any pane, so no assertion made here
    // could tell "declines `ResumeKickoff`" apart from "had no pane to type
    // into". A delivery-side pin — that no `ResumeKickoff` is PRODUCED for this
    // spawn — would be the fail-able one and needs a seam this slice does not
    // add. Stated rather than implied, so nobody reads the line below as more
    // than it is.
    assert_eq!(m["task"], json!(""), "a resumed manager carries no task to be delivered");

    // THE CONTROL for the resume half: a launch that does NOT resume
    // cold-starts the manager even with that same record sitting on disk.
    let reg3 = Arc::new(relaunch_registry(dir.path()));
    create_orchestration_group(
        &reg3,
        &repo.path(),
        persisted,
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
    // Scoped to the WATCHDOG's own records rather than the whole file: every
    // agent has an `agent-spawn` line naming it, so a file-wide substring search
    // for the manager's id can never be false — which would make this assertion
    // unfailable in the wrong direction (it would fire on a correct build).
    // `watchdog-suppressed` is included because it is the watchdog's OTHER
    // record, and a manager reaching it would be just as wrong.
    //
    // **This loop is not independently fail-able, and that is fine** (#1161 M3
    // review N6). Removing `Role::Manager` from the watchdog guard reddens the
    // `assert_eq!(watchdog_tick(...), vec![w.id])` above FIRST, and the audit
    // records are written from that same selection — so the state this loop
    // catches (tick excluded the manager, audit named it anyway) is unreachable
    // while one selection feeds both. It is a cheap guard against the two
    // diverging later, not the pin for the exemption; the `assert_eq!` above is
    // the pin. Said out loud so a future reader does not mistake which is which.
    let watchdog_lines: Vec<&str> =
        log.lines().filter(|l| l.contains("watchdog-stall") || l.contains("watchdog-suppressed")).collect();
    assert!(!watchdog_lines.is_empty(), "control: the watchdog wrote something");
    for line in &watchdog_lines {
        assert!(
            !line.contains(&tasked_manager.id),
            "no watchdog record may name the manager: {line}"
        );
    }

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
    assert!(!counts_against_max_agents(Role::Manager), "the predicate itself");
    assert!(!counts_against_max_agents(Role::Orchestrator), "...and the class it joins");
    assert!(
        [Role::Worker, Role::Reviewer, Role::Planner].iter().all(|r| counts_against_max_agents(*r)),
        "control: every delegate class still counts"
    );

    let rails = Guardrails { max_agents: 1, ..rails() };
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails);
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
    // ...and it must be the `mcp.rs` sentence SPECIFICALLY (#1161 M3 review N8).
    // Both refusals contain "manager" and "ask_human", so the assertion above
    // cannot tell them apart: delete the `mcp.rs` arm and `spawn_agent_bound`'s
    // refusal comes back instead, still satisfying it. The opener is what
    // distinguishes them, so the opener is what is pinned.
    assert!(
        bare.starts_with("that session belongs to"),
        "the refusal must come from the `mcp.rs` arm (which names the SESSION), \
         not from `spawn_agent_bound`'s block-shaped one: {bare}"
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

// ------------------------------------------------ the copy, and the rate cap ----

/// Every refusal this slice adds is a **sentence** — the `mcp.rs` arm's entire
/// stated justification is the copy, not the enforcement — so the copy is pinned
/// (#1161 M3 review B2).
///
/// The defect this exists for shipped green: all three messages carried a
/// literal `\n` escape plus the source indentation that followed it, so a
/// caller received hard line breaks with 17-25 spaces baked in. Every
/// assertion in this file was blind to it, because each checks a `contains(…)`
/// substring and no such substring straddled an inserted break — which is
/// exactly why the check has to be on the SHAPE of the whole string rather than
/// on any phrase inside it.
/// The predicate, split out from the assertion so the control below can exercise
/// it as a VALUE — wrapping a failing assert in `catch_unwind` would work too,
/// but it prints a panic to stderr from a PASSING test, which is its own small
/// lie to whoever reads the log.
///
/// Two conditions, not one: the break is what a reader sees, and the run of
/// indentation is what makes it unmistakably a source artefact rather than
/// deliberate formatting. Checking both means the half-fix — dropping the `n`
/// but leaving the spaces behind — still fails.
fn is_one_paragraph(msg: &str) -> bool {
    !msg.contains('\n') && !msg.contains("          ")
}

fn assert_one_paragraph(what: &str, msg: &str) {
    assert!(
        is_one_paragraph(msg),
        "{what}: a refusal is one paragraph — the house idiom is a `\\` line \
         continuation, which strips the newline AND the indentation. This one \
         ships a hard break or leaked indentation: {msg:?}"
    );
}

#[test]
fn the_three_manager_refusals_each_render_as_one_paragraph() {
    let (reg, _d, repo, gid) = launch(WITH_MANAGER, rails());
    let caller = orch_caller(&reg, &gid);
    let manager_session = the_manager(&reg, &gid)["session"].as_str().unwrap().to_string();

    // 1. `spawn_agent_bound`'s named-block refusal — the enforcement.
    let (reg2, _d2) = test_registry();
    let repo2 = Repo::new(Some(WITH_MANAGER));
    let g2 = reg2.create_group(&repo2.path(), rails()).unwrap();
    let named = reg2
        .spawn_agent_ex(&g2.id, Role::Worker, Some("manager".into()), "m", "", false, None, None, None, None, None)
        .expect_err("a named manager block is refused");
    assert_one_paragraph("the named-block refusal", &named);

    // 2. The singleton refusal.
    let singleton = reg
        .spawn_agent_ex(&gid, Role::Manager, None, "second", "", false, None, None, None, None, None)
        .expect_err("a second live manager is refused");
    assert_one_paragraph("the singleton refusal", &singleton);

    // 3. The `mcp.rs` bare-resume sentence — the one whose whole reason for
    //    existing is that it says something better than the generic refusal.
    let out = dispatch(
        &reg,
        &caller,
        "tools/call",
        &json!({ "name": "spawn_agent", "arguments": {
            "resume_session": manager_session, "task": "t", "cwd": repo.path(),
        }}),
    )
    .unwrap();
    assert_eq!(out["isError"], json!(true));
    let sentence = out["content"][0]["text"].as_str().unwrap();
    // IDENTIFY the arm before checking its shape (#1161 M3 review N8). `call_tool`'s
    // check runs before `spawn_agent_ex`, so this is the `mcp.rs` sentence today —
    // but if that arm were removed, `spawn_agent_bound`'s refusal would be returned
    // instead, case 1 already pins THAT one, and this case would stay green while
    // the sentence it names went unpinned. The opener is the distinguishing token.
    assert!(
        sentence.starts_with("that session belongs to"),
        "case 3 must be exercising the `mcp.rs` arm, not `spawn_agent_bound`'s: {sentence}"
    );
    assert_one_paragraph("the bare-resume sentence", sentence);
    // THE POSITIVE CONTROL for the check itself: it must reject the shape the
    // defect had, or all three assertions above are decoration. Built the way
    // the bug actually was — a newline plus the source indentation that followed
    // it — and then each half on its own, so neither condition can be dropped
    // without this failing. Plus the negative: it accepts the shape the three
    // real messages now have, so it is not simply refusing everything.
    let mangled = "a refusal that ships a hard break — the human's own\n                 interface, opened at launch.";
    assert!(!is_one_paragraph(mangled), "the check must reject a mangled message");
    assert!(!is_one_paragraph("break only\nhere"), "...a bare hard break");
    assert!(!is_one_paragraph("leaked           indentation"), "...and leaked indentation");
    assert!(
        is_one_paragraph("one paragraph — as every refusal above renders"),
        "...while accepting the shape the three real messages have"
    );
}

#[test]
fn a_manager_opens_even_when_the_spawn_rate_backstop_is_exhausted() {
    // #1161 M3 review N1. `check_and_record_spawn` sits INSIDE
    // `if counts_against_max_agents(role)`, so a manager open skips the
    // spawn-rate backstop and records no timestamp against the hour. That is a
    // documented exemption (the design note's table says so), and CLAUDE.md's
    // escape-hatch rule is that a documented counterfactual is only pinned by a
    // test that performs it.
    //
    // The reason it is the right exemption: the backstop guards against a
    // RUNAWAY ORCHESTRATOR, and the manager is opened once per group by the
    // launch path, which no agent can reach.
    //
    // This test pins ONE direction — the manager opens against an exhausted
    // budget. The other — that opening it does not SPEND a delegate's slot —
    // cannot be shown on an exhausted fixture, where any assertion holds under
    // both implementations; it has its own headroom fixture in
    // `a_manager_open_does_not_consume_an_hour_slot_from_the_delegates`.
    let rails = Guardrails { max_spawns_per_hour: 2, max_agents: 8, ..rails() };
    let (reg, _d) = test_registry();
    let repo = Repo::new(Some(WITH_MANAGER));
    let g = reg.create_group(&repo.path(), rails).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();

    // Burn the hour's budget with ordinary delegates.
    for i in 0..2 {
        reg.spawn_agent(&g.id, Role::Worker, &format!("w{i}"), "", false, None)
            .unwrap_or_else(|e| panic!("delegate {i} must fit inside the budget: {e}"));
    }

    // THE CONTROL, and it runs FIRST so the exemption below cannot be read as
    // "the limit was never reached": the very next delegate is refused, by the
    // rate backstop specifically and not by the cap (max_agents is 8, and only
    // three panes are live).
    let refused = reg
        .spawn_agent(&g.id, Role::Worker, "w3", "", false, None)
        .expect_err("the spawn-rate backstop must bite");
    assert!(
        refused.contains("spawn-rate"),
        "refused by the rate backstop, not by something else: {refused}"
    );

    // ...and the manager opens anyway, through the same call the launch path makes.
    let m = reg
        .spawn_agent_ex(&g.id, Role::Manager, None, "", "", false, None, None, None, None, None)
        .expect("a manager is exempt from the spawn-rate backstop");
    assert_eq!(m.role, Role::Manager);

    assert_eq!(
        rows_of(&reg, &g.id, "manager").len(),
        1,
        "and exactly one manager exists — the exempt open really happened"
    );
}

#[test]
fn a_manager_open_does_not_consume_an_hour_slot_from_the_delegates() {
    // The OTHER half of the spawn-rate exemption (#1161 M3 review N9), and it
    // needs its own fixture because the obvious one cannot fail.
    //
    // The tempting version asserts it on the EXHAUSTED group above: open the
    // manager, then watch another worker still be refused. That passes
    // identically whether or not the manager consumed a slot — the budget was
    // already gone two spawns earlier — so it holds under the very
    // implementation the sentence claims to exclude. Vacuity, stated as proof.
    //
    // The fail-able fixture is HEADROOM, not exhaustion: leave exactly one slot,
    // spend it on the manager, and require a delegate to still get it. That
    // reddens the moment `check_and_record_spawn` starts running for a manager.
    let rails = Guardrails { max_spawns_per_hour: 3, max_agents: 8, ..rails() };
    let (reg, _d) = test_registry();
    let repo = Repo::new(Some(WITH_MANAGER));
    let g = reg.create_group(&repo.path(), rails).unwrap();
    reg.spawn_agent(&g.id, Role::Orchestrator, "orch", "", false, None).unwrap();

    // Two of the three slots go to delegates; one is left.
    for i in 0..2 {
        reg.spawn_agent(&g.id, Role::Worker, &format!("w{i}"), "", false, None)
            .unwrap_or_else(|e| panic!("delegate {i} is inside the budget: {e}"));
    }

    // The manager opens. If it recorded a timestamp, the last slot is now spent.
    reg.spawn_agent_ex(&g.id, Role::Manager, None, "", "", false, None, None, None, None, None)
        .expect("the manager opens");

    // THE ASSERTION THAT MOVES: the third delegate must still get the slot the
    // manager did not take.
    reg.spawn_agent(&g.id, Role::Worker, "w2", "", false, None).unwrap_or_else(|e| {
        panic!("the manager consumed a delegate's hour slot — the exemption is one-directional: {e}")
    });

    // THE CONTROL, so the success above cannot be read as "the limit never
    // applied here": the FOURTH delegate is refused, by the rate backstop
    // specifically. Three delegates admitted against a limit of three.
    let refused = reg
        .spawn_agent(&g.id, Role::Worker, "w3", "", false, None)
        .expect_err("the budget really is three");
    assert!(refused.contains("spawn-rate"), "refused by the rate backstop: {refused}");
    assert_eq!(rows_of(&reg, &g.id, "manager").len(), 1, "and the manager is live throughout");
}

// ─────── the orchestrator's own counting claim (#1429 round-5 N3) ───────
//
// `templates/orchestrator.md` states the guardrail as "at most {{MAX_AGENTS}}
// live delegates (workers+reviewers+planners count together)". That
// parenthetical dates to #76 and was true only by the accident of predating
// every class that does NOT count: `Role::Orchestrator` was the whole exemption
// when it was written, and `Role::Manager` joined it with decision D3 (#1161
// M3). It enumerates the counting classes rather than asserting an exemption,
// so it is still accurate — and nothing read it for this property, which means
// the day a class is added that DOES count it becomes a false claim on a
// GOLDENED template, with a green suite and nothing mechanical pointing at it.
//
// The pin below closes that by DERIVING both sides rather than restating
// either: the enum's variants come from the enum's own source, the population
// comes from `workflow::kind_from_str`, the counting set comes from
// `counts_against_max_agents`, and the classes named come from the template.
// It lives here, beside the exemptions it is a claim about, rather than in
// `manager_prose.rs` (which pins the MANAGER's own surfaces) or `prompts.rs`
// (which pins what a DEFAULT group reads, and a default group has no manager).

/// Every `Role` variant, harvested from the enum's own source text.
///
/// Harvested rather than listed, because a hand-written population is exactly
/// the shape a new class slips past (CLAUDE.md's source-scanning-guard
/// convention): a list would still hold five names on the day a sixth counting
/// class is added, and the guard would pass while the claim it guards went
/// false. Doc comments, attributes and blank lines are skipped; a variant is a
/// bare capitalized identifier followed by a comma, which is every arm of this
/// enum — none carries a payload.
fn role_variants_in_source() -> Vec<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/loomux-engine/src/model.rs");
    let src = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("the Role enum's source must be readable at {path}: {e}"));
    const OPEN: &str = "pub enum Role {";
    let start = src
        .find(OPEN)
        .unwrap_or_else(|| panic!("`{OPEN}` must exist in {path} — this scan reads it by name"));
    let body = &src[start + OPEN.len()..];
    let end = body
        .find("\n}")
        .unwrap_or_else(|| panic!("the Role enum's closing brace must be findable in {path}"));
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with("#["))
        .filter_map(|l| l.strip_suffix(','))
        .filter(|name| {
            name.chars().all(|c| c.is_ascii_alphanumeric())
                && name.starts_with(|c: char| c.is_ascii_uppercase())
        })
        .map(str::to_string)
        .collect()
}

/// The `Role` a harvested variant name denotes.
///
/// `None` is the ALARM, not a row to skip: a variant this test has never seen
/// has not been classified, and a guard that counted it as covered would be
/// certifying coverage it never delivered (CLAUDE.md's population-control rule
/// — count at the VERIFIED site, never at the MATCH site).
fn role_named(variant: &str) -> Option<Role> {
    Some(match variant {
        "Orchestrator" => Role::Orchestrator,
        "Worker" => Role::Worker,
        "Reviewer" => Role::Reviewer,
        "Planner" => Role::Planner,
        "Manager" => Role::Manager,
        "Solo" => Role::Solo,
        _ => return None,
    })
}

/// Whitespace-collapsed, so a rule that is re-wrapped across a line break does
/// not read as a deleted one — `manager_prose.rs`'s `flat`, for its reason. The
/// parenthetical this test reads is straddled by a line break in the template
/// today, so this is load-bearing rather than defensive.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One trailing plural `s`, removed exactly once — never `trim_end_matches`,
/// which would eat the last letter of a future class whose own name ends in one.
fn singular(word: &str) -> String {
    let w = word.trim().to_ascii_lowercase();
    w.strip_suffix('s').map(str::to_string).unwrap_or(w)
}

#[test]
fn the_orchestrators_counting_parenthetical_names_exactly_the_classes_that_count() {
    let variants = role_variants_in_source();

    // POSITIVE CONTROL, because this guard's success shape is "the two sets
    // matched" — byte-identical to a scan that read nothing at all and compared
    // two empty sets (CLAUDE.md). The population IS the guard here, so it is
    // checked before anything is concluded from it.
    assert!(
        variants.len() >= 6,
        "the Role harvest found only {variants:?} — a scan that cannot see the enum enforces nothing"
    );
    for expected in ["Worker", "Manager", "Solo"] {
        assert!(
            variants.iter().any(|v| v == expected),
            "the Role harvest missed `{expected}`: {variants:?}"
        );
    }

    let mut counted: BTreeSet<String> = BTreeSet::new();
    let mut verified = 0usize;
    for v in &variants {
        let role = role_named(v).unwrap_or_else(|| {
            panic!(
                "`Role::{v}` is new and unclassified. Add it to `role_named` above, then decide \
                 two things about it: is it DECLARABLE in a workflow file \
                 (`workflow::kind_from_str`), and does it spend a `max_agents` slot \
                 (`counts_against_max_agents`)? If BOTH, `templates/orchestrator.md`'s guardrail \
                 parenthetical has to name it too — and that is a goldened template, so the \
                 `pre222` fixtures are re-blessed in the same commit. This is the whole reason \
                 this test exists (#1429 round-5 N3)."
            )
        });
        // matched == verified: every harvested variant is classified, so one the
        // scan SAW but could not judge fails above rather than riding through as
        // covered.
        verified += 1;
        // The population is DERIVED, not excluded by hand: a class no workflow
        // file can name can never occupy a slot in a group, so the claim does
        // not reach it. `Role::Solo` is the only one today — see the residual
        // test below, which pins that blind spot and the bound on it.
        if workflow::kind_from_str(&v.to_ascii_lowercase()) == Some(role)
            && counts_against_max_agents(role)
        {
            counted.insert(role.as_str().to_string());
        }
    }
    assert_eq!(verified, variants.len(), "every harvested variant must be classified");
    assert!(
        !counted.is_empty(),
        "no declarable class counts against `max_agents` — that is the derivation broken, not the claim"
    );

    // The other side, read off the template rather than restated here.
    let tpl = flat(ORCHESTRATOR_TPL);
    const ANCHOR: &str = "live delegates (";
    let at = tpl.find(ANCHOR).unwrap_or_else(|| {
        panic!(
            "`templates/orchestrator.md` no longer states the guardrail as \
             \"... live delegates (<classes> count together)\". If the sentence moved or was \
             reworded, MOVE this pin with it rather than deleting it: that parenthetical is a \
             claim about `counts_against_max_agents`, and this is the only thing that reads it \
             (#1429 round-5 N3)."
        )
    });
    let rest = &tpl[at + ANCHOR.len()..];
    let close = rest
        .find(')')
        .expect("the guardrail parenthetical must close on the same (flattened) line");
    let inside = &rest[..close];
    let listed = inside.split("count together").next().unwrap_or(inside);
    let named: BTreeSet<String> =
        listed.split('+').map(singular).filter(|w| !w.is_empty()).collect();

    assert_eq!(
        named, counted,
        "`templates/orchestrator.md`'s guardrail parenthetical names {named:?}, but the classes a \
         workflow file may declare AND that spend a `max_agents` slot are {counted:?}. Whichever \
         moved, the two have to agree — the orchestrator is being told, in its own instructions, \
         which of its panes spend the cap it plans against. Editing that template re-blesses \
         `src-tauri/tests/fixtures/pre222/` in the same commit."
    );
}

#[test]
fn the_counting_pin_is_blind_to_a_class_no_workflow_file_can_name_and_that_is_bounded() {
    // THE RESIDUAL, pinned rather than merely disclosed (CLAUDE.md): the test
    // above derives its population from `kind_from_str`, so a class that counts
    // against the cap but is NOT declarable would be invisible to it.
    //
    // `Role::Solo` is exactly that class today — the predicate would count it…
    assert!(
        counts_against_max_agents(Role::Solo),
        "`counts_against_max_agents` defaults a new class to COUNTED; if Solo stopped counting, \
         this residual changed shape and the note above it went stale"
    );
    // …and it is outside the population by DERIVATION, not by a hand-written
    // exclusion in the test above.
    assert_eq!(
        workflow::kind_from_str("solo"),
        None,
        "if `solo` became declarable the pin above starts reading it, and the template's \
         parenthetical has to name it"
    );

    // THE BOUND on that blindness: a class the workflow file cannot name also
    // cannot be minted into a group by an agent, because `spawn_agent` resolves
    // its `kind` through the very same `kind_from_str`. So an undeclarable class
    // cannot reach a group's cap at all — which is what makes the population the
    // pin above derives the whole population the claim is about.
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails());
    let caller = orch_caller(&reg, &gid);
    let before = reg.list_agents(&gid).as_array().unwrap().len();
    let out = dispatch(
        &reg,
        &caller,
        "tools/call",
        &json!({ "name": "spawn_agent", "arguments": { "kind": "solo", "task": "t" } }),
    )
    .unwrap();
    assert_eq!(out["isError"], json!(true), "`kind: solo` must be refused: {out}");
    assert_eq!(
        reg.list_agents(&gid).as_array().unwrap().len(),
        before,
        "and nothing may be minted by the attempt"
    );

    // The non-vacuity control for that refusal: the same call shape with a kind
    // the file CAN name is admitted, so the assertion above is about `solo` and
    // not about a `spawn_agent` that refuses everything.
    let ok = dispatch(
        &reg,
        &caller,
        "tools/call",
        &json!({ "name": "spawn_agent", "arguments": { "kind": "worker", "task": "t" } }),
    )
    .unwrap();
    assert_ne!(ok["isError"], json!(true), "a declarable kind still spawns: {ok}");
}

// ───────── the manager-lifecycle residuals (#1433, folded into M5) ─────────
//
// Two premortem items #1426 filed rather than absorbed. Both come down to one
// question the app could not answer: **the manager pane is not there — does
// anyone tell the human?** #1426's arms tell the ORCHESTRATOR (so its `not
// live` fallback can fire) and write the audit trail; neither reaches the
// person whose pane it is.
//
// The answer is a NOTICE, never a repair, and that is a decided design point
// rather than a smaller scope — `docs/features/manager.md` already promises the
// human "if you close the manager pane, the group behaves as it always has", so
// closing it is a legitimate act, and no code can tell a deliberate close from
// a crash. Reopening on a guess would contradict a shipped promise. See
// `doc/design/manager.md`, "Why nothing reopens a dead manager".
//
// `group_summary`'s `manager_declared` is the one fact the panel was missing:
// `roles.manager` counts LIVE managers, and the human's question is the
// DIFFERENCE between declared and live. `src/group.ts`'s `managerAbsenceNotice`
// turns the pair into the line, and `test/group.test.ts` pins its wording.

/// Launch a group on a HAND-SUPPLIED roster with no workflow file in the repo.
///
/// The launch falls back to the caller's roster when the repo declares nothing
/// (`promote_orchestrator_cli`'s comment states it: "a broken or absent file
/// falls back to the caller's roster, exactly as the launch does"), which is the
/// only way to build the roster `spawn_agent_bound`'s unsupported-CLI branch
/// exists for — its own comment says so: "an unsupported one here means a
/// hand-edited group.json", and `parse_workflow` refuses to produce one.
///
/// Nothing is executed. The failure this reaches is a guardrail refusal on a
/// STRING, several steps before any process would be spawned, so constraint 3
/// (never run a real agent CLI) holds by construction rather than by luck.
fn launch_on_rails(rails: Guardrails) -> (Arc<OrchRegistry>, tempfile::TempDir, Repo, GroupId) {
    let (reg, dir) = test_registry();
    let repo = Repo::new(None);
    let req =
        create_orchestration_group(&reg, &repo.path(), rails, SessionOrigin::Fresh, None, None)
            .expect("the group must launch even when the manager cannot open");
    let gid = req.group_id.clone();
    (reg, dir, repo, gid)
}

/// `WITH_MANAGER`'s roster, parsed, with the manager block's CLI replaced by one
/// no build supports.
///
/// Built by PARSING rather than by constructing `Block`s field by field, so a
/// new field on `Block` cannot silently give this fixture a different shape from
/// the one a real workflow file produces.
fn rails_with_unopenable_manager() -> Guardrails {
    let mut blocks = workflow::parse_workflow(WITH_MANAGER)
        .expect("the fixture roster must parse")
        .blocks;
    let mut patched = 0;
    for b in &mut blocks {
        if b.kind == Role::Manager {
            b.cli = "notacli".into();
            patched += 1;
        }
    }
    // The mutation must have LANDED: an anchor that matched nothing leaves the
    // test green for the wrong reason (CLAUDE.md), and here "nothing patched"
    // would mean the manager opens perfectly and the arm below is never reached.
    assert_eq!(patched, 1, "exactly one manager block must have been made unopenable");
    Guardrails { blocks, ..rails() }
}

#[test]
fn a_manager_that_cannot_open_at_launch_is_audited_and_the_group_still_starts() {
    let (reg, _d, _repo, gid) = launch_on_rails(rails_with_unopenable_manager());

    // The launch DEGRADES rather than failing: a repo file must never be able to
    // stop a group from starting, and the orchestrator is the pane that can act
    // on the absence.
    assert_eq!(
        rows_of(&reg, &gid, "orchestrator").len(),
        1,
        "the orchestrator opens regardless — the manager is not a launch prerequisite"
    );
    assert!(
        rows_of(&reg, &gid, "manager").is_empty(),
        "no manager pane may exist when its own spawn was refused"
    );

    // The failure is on the record, with the reason legible — this is the arm
    // #1426's review noted no test reaches at all.
    let log = audit_text(&reg, &gid);
    assert!(
        log.contains("manager pane open failed"),
        "the failed open must be audited as an error: {log}"
    );
    assert!(
        log.contains("notacli"),
        "the audit must carry WHY it failed, not just that it did: {log}"
    );
    // …and it is recorded as an `error`, not as the `manager-already-live`
    // outcome, which is the other arm and means something entirely different.
    assert!(
        !log.contains("manager-already-live"),
        "a manager that never opened is not one that was already live: {log}"
    );
}

#[test]
fn the_panel_can_tell_a_missing_manager_from_a_group_that_declares_none() {
    // THE PAIR that makes the notice fail-able. `roles.manager` alone cannot
    // distinguish these two — it is 0 for both — which is exactly why
    // `manager_declared` had to exist.

    // (1) Declared, and its pane could not open: the human's interface is gone.
    let (reg, _d, _repo, gid) = launch_on_rails(rails_with_unopenable_manager());
    let s = reg.group_summary(&gid);
    assert_eq!(s["manager_declared"], json!(true), "the roster declares one: {s}");
    assert_eq!(s["roles"]["manager"], json!(0), "and none is live: {s}");

    // (2) The same launch with a manager that DOES open — the control, so (1)
    // cannot be read as "manager_declared is always true".
    let (reg2, _d2, _repo2, gid2) = launch(WITH_MANAGER, rails());
    let s2 = reg2.group_summary(&gid2);
    assert_eq!(s2["manager_declared"], json!(true), "still declared: {s2}");
    assert_eq!(s2["roles"]["manager"], json!(1), "and live: {s2}");

    // (3) A roster with no manager block at all — the common case, which must
    // report `false` rather than "0 live", or the panel would tell every group
    // in the app that its manager is missing.
    let (reg3, _d3, _repo3, gid3) = launch(WITHOUT_MANAGER, rails());
    let s3 = reg3.group_summary(&gid3);
    assert_eq!(s3["manager_declared"], json!(false), "no manager block is declared: {s3}");
    assert_eq!(s3["roles"]["manager"], json!(0), "{s3}");
}

#[test]
fn a_manager_that_dies_leaves_the_group_declaring_one_it_no_longer_has() {
    // #1433's SECOND item, on the same surface as the first: the reaper and the
    // watchdog both skip this class and the launch path is the only automatic
    // opener, so a manager that dies mid-session leaves a hole nothing fills.
    // What changes here is not that something fills it — deliberately — but that
    // the panel can now SEE it.
    let (reg, _d, _repo, gid) = launch(WITH_MANAGER, rails());
    let before = reg.group_summary(&gid);
    assert_eq!(before["roles"]["manager"], json!(1), "it starts live: {before}");

    let mid = the_manager(&reg, &gid)["id"].as_str().unwrap().to_string();
    reg.mark_dead(&mid, Some(0));

    let after = reg.group_summary(&gid);
    assert_eq!(
        after["manager_declared"],
        json!(true),
        "the roster still declares one — a death does not un-declare it: {after}"
    );
    assert_eq!(
        after["roles"]["manager"],
        json!(0),
        "and it is no longer live, which is the pair the notice reads: {after}"
    );

    // And nothing brought it back on its own, which is the decided behaviour
    // rather than a gap: this assertion is what would fail if a later slice
    // added an auto-reopen without revisiting the argument in
    // `doc/design/manager.md` and the promise in `docs/features/manager.md`.
    assert!(
        rows_of(&reg, &gid, "manager").iter().all(|m| m["status"] == json!("dead")),
        "nothing may reopen a manager automatically: {:?}",
        rows_of(&reg, &gid, "manager")
    );
}
