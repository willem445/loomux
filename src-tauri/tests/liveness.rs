//! Liveness tests: does the app still answer while a registry lock is held?
//!
//! Plan #1600 §2.2 is why this file exists. Every other guard in this repo is a
//! **shape** scan — `perf_dispatch.rs` reads source and asserts structure — and
//! all four hangs (#1564, #1592, #1595 and the beta6 field report) shipped past
//! a growing wall of them, because a shape scan pins the last incident and none
//! of them can ask *does this still return while something is stuck*. These
//! ask it.
//!
//! Must be an integration test, not a unit test (CLAUDE.md constraint 4 — the
//! Windows test exe needs build.rs's comctl32-v6 manifest). No real agent CLI
//! anywhere (constraint 3): the holds come from
//! `OrchRegistry::hold_lock_for_test`, Phase 0's `#[doc(hidden)]` seam.
//!
//! | id | property | slice |
//! |---|---|---|
//! | L1 | both published reads return while every holdable lock is held; `stale` flips on the clock and clears on a publish | Phase 1 (#1608) |
//!
//! L0 (the negative control as a test of its own) and L3 (the pty writer's pool
//! isolation, with its pool-depth clause) belong to #1612 and land with it;
//! whichever of the two PRs is second APPENDS rather than rewrites. L1 keeps
//! its own in-test control so it is non-vacuous either side of that merge.

use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use serde_json::Value;

use loomux_engine::lockwatch::tracked_lock_names;
use loomux_lib::orchestration::views::{group_view_payload, strip_view_payload, VIEW_STALE_AFTER_MS};
use loomux_lib::orchestration::{Guardrails, OrchRegistry};

/// Long enough that a loaded CI runner never trips it, short enough that a real
/// regression fails the job rather than hanging it. Every use is a "did this
/// make progress at all" question, not a latency measurement — the fix moves
/// the wait from unbounded to nothing, so there is no near-miss to tune around.
/// Same value and same reasoning as `perf_leaflocks.rs`.
const GRACE: Duration = Duration::from_secs(10);

/// How long a parked call is given to *be* parked before the property is
/// probed. Asserted as a NEGATIVE (`recv_timeout` must time out), so this is
/// not a guess about scheduling: a call that has not returned after 300 ms,
/// when its unparked cost is microseconds, is parked at the one place in it
/// that can block.
const SETTLE: Duration = Duration::from_millis(300);

/// Longer than any assertion below needs, so a hold never expires mid-probe and
/// turns a real failure into a flake that reads as a pass.
const HOLD_MS: u64 = 30_000;

/// A hold that is meant to END during the test, for the one assertion whose
/// subject is RECOVERY rather than liveness-under-hold.
const SHORT_HOLD_MS: u64 = 1_500;

fn rails() -> Guardrails {
    Guardrails { max_agents: 4, agent_cli: "claude".into(), ..Guardrails::default() }
}

/// #464: every registry construction here goes through this, so no spawn can
/// write into the developer's REAL `~/.claude/agents` or `~/.copilot/agents`.
fn test_registry() -> (Arc<OrchRegistry>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45993);
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (Arc::new(reg), dir)
}

/// Run `f` on its own thread; report whether it finished within `t`.
///
/// A `false` means the call is still blocked. The thread is left parked
/// deliberately: it is waiting on a lock this test still holds, and the harness
/// exits the process when the run ends.
fn completes_within<T: Send + 'static>(t: Duration, f: impl FnOnce() -> T + Send + 'static) -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(t).is_ok()
}

// ---------- L1: the published reads answer regardless ----------

/// Whether the published snapshot has no groups in it — the state in which both
/// payload builders take an early exit rather than assembling anything.
fn views_snapshot_is_empty(reg: &Arc<OrchRegistry>) -> bool {
    reg.views.load().value.groups.is_empty()
}

/// Split ONE snapshot of the tracked-lock names into the ones Phase 0's seam
/// can hold and the ones it refuses.
///
/// `hold_lock_for_test` knows four names and returns `false` for the rest — a
/// deliberate choice documented at that seam ("a representative handful rather
/// than all 82"). So this test iterates EVERY tracked name, holds the ones it
/// can, and reports the rest as a stated residual rather than silently
/// covering four and reading as covering all of them. Widening the seam widens
/// this test with no edit here.
fn classify(reg: &Arc<OrchRegistry>, names: &[String]) -> (Vec<String>, Vec<String>) {
    let mut holdable = Vec::new();
    let mut refused = Vec::new();
    for name in names {
        // A 1 ms probe: this only asks whether the seam KNOWS the name. The
        // real holds are taken per-lock in the test below.
        if reg.hold_lock_for_test(name, 1) {
            holdable.push(name.clone());
        } else {
            refused.push(name.clone());
        }
    }
    (holdable, refused)
}

