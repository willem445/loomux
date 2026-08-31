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

// ── §3.1's never-merges scan ────────────────────────────────────────────────

/// The three files that are the review driver, and the whole of it.
///
/// **A file scope rather than a name scope, and that is the point.** CLAUDE.md's
/// source-scanning-guard convention forbids deciding from a binding's name, and
/// the design note says so about this scan in particular: "any scope keyed on a
/// name — a module, an `rd_*` prefix — is stepped over by a landing verb added
/// in a function that does not carry it". A file is not a name; the driver's
/// registry wiring was moved into `rdtick.rs` precisely so this list could be
/// files rather than a prefix.
const DRIVER_FILES: [&str; 3] = [
    "../crates/loomux-engine/src/reviewdrive.rs",
    "../crates/loomux-engine/src/rddrive.rs",
    "src/orchestration/rdtick.rs",
];

/// Argv tokens that can only appear in this scope if the driver is building
/// something §3.1's closed list forbids — matched as **whole quoted literals**,
/// so `merge_gate`, `mergeq` and `merge-base` are not hits and a real `"merge"`
/// argument is.
///
/// One row per §3.1 item, and the reason is the item:
///
/// - item 1, merge or any landing verb: `merge`, `push`, `ready`, the branch
///   delete;
/// - item 3, relabel or edit an issue or a PR: `edit`, `close`, `reopen`, the
///   two label flags, and `--body`;
/// - items 2 and 7, the two directories a grant and a verdict live in.
///
/// `api` is here and is not in the note's list: `gh api` is how every verb above
/// is spelled when its subcommand is inconvenient, so a scan that denied the
/// subcommands and allowed the escape hatch would deny nothing.
const FORBIDDEN_LITERALS: [(&str, &str); 13] = [
    ("merge", "item 1: no `gh pr merge`"),
    ("push", "item 1: no `git push` to any ref"),
    ("ready", "item 1: no `gh pr ready`"),
    ("--delete-branch", "item 1: no branch delete"),
    ("edit", "item 3: no `gh pr edit` / `gh issue edit`, bodies included"),
    ("close", "item 3: the driver never closes an issue or a PR"),
    ("reopen", "item 3: nor reopens one"),
    ("--add-label", "item 3: no relabelling"),
    ("--remove-label", "item 3: nor un-labelling"),
    ("--body", "item 3: no body the driver authored reaches a PR"),
    ("api", "items 1 and 3: `gh api` is how each of the above is spelled otherwise"),
    ("merge_grants", "item 2: the driver never writes a merge grant"),
    ("verdicts", "item 7: only a reviewer's `review_verdict` opens the gate"),
];

/// Identifiers whose presence in this scope IS the forbidden capability,
/// whatever the surrounding argv looks like.
///
/// Not argv tokens — the registry's own functions — so they are matched as
/// identifiers rather than as quoted literals. `grant_merge` is the sharpest:
/// §3.1 item 2 says it is a `pub fn` on the shared registry that takes an
/// unvalidated `actor` with no role gate near it, so "human-only" holds today by
/// the absence of a wired MCP arm rather than by a barrier. The driver is
/// backend Rust in the same crate, and nothing else stops it. This is the
/// barrier.
const FORBIDDEN_IDENTS: [(&str, &str); 5] = [
    ("grant_merge", "item 2: no barrier exists on that function; this is the barrier"),
    ("kill_agent", "item 5: a quiet lane becomes held(lane-stalled), never dead"),
    ("reap_idle_agents", "item 5: the reaper is not the driver's to call"),
    ("record_verdict", "item 7: the driver reads verdicts and can never write one"),
    ("queue_merge", "§8.1: a driven PR may not be queued, and not by the driver"),
];

/// Hits that are argued rather than forbidden, each carrying the reason it is
/// not what the scan is looking for: `(file suffix, token, why)`.
///
/// **Default-deny** — anything not on this list fails — and a row that stops
/// matching anything fails too, because a stale exemption is an exemption nobody
/// re-checked. Empty today, and the mechanism is here for the first row that
/// needs it rather than added when one does.
const ALLOWED: [(&str, &str, &str); 0] = [];

