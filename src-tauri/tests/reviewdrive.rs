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
use loomux_lib::orchestration::mqdriver::CmdOut;
use loomux_lib::orchestration::rddrive::RdRunner;
use loomux_lib::orchestration::{GroupId, Guardrails, OrchRegistry, RdDriveReport};

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

// ── the tick, through the registry ──────────────────────────────────────────

/// Build a registry against `dir` with every test-only directory override
/// applied — see `orchestration.rs`'s `relaunch_registry` (same rationale,
/// duplicated because these are separate integration-test binaries): a second
/// `OrchRegistry::new` built directly, without reapplying these overrides,
/// falls through to the REAL `~/.claude/agents`/`~/.copilot/agents` on the next
/// spawn (#464).
fn relaunch_registry(dir: &std::path::Path) -> OrchRegistry {
    let reg = OrchRegistry::new(dir.to_path_buf());
    reg.set_port(45999);
    reg.set_claude_agents_dir_override(dir.join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.join("copilot-hooks"));
    reg
}

fn rails() -> Guardrails {
    Guardrails {
        max_agents: 6,
        agent_cli: "claude".into(),
        auto_ops: false,
        advanced_orchestrator: true,
        ..Guardrails::default()
    }
}

const WORKFLOW: &str = r#"version: 1
blocks:
  - id: worker
    kind: worker
  - id: rev-std
    name: Standard review
    kind: reviewer