#[test]
fn l1_a_published_read_returns_while_every_holdable_registry_lock_is_held() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    // Vacuity guard: the count `tracked_lock_names()` ACTUALLY returns. Phase 0
    // measured 82 (the plan said 85); this is a floor, not a pin, because the
    // number moves whenever a registry field is added and a test that has to be
    // edited for that is a test people edit without reading.
    // ONE read of the process-global registry, reused everywhere below.
    //
    // Reading it twice and comparing the two reads is a race, not a check:
    // `tracked_lock_names()` lists every LIVE tracked lock in the PROCESS, and
    // cargo runs this file's tests on separate threads in one process, so the
    // other test's registry construction lands between two reads. That is
    // exactly how this test first failed (87 vs 86).
    let names: Vec<String> = {
        let mut n: Vec<String> = tracked_lock_names().into_iter().map(str::to_string).collect();
        n.sort();
        n.dedup();
        n
    };
    let total = names.len();
    assert!(
        total >= 80,
        "tracked_lock_names() returned only {total} names — the lock registry stopped \
         registering, so iterating it proves nothing"
    );

    let (holdable, refused) = classify(&reg, &names);
    assert!(
        holdable.len() >= 4,
        "Phase 0's hold seam accepted only {} of {total} tracked locks ({holdable:?}) — it knows \
         four by name, so fewer than that means the seam broke, not that the registry shrank",
        holdable.len()
    );
    // NOT a partition assertion: `classify` walks `names` once, so
    // `holdable + refused == total` holds by construction and could never
    // fail. What CAN fail is the seam accepting a name this snapshot never
    // listed — which would mean the two are reading different registries.
    // `holdable` is BUILT from `names`, so asserting membership in `names`
    // would be a second tautology in place of the first. What CAN fail: the
    // four names Phase 0 documents its seam as knowing must all be holdable.
    // A renamed registry field, or a seam that stopped matching, reddens here
    // instead of quietly shrinking this test's coverage to nothing while its
    // own message still reports a count.
    for expected in ["agents", "groups", "mq_state_lock", "tasks_lock"] {
        assert!(
            holdable.iter().any(|h| h == expected),
            "`{expected}` is documented as holdable by hold_lock_for_test but was refused. \
             Holdable: {holdable:?}. Either the registry field was renamed or the seam broke; \
             either way this test now covers fewer locks than it reports."
        );
    }
    assert!(
        !refused.is_empty(),
        "every one of the {total} tracked locks was holdable, so the residual this test \
         reports is empty — the seam is documented as knowing four names, so this means it \
         stopped refusing rather than that it grew"
    );

    // Publish first (#1625 review N1). Without this the loop probes an EMPTY
    // snapshot: `group_view_payload` takes its `return Value::Null` early exit
    // and `strip_view_payload` its no-groups branch, so the class property is
    // exercised but the POPULATED path never is. One pass makes the strip probe
    // walk a real map and the group probe assemble a real payload, under a hold.
    reg.views.note_view_lease(&g.id);
    reg.views.publish_pass_at(&reg, Instant::now());
    assert!(
        !views_snapshot_is_empty(&reg),
        "setup: the snapshot must be populated, or the probes below take their empty-map \
         early exits and this loop proves less than it reports"
    );

    // THE PROPERTY. For each lock the seam can hold: both published reads must
    // return. Before #1608 each of these was ten (and two) registry
    // acquisitions per tick, so this is exactly L0's shape with the fix in.
    for name in &holdable {
        assert!(
            reg.hold_lock_for_test(name, HOLD_MS),
            "setup: the `{name}` hold must be real, or the assertions below prove nothing"
        );

        // NEGATIVE CONTROL, per lock, and the reason this test is not a
        // tautology: a REGISTRY read must NOT return while `agents` is held.
        // If the seam ever stops actually holding, every `completes_within`
        // below starts passing for the wrong reason and reads exactly like
        // coverage. Only `agents` is probed this way — it is the lock
        // `group_summary` takes first — so the control is asserted against a
        // read whose parking is known, not against every lock's shape.
        //
        // (`tests/liveness.rs` L0 states the same class as a test of its own;
        // it arrives with #1612. This is the in-test control, so L1 is
        // non-vacuous standing alone, before or after that lands.)
        if name == "agents" {
            let probe = reg.clone();
            let gid = g.id.clone();
            assert!(
                !completes_within(SETTLE, move || probe.group_summary(&gid)),
                "SETUP FAILURE, not a pass: `group_summary` returned while `agents` was held, \
                 so the hold is not holding and every assertion in this test is vacuous"
            );
        }

        let probe = reg.clone();
        let gid = g.id.clone();
        assert!(
            completes_within(GRACE, move || {
                group_view_payload(&probe.views.load(), &gid, Instant::now())
            }),
            "orch_group_view's body did not return while `{name}` was held. A polled read must \
             take NO registry lock: that is what stops one long hold parking a blocking-pool \
             thread per poller per tick until write_pty cannot be scheduled (#1600 §1.2)"
        );

        let probe = reg.clone();
        assert!(
            completes_within(GRACE, move || {
                strip_view_payload(&probe.views.load(), Instant::now())
            }),
            "orch_strip_view's body did not return while `{name}` was held (same property, the \
             other poll site)"
        );
    }

    // The residual, stated rather than implied — and printed, so a widening of
    // the seam is visible in the log rather than needing an edit here.
    println!(
        "L1 covered {}/{total} tracked locks: {holdable:?}. NOT holdable by Phase 0's seam \
         (stated residual, not a silent gap): {} names.",
        holdable.len(),
        refused.len()
    );
}