/// One driver file as the scan reads it: **production source only**.
///
/// Two things are removed, and each removal is a stated blind spot rather than a
/// convenience. `#[cfg(test)]` onward is cut, because a test in one of these
/// files may legitimately build a landing verb — one deliberately does, to prove
/// the `GitDenied` bridge REFUSES `git push`, and a scan that fired on it would
/// be a scan the fix for is to delete the proof. Line comments are cut, because
/// the design note is quoted at length in these files and a `///` block naming
/// `queue_merge` or "merge" is prose, not a capability.
///
/// The comment cut is textual and is fooled by a `//` inside a string literal on
/// a line with an odd number of quotes before it. No such line exists here, and
/// the literal floor asserted below is what would notice if the cut ever started
/// eating real code.
fn driver_production_source(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let src = match src.find("\n#[cfg(test)]") {
        Some(i) => src[..i].to_string(),
        None => src,
    };
    src.lines()
        .map(|line| {
            let mut quotes = 0usize;
            let b: Vec<char> = line.chars().collect();
            for k in 0..b.len() {
                if b[k] == '"' && (k == 0 || b[k - 1] != '\\') {
                    quotes += 1;
                }
                if b[k] == '/' && b.get(k + 1) == Some(&'/') && quotes % 2 == 0 {
                    return b[..k].iter().collect::<String>();
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every whole double-quoted literal in `src`, without parsing Rust.
fn quoted_literals(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut prev = '\0';
    for c in src.chars() {
        match (&mut cur, c) {
            (Some(buf), '"') if prev != '\\' => {
                out.push(std::mem::take(buf));
                cur = None;
            }
            (Some(buf), _) => buf.push(c),
            (None, '"') => cur = Some(String::new()),
            _ => {}
        }
        prev = if prev == '\\' { '\0' } else { c };
    }
    out
}

/// §3.1 item 1's enforcement, prescribed on this slice: **the driver may never
/// build a merge or any other landing verb** — plus items 2, 3, 5 and 7, which
/// the note folds into the same scan because they are argv-shaped the same way.
///
/// **Default-deny over a file scope, decided on the shape.** A forbidden argv
/// token is matched as a whole quoted literal, so `merge_gate`, `mergeq` and
/// `merge-base` are not hits and a real `"merge"` argument is; a forbidden
/// capability is matched as an identifier. Nothing is keyed on a function's
/// name, which is the axis a rename steps over.
///
/// # The residual, stated because a scan must state one
///
/// **A landing verb the driver reaches through a shared helper it does not
/// own.** This scan reads three files; a registry method called from one of them
/// that itself pushes is invisible to it.
///
/// Half of that residual is closed structurally rather than by scanning:
/// `rddrive::RdRunner` has a `gh` method and **no `git`**, so a driver holding
/// one cannot reach `git push` at all, whatever a later author writes — the
/// compiler enforces it, not this test — and the single bridge to the wider
/// trait answers `git` with a refusal
/// (`the_drivers_git_is_a_refusal_rather_than_an_absence`, in that module). The
/// other half — a `gh` write reached through a shared helper — is closed by
/// nothing here.
///
/// Three further blind spots, named rather than left to be discovered: a verb
/// assembled from fragments (`"mer".to_string() + "ge"`), a verb read from a
/// file or a config value at runtime, and a macro that expands to one. None
/// appears today. The scan is textual, and this paragraph is its limit.
#[test]
fn the_driver_never_builds_a_landing_verb_and_never_grants_a_merge() {
    let mut unmatched_allow: Vec<&str> = ALLOWED.iter().map(|(_, l, _)| *l).collect();
    let mut findings: Vec<String> = Vec::new();
    let mut scanned_literals = 0usize;
    let mut verified_files = 0usize;
    for rel in DRIVER_FILES {
        let src = driver_production_source(rel);
        let lits = quoted_literals(&src);
        // The population control. A scan that read nothing reports clean, which
        // is byte-identical to a scan that found nothing — and this one has two
        // ways to read nothing: a path that stopped resolving, and a comment cut
        // that started eating code. Every one of these files carries argv,
        // audit-detail and template-key literals in its production half.
        assert!(
            lits.len() >= 20,
            "{rel}: only {} production literals extracted — the scan read (almost) nothing, \
             which is not the same as finding nothing",
            lits.len()
        );
        scanned_literals += lits.len();
        verified_files += 1;
        for (bad, why) in FORBIDDEN_LITERALS {
            if lits.iter().any(|l| l == bad) {
                if let Some(i) = ALLOWED.iter().position(|(f, l, _)| rel.ends_with(f) && *l == bad)
                {
                    unmatched_allow.retain(|l| *l != ALLOWED[i].1);
                    continue;
                }
                findings.push(format!("{rel}: builds the argv token {bad:?} — {why}"));
            }
        }
        for (bad, why) in FORBIDDEN_IDENTS {
            if src.contains(bad) {
                if let Some(i) = ALLOWED.iter().position(|(f, l, _)| rel.ends_with(f) && *l == bad)
                {
                    unmatched_allow.retain(|l| *l != ALLOWED[i].1);
                    continue;
                }
                findings.push(format!("{rel}: names {bad} — {why}"));
            }
        }
    }
    // A count at the VERIFIED site, not the match site: "three files listed" is
    // not "three files read".
    assert_eq!(
        verified_files,
        DRIVER_FILES.len(),
        "every file in scope must have been read, not merely listed"
    );
    assert!(scanned_literals > 60, "only {scanned_literals} literals across three files");
    assert!(findings.is_empty(), "review-driver.md §3.1:\n  {}", findings.join("\n  "));
    assert!(
        unmatched_allow.is_empty(),
        "stale exemptions — these rows match nothing any more, so they are exemptions nobody \
         re-checked: {unmatched_allow:?}"
    );
}

/// The positive control for the scan above, and it is the one the #1395 census
/// bullet asks for: the harness is itself an instrument, so prove its extraction
/// against a subject known to FAIL before reading its clean report.
///
/// Without this, `quoted_literals` could return an empty vector for every input
/// — an escape-handling slip, a comment cut that ate the file — and the scan
/// would read green over a driver that pushed on every tick. The second half is
/// the near-miss control: the driver's real source is full of `merge_gate`,
/// `mergeq` and `merge_queue.json`, and a scan that fired on those would be
/// deleted within a day.
#[test]
fn the_landing_verb_scan_really_fires_on_a_landing_verb() {
    let hostile = r##"
        fn land(r: &dyn RdRunner) {
            let _ = r.gh(&["pr", "merge", "--delete-branch"]);
            reg.grant_merge(group, pr, "human");
        }
    "##;
    let lits = quoted_literals(hostile);
    assert!(lits.contains(&"merge".to_string()), "extraction missed a whole literal: {lits:?}");
    assert!(lits.contains(&"--delete-branch".to_string()));
    assert!(hostile.contains("grant_merge"));

    let benign = r##"
        let gate = self.merge_gate(group);
        let spec = mergeq::GateSpec::Absent;
        let _ = "merge-base";
        let _ = "merge_queue.json";
    "##;
    let benign_lits = quoted_literals(benign);
    assert!(
        !benign_lits.iter().any(|l| l == "merge"),
        "the scan would fire on `merge-base` / `merge_queue.json`: {benign_lits:?}"
    );
    assert!(!benign.contains("grant_merge"));
}

/// The comment cut is a removal, and a removal is a place a scan can go blind —
/// so it gets its own control rather than being trusted.
///
/// A `///` block quoting the design note must not be a hit (that is why the cut
/// exists) while the identical text in *code* still must be.
#[test]
fn the_comment_cut_removes_prose_and_not_code() {
    let src = "/// comes back through a fresh `queue_merge` as a NEW entry\nlet x = 1; // and \"merge\"\nlet y = \"merge\";\n";
    let cut = {
        // The same transformation `driver_production_source` applies, on a
        // string rather than a file — the function itself takes a path, and the
        // point here is the transformation.
        src.lines()
            .map(|line| match line.find("//") {
                Some(k) if line[..k].matches('"').count() % 2 == 0 => line[..k].to_string(),
                _ => line.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(!cut.contains("queue_merge"), "a doc block naming a forbidden ident is prose: {cut:?}");
    assert!(
        quoted_literals(&cut).contains(&"merge".to_string()),
        "…and a real literal survives the cut: {cut:?}"
    );
}
