//! The **lead pane's capability class** (#2519 slice A): what `Role::Lead` may
//! see, what it may dispatch, what it may open, and where a child's report
//! lands.
//!
//! Its own integration-test binary rather than more of `orchestration.rs`, for
//! `manager_lifecycle.rs`'s two reasons. The mechanical one: an end-of-file
//! append to that file conflicts on its shared trailing tokens rather than on
//! its content (CLAUDE.md's git section), so concurrent slices splice into each
//! other's final assertion. The real one: these tests are one subject — one
//! capability class — and belong together for a reader.
//!
//! An integration test (not a unit test) because a test executable linking the
//! full lib needs the common-controls-v6 manifest `build.rs` embeds via
//! `rustc-link-arg-tests` — CLAUDE.md constraint 4.
//!
//! No test here spawns a real agent CLI (constraint 3): panes are registry
//! entries, deliveries are observed through the queue and the audit log, and
//! nothing is executed.
//!
//! **What slice A can and cannot cover, stated once.** There is no launch path
//! for a lead yet (`orch_lead_prepare` is slice B), and that is deliberate —
//! `workflow::kind_from_str` has no `lead` arm, so neither a workflow file nor
//! `spawn_agent` can mint one. Every fixture here therefore builds the lead
//! block itself, through `workflow::default_roster`, which is the same
//! construction slice B's group-minting will use. What that does NOT cover is
//! the launcher's own refusals and the group-creation invariant ("exactly one
//! root"), because neither exists to test yet; both land with the code that
//! creates them.