#[test]
fn l1_stale_flips_on_the_clock_while_a_lock_is_held_and_clears_on_the_next_publish() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    // One good publish, then the registry wedges.
    //
    // Stamped in the PAST, and that is what makes the release assertion at
    // the end of this test able to fail (#1625 review B3ii). With
    // `Instant::now()` the un-republished world was ALSO under the 5 s
    // threshold at `recovered`, so `!stale_at(recovered)` held whether or
    // not the republish did anything — the two candidate outcomes did not
    // diverge, and the only way it could ever have moved was a setup pass
    // slower than 5 s, i.e. a flake. Sixty seconds back makes the
    // un-republished world unambiguously stale.
    let published_at = Instant::now()
        .checked_sub(Duration::from_secs(60))
        .expect("the test host has more than 60s of uptime");
    reg.views.note_view_lease_at(&g.id, published_at);
    reg.views.publish_pass_at(&reg, published_at);
    // SHORT, unlike the holds above: this test's last step needs the hold to
    // END so the publisher can recover. Long enough that every assertion
    // before it runs with the lock genuinely held (they are all injected-clock
    // reads, so they take microseconds), short enough to elapse well inside
    // `GRACE`.
    assert!(
        reg.hold_lock_for_test("groups", SHORT_HOLD_MS),
        "setup: the `groups` hold must be real"
    );

    let stale_at = |now: Instant| -> bool {
        let payload = group_view_payload(&reg.views.load(), &g.id, now);
        payload
            .get("meta")
            .and_then(|m| m.get("stale"))
            .and_then(Value::as_bool)
            .expect("meta.stale is a bool")
    };

    // The read still answers with the wedge in place — that is L1 — and what it
    // answers is HONEST about its age. Injected clock rather than a sleep: the
    // property is the threshold, not the wall time.
    assert!(!stale_at(published_at), "a payload just published is not stale");
    assert!(
        stale_at(published_at + Duration::from_millis(VIEW_STALE_AFTER_MS + 1)),
        "past the threshold, a snapshot the publisher can no longer refresh must report stale — \
         a frozen panel that looks live is the disclosure gap #1604 review N3 deferred here"
    );
    assert!(
        stale_at(published_at + Duration::from_secs(3600)),
        "and waiting longer never clears it: the badge is released by EVIDENCE (the next \
         successful store), never by elapsed time"
    );

    // RELEASE ON EVIDENCE — and the publisher is the thread that pays for it.
    //
    // An earlier version of this asserted the republish must NOT park behind
    // `groups`. That was false, and this test caught it: `publish_group_at`
    // calls `compute_group` calls `group_summary`, which takes `agents` and
    // then `groups`. It parks BY DESIGN — being the one thread that waits is
    // the whole shape of Phase 1, and the READS staying live while it waits is
    // what the test above asserts.
    //
    // So what this step asserts is the recovery: the publisher parks, the hold
    // ends, the publish completes on its own, and the badge clears on that
    // evidence rather than on a timer. `SHORT_HOLD_MS` is what makes it
    // observable inside `GRACE` — the hold really does elapse during the call.
    let recovered = Instant::now();

    // The control BRACKETS the republish at one instant, which is the only
    // placement that discriminates: stale at `recovered` before it, not stale
    // at `recovered` after it. Reading it afterwards — as this first did —
    // measures the snapshot the republish has already re-stamped, so it
    // asserts the pre-republish world's staleness while looking at the
    // post-republish one.
    //
    // It can only be true because `published_at` is 60 s back: with
    // `Instant::now()` both worlds sit under the 5 s threshold and neither
    // this control nor the assertion it guards could ever fail.
    assert!(
        stale_at(recovered),
        "control: the snapshot must be stale at `recovered` BEFORE the republish, or the \
         assertion after it is a reading of one outcome rather than a choice between two"
    );

    assert!(
        completes_within(GRACE, {
            let reg = reg.clone();
            let gid = g.id.clone();
            move || reg.views.publish_group_at(&reg, &gid, recovered)
        }),
        "the publisher must RECOVER once the hold ends: it parks behind `groups` by design, \
         and a publish that never completes afterwards would mean the badge can never clear"
    );
    assert!(!stale_at(recovered), "one successful publish is the evidence that clears the badge");
}