gates:
  merge:
    require: all-pass
    reviewers: [rev-std]
    routing:
      - paths: [src/**]
        reviewers: [rev-std]
"#;

/// A throwaway repo one level below its own temp root — `orchestration.rs`'s
/// `RealRepo` rationale: a worktree is cut SIBLING to the repo, so nesting keeps
/// it inside the root that `Drop` reclaims.
struct Repo {
    _root: tempfile::TempDir,
    repo: std::path::PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let path = repo.to_string_lossy().replace('\\', "/");
        // Written through the reader's own path resolution rather than a
        // hard-coded directory name, so a rename of the config directory cannot
        // leave this fixture writing where nothing reads.
        let wf = loomux_lib::orchestration::workflow::workflow_file(&path);
        std::fs::create_dir_all(wf.parent().unwrap()).unwrap();
        std::fs::write(&wf, WORKFLOW).unwrap();
        Repo { _root: root, repo }
    }
    fn path(&self) -> String {
        self.repo.to_string_lossy().replace('\\', "/")
    }
}

const HEAD_A: &str = "aa11bb22cc33dd44ee55ff6677889900aabbccdd";
const HEAD_B: &str = "bb22cc33dd44ee55ff6677889900aabbccddeeff";

/// A canned `gh`, keyed on the SUBCOMMAND rather than on call order, so a test
/// asserts what the driver concluded and not the sequence it happened to read
/// in — the order is the tick's business and is pinned in `rddrive`'s own tests.
struct FakeGh {
    facts: std::sync::Mutex<Result<String, String>>,
    checks: String,
    calls: std::sync::Mutex<Vec<Vec<String>>>,
}

impl FakeGh {
    fn green(head: &str) -> FakeGh {
        FakeGh {
            facts: std::sync::Mutex::new(Ok(facts_json("OPEN", head))),
            checks: r#"[{"name":"build","state":"SUCCESS","link":"x"}]"#.to_string(),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }
    /// The seam itself failing — `gh` missing, or a child killed at the command
    /// timeout. Not a `gh` refusal, and not a fact about the PR.
    fn seam_down(&self) {
        *self.facts.lock().unwrap_or_else(|e| e.into_inner()) = Err("gh-not-found".into());
    }
    fn set_facts(&self, state: &str, head: &str) {
        *self.facts.lock().unwrap_or_else(|e| e.into_inner()) = Ok(facts_json(state, head));
    }
    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn facts_json(state: &str, head: &str) -> String {
    format!(
        r#"{{"state":"{state}","headRefOid":"{head}","baseRefName":"main","body":"b",
             "mergeStateStatus":"CLEAN","additions":1,"deletions":1}}"#
    )
}

impl RdRunner for FakeGh {
    fn gh(&self, args: &[&str]) -> Result<CmdOut, String> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(args.iter().map(|s| s.to_string()).collect());
        let checks = args.iter().any(|a| *a == "checks");
        let out = |s: &str| Ok(CmdOut { code: Some(0), stdout: s.to_string(), stderr: String::new() });
        if checks {
            return out(&self.checks);
        }
        match &*self.facts.lock().unwrap_or_else(|e| e.into_inner()) {
            Ok(s) => out(s),
            Err(e) => Err(e.clone()),
        }
    }
}

/// A registry with a group, the driver enabled, and one drive started on PR 1758
/// through the real `drive_review`.
fn driven(
    reg: &OrchRegistry,
    repo: &Repo,
    gh: &FakeGh,
) -> (GroupId, String) {
    let group = reg.create_group(&repo.path(), rails()).unwrap();
    reg.set_rd_policy_override(Some((true, DriveLimits::default())));
    // A full, well-shaped session id this roster never recorded takes
    // `resolve_session_ref`'s passthrough arm and is accepted — §5.1 says so,
    // and says the deferral is deliberate: resolving is not proving resumable.
    let session = "cafb930d-1111-2222-3333-444444444444".to_string();
    let out = reg.drive_review_with(&group, gh, 1758, &session, false, 0, "orch-1");
    assert_eq!(out["driving"], serde_json::json!(true), "drive_review refused: {out}");
    (group, session)
}

fn status_head(reg: &OrchRegistry, group: &GroupId) -> String {
    let s = reg.review_drive_status(group);
    s["drives"][0]["head"].as_str().unwrap_or_default().to_string()
}

fn status_state(reg: &OrchRegistry, group: &GroupId) -> String {
    let s = reg.review_drive_status(group);
    s["drives"][0]["state"].as_str().unwrap_or_default().to_string()
}

/// **THE hazard**, named by two reviewers on S1 as the line that would be
/// forgotten, and pinned here in both directions.
///
/// `DriveEntry::head` is only ever *compared* against the live head — arc 6 in
/// `review-wait`, arc 7 in `fix-wait` — so:
///
/// - **A tick that resolves the head must persist it.** Recording it once at
///   `drive_review` time and never again makes that comparison permanently true:
///   the drive takes arc 6 to `ci-wait`, goes green, comes back to
///   `review-wait`, takes arc 6 again, forever. Nothing goes red in the engine
///   crate for that, because the defect is emergent at the seam.
/// - **A tick whose head read FAILED must not write an empty head.** Unknown is
///   not a value — the same class as `fix_handback_ms == 0` meaning "ancient"
///   rather than "unset". A stored `""` makes `lane_open_for` refuse every
///   record briefed at a real head, so `review-wait` yields `OpenLane{k}` on
///   every tick: a reviewer spawned per tick, each brief re-arming `spawned_ms`
///   so `lane-stalled` can never fire.
///
/// The first half is its own control for the second: the head demonstrably MOVES
/// from empty to `HEAD_A` before the failed read, so "unchanged" afterwards is a
/// statement about the guard rather than about a field nothing ever wrote.
#[test]
fn a_tick_persists_the_head_it_resolved_and_never_writes_a_head_it_could_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let reg = relaunch_registry(dir.path());
    let repo = Repo::new();
    let gh = FakeGh::green(HEAD_A);
    let (group, _session) = driven(&reg, &repo, &gh);

    // A fresh entry has no head: §5.2's `head` is a record of what was SEEN, and
    // nothing has been seen yet.
    assert_eq!(status_head(&reg, &group), "", "a fresh drive has resolved no head");

    // One tick with the head resolvable. It must land in the file.
    reg.rd_drive_group_with(&group, &gh, 10_000);
    assert_eq!(
        status_head(&reg, &group),
        HEAD_A,
        "the tick resolved a head and did not persist it — every later arc-6 \
         comparison is now permanently true"
    );
    assert_eq!(status_state(&reg, &group), "review-wait", "green CI is arc 2");

    // Now the seam fails. `observe_pr` yields an empty head, `decide` refuses to
    // dispatch on one, and the ENTRY must come through untouched.
    gh.seam_down();
    reg.rd_drive_group_with(&group, &gh, 20_000);
    assert_eq!(
        status_head(&reg, &group),
        HEAD_A,
        "a failed head read was written as an empty head — unknown is not a value"
    );
    assert_eq!(status_state(&reg, &group), "review-wait", "and nothing advanced on it");
}

/// **A state pays only for the facts it reads.** `decide` reads the routed lane
/// list in `review-wait` and `gate-check` and nowhere else, so `ci-wait` and
/// `fix-wait` must not spend the `gh pr view --json files` call that resolves
/// it — on the loop that also delivers every `notify_when` notice in the fleet.
///
/// The fixture's gate DECLARES routing, which is what makes this discriminating:
/// with no `routing:` key `route_reviewers` never looks at the file list and the
/// call is skipped for every state, so the assertion would hold under an
/// implementation that resolved the lanes unconditionally. The counts below are
/// the whole pin — two calls in `ci-wait` (the PR's facts, then its checks),
/// three once the drive is in `review-wait` (those two plus the changed files).
#[test]
fn a_state_spends_gh_calls_only_on_the_facts_it_reads() {
    let dir = tempfile::tempdir().unwrap();
    let reg = relaunch_registry(dir.path());
    let repo = Repo::new();
    let gh = FakeGh::green(HEAD_A);
    let (group, _session) = driven(&reg, &repo, &gh);
    let base = gh.calls().len();

    // Tick 1: the entry is in `ci-wait`, which reads neither the lane list nor
    // the gate.
    reg.rd_drive_group_with(&group, &gh, 10_000);
    let ci_wait_calls: Vec<String> =
        gh.calls()[base..].iter().map(|c| c.join(" ")).collect();
    assert_eq!(
        ci_wait_calls.len(),
        2,
        "ci-wait must spend the PR facts and its checks and nothing else: {ci_wait_calls:?}"
    );
    assert!(
        !ci_wait_calls.iter().any(|c| c.contains("files")),
        "…and in particular not the routing read: {ci_wait_calls:?}"
    );
    assert_eq!(status_state(&reg, &group), "review-wait");

    // Tick 2: now in `review-wait`, which DOES read the lane list — so the same
    // gate now costs the third call. That is the control: the absence above is
    // the state, not the fixture.
    let mark = gh.calls().len();
    reg.rd_drive_group_with(&group, &gh, 20_000);
    let review_calls: Vec<String> = gh.calls()[mark..].iter().map(|c| c.join(" ")).collect();
    assert!(
        review_calls.iter().any(|c| c.contains("files")),
        "review-wait must resolve the routed lane list: {review_calls:?}"
    );
}

/// The other half of the same field: when the head really does move, the entry
/// follows it — arc 6, and the reason the write above is placed AFTER `decide`
/// rather than before it.
///
/// Writing the head first would make `entry.head != facts.head` false before
/// anything compared them, so arc 6 would be unreachable rather than permanent —
/// the opposite failure, and equally invisible to the engine crate.
#[test]
fn a_head_that_moves_under_a_lane_re_enters_ci_wait_and_the_entry_follows() {
    let dir = tempfile::tempdir().unwrap();
    let reg = relaunch_registry(dir.path());
    let repo = Repo::new();
    let gh = FakeGh::green(HEAD_A);
    let (group, _session) = driven(&reg, &repo, &gh);

    reg.rd_drive_group_with(&group, &gh, 10_000);
    assert_eq!(status_state(&reg, &group), "review-wait");
    assert_eq!(status_head(&reg, &group), HEAD_A);

    // The worker pushed while the lane was mid-review (§8's own row).
    gh.set_facts("OPEN", HEAD_B);
    reg.rd_drive_group_with(&group, &gh, 20_000);
    assert_eq!(status_state(&reg, &group), "ci-wait", "arc 6: the head moved under a lane");
    assert_eq!(status_head(&reg, &group), HEAD_B, "and the entry follows the head it saw");
}

/// §5.3's opt-in, checked at the seam a test uses rather than only at the group
/// selector — "a seam that skipped the product's own opt-in would be testing
/// something the product cannot do".
///
/// The control is that the identical call with the driver ON does move the
/// drive, so the assertion below is the opt-in and not a tick that does nothing.
#[test]
fn a_disabled_driver_is_unreachable_even_through_the_test_seam() {
    let dir = tempfile::tempdir().unwrap();
    let reg = relaunch_registry(dir.path());
    let repo = Repo::new();
    let gh = FakeGh::green(HEAD_A);
    let (group, _session) = driven(&reg, &repo, &gh);

    reg.set_rd_policy_override(Some((false, DriveLimits::default())));
    let report = reg.rd_drive_group_with(&group, &gh, 10_000);
    assert_eq!(report, RdDriveReport::default(), "a disabled driver does nothing at all");
    assert_eq!(status_head(&reg, &group), "", "…and reads nothing: no head was resolved");
    assert!(gh.calls().is_empty(), "…and spends no `gh` call: {:?}", gh.calls());

    // The control.
    reg.set_rd_policy_override(Some((true, DriveLimits::default())));
    reg.rd_drive_group_with(&group, &gh, 10_000);
    assert_eq!(status_head(&reg, &group), HEAD_A, "the same call with the driver on does move it");
}

/// §8.1's mutual refusal, direction 1, **with colliding operands**: one PR
/// number, refused by the queue because the driver holds it.
///
/// A non-interference pin whose two operands never meet holds under every
/// implementation, the symmetric one included — so the PR queued here is the PR
/// driven above, and the control is a DIFFERENT PR getting some other refusal,
/// which is what makes this the driver's hold rather than `queue_merge`
/// refusing everything in a repo with no real remote.
///
/// # The other direction is disclosed rather than faked
///
/// `drive_review`'s `in-merge-queue` arm — the driver refusing a PR the QUEUE
/// holds — is **not pinned here**, and stating that is better than a fixture
/// whose operands never meet. Reaching it needs a live `merge_queue.json` entry,
/// and no surface a test can call seeds one: `queue_merge` is the only writer,
/// it needs a gate this repo has no verdict for, and the group directory these
/// files live in has no public accessor by design (`group_dir` takes a
/// `GroupId` and is private, which is CLAUDE.md constraint 6 working).
///
/// What bounds the residual: that arm is three lines reading
/// `mqloop::load_state(..).entry(pr).map(|e| !e.state().is_terminal())` — the
/// same predicate `already-queued` itself uses and the same one the pure test
/// beside `mqloop::enqueue` exercises from the other side. The unpinned part is
/// the wiring, not the decision.
#[test]
fn a_driven_pr_may_not_be_queued() {
    let dir = tempfile::tempdir().unwrap();
    let reg = relaunch_registry(dir.path());
    let repo = Repo::new();
    let gh = FakeGh::green(HEAD_A);
    let (group, _session) = driven(&reg, &repo, &gh);

    let refused = reg.queue_merge(&group, 1758, None);
    assert_eq!(
        refused["refused"],
        serde_json::json!("in-review-drive"),
        "queue_merge must name the HOLDER: {refused}"
    );
    // The control. A different PR is refused for some other reason, so the
    // refusal above is about THIS PR being driven rather than about
    // `queue_merge` refusing every call in a repo with no remote.
    let other = reg.queue_merge(&group, 1759, None);
    assert_ne!(other["refused"], serde_json::json!("in-review-drive"), "{other}");

    // …and once the drive is gone, so is the refusal: the hold is the DRIVE,
    // not the PR number.
    assert_eq!(
        reg.cancel_review_drive(&group, 1758, "orch-1")["cancelled"],
        serde_json::json!(true)
    );
    let after = reg.queue_merge(&group, 1758, None);
    assert_ne!(after["refused"], serde_json::json!("in-review-drive"), "{after}");
}
