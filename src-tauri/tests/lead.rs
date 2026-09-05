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
    counts_against_max_agents, spawn_opens_minimized, AgentEntry, Caller, Delivery, GroupId,
    Guardrails, OrchRegistry, Role,
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
