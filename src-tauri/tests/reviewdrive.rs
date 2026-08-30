//! Integration tests for the engine-driven review driver (#1778 S3/S4).
//!
//! Design note: `doc/design/review-driver.md`. The pure core's own properties
//! are pinned inline in `crates/loomux-engine/src/reviewdrive.rs`; what lives
//! here is everything that needs a **crate boundary** or the registry — the
//! tick's wiring, the interception arms, the tools, and the brief rendering.
//!
//! # Why a new file rather than `tests/orchestration.rs`
//!
//! The plan on #1778 named `tests/orchestration.rs`, and its parenthetical says
//! why: CLAUDE.md constraint 4, integration tests rather than unit tests,
//! because a test executable linking the full lib needs the comctl32-v6
//! manifest `build.rs` embeds through `-tests`-scoped link args. A new
//! integration-test *target* satisfies that identically — the reason is the
//! target kind, not the file name. What a new file also avoids is the
//! end-of-file append conflict CLAUDE.md catalogues on that file: every test
//! block there ends `);` + `}`, so two branches appending to a 33k-line file
//! get that tail matched as common context and each side arrives ending
//! mid-assertion. `tests/mergequeue.rs` is the standing precedent for giving a
//! subsystem its own file, and this subsystem has slices still to land.
//! `tests/smoke.rs` is untouched, per that same constraint.
//!
//! No test here spawns a real agent CLI (constraint 3) or a real `git`/`gh`
//! child.

use loomux_lib::orchestration::reviewdrive::{
    self, CiObservation, Counter, Counters, DriveEntry, DriveFacts, DriveLimits, DriveState,
    DriveStep, GateOutcome, HeldReason, LaneFact, WorkerSignal, MAX_REBASE_CEILING,
    MAX_ROUNDS_CEILING,
};
use loomux_lib::orchestration::workflow::{ReviewVerdict, Verdict};

// ── fixtures ────────────────────────────────────────────────────────────────

/// An entry walked to `state` through legal arcs only — a fixture cannot encode
/// a state the machine refuses to reach, and from out here `advance` is the
/// only way in anyway: `DriveEntry::state` is private and `DriveEntry::new`
/// lands in `ci-wait`.
fn entry_at(state: DriveState) -> DriveEntry {
    let mut e = DriveEntry::new(1758, "sess-full", "orch-1", Counters::default(), 1_000);
    match state {
        DriveState::CiWait => {}
        DriveState::ReviewWait => {
            e.advance(DriveState::ReviewWait, None, None, 1_000).unwrap();
        }
        DriveState::FixWait => {
            e.advance(DriveState::FixWait, None, None, 1_000).unwrap();
        }
        DriveState::GateCheck => {
            e.advance(DriveState::ReviewWait, None, None, 1_000).unwrap();
            e.advance(DriveState::GateCheck, None, None, 1_000).unwrap();
        }
        DriveState::Held => {
            e.advance(DriveState::Held, Some(HeldReason::CiLimit), None, 1_000)
                .unwrap();
        }
        DriveState::Satisfied => {
            e.advance(DriveState::ReviewWait, None, None, 1_000).unwrap();
            e.advance(DriveState::GateCheck, None, None, 1_000).unwrap();
            e.advance(DriveState::Satisfied, None, None, 1_000).unwrap();
        }
        DriveState::Cancelled => {
            e.advance(DriveState::Cancelled, None, None, 1_000).unwrap();
        }
    }
    assert_eq!(e.state(), state);
    e
}

fn facts_at(head: &str) -> DriveFacts {
    DriveFacts {
        now_ms: 2_000,
        pr_open: Some(true),
        head: head.to_string(),
        body_digest: Some("d1".to_string()),
        required_lanes: Some(Vec::new()),
        ci: CiObservation::Pending,
        worker: WorkerSignal::Silent,
        gate: GateOutcome::NotEvaluated,
        messaged: false,
    }
}

fn lane_fact(block: &str, v: Option<Verdict>, at_head: &str, digest: &str) -> LaneFact {
    LaneFact {
        block: block.to_string(),
        verdict: v.map(|verdict| ReviewVerdict {
            pr: 1758,
            block: block.to_string(),
            agent_id: "rev-1".into(),
            verdict,
            head: at_head.to_string(),
            body_digest: digest.to_string(),
            summary: String::new(),
            ts_ms: 0,
        }),
    }
}