use loomux_lib::orchestration::mcp::{dispatch, every_tool_name};
use loomux_lib::orchestration::workflow;
use loomux_lib::orchestration::{
    counts_against_max_agents, is_live_cap_refusal, mechanics_core, resume_recorded_session,
    spawn_opens_minimized, AgentEntry, Caller, Delivery, GroupId, Guardrails, OrchRegistry,
    Role, CLI_CAPS,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

/// Build a registry against `dir` with every test-only directory override
/// applied. Duplicated from `orchestration.rs`/`manager_lifecycle.rs` because
/// these are separate integration-test BINARIES and helpers do not cross them —
/// and it is a real requirement, not ceremony: a registry built without these
/// overrides writes a generated agent file into the REAL `~/.claude` /
/// `~/.copilot` agents dir on its first spawn (#464).
/// `no_registry_construction_bypasses_the_test_agent_dir_overrides` in
/// `orchestration.rs` enforces that this file has exactly one raw
/// `OrchRegistry::new`, here.
fn relaunch_registry(dir: &Path) -> OrchRegistry {
    let reg = OrchRegistry::new(dir.to_path_buf());
    reg.set_port(45997);
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

/// Guardrails whose roster declares a LEAD block plus the delegate classes the
/// refusal tests need to name.
///
/// `idle_kill_minutes` and `watchdog_stall_minutes` are deliberately non-zero:
/// a 0 disables the reaper/watchdog outright, and a guardrail test against a
/// disabled guardrail is the vacuity shape CLAUDE.md names.
///
/// The roster is built with `workflow::default_roster` rather than from YAML
/// because a workflow file CANNOT declare a lead — that is the whole of the
/// no-recursion argument, and `a_workflow_file_cannot_declare_kind_lead` pins
/// it. `Guardrails::clamped` prepends an orchestrator BLOCK (step 4) since none
/// is declared; that is a roster row, not a pane, and no test here spawns an
/// orchestrator AGENT, which is what the root lookup reads.
fn lead_rails() -> Guardrails {
    Guardrails {
        max_agents: 4,
        agent_cli: "claude".into(),
        auto_ops: false,
        idle_kill_minutes: 5,
        watchdog_stall_minutes: 5,
        blocks: workflow::default_roster(&[
            (Role::Lead, "claude", ""),
            (Role::Worker, "claude", ""),
            (Role::Reviewer, "claude", ""),
            (Role::Planner, "claude", ""),
        ]),
        ..Guardrails::default()
    }
}

/// A group whose roster carries a lead block, and a LIVE lead pane in it.
///
/// The fixture asserts its own validity: a roster that lost the lead block
/// (`Guardrails::clamped` drops blocks on several grounds) would make every
/// "the lead was refused / the lead received it" assertion below pass for the
/// wrong reason.
fn lead_group() -> (Arc<OrchRegistry>, tempfile::TempDir, tempfile::TempDir, GroupId, AgentEntry)
{
    let (reg, d) = test_registry();
    let td = tempfile::tempdir().unwrap();
    let g = reg.create_group(&td.path().to_string_lossy(), lead_rails()).unwrap();
    let gid = g.id.clone();
    assert!(
        reg.group(&gid).unwrap().guardrails.block_for(Role::Lead).is_some(),
        "fixture must really carry a lead block, or nothing below tests what it says"
    );
    let lead = reg.spawn_agent(&gid, Role::Lead, "lead", "", false, None).unwrap();
    assert_eq!(lead.role, Role::Lead, "precondition: the pane really is a lead");
    (reg, d, td, gid, lead)
}

fn caller_for(reg: &OrchRegistry, a: &AgentEntry) -> Caller {
    reg.resolve_token(&a.token).expect("a spawned agent's token must resolve")
}

fn listed_tools(reg: &OrchRegistry, c: &Caller) -> Vec<String> {
    dispatch(reg, c, "tools/list", &Value::Null).unwrap()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect()
}

fn q_call(reg: &OrchRegistry, c: &Caller, name: &str, args: Value) -> Value {
    dispatch(reg, c, "tools/call", &json!({ "name": name, "arguments": args })).unwrap()
}

fn q_text(out: &Value) -> String {
    out["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

/// **The lead's tool surface is exactly this list.**
///
/// Asserted as the WHOLE listing rather than as a handful of `contains` checks,
/// on `manager_tool_surface_is_exactly_the_enumerated_set`'s model and for its
/// reason: a surface pinned by membership grows silently, and this one is a
/// capability boundary. A tool added to the shared read tier by a later slice
/// reddens this test, which is the direction a capability list should fail in.
///
/// `report` is called out separately even though the equality already covers
/// it, because it is not hypothetical — it is the exact defect #1161 M1 shipped
/// for the manager: the surface was whatever the `role == Orchestrator`
/// else-branch left over, and `report`'s own arm excluded only the
/// orchestrator, so a class whose instruction file says it has no `report`
/// could dispatch one. A lead is the second class with that shape.
#[test]
fn lead_tool_surface_is_exactly_the_enumerated_set() {
    let (reg, _d, _td, _gid, lead) = lead_group();
    let c = caller_for(&reg, &lead);

    assert_eq!(
        listed_tools(&reg, &c),
        vec![
            // the shared read tier, filtered to what a group-less-board pane
            // can honestly answer
            "list_agents",
            "request_compact",
            "note_directive",
            // the capability the toggle exists to grant
            "spawn_agent",
            "send_prompt",
            "get_output",
            "kill_agent",
            "focus_agent",
            "rename_agent",
            // the cross-workspace channel a human may connect this pane into
            "channel_send",
            "channel_status",
            // "what is this costing", answered where the human is asking
            "group_usage",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>(),
        "the lead's surface is a positive enumeration — see doc/design/lead-pane.md"
    );

    let names = listed_tools(&reg, &c);
    assert!(
        !names.contains(&"report".to_string()),
        "a lead is the ROOT of its group — there is nobody above it to report to"
    );
    // One per withheld REASON in the design note's table, rather than a longer
    // list of the same reason: no orchestrator to reach, no board, no question
    // registry, no review gate, no merge queue, and no self-registered watch.
    for (withheld, why) in [
        ("message_orchestrator", "a lead group has no orchestrator"),
        ("upsert_task", "a lead group has no task board"),
        ("ask_human", "the human is IN this pane"),
        ("review_verdict", "a lead group has no review gate"),
        ("queue_merge", "a lead group has no merge queue"),
        ("notify_when", "a fired watch is an injection the human did not ask for"),
        ("get_state", "nothing can write a lead group's state blob — see the pair test below"),
        ("set_state", "the write half of that same pair"),
    ] {
        assert!(!names.contains(&withheld.to_string()), "{withheld} must not be listed — {why}");
    }
}

/// **The group-state pair is granted together or not at all** (rev-final B1).
///
/// `get_state` was on the lead's surface in this PR's first draft, justified as
/// a durable scratchpad. It is not one, and the reason is structural rather
/// than a matter of taste: `state.json` has exactly ONE writer in the tree —
/// `OrchRegistry::set_state`, reached only from the `set_state` MCP arm, which
/// is `require_orchestrator` — and a lead group has no orchestrator by design.
/// A lead holding the read alone would call it forever and get `"{}"` every
/// time, which is the "advertise a route with nothing behind it" failure the
/// withheld column exists to prevent, in its silent form.
///
/// So the invariant is the PAIR, not either tool: whoever wants a lead to hold
/// durable state has to argue for the WRITE, and the read follows it. Asserted
/// as an equality of the two memberships rather than as two `!contains`
/// checks, because the shape this must fail on is one of them being added back
/// alone — which two independent negative assertions would let through in the
/// direction that matters (grant the read, forget the write).
#[test]
fn the_group_state_pair_is_granted_together_or_not_at_all() {
    let (reg, _d, _td, _gid, lead) = lead_group();
    let c = caller_for(&reg, &lead);
    let names = listed_tools(&reg, &c);

    let has_read = names.contains(&"get_state".to_string());
    let has_write = names.contains(&"set_state".to_string());
    assert_eq!(
        has_read, has_write,
        "the lead's surface lists get_state={has_read} and set_state={has_write}. A read \
         without the write is a call that returns \"{{}}\" forever — nothing in a lead group \
         can write that blob. Grant both with an argument for the write, or neither."
    );
    assert!(!has_read, "today it is neither — see doc/design/lead-pane.md's Withheld table");

    // The non-vacuity control: the equality above also holds when the surface
    // is empty or broken, so pin that this pane really does have a surface and
    // that a tool of the same shared read tier IS on it.
    assert!(
        names.contains(&"list_agents".to_string()),
        "the shared read tier must still reach this class, or the assertion above is about \
         a pane with no tools at all: {names:?}"
    );
}

/// **Every tool loomux has, minus the lead's own surface, is refused to a
/// lead** — the withheld half, iterated rather than sampled.
///
/// `every_tool_name()` is the union over every `(role, hint)` the listing
/// branches on, so this covers tools no test here knows the name of, including
/// ones a later slice adds. Sampling would only ever cover what the author
/// happened to think of, and the failure this guards against is precisely the
/// one nobody thought of.
///
/// **Population control.** The subtraction is what makes the loop meaningful
/// and also what could silently empty it — a bug in `every_tool_name`, a
/// surface that grew to cover everything, or a `retain` that kept nothing. So
/// the iterated set is asserted non-empty AND asserted to contain a specific
/// name that must be in it (`set_state`, the orchestrator's board write). Both,
/// deliberately: a floor alone passes on a garbage set, and a membership check
/// alone passes on a set of one.
#[test]
fn lead_cannot_dispatch_any_withheld_tool() {
    let (reg, _d, _td, _gid, lead) = lead_group();
    let c = caller_for(&reg, &lead);

    let granted: BTreeSet<String> = listed_tools(&reg, &c).into_iter().collect();
    let withheld: Vec<String> =
        every_tool_name().into_iter().filter(|n| !granted.contains(n)).collect();

    assert!(
        withheld.len() >= 20,
        "the withheld set collapsed to {} names — `every_tool_name()` or the lead's surface \
         changed shape, and this loop is no longer asserting anything: {withheld:?}",
        withheld.len()
    );
    assert!(
        withheld.contains(&"set_state".to_string()),
        "the orchestrator's own board write must be in the iterated set, or the subtraction \
         is not producing the population this test claims: {withheld:?}"
    );

    for name in &withheld {
        let out = q_call(&reg, &c, name, json!({}));
        assert_eq!(
            out["isError"],
            json!(true),
            "{name} is not on the lead's surface and must be refused"
        );
        assert!(
            q_text(&out).contains("not on a lead pane's surface"),
            "{name} must be refused BY THE LEAD GATE, not incidentally by its own argument \
             validation — a tool that happens to reject an empty argument object today would \
             pass a weaker assertion while being fully reachable tomorrow. Said: {:?}",
            q_text(&out)
        );
    }
}

/// **The dispatch gate and the listing agree, in both directions.**
///
/// The #243 double gate is only a double gate if its two halves say the same
/// thing, and they are deliberately spelled twice in `mcp.rs` rather than
/// shared — a single constant would make one edit move both, which is the drift
/// a double gate exists to catch. So the agreement is asserted rather than
/// assumed, over a set that includes tools the lead HAS and tools it does not,
/// because a test that only probes refusals passes just as well against a build
/// where every tool is broken.
#[test]
fn the_gate_and_the_listing_agree_for_a_lead() {
    let (reg, _d, _td, _gid, lead) = lead_group();
    let c = caller_for(&reg, &lead);
    let listed = listed_tools(&reg, &c);

    // Probe args are shaped so a tool which is NOT gated fails (if at all) on
    // its own arguments rather than on permission — that is what lets the
    // assertion distinguish "refused by the gate" from "refused at all".
    let probes: Vec<(&str, Value)> = vec![
        // on the surface
        ("list_agents", json!({})),
        ("group_usage", json!({})),
        ("note_directive", json!({ "text": "the human asked for X" })),
        ("channel_status", json!({})),
        ("spawn_agent", json!({ "kind": "worker", "task": "x" })),
        ("send_prompt", json!({ "agent_id": "w-1", "text": "hi" })),
        ("get_output", json!({ "agent_id": "w-1" })),
        ("kill_agent", json!({ "agent_id": "w-1" })),
        ("focus_agent", json!({ "agent_id": "w-1" })),
        ("rename_agent", json!({ "agent_id": "w-1", "name": "x" })),
        // off it
        ("report", json!({ "outcome": "done", "note": "x" })),
        ("message_orchestrator", json!({ "text": "hi" })),
        ("message_manager", json!({ "text": "hi" })),
        ("check_mail", json!({})),
        ("list_tasks", json!({})),
        ("get_task", json!({ "id": "t-1" })),
        ("upsert_task", json!({ "title": "t" })),
        ("remove_task", json!({ "id": "t-1" })),
        // Both halves of the group-state pair, deliberately adjacent: the read
        // was granted in this PR's first draft and is now withheld, and the
        // gate must refuse it exactly as it refuses the write (rev-final B1).
        ("get_state", json!({})),
        ("set_state", json!({ "state": "{}" })),
        ("list_questions", json!({})),
        ("list_needs_you", json!({})),
        ("ask_human", json!({ "text": "?" })),
        ("request_attention", json!({ "text": "look" })),
        ("list_verdicts", json!({})),
        ("review_verdict", json!({ "pr": "1", "verdict": "pass", "summary": "ok" })),
        ("queue_merge", json!({ "pr": "1" })),
        ("notify_when", json!({ "kind": "pr_checks", "pr": "1" })),
        ("session_digest", json!({})),
    ];
    let mut denied_by_gate = 0;
    for (name, args) in probes {
        let out = q_call(&reg, &c, name, args);
        let text = q_text(&out);
        let gated = out["isError"] == json!(true) && text.contains("not on a lead pane's surface");
        assert_eq!(
            gated,
            !listed.contains(&name.to_string()),
            "{name}: the gate and the listing disagree — listed={}, gate-denied={gated}, \
             said {text:?}",
            listed.contains(&name.to_string())
        );
        if gated {
            denied_by_gate += 1;
        }
    }
    // Non-vacuity: the loop above would also pass if the gate refused nothing
    // and the listing offered everything.
    assert_eq!(denied_by_gate, 19, "every off-surface probe must be refused BY THE GATE");
}

/// **A lead may open a worker, and nothing else** — each refusal named, with
/// the check that produced it identified rather than assumed.
///
/// The distinction matters because the refusals come from THREE different
/// places and only one of them is an arm anybody wrote for this class:
///
/// - `reviewer` / `planner` are refused by the lead's own caller-class check,
///   which reads the EFFECTIVE class (below the block resolution) so all three
///   spellings — `kind:`, `block:`, and an inherited resume — arrive at it;
/// - `orchestrator` / `manager` are refused by the two pre-existing argument
///   checks, unchanged, and are here as a control that this slice did not move
///   them;
/// - **`lead` is refused by the KIND VOCABULARY** — `workflow::kind_from_str`
///   has no `lead` arm — which is the no-recursion rule. Asserting WHICH check
///   said no is the whole point of this case: a refusal that quotes the
///   accepted vocabulary is the parse's, and if an arm were ever added to
///   `kind_from_str` this assertion fails while a bare "it was refused" check
///   would keep passing against a build where a lead can open a lead.
#[test]
fn a_lead_may_spawn_a_worker_and_nothing_else() {
    let (reg, _d, _td, gid, lead) = lead_group();
    let c = caller_for(&reg, &lead);
    let before = reg.list_agents(&gid).as_array().unwrap().len();

    let refusal = |kind: &str| -> String {
        let out = q_call(&reg, &c, "spawn_agent", json!({ "kind": kind, "task": "t" }));
        assert_eq!(out["isError"], json!(true), "kind {kind:?} must be refused: {out:?}");
        q_text(&out)
    };

    for kind in ["reviewer", "planner"] {
        let msg = refusal(kind);
        assert!(
            msg.contains("kind must be worker"),
            "{kind} must be refused by the lead's own class check: {msg}"
        );
        assert!(
            msg.contains("no review gate") && msg.contains("no task board"),
            "…and the refusal must say WHY, and what to do instead: {msg}"
        );
    }

    // The pre-existing refusals, unmoved. Both fire ABOVE the lead check (they
    // read the `kind` argument), so their wording is the orchestrator's, and
    // that is the control: this slice added a check, it did not rewrite these.
    assert!(
        refusal("orchestrator").contains("a group has exactly one"),
        "the orchestrator refusal is untouched by this slice"
    );
    assert!(
        refusal("manager").contains("the human's own"),
        "the manager refusal is untouched by this slice"
    );

    // NO RECURSION — and refused by the VOCABULARY, which is what makes it
    // structural. The refusal quotes what `kind_from_str` does accept, so this
    // asserts which check said no.
    let lead_kind = refusal("lead");
    assert!(
        lead_kind.contains("unknown kind"),
        "`kind: lead` must be refused as an unknown kind: {lead_kind}"
    );
    assert!(
        lead_kind.contains(&workflow::kind_names()),
        "…by `kind_from_str`, naming the vocabulary it does accept — if a `lead` arm is ever \
         added there, a lead can open a lead and this is what says so: {lead_kind}"
    );
    assert!(
        !workflow::kind_names().contains("lead"),
        "the accepted-kind vocabulary must not name `lead`, or the assertion above is circular"
    );

    // …and not one of those attempts may mint anything.
    assert_eq!(
        reg.list_agents(&gid).as_array().unwrap().len(),
        before,
        "a refused spawn must leave the roster exactly as it was"
    );

    // THE POSITIVE CONTROL, and it is what stops every assertion above from
    // holding in a build where a lead can spawn nothing at all. Deliberately
    // not "and then succeeds": a worker spawn cuts a real git worktree and this
    // fixture has no git repo under it, so asserting success would pin the
    // environment rather than the class check. What must be true is that
    // `worker` gets PAST the class check — so the refusal it does get must not
    // be that one.
    let worker = q_call(&reg, &c, "spawn_agent", json!({ "kind": "worker", "task": "t" }));
    assert!(
        !q_text(&worker).contains("kind must be worker"),
        "a worker must get past the lead's class check: {}",
        q_text(&worker)
    );
    assert!(
        !q_text(&worker).contains("not on a lead pane's surface"),
        "…and past the dispatch gate: {}",
        q_text(&worker)
    );
}

/// **A `block:` argument cannot spell around the worker-only rule.**
///
/// The reason this is its own case: the two manager refusals in the same arm
/// needed THREE separate checks between them precisely because a block's kind
/// WINS over `kind:` at `spawn_agent_ex`. A lead check written against the
/// `kind` argument alone would have exactly that hole with none of it plugged —
/// `kind: "worker", block: "reviewer"` would open a reviewer.
///
/// The fixture's roster carries real `reviewer` and `planner` blocks (see
/// `lead_rails`), so the block ids named here resolve rather than erroring for
/// an unrelated reason. The `worker` block is the non-vacuity control: the same
/// call shape with a block the lead MAY open is not refused by this check.
#[test]
fn a_lead_cannot_spell_around_the_worker_rule_with_a_block() {
    let (reg, _d, _td, gid, lead) = lead_group();
    let c = caller_for(&reg, &lead);
    let before = reg.list_agents(&gid).as_array().unwrap().len();

    for block in ["reviewer", "planner"] {
        // `kind: worker` is what makes this a real test rather than a restating
        // of the case above: the ARGUMENT says worker and only the resolved
        // block says otherwise.
        let out = q_call(
            &reg,
            &c,
            "spawn_agent",
            json!({ "kind": "worker", "block": block, "task": "t" }),
        );
        assert_eq!(out["isError"], json!(true), "block {block:?} must be refused: {out:?}");
        assert!(
            q_text(&out).contains("kind must be worker"),
            "…by the lead's class check reading the EFFECTIVE class, not the argument: {}",
            q_text(&out)
        );
        assert!(
            q_text(&out).contains(block) || q_text(&out).contains("resolves to"),
            "…and the refusal must say what it resolved to: {}",
            q_text(&out)
        );
    }
    assert_eq!(
        reg.list_agents(&gid).as_array().unwrap().len(),
        before,
        "a refused spawn must leave the roster exactly as it was"
    );

    // Control: the same shape with the block a lead MAY open is not refused by
    // this check (it fails later, on the missing git repo, as the case above
    // explains).
    let ok = q_call(&reg, &c, "spawn_agent", json!({ "block": "worker", "task": "t" }));
    assert!(
        !q_text(&ok).contains("kind must be worker"),
        "a worker BLOCK must get past the class check: {}",
        q_text(&ok)
    );
}

/// **A workflow file cannot declare `kind: lead`** — and the refusal is the
/// parser's own "unknown kind is rejected, never coerced".
///
/// This is the load-bearing half of the no-recursion argument and of "a repo
/// file can never hand a pane fleet control": the class is minted by exactly
/// one path, the launcher toggle a human flips, and its absence from
/// `kind_from_str` is what makes that true rather than a convention.
///
/// Both layers, because they are two mechanisms: the vocabulary function
/// itself, and a real `parse_workflow` over a file that names it — a parser
/// that read `kind` some other way would pass the first check alone.
#[test]
fn a_workflow_file_cannot_declare_kind_lead() {
    assert_eq!(
        workflow::kind_from_str("lead"),
        None,
        "`lead` must not be in the workflow vocabulary — see the arm-less comment in \
         `kind_from_str` for the three refusals that absence produces"
    );
    // The control: the vocabulary is not simply rejecting everything.
    assert_eq!(workflow::kind_from_str("worker"), Some(Role::Worker));
    assert_eq!(workflow::kind_from_str("manager"), Some(Role::Manager));

    let yaml = "version: 1\nblocks:\n\
                \x20 - id: helper\n    kind: lead\n\
                \x20 - id: worker\n    kind: worker\n";
    let errs = workflow::parse_workflow(yaml).expect_err("a lead block must fail the parse");
    let joined = errs.join(" | ");
    assert!(
        joined.contains("lead"),
        "the error must name the kind the author wrote: {joined}"
    );
    assert!(
        joined.contains(&workflow::kind_names()),
        "…and the vocabulary it does accept, so the author can fix the line: {joined}"
    );

    // The control again, at the parser this time: the SAME file with the one
    // word changed parses, so nothing above is a fact about the fixture being
    // malformed for an unrelated reason.
    let ok = yaml.replace("kind: lead", "kind: worker").replace("id: helper", "id: helper2");
    workflow::parse_workflow(&ok).expect("the same file with a legal kind must parse");
}

/// **A child's `report` is typed into the LEAD's pane, and the same fixture's
/// report is refused into a manager's** — the two poles, side by side.
///
/// One test rather than two because the asymmetry IS the property: the lookup
/// `deliver_relayed_to_root` performs asks `Role::is_root`, which admits an
/// orchestrator and a lead and excludes a manager, and the no-injection
/// guarantee at `deliver_prompt`'s door refuses a manager independently. Two
/// separate tests would each pass against a build where `is_root` had been
/// folded into `is_fixture` — the one edit that would make a manager a report
/// target — because neither would ever put the two answers next to each other.
///
/// Delivery is observed through the pause queue for `nothing_loomux_sends_mid_
/// session_can_reach_a_manager_pane`'s reason: test mode has no real PTY, so an
/// ACCEPTED delivery is a queued one, and the queue depth is the observation.
#[test]
fn a_child_report_is_typed_into_the_lead_pane_and_refused_into_a_manager() {
    let (reg, _d, _td, gid, lead) = lead_group();
    let worker = reg.spawn_agent(&gid, Role::Worker, "w", "task", false, None).unwrap();
    let cw = caller_for(&reg, &worker);

    reg.set_pty_for_test(&lead.id, 7401);
    reg.pause_group(&gid).unwrap();

    let out = q_call(
        &reg,
        &cw,
        "report",
        json!({ "outcome": "done", "note": "found it", "ref": "#1" }),
    );
    assert_eq!(out["isError"], json!(false), "a child's report must be accepted: {out:?}");
    assert_eq!(
        reg.queue_depth(7401),
        1,
        "the report must be admitted into the LEAD's pane — there is no orchestrator in this \
         group, and before `deliver_relayed_to_root` this call failed with `no live orchestrator`"
    );

    // …and it is the report, not merely something. The audit line names the
    // recipient, so this pins WHERE it went rather than that a queue moved.
    let line = reg
        .audit_log(&gid)
        .into_iter()
        .find(|e| e.action == "prompt" && e.detail["to"] == json!(lead.id))
        .expect("the delivery must be recorded against the lead");
    assert!(
        line.detail["text"].as_str().unwrap_or_default().contains("reports done"),
        "the queued text must be the child's report: {:?}",
        line.detail["text"]
    );

    // THE OTHER POLE. A manager pane, in its own group, takes no mid-session
    // delivery at all — the guarantee this slice must not weaken. Driven at
    // `deliver_prompt` with the SAME payload shape, which is what every
    // producer including `report` funnels through.
    let (mreg, _md, _mtd, mgid) = manager_group();
    let mgr = mreg.spawn_agent(&mgid, Role::Manager, "manager", "", false, None).unwrap();
    assert!(
        !mgr.role.is_root(),
        "a manager must never be a root — that is the one edit that would make it a report target"
    );
    let err = mreg
        .deliver_prompt(
            &mgr.id,
            "[orrerix] w-1 reports done: found it",
            "w-1",
            Delivery::MidSession,
        )
        .expect_err("a report-shaped mid-session delivery into a manager pane must be refused");
    assert!(
        err.contains("takes no delivery"),
        "the refusal must be the manager's own no-injection guarantee: {err}"
    );
}

/// A group whose roster declares a MANAGER — the other pole's fixture.
fn manager_group() -> (Arc<OrchRegistry>, tempfile::TempDir, tempfile::TempDir, GroupId) {
    let (reg, d) = test_registry();
    let td = tempfile::tempdir().unwrap();
    let rails = Guardrails {
        max_agents: 4,
        agent_cli: "claude".into(),
        advanced_orchestrator: true,
        blocks: workflow::default_roster(&[
            (Role::Orchestrator, "claude", ""),
            (Role::Manager, "claude", ""),
            (Role::Worker, "claude", ""),
        ]),
        ..Guardrails::default()
    };
    let g = reg.create_group(&td.path().to_string_lossy(), rails).unwrap();
    let id = g.id.clone();
    assert!(
        reg.group(&id).unwrap().guardrails.block_for(Role::Manager).is_some(),
        "fixture must really declare a manager block"
    );
    (reg, d, td, id)
}

/// **A `progress` report from a lead's child types nothing into the lead's
/// pane** — #1958's rule, unchanged by this slice, in a group with no board.
///
/// The POSITIVE CONTROL is the whole test: "nothing was typed" is an
/// absence-only assertion and passes just as well against a build where
/// delivery is broken for everybody, so the same child, in the same pane, in
/// the same call shape, sends a `done` first and that IS typed. The queue depth
/// then moves once and only once.
///
/// It also pins the honest answer in a board-less group: `report_task_note`
/// finds no board to read, so the tool says so rather than claiming a note it
/// did not write.
#[test]
fn progress_from_a_lead_child_types_nothing() {
    let (reg, _d, _td, gid, lead) = lead_group();
    let worker = reg.spawn_agent(&gid, Role::Worker, "w", "task", false, None).unwrap();
    let cw = caller_for(&reg, &worker);

    reg.set_pty_for_test(&lead.id, 7402);
    reg.pause_group(&gid).unwrap();

    // The control first, so a failure below cannot be "delivery never worked".
    let done = q_call(&reg, &cw, "report", json!({ "outcome": "done", "note": "landed" }));
    assert_eq!(done["isError"], json!(false), "{done:?}");
    assert_eq!(reg.queue_depth(7402), 1, "a done report reaches the lead's pane");

    let progress =
        q_call(&reg, &cw, "report", json!({ "outcome": "progress", "note": "still going" }));
    assert_eq!(progress["isError"], json!(false), "a progress report is accepted, not refused");
    assert_eq!(
        reg.queue_depth(7402),
        1,
        "…and typed nowhere: #1958's rule holds in a lead group exactly as in an \
         orchestration one"
    );
}

/// **A lead is a FIXTURE for the cap, the reaper and the dock** — three of the
/// seven guardrails that shared one hand-written `matches!` before this slice,
/// driven through their real consumers.
///
/// Driven through the consumers rather than only through `Role::is_fixture`,
/// because the predicate's set is already pinned in `model.rs`
/// (`the_fixture_classes_are_exactly_these_three`) and what this asserts is the
/// other half: that each CONSUMER really routes through it. A consumer that
/// kept its own copy of the old `Orchestrator | Manager` spelling would still
/// pass a predicate test.
///
/// The `Role::Worker` row in each pair is the non-vacuity control: without it
/// every assertion here would also hold in a build where the guardrail is
/// disabled outright.
///
/// **RESIDUAL, stated rather than implied**: two of the seven consumers have no
/// seam this binary can reach — the watchdog's eligibility check is inside a
/// private tick, and `release_driven_pane` is `pub(crate)`. Both read
/// `a.role.is_fixture()` in `mod.rs` and neither is exercised here, so for those
/// two the coverage is the predicate pin plus the compiler, not a behavioural
/// test. Do not read this test's name as covering them.
#[test]
fn a_lead_is_a_fixture_for_the_cap_the_reaper_and_the_dock() {
    assert!(!counts_against_max_agents(Role::Lead), "a lead does not spend a delegate slot");
    assert!(counts_against_max_agents(Role::Worker), "…and a worker does — the control");

    // The dock (#260): the pane the human works through is never minimized.
    assert!(!spawn_opens_minimized(Role::Lead, false), "a lead pane is never docked");
    assert!(spawn_opens_minimized(Role::Worker, false), "…and a delegate is — the control");

    let (reg, _d, _td, gid, lead) = lead_group();
    let worker = reg.spawn_agent(&gid, Role::Worker, "w", "", false, None).unwrap();

    // Both panes are idle (both were spawned with an empty task, which is what
    // stamps `idle_since_ms`), and the group's timeout is 5 minutes — so a reap
    // run far in the future must name the worker and must not name the lead.
    //
    // `u64::MAX / 2`, not a hand-written "big" number: `idle_should_kill`
    // measures `now - idle_since` against a wall-clock epoch stamp, so a
    // literal that merely LOOKS large (1e12 ms is the year 2001) is in the
    // PAST and saturates the subtraction to zero. The control below caught
    // exactly that, which is what it is for.
    let far_future = u64::MAX / 2;
    let reaped = reg.idle_reap_candidates(far_future);
    assert!(
        reaped.contains(&worker.id),
        "the control: an idle worker past the timeout IS a reap candidate, or this test is \
         measuring a disabled reaper — got {reaped:?}"
    );
    assert!(
        !reaped.contains(&lead.id),
        "a lead pane is silent exactly when its human is reading — reaping one closes their \
         own pane under them: {reaped:?}"
    );
}

/// **No agent may kill a lead pane** — including the lead itself, which holds
/// `kill_agent` on its own surface.
///
/// A hole this slice would otherwise OPEN rather than a pre-existing one:
/// `kill_agent` is on the lead's enumerated surface and `require_in_group`
/// passes for the caller's own id, so without the guard a lead could end the
/// human's pane from inside it.
///
/// The worker row is the control: the same tool, the same caller, a target that
/// SHOULD die — without it this would pass against a build where `kill_agent`
/// is broken for everything.
#[test]
fn no_agent_may_kill_a_lead_pane() {
    let (reg, _d, _td, gid, lead) = lead_group();
    let c = caller_for(&reg, &lead);
    let worker = reg.spawn_agent(&gid, Role::Worker, "w", "", false, None).unwrap();

    let out = q_call(&reg, &c, "kill_agent", json!({ "agent_id": lead.id }));
    assert_eq!(out["isError"], json!(true), "a lead must not be able to kill its own pane");
    assert!(
        q_text(&out).contains("the human's own"),
        "the refusal must say whose pane it is: {}",
        q_text(&out)
    );

    // The control. A worker with no bound pty cannot actually be killed in test
    // mode, so what is asserted is that it gets PAST the class guard — the
    // refusal it does get must not be the lead one.
    let w = q_call(&reg, &c, "kill_agent", json!({ "agent_id": worker.id }));
    assert!(
        !q_text(&w).contains("the human's own"),
        "a worker must get past the lead-pane guard: {}",
        q_text(&w)
    );
}

// ── slice B: the launch path ────────────────────────────────────────────────
//
// Slice A's header says what it could not cover: there was no way to MINT a
// lead, so the launcher's refusals and the one-root invariant had no code to
// test. This half is that code. The fixtures below therefore stop building a
// roster by hand and go through `lead_prepare`, which is the only path a lead
// group is ever created by.

/// A minimal real git repo (one commit on the default branch), one level below
/// a private temp root.
///
/// The nesting is not cosmetic and is `orchestration.rs`'s `RealRepo` verbatim
/// in intent (#464): `git_worktree_add` cuts a spawned worker's worktree to a
/// directory SIBLING to the repo, so a repo AT the temp root leaks that sibling
/// into `%TEMP%` on every passing run. One level down puts both the sibling and
/// the `.git/worktrees/<name>` registration inside the `TempDir`, so its `Drop`
/// reclaims everything with no git teardown a call site could forget.
///
/// Copied rather than shared because Rust compiles each integration test as its
/// own crate — the same reason `relaunch_registry` above is duplicated.
struct RealRepo {
    _root: tempfile::TempDir,
    repo: std::path::PathBuf,
}

impl RealRepo {
    fn path(&self) -> String {
        self.repo.to_string_lossy().into_owned()
    }
}

fn real_repo() -> RealRepo {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .expect("git must be installed for this test");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "hi").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);
    RealRepo { _root: root, repo }
}

/// A lead group minted the way the launcher mints one: `lead_prepare` against a
/// real repo, then `lead_bind` onto a fake pty.
///
/// Returns the prepare payload too, because several tests below assert on it
/// and re-deriving it would be a second construction of the thing under test.
fn prepared_lead(
    cli: &str,
    max_agents: u32,
) -> (Arc<OrchRegistry>, tempfile::TempDir, RealRepo, GroupId, String, Value) {
    let (reg, d) = test_registry();
    let repo = real_repo();
    let out = reg
        .lead_prepare(cli, &repo.path(), "my lead", max_agents, false, 5, 0, 5)
        .unwrap_or_else(|e| panic!("lead_prepare({cli}) must succeed: {e}"));
    let gid = GroupId::parse(out["group_id"].as_str().expect("group_id")).expect("a valid group id");
    let agent_id = out["agent_id"].as_str().expect("agent_id").to_string();
    (reg, d, repo, gid, agent_id, out)
}

/// **A `"role": "lead"` row can never reach an `unreachable!`** — the residual
/// slice A shipped, closed.
///
/// Slice A made `Role`'s serde form carry the wire string `"lead"`, so an
/// `agents.json` row reading `"role": "lead"` PARSES; what it left was two
/// panicking arms (`mechanics_core`, `kickoff_body`) reachable the moment such
/// a row meets a public path. Under CLAUDE.md constraint 10 an unwind out of a
/// synchronous `#[tauri::command]` is a process ABORT, not a degrade, so
/// "unreachable today" was not a safe place to leave them.
///
/// Both are exercised through the public entry points their private callers use
/// — `mechanics_core` directly (it is what `render_block_instructions`'s
/// replace-mode arm and `copilot_agent_body` both call) and `kickoff_prompt`,
/// which wraps `kickoff_body`.
///
/// The worker rows are the control: without them every assertion here would
/// hold against a build that returned one shared string for every class.
#[test]
fn a_lead_role_never_reaches_a_panicking_arm() {
    let core = mechanics_core(Role::Lead, None);
    assert!(!core.trim().is_empty(), "a lead's mechanics core must be real text");
    assert!(
        core.contains("root of this group") || core.contains("ROOT of this group"),
        "…and must say the thing that makes the class what it is: {core}"
    );
    assert!(
        core.contains("no `report`") || core.contains("hold no `report`"),
        "…including the tool it does NOT have, which is the manager's own lesson: {core}"
    );
    assert_ne!(
        core,
        mechanics_core(Role::Worker, None),
        "the control: a lead's mechanics are not the worker's, so the assertions above are \
         about this arm rather than about one shared string"
    );

    let (reg, _d, _repo, gid, agent_id, _out) = prepared_lead("claude", 4);
    let a = reg.agent(&agent_id).expect("the lead entry");
    let g = reg.group(&gid).expect("the lead group");
    assert_eq!(a.role, Role::Lead, "precondition: this really is a lead row");
    let kickoff = reg.kickoff_prompt(&a, &g, "", None);
    assert!(kickoff.contains("LEAD of orrerix group"), "the lead's own kickoff: {kickoff}");
    assert!(
        kickoff.contains(&gid.to_string()) && kickoff.contains(&agent_id),
        "…naming the group and the pane it is addressed to: {kickoff}"
    );
    assert!(
        kickoff.contains("lead.md"),
        "…and pointing at the instructions file `write_instruction_files` wrote: {kickoff}"
    );
}

/// **`lead_prepare` mints a group with the built-in roster and no
/// orchestrator.**
///
/// Four properties, each one a different way the mint could be wrong:
/// the group exists and is real; it runs the BUILT-IN roster (no workflow file
/// was read); its only live agent is the lead, so nothing spawned an
/// orchestrator; and the lead is a member of it rather than of `__solo__`.
///
/// The orchestrator BLOCK is asserted to be present on purpose — `clamped`
/// prepends one to any roster declaring none, and slice A's note flags exactly
/// that as the tripwire this function must not turn into a pane. Pinning "the
/// block is there AND no agent holds it" is what makes the distinction
/// reviewable rather than implied.
#[test]
fn lead_prepare_mints_a_group_with_the_builtin_roster_and_no_orchestrator() {
    let (reg, _d, repo, gid, agent_id, out) = prepared_lead("claude", 3);
    let g = reg.group(&gid).expect("the group must exist in the registry");
    assert_eq!(g.repo, repo.path(), "the group is on the repo the toggle was flipped in");
    assert_eq!(g.guardrails.max_agents, 3, "the launcher's cap is the group's cap");
    assert!(
        !g.guardrails.advanced_orchestrator,
        "a lead group never reads the repo's workflow file — that is the consent argument"
    );

    let kinds: Vec<Role> = g.guardrails.blocks.iter().map(|b| b.kind).collect();
    for want in [Role::Lead, Role::Worker, Role::Reviewer, Role::Planner] {
        assert!(kinds.contains(&want), "the built-in roster must declare {want:?}: {kinds:?}");
    }
    assert!(
        g.guardrails.block_for(Role::Orchestrator).is_some(),
        "`Guardrails::clamped` step 4 prepends an orchestrator BLOCK to a roster with none — if \
         that ever stops being true this test's real assertion below stops meaning anything"
    );

    let live: Vec<(String, String)> = reg
        .list_agents(&gid)
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["id"].as_str().unwrap_or_default().to_string(),
                a["role"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        live,
        vec![(agent_id.clone(), "lead".to_string())],
        "…and NO agent holds it: the lead is the group's only pane, and the one root"
    );

    let a = reg.agent(&agent_id).expect("the lead entry");
    assert_eq!(a.group, gid, "the lead belongs to its own group, never `__solo__`");
    assert_eq!(a.block, "lead", "…on the lead block the roster declares");
    assert!(a.role.is_root(), "…and it is the group's root");
    assert!(a.solo_cli.is_none(), "a non-solo agent's CLI comes from its block, not this field");
    assert!(
        !out["mcp_args"].as_str().unwrap_or_default().is_empty(),
        "the launcher needs flags to append, or the pane boots with no MCP server at all: {out}"
    );

    // The durable marker, which is what every post-restart reader asks (the
    // roster cannot answer: `read_blocks` drops a `kind: "lead"` row).
    assert!(reg.is_lead_group(&gid), "the group must record that it is a lead group");
}

/// **The marker is load-bearing, because the ROSTER cannot answer after a
/// restart.**
///
/// This is the fact `lead_prepare`'s marker comment claims, asserted rather
/// than asserted-in-prose: reload the group from disk exactly as a relaunched
/// app does, and the lead block is GONE — `read_blocks` resolves every
/// persisted `kind` through `workflow::kind_from_str`, which has no `lead` arm
/// by design. Anything that asked the reloaded roster "is this a lead group?"
/// would answer no for every lead group there has ever been.
#[test]
fn a_reloaded_lead_group_loses_its_lead_block_but_keeps_its_marker() {
    let (reg, _d, _repo, gid, _agent_id, _out) = prepared_lead("claude", 4);
    assert!(
        reg.group(&gid).unwrap().guardrails.block_for(Role::Lead).is_some(),
        "the control: in memory the roster DOES carry the lead block"
    );

    let (_repo_path, reloaded) = reg.load_group_file(&gid).expect("group.json must be on disk");
    assert!(
        reloaded.block_for(Role::Lead).is_none(),
        "a persisted `kind: \"lead\"` row is dropped on reload — if this ever starts passing, \
         `kind_from_str` has grown a `lead` arm and a workflow file can declare one"
    );
    assert!(
        reloaded.block_for(Role::Worker).is_some(),
        "…and the control: the rest of the roster survives, so the line above is about `lead` \
         rather than about a broken load"
    );
    assert!(reg.is_lead_group(&gid), "which is why the marker, not the roster, carries the fact");
}

/// **A CLI whose MCP config cannot ride on its command line is refused**, and
/// the refusal names the follow-up rather than reading as a policy.
///
/// `solo_prepare` degrades such a CLI to delivery-only, which is still a useful
/// pane. A lead cannot: the flags ARE the capability the toggle grants, so a
/// pane that launched anyway would be a lead in name only.
#[test]
fn lead_prepare_refuses_a_non_argv_seam_cli() {
    let (reg, _d) = test_registry();
    let repo = real_repo();

    let no_seam: Vec<&str> = CLI_CAPS
        .iter()
        .filter(|c| c.orchestration && !c.mcp_argv_seam)
        .map(|c| c.cli)
        .collect();
    assert!(
        no_seam.contains(&"opencode"),
        "this test is vacuous unless the table really has opencode as a non-seam CLI: {no_seam:?}"
    );
    for cli in &no_seam {
        let err = reg
            .lead_prepare(cli, &repo.path(), "l", 4, false, 5, 0, 5)
            .expect_err("a non-seam CLI must be refused");
        assert!(
            err.contains("cannot host a lead pane yet"),
            "{cli}: the refusal must be the seam one: {err}"
        );
        assert!(
            err.contains("follow-up") && err.contains("lead-pane.md"),
            "{cli}: …and must name the follow-up, so it reads as a missing seam rather than a \
             policy: {err}"
        );
    }

    // THE CONTROL, and it is what stops this passing against a build that
    // refuses every CLI: a seam CLI is accepted, on the same repo.
    reg.lead_prepare("claude", &repo.path(), "l", 4, false, 5, 0, 5)
        .expect("an argv-seam CLI must be accepted");
}

/// **Every argv-seam CLI has a lead arm**, and the arm really carries the MCP
/// wiring.
///
/// `every_argv_seam_cli_has_a_solo_mcp_arm`'s shape, with the failure mode
/// inverted: an empty string is a refusal here rather than a degraded pane, so
/// what this pins is that no seam CLI takes that path today.
#[test]
fn every_argv_seam_cli_has_a_lead_mcp_arm() {
    let seam: Vec<&str> =
        CLI_CAPS.iter().filter(|c| c.orchestration && c.mcp_argv_seam).map(|c| c.cli).collect();
    assert!(
        seam.contains(&"pi") && seam.contains(&"copilot"),
        "this test is vacuous unless the table really has more than claude as a seam CLI: {seam:?}"
    );
    for cli in seam {
        let (reg, _d, _repo, _gid, _agent_id, out) = prepared_lead(cli, 4);
        let args = out["mcp_args"].as_str().unwrap_or_default();
        assert!(
            args.contains("mcp"),
            "{cli} is declared argv-seam, so lead_prepare must produce its MCP flags: {out}"
        );
        drop(reg);
    }
}

/// **Claude's lead command line denies the `Agent` tool, and no other CLI's
/// pretends to.**
///
/// The denial is what makes the toggle mean what it says: a lead that can still
/// reach its harness's own in-process subagents will use them, because they are
/// one call away and `spawn_agent` is three. The flag is cited to the vendor
/// docs in `lead_mcp_args`' own doc comment.
///
/// The other two arms are the honest half. Copilot HAS subagents and documents
/// no tool name to deny (`--deny-tool`'s documented value shapes are
/// `shell(...)`, `write` and `MCP(tool)`), so it is instruction-only; pi
/// documents having none at all. Asserting that neither carries an invented
/// denial is what stops a later edit shipping a flag value the vendor does not
/// have.
#[test]
fn the_lead_command_line_denies_claudes_agent_tool_and_nothing_elses() {
    let (_r1, _d1, _repo1, _g1, _a1, claude) = prepared_lead("claude", 4);
    let line = claude["mcp_args"].as_str().unwrap_or_default().to_string();
    assert!(
        line.contains("--disallowedTools Agent"),
        "claude's lead line must deny the Agent tool: {line}"
    );

    // The solo line is the BASELINE: a lead's flags are a solo pane's plus the
    // denial, so diffing them is what shows the denial is the only addition.
    let (reg, _d) = test_registry();
    let repo = real_repo();
    let solo = reg.solo_prepare("claude", &repo.path(), "s").expect("a solo claude pane");
    let solo_line = solo["mcp_args"].as_str().unwrap_or_default();
    let solo_flags: Vec<&str> = solo_line.split_whitespace().filter(|t| t.starts_with("--")).collect();
    let lead_flags: Vec<&str> = line.split_whitespace().filter(|t| t.starts_with("--")).collect();
    let added: Vec<&&str> = lead_flags.iter().filter(|f| !solo_flags.contains(f)).collect();
    assert_eq!(
        added,
        vec![&"--disallowedTools"],
        "the lead line is the solo line plus exactly one flag: solo={solo_flags:?} \
         lead={lead_flags:?}"
    );

    for cli in ["copilot", "pi"] {
        let (_r, _d, _repo, _g, _a, out) = prepared_lead(cli, 4);
        let l = out["mcp_args"].as_str().unwrap_or_default();
        assert!(
            !l.contains("disallow") && !l.contains("deny") && !l.contains("exclude"),
            "{cli} documents no subagent tool name to deny, so its line must not invent one — \
             see `lead_mcp_args` for the citation: {l}"
        );
    }
}

/// **`lead_bind` types the lead's kickoff, exactly once.**
///
/// Delivery is observed through the pause queue, for
/// `a_child_report_is_typed_into_the_lead_pane_and_refused_into_a_manager`'s
/// reason: test mode has no real PTY, so an ACCEPTED delivery is a queued one.
///
/// The second bind is refused rather than tolerated, and that matters more than
/// it looks: the kickoff is not idempotent, so a tolerated re-bind would type a
/// second contract into a conversation already under way.
#[test]
fn lead_bind_delivers_the_lead_kickoff_once() {
    let (reg, _d, _repo, gid, agent_id, _out) = prepared_lead("claude", 4);
    reg.pause_group(&gid).unwrap();

    assert_eq!(reg.queue_depth(9101), 0, "the control: nothing is queued before the bind");
    reg.lead_bind(&agent_id, 9101).expect("the first bind must succeed");
    assert_eq!(reg.queue_depth(9101), 1, "the bind must type exactly one kickoff");

    let texts: Vec<String> = reg
        .audit_log(&gid)
        .into_iter()
        .filter(|e| e.action == "prompt" && e.detail["to"] == json!(agent_id))
        .filter_map(|e| e.detail["text"].as_str().map(str::to_string))
        .collect();
    assert_eq!(texts.len(), 1, "one delivery, recorded against the lead: {texts:?}");
    assert!(
        texts[0].contains("LEAD of orrerix group"),
        "…and it is the lead's kickoff rather than something else: {}",
        texts[0]
    );

    let err = reg.lead_bind(&agent_id, 9102).expect_err("a second bind must be refused");
    assert!(err.contains("already bound"), "{err}");
    assert_eq!(reg.queue_depth(9101), 1, "…and must not have typed a second kickoff");
    assert_eq!(reg.queue_depth(9102), 0, "…nor into the pty it named");

    // The class guard: `lead_bind` is not a general bind.
    let worker = reg.spawn_agent(&gid, Role::Worker, "w", "", false, None).unwrap();
    let werr = reg.lead_bind(&worker.id, 9103).expect_err("a worker must not take this path");
    assert!(werr.contains("only for lead panes"), "{werr}");
}

/// **A kickoff that cannot be delivered leaves the pane bound, and leaves a
/// trail.**
///
/// The counterfactual `lead_bind`'s doc states, performed rather than
/// asserted in prose (CLAUDE.md: a documented escape hatch is only pinned by a
/// test that takes it). By the time `lead_bind` runs the pane is open and the
/// human is looking at it, so an `Err` would report a launch failure for a
/// launch that plainly happened — but a dropped kickoff must not be INVISIBLE,
/// or a lead that never learned it is one becomes a pane whose behaviour
/// nobody can explain from the outside.
///
/// Headless is the real thing here rather than a stand-in: with no `AppHandle`
/// `deliver_prompt` withdraws its own admission and returns
/// `Err("no app handle")` (see `announce_pause_suppression`'s doc), which is
/// exactly the shape a production drop takes — the queue ends up empty and the
/// caller gets an error. The contrast with
/// `lead_bind_delivers_the_lead_kickoff_once`, which pauses the group and so
/// takes the early Ok, is what makes this test about the FAILURE path.
#[test]
fn an_undeliverable_lead_kickoff_degrades_rather_than_failing_the_bind() {
    let (reg, _d, _repo, gid, agent_id, _out) = prepared_lead("claude", 4);
    // Deliberately NOT paused: the delivery is attempted for real and fails.
    reg.lead_bind(&agent_id, 9701).expect("the bind must succeed even so");

    let a = reg.agent(&agent_id).expect("the lead entry");
    assert_eq!(a.pty_id, Some(9701), "the pane is bound…");
    assert_eq!(reg.queue_depth(9701), 0, "…and the kickoff really was not delivered");

    let row = reg
        .audit_log(&gid)
        .into_iter()
        .find(|e| e.action == "error" && e.detail["what"] == json!("lead kickoff not delivered"))
        .expect("a dropped kickoff must be findable in the trail, not inferred from silence");
    assert_eq!(row.detail["agent"], json!(agent_id), "{:?}", row.detail);
    assert!(
        row.detail["err"].as_str().unwrap_or_default().contains("no app handle"),
        "…carrying the delivery's own error rather than a generic one: {:?}",
        row.detail
    );
}

/// **A dead lead takes its children with it**, and a dead worker takes nothing.
///
/// One test, both poles, because the asymmetry IS the property: the arm added
/// to `on_pty_exit` keys on `Role::Lead`, and a build that ended a group's
/// delegates on ANY pane's exit would pass a lead-only test.
///
/// Driven through `on_pty_exit`, which is the real entry point every ending
/// funnels through — a close, a kill, or the CLI dying on its own — rather than
/// through `end_lead_children` directly, so the routing is part of what is
/// pinned.
#[test]
fn a_dead_lead_takes_its_children_with_it() {
    let (reg, _d, _repo, gid, agent_id, _out) = prepared_lead("claude", 4);
    reg.lead_bind(&agent_id, 9201).unwrap();
    let w1 = reg.spawn_agent(&gid, Role::Worker, "w1", "t", false, None).unwrap();
    let w2 = reg.spawn_agent(&gid, Role::Worker, "w2", "t", false, None).unwrap();
    reg.set_pty_for_test(&w1.id, 9202);
    reg.set_pty_for_test(&w2.id, 9203);

    // THE POSITIVE CONTROL, first: one worker dying ends that worker and
    // nothing else. Without it, every assertion below would hold against a
    // build that tore the group down on any exit at all.
    reg.on_pty_exit(9202, Some(0), "", 0, true);
    let after_worker = reg.list_agents(&gid);
    let live_ids: Vec<String> = after_worker
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["status"] != json!("dead"))
        .map(|a| a["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        live_ids.contains(&agent_id) && live_ids.contains(&w2.id),
        "a dead worker takes nothing with it: {live_ids:?}"
    );
    assert!(!live_ids.contains(&w1.id), "…and it is itself dead — the control's own control");

    // Now the lead.
    reg.on_pty_exit(9201, Some(1), "", 0, false);
    let live_after: Vec<String> = reg
        .list_agents(&gid)
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["status"] != json!("dead"))
        .map(|a| a["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        live_after.is_empty(),
        "a lead that dies ends its helpers: nothing may outlive the pane it reports to — \
         still live: {live_after:?}"
    );

    let row = reg
        .audit_log(&gid)
        .into_iter()
        .find(|e| e.action == "lead-children-ended")
        .expect("the teardown must be recorded, or an operator cannot tell it from a crash");
    assert_eq!(row.detail["lead"], json!(agent_id), "{:?}", row.detail);
    assert_eq!(
        row.detail["ended"],
        json!([w2.id]),
        "exactly the ones that were still alive — the already-dead worker is not re-killed: {:?}",
        row.detail
    );
}

/// **The cap and the spawn-rate backstop apply to a lead's children.**
///
/// Q6's row, in code: what is EXEMPT is the lead's own seat, not the helpers.
/// A cap of 1 must therefore admit one helper and refuse the second — and the
/// refusal must be the CAP's, which is why `is_live_cap_refusal` is asked
/// rather than the text being re-spelled here.
///
/// The lead is deliberately live throughout: if the lead counted, a cap of 1
/// would refuse the FIRST helper, and this test's first assertion is what says
/// it does not.
#[test]
fn lead_group_cap_and_spawn_rate_apply_to_children() {
    let (reg, _d, repo, gid, agent_id, _out) = prepared_lead("claude", 1);
    reg.lead_bind(&agent_id, 9301).unwrap();
    assert_eq!(reg.group(&gid).unwrap().guardrails.max_agents, 1, "precondition: a cap of one");

    let first = reg
        .spawn_agent(&gid, Role::Worker, "w1", "t", false, None)
        .expect("the lead's seat is exempt, so a cap of 1 still admits one helper");
    let err = reg
        .spawn_agent(&gid, Role::Worker, "w2", "t", false, None)
        .expect_err("…and the helpers are counted, so the second is refused");
    assert!(
        is_live_cap_refusal(&err),
        "the refusal must be the live-agent cap's, not a bad block or the rate backstop: {err}"
    );
    assert!(err.contains(&first.id), "…and must name what is holding the slot: {err}");

    // The spawn-rate backstop, on its own group so the cap cannot be what
    // refuses. A limit of 1 per hour with the lead's own mint not counted.
    let (reg2, _d2) = test_registry();
    let out2 = reg2
        .lead_prepare("claude", &repo.path(), "l2", 8, false, 5, 1, 5)
        .expect("a second lead group, on a repo that already has one");
    let gid2 = GroupId::parse(out2["group_id"].as_str().unwrap()).unwrap();
    // Its own registry means its own orchestration ROOT, so the id it derives
    // from this repo path is the same one — `next_group_id` reads the live
    // agents of THIS registry and `reg2` has none. That is why the precondition
    // to assert is the guardrail pair rather than the id: a cap of 8 cannot be
    // what refuses below, so a refusal there is the rate backstop's.
    let rails2 = reg2.group(&gid2).unwrap().guardrails;
    assert_eq!(rails2.max_spawns_per_hour, 1, "one spawn per hour");
    assert_eq!(rails2.max_agents, 8, "…and a cap far above the two spawned here");
    reg2.spawn_agent(&gid2, Role::Worker, "w1", "t", false, None).expect("the first is admitted");
    let rate = reg2
        .spawn_agent(&gid2, Role::Worker, "w2", "t", false, None)
        .expect_err("the second exceeds one spawn per hour");
    assert!(rate.contains("spawn-rate limit reached"), "{rate}");
}

/// **A lead's `spawn_agent` really opens a worker — end to end.**
///
/// Slice A could only assert that a worker gets PAST the class check, because
/// its fixture had no git under it. This one has a real repo, so the spawn runs
/// the whole way through `spawn_agent_ex`: the worktree is cut, the branch is
/// created, and the pane request is built. That is the difference between "the
/// refusal does not fire" and "the feature works".
///
/// Dispatched through the MCP surface rather than the Rust API, because the
/// worktree default is a property of that surface (#338/#359) and it is the
/// only surface a lead can actually reach.
#[test]
fn a_lead_really_opens_a_worker_in_its_own_worktree() {
    let (reg, _d, repo, gid, agent_id, _out) = prepared_lead("claude", 4);
    reg.lead_bind(&agent_id, 9401).unwrap();
    let lead = reg.agent(&agent_id).expect("the lead entry");
    let c = caller_for(&reg, &lead);

    let out = q_call(
        &reg,
        &c,
        "spawn_agent",
        json!({ "kind": "worker", "name": "helper", "task": "read the tests and report back" }),
    );
    assert_eq!(out["isError"], json!(false), "a lead's worker spawn must succeed: {out:?}");

    let worker = reg
        .list_agents(&gid)
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["role"] == json!("worker"))
        .map(|a| a["id"].as_str().unwrap_or_default().to_string())
        .expect("the group must now hold a worker");
    let w = reg.agent(&worker).expect("the worker entry");
    assert_eq!(w.group, gid, "the helper joins the lead's group, not `__solo__`");
    assert_eq!(w.task, "read the tests and report back", "…carrying the brief the lead wrote");

    let branch = w.branch.clone().expect("a worker gets its own branch (#338), not the lead's");
    assert_ne!(w.cwd, repo.path(), "…and its own worktree, never the human's checkout");
    assert!(
        std::path::Path::new(&w.cwd).is_dir(),
        "the worktree must really be on disk at {} — this is the assertion slice A could not \
         make",
        w.cwd
    );
    let branches = std::process::Command::new("git")
        .current_dir(&repo.repo)
        .args(["branch", "--list", &branch])
        .output()
        .expect("git");
    assert!(
        String::from_utf8_lossy(&branches.stdout).contains(&branch),
        "…and the branch must exist in the repo: {branch}"
    );

    // The helper is an ordinary worker, which is what makes the guardrail table
    // true: it counts, and it is reapable.
    assert!(counts_against_max_agents(w.role), "a helper spends a cap slot");
    assert!(!w.role.is_root(), "…and is nobody's root");
}

/// **A lead cannot open a lead by naming the lead block** — and this slice is
/// what made the case reachable.
///
/// Slice A's design note argued the property from the VOCABULARY: "no block can
/// have kind `Lead` while `kind_from_str` cannot name one". That sentence was
/// true while nothing minted a lead block, and `lead_prepare` is what makes it
/// false — a real group on disk now has one. So the property moves onto the
/// EFFECTIVE-class check, which reads the resolved block rather than the `kind`
/// argument, and this is where it is pinned.
///
/// The `kind: "worker"` argument is what makes this a real case rather than a
/// restating of `a_lead_may_spawn_a_worker_and_nothing_else`: the argument says
/// worker and only the resolved block says otherwise, and a block's kind WINS
/// at `spawn_agent_ex`.
#[test]
fn a_lead_cannot_open_a_lead_by_naming_its_own_block() {
    let (reg, _d, _repo, gid, agent_id, _out) = prepared_lead("claude", 4);
    let lead = reg.agent(&agent_id).expect("the lead entry");
    assert_eq!(
        reg.group(&gid).unwrap().guardrails.block("lead").map(|b| b.kind),
        Some(Role::Lead),
        "the premise: a MINTED group really does hold a block whose kind is Lead — slice A's \
         note said none could exist, and this function is what changed that"
    );
    let c = caller_for(&reg, &lead);
    let before = reg.list_agents(&gid).as_array().unwrap().len();

    let out = q_call(&reg, &c, "spawn_agent", json!({ "kind": "worker", "block": "lead", "task": "t" }));
    assert_eq!(out["isError"], json!(true), "no recursion: {out:?}");
    assert!(
        q_text(&out).contains("kind must be worker"),
        "…refused by the effective-class check, which reads the resolved BLOCK: {}",
        q_text(&out)
    );
    assert!(
        q_text(&out).contains("kind \"lead\""),
        "…and the refusal must say what it resolved to, so the next reader can see WHICH check \
         said no: {}",
        q_text(&out)
    );
    assert_eq!(
        reg.list_agents(&gid).as_array().unwrap().len(),
        before,
        "a refused spawn must leave the roster exactly as it was"
    );
}

/// **A group never ends up with two roots** — and this pins WHICH mechanism
/// keeps that true, because there are two and only one of them fires.
///
/// The invariant matters because `deliver_relayed_to_root` is a `find` over a
/// `HashMap`, whose iteration order is not stable: a group holding two
/// `is_root()` agents would deliver a child's report to whichever came back,
/// with no error on either side and possibly a different answer between runs.
/// Slice A's note names it as the tripwire this slice's mint must close.
///
/// What actually refuses today is `next_group_id`, which picks the first
/// candidate id with no LIVE agent — so a second toggle on a repo whose lead is
/// running opens a SECOND group rather than a second root in one. The explicit
/// check in `lead_prepare` sits under the same `creation` lock as a BACKSTOP
/// and, stated plainly because it is the kind of claim that rots: **no test
/// here can drive it**, since the liveness scan reaches first on every public
/// path. It is defence against a future caller that names an id (the
/// `expect_group` shape `create_orchestration_group` already has) rather than
/// letting the id be chosen.
///
/// So this asserts the PROPERTY, over both groups, rather than a refusal
/// string — and the dead-lead case below is the discriminating half: once the
/// first lead is gone the id IS reused, and the property must still hold.
#[test]
fn a_group_never_ends_up_with_two_roots() {
    let (reg, _d, repo, gid, agent_id, _out) = prepared_lead("claude", 4);
    reg.lead_bind(&agent_id, 9501).unwrap();

    let second = reg
        .lead_prepare("claude", &repo.path(), "l2", 4, false, 5, 0, 5)
        .expect("a second toggle on a live repo opens its own group rather than failing");
    let gid2 = GroupId::parse(second["group_id"].as_str().unwrap()).unwrap();
    assert_ne!(gid2, gid, "…in a DIFFERENT group, which is how the invariant is kept");

    let roots_of = |g: &GroupId| -> Vec<String> {
        reg.list_agents(g)
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["role"] == json!("lead") || a["role"] == json!("orchestrator"))
            .filter(|a| a["status"] != json!("dead"))
            .map(|a| a["id"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    for g in [&gid, &gid2] {
        assert_eq!(roots_of(g).len(), 1, "group {g} must hold exactly one live root");
    }

    // THE DISCRIMINATING HALF. Kill the first lead, then toggle again: the id
    // is now free, so this mint REATTACHES to `gid` rather than taking a third.
    // A build whose mint left the dead root behind — or added a second live one
    // — fails here, and only here.
    reg.on_pty_exit(9501, Some(0), "", 0, true);
    let third = reg
        .lead_prepare("claude", &repo.path(), "l3", 4, false, 5, 0, 5)
        .expect("a dead lead's group id is free again");
    let gid3 = GroupId::parse(third["group_id"].as_str().unwrap()).unwrap();
    assert_eq!(gid3, gid, "the control: a dead group's id really is reused, or nothing above is \
                           testing reattachment");
    assert_eq!(
        roots_of(&gid),
        vec![third["agent_id"].as_str().unwrap().to_string()],
        "…and the reattached group holds exactly the NEW lead: the dead one is not a root, and \
         no second live one was minted beside it"
    );
}

/// **A lead group cannot be resumed.**
///
/// The lead's own pane is a human-launched CLI orrerix never opened and cannot
/// relaunch; its helpers, rejoined into a group whose root is gone, would have
/// no pane to report into at all. That is the restore residual
/// `doc/design/lead-pane.md` records, and the refusal is what the session
/// browser shows in its place.
///
/// **A helper's session is the only one this can be driven with**, and that is
/// a fact about the slice rather than a shortcut: orrerix never learns a lead
/// pane's own session id, because it does not build that pane's command line.
/// So the row here is a dead helper's — which is exactly the row a human would
/// click on in the session browser after their lead pane went away.
///
/// The session is put on the record through `set_session_for_test` plus a real
/// `mark_dead`, which is what persists it: `session_roles` reads `agents.json`,
/// not the in-memory map.
#[test]
fn a_lead_group_cannot_be_resumed() {
    let (reg, _d, _repo, gid, agent_id, _out) = prepared_lead("claude", 4);
    reg.lead_bind(&agent_id, 9601).unwrap();
    let worker = reg.spawn_agent(&gid, Role::Worker, "w", "t", false, None).unwrap();
    let sid = "aaaabbbb-cccc-4ddd-8eee-ffff00001111";
    reg.set_session_for_test(&worker.id, sid);
    reg.mark_dead(&worker.id, Some(0));
    assert!(
        reg.session_roles().iter().any(|r| r.session_id == sid),
        "precondition: the helper's session must be on the persisted record, or the resume below \
         fails for a reason that has nothing to do with lead groups"
    );

    let err = resume_recorded_session(&reg, sid, None, false)
        .expect_err("a lead group's helper must not be rejoined");
    assert!(
        err.starts_with("resume-lead-group:"),
        "…tagged so the frontend can report it specifically: {err}"
    );
    assert!(
        err.contains("no lead to report to"),
        "…and saying why, in terms of what the human would get back: {err}"
    );
    assert!(
        resumeerror_tag_is_known(&err),
        "…and the tag must be one the frontend maps, or the human sees the generic fallback"
    );

    // THE CONTROL. An ordinary orchestration group's helper, built exactly the
    // same way, reaches a DIFFERENT refusal — so the assertions above are about
    // lead groups rather than about a resume path that refuses everything.
    let (reg2, _d2) = test_registry();
    let repo2 = real_repo();
    let og = reg2
        .create_group(
            &repo2.path(),
            Guardrails {
                max_agents: 4,
                agent_cli: "claude".into(),
                blocks: workflow::default_roster(&[
                    (Role::Orchestrator, "claude", ""),
                    (Role::Worker, "claude", ""),
                ]),
                ..Guardrails::default()
            },
        )
        .unwrap();
    assert!(!reg2.is_lead_group(&og.id), "the control group must not be a lead group");
    let ow = reg2.spawn_agent(&og.id, Role::Worker, "w", "t", false, None).unwrap();
    let osid = "22223333-4444-4555-8666-777788889999";
    reg2.set_session_for_test(&ow.id, osid);
    reg2.mark_dead(&ow.id, Some(0));
    let control = resume_recorded_session(&reg2, osid, None, false).err().unwrap_or_default();
    assert!(
        !control.starts_with("resume-lead-group:"),
        "an ordinary group's helper must not hit the lead refusal: {control}"
    );
}

/// The frontend half of the `resume-<tag>:` contract, asserted from the backend
/// side: `src/resumeerror.ts`'s `TAG_KIND` must map the tag this slice adds, or
/// `resumeFailureKind` returns `null` and the human gets "It could not be
/// resumed." instead of the message above.
///
/// A source read rather than a shared constant because the two halves are in
/// different languages; `test/resumeerror.test.ts` pins the mapping's behaviour
/// on the frontend side, and this pins that the two files agree on the string.
fn resumeerror_tag_is_known(err: &str) -> bool {
    let tag = err.split(':').next().unwrap_or_default();
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("src").join("resumeerror.ts"),
    )
    .expect("src/resumeerror.ts must be readable from the test");
    src.contains(&format!("\"{tag}\":"))
}