/// `DriveStep::Advance` into a hold. The engine's own `DriveStep::held` and
/// `DriveStep::spend` are private to that module — deliberately, since a
/// caller outside it applies a step rather than authoring one — so out here the
/// variant is built from its `pub` fields, which is also the more legible form
/// for a test: the assertion names the arc, the reason and the counter.
fn held(reason: HeldReason) -> DriveStep {
    DriveStep::Advance { to: DriveState::Held, held_reason: Some(reason), bump: None }
}

/// `DriveStep::Advance` that takes an arc AND spends a counter.
fn spend(to: DriveState, bump: Counter) -> DriveStep {
    DriveStep::Advance { to, held_reason: None, bump: Some(bump) }
}

/// A `review-wait` drive whose one required lane has recorded `fail` at the
/// live revision — the facts arc 5 acts on.
fn failing_lane_facts(head: &str) -> DriveFacts {
    DriveFacts {
        required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Fail), head, "d1")]),
        ..facts_at(head)
    }
}

// ── the `DriveLimits` seal, exercised from OUTSIDE the crate that defines it ─

/// The corrected claim on `DriveLimits`, **performed rather than asserted in
/// prose** (#1778 S3; rev-final's non-blocking on #1783, carried here).
///
/// That type's doc block used to say a private `_seal` field meant an
/// out-of-range bound "cannot be *spelled* from outside". That was false, and
/// false in the direction that matters: `_seal` blocks a struct **literal**
/// (E0451) and nothing else, while every bound field is `pub` — deliberately,
/// so a caller can read the limits a drive is running against. A caller in
/// another crate can therefore take any clamped value and then assign a wider
/// one into it, which is exactly what the first block below does.
///
/// **It compiles, and that is half the point.** This file is a different crate
/// from `crates/loomux-engine`, so if the seal really did make an out-of-range
/// bound unspellable from outside, this test would not build — the claim it
/// replaces was checkable all along, in the one place nobody looked.
///
/// What is true is stronger, and it is about *reach* rather than spelling: an
/// out-of-range bound **cannot reach a decision**, because `decide` clamps
/// unconditionally and shadows its own binding, so no read below that line sees
/// the argument as passed. The engine's own
/// `a_repo_cannot_raise_invariant_9_by_handing_decide_a_wider_bound` pins that
/// from *inside* the module, where `..DriveLimits::default()` can build the raw
/// value directly. This pins the same property across the crate boundary a real
/// caller sits on, by the only route a real caller has.
///
/// The third block is the negative control: one under each ceiling still hands
/// back, so the two holds above are the clamp biting rather than `decide`
/// refusing everything it is handed.
#[test]
fn a_wider_bound_can_be_spelled_from_another_crate_and_still_cannot_reach_a_decision() {
    // 1. It CAN be spelled — not as a literal (the seal really does stop that)
    //    but as a post-construction write to a `pub` field, from out here.
    let mut wide = DriveLimits::default();
    wide.max_review_rounds = 9;
    wide.max_ci_attempts = 9;
    wide.max_rebase_attempts = 9;
    assert_eq!(wide.max_review_rounds, 9, "the fixture really is over-bound");
    assert_eq!(wide.max_rebase_attempts, 9);

    // 2. It cannot reach a decision. At the ceiling, a further `fail` PARKS.
    let mut e = entry_at(DriveState::ReviewWait);
    e.head = "head-a".into();
    e.counters.review_rounds = MAX_ROUNDS_CEILING;
    assert_eq!(
        reviewdrive::decide(&e, &failing_lane_facts("head-a"), &wide),
        held(HeldReason::ReviewLimit),
        "a caller outside the engine must not buy a fourth review round"
    );

    let mut r = entry_at(DriveState::CiWait);
    r.head = "head-a".into();
    r.counters.rebase_attempts = MAX_REBASE_CEILING;
    let conflict = DriveFacts { ci: CiObservation::Conflicting, ..facts_at("head-a") };
    assert_eq!(
        reviewdrive::decide(&r, &conflict, &wide),
        held(HeldReason::RebaseLimit),
        "nor a second rebase hand-back"
    );

    // 3. The negative control: one under the ceiling still hands back, so the
    //    two holds above are the clamp and not a blanket refusal.
    let mut ok = entry_at(DriveState::ReviewWait);
    ok.head = "head-a".into();
    ok.counters.review_rounds = MAX_ROUNDS_CEILING - 1;
    assert_eq!(
        reviewdrive::decide(&ok, &failing_lane_facts("head-a"), &wide),
        spend(DriveState::FixWait, Counter::ReviewRounds)
    );
}
