//! Integration tests for the merge-queue **driver** (#581 slice D1).
//!
//! Design note: `doc/design/merge-queue.md`. These pin the two things that note
//! says can only be pinned here:
//!
//! - **§7.5's five refusals**, so the constraint-7 proof is executed rather than
//!   argued: enqueue against the default refused; a batch build whose target
//!   became the default aborted; the adversarial rename refused at landing; a
//!   failed lookup refused rather than defaulted; and the landing refspec
//!   asserted to be exactly `<tested-sha>:refs/heads/<target>`.
//! - **§4/§7.5's argv-level assertions**, on the exact argument vector handed to
//!   `git`/`gh` rather than on any resulting ref. Every way of getting the
//!   create-only scratch push wrong degrades to a *silently successful ordinary
//!   push*, so an outcome-only test passes in exactly the cases the check exists
//!   to prevent. A fake runner is the only way to see the bytes.
//!
//! # Why a new file rather than `tests/orchestration.rs`
//!
//! The note names `tests/orchestration.rs` as the home for the refusal tests.
//! That file is now 33.9k lines and is the serialization point slices A, D and E
//! already contend on; `tests/workflow.rs` is the standing precedent for a
//! per-subsystem integration-test file. Nothing about these tests needs the
//! registry, so they get their own target. `tests/smoke.rs` is untouched —
//! CLAUDE.md constraint 4 needs at least one integration-test target to exist
//! for the Windows comctl32-v6 manifest link args, and this file adds one more
//! rather than replacing it.
//!
//! # No network, no real CLIs
//!
//! CLAUDE.md constraint 3. Every test drives a [`Fake`] runner: it records each
//! argv it is handed and replies from a canned script. No `git`, no `gh`, no
//! remote, and no agent CLI is spawned by anything in this file.

use loomux_lib::orchestration::mergeq::{GateSpec, MERGE_QUEUE_VERSION};
use loomux_lib::orchestration::mqdriver::{
    classify_checks, cleanup_scratch, close_draft_argv, default_branch_argv, delete_scratch_argv,
    land_batch, land_push_argv, ls_remote_argv, mint_scratch, pr_checks_argv, pr_ci_green,
    pr_facts_argv, push_scratch, resolve_and_validate_target, resolve_default_branch, resolve_pr,
    scratch_exists, scratch_push_argv, validate_target, BatchVerification, CmdOut, LandRefusal,
    MintError, MqRunner, TargetRefusal, MINT_ATTEMPTS, REMOTE,
};
use loomux_lib::orchestration::workflow::{
    body_digest, parse_gate_file, BlockId, ReviewVerdict, Verdict,
};
use std::collections::BTreeMap;
use std::sync::Mutex;

// ── the fake runner ─────────────────────────────────────────────────────────

/// One canned reply, matched against the argv by a substring of the joined
/// arguments. Substring rather than exact-match on purpose: a test that had to
/// restate the whole argv to *stub* a call would be asserting the argv twice,
/// and the second copy would silently become the thing under test.
struct Reply {
    matches: &'static str,
    out: CmdOut,
}

fn out(code: i32, stdout: &str, stderr: &str) -> CmdOut {
    CmdOut { code: Some(code), stdout: stdout.to_string(), stderr: stderr.to_string() }
}

/// Records every argv, replies from a script. The whole test seam.
struct Fake {
    git_replies: Vec<Reply>,
    gh_replies: Vec<Reply>,
    /// `("git"|"gh", argv)` in call order.
    seen: Mutex<Vec<(&'static str, Vec<String>)>>,
    /// Commands to fail to *spawn* at all (the `gh-not-found` / `git-not-found`
    /// sentinel path, §10).
    spawn_fails: Vec<&'static str>,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            git_replies: Vec::new(),
            gh_replies: Vec::new(),
            seen: Mutex::new(Vec::new()),
            spawn_fails: Vec::new(),
        }
    }

    fn git(mut self, matches: &'static str, code: i32, stdout: &str, stderr: &str) -> Fake {
        self.git_replies.push(Reply { matches, out: out(code, stdout, stderr) });
        self
    }

    fn gh(mut self, matches: &'static str, code: i32, stdout: &str, stderr: &str) -> Fake {
        self.gh_replies.push(Reply { matches, out: out(code, stdout, stderr) });
        self
    }

    fn spawn_fail(mut self, bin: &'static str) -> Fake {
        self.spawn_fails.push(bin);
        self
    }

    /// Every argv this runner was handed, in call order, joined with spaces.
    fn calls(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(bin, args)| format!("{bin} {}", args.join(" ")))
            .collect()
    }

    /// The raw argv of the Nth call to `bin` — the shape the argv-level
    /// assertions compare against, element by element, never as a joined string
    /// (a joined string cannot tell `a b` from a single argument `"a b"`, and
    /// argument boundaries are half of what these tests are about).
    fn argv(&self, bin: &str, nth: usize) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(b, _)| *b == bin)
            .map(|(_, a)| a.clone())
            .nth(nth)
            .unwrap_or_else(|| panic!("no {bin} call #{nth}; saw {:?}", self.calls()))
    }

    fn reply(
        &self,
        bin: &'static str,
        replies: &[Reply],
        args: &[&str],
    ) -> Result<CmdOut, String> {
        self.seen
            .lock()
            .unwrap()
            .push((bin, args.iter().map(|s| s.to_string()).collect()));
        if self.spawn_fails.contains(&bin) {
            return Err(format!("{bin}-not-found"));
        }
        let joined = args.join(" ");
        for r in replies {
            if joined.contains(r.matches) {
                return Ok(r.out.clone());
            }
        }
        panic!("fake {bin}: no canned reply for {joined:?}");
    }
}

impl MqRunner for Fake {
    fn git(&self, args: &[&str]) -> Result<CmdOut, String> {
        self.reply("git", &self.git_replies, args)
    }
    fn gh(&self, args: &[&str]) -> Result<CmdOut, String> {
        self.reply("gh", &self.gh_replies, args)
    }
}

/// A `gh pr view --json baseRefName,headRefOid,body` reply.
fn pr_json(base: &str, head: &str, body: &str) -> String {
    serde_json::json!({ "baseRefName": base, "headRefOid": head, "body": body }).to_string()
}

/// A `gh pr checks --json state,name,link` reply.
fn checks_json(rows: &[(&str, &str)]) -> String {
    let v: Vec<_> = rows
        .iter()
        .map(|(name, state)| serde_json::json!({ "name": name, "state": state, "link": "" }))
        .collect();
    serde_json::Value::Array(v).to_string()
}

const SCRATCH: &str = "0123456789abcdef0123456789abcdef01234567";
const PR_HEAD: &str = "fedcba9876543210fedcba9876543210fedcba98";

/// A one-reviewer `all-pass` gate, read through the shim's own parser so this
/// file cannot disagree with `workflow.rs` about what a gate file says.
fn one_reviewer_gate() -> GateSpec {
    GateSpec::Declared(parse_gate_file("require all-pass\nreviewer rev-a\n").expect("gate parses"))
}

fn verdict_map(head: &str, body: &str) -> BTreeMap<BlockId, ReviewVerdict> {
    [(
        "rev-a".to_string(),
        ReviewVerdict {
            pr: 612,
            block: "rev-a".into(),
            agent_id: "rev-1".into(),
            verdict: Verdict::Pass,
            head: head.into(),
            body_digest: body_digest(body),
            summary: String::new(),
            ts_ms: 0,
        },
    )]
    .into()
}

// ════════════════════════════════════════════════════════════════════════════
// §7.5 — the five refusals. The constraint-7 proof, executed.
// ════════════════════════════════════════════════════════════════════════════

/// **Refusal 1 (§7.1, enqueue).** A PR whose live base IS the repository
/// default branch is refused, and the refusal happens on the *live* lookups —
/// not on any stored hint.
#[test]
fn refusal_1_enqueue_against_the_default_branch_is_refused() {
    let f = Fake::new()
        .gh("pr view 612", 0, &pr_json("main", PR_HEAD, "b"), "")
        .gh("repo view", 0, "main\n", "");

    assert_eq!(
        resolve_and_validate_target(&f, 612, None, None),
        Err(TargetRefusal::BaseIsDefault)
    );
    assert_eq!(TargetRefusal::BaseIsDefault.code(), "base-is-default");

    // The refusal came from the two lookups the shim makes, live — and the
    // default-branch one passes the repo POSITIONALLY (it passes no repo at
    // all, inferring it from the runner's cwd), never through `-R`, which
    // `gh repo view` does not accept (#294).
    assert_eq!(
        f.argv("gh", 1),
        vec!["repo", "view", "--json", "defaultBranchRef", "--jq", ".defaultBranchRef.name"]
    );
    assert!(!f.calls().iter().any(|c| c.contains("-R")), "calls: {:?}", f.calls());

    // A refusal that misses on a spelling is a refusal that does not exist — so
    // a **fully-qualified default** must still trip it. This is the side the
    // normalization has to work on: `gh` answers short names today, and if it
    // ever answered `refs/heads/main` the constraint-7 comparison must still
    // land rather than quietly stop matching.
    assert_eq!(
        validate_target("main", "refs/heads/main", None, None),
        Err(TargetRefusal::BaseIsDefault)
    );
    assert_eq!(
        validate_target("integration", "refs/heads/integration", None, None),
        Err(TargetRefusal::BaseIsDefault)
    );

    // The other spelling is refused too, but as `base-unverifiable`, and the
    // asymmetry is deliberate rather than an oversight: `base` is the string a
    // refspec is built from, so a `refs/`-qualified one is rejected outright
    // (`refs/heads/refs/heads/main` is not a ref anybody meant), while `default`
    // is only ever compared. Both are refusals; only one of them could ever
    // reach an argument position.
    assert_eq!(
        validate_target("refs/heads/main", "main", None, None),
        Err(TargetRefusal::BaseUnverifiable)
    );
}

/// The recorded target and the caller's assertion are compared, never written,
/// so they normalize the same way the default does. Pinned because
/// `same_branch`'s normalization would otherwise be a claim in a doc comment
/// with no executed case behind it — and a helper whose normalization can never
/// fire is the "a claim is a deliverable" defect, not a spare guard.
#[test]
fn the_recorded_target_and_the_assertion_normalize_the_qualified_spelling() {
    // A target recorded by another build in the qualified spelling still names
    // the same branch, so this is not a retarget.
    assert_eq!(
        validate_target("integration", "main", Some("refs/heads/integration"), None),
        Ok("integration".to_string())
    );
    assert_eq!(
        validate_target("integration", "main", None, Some("refs/heads/integration")),
        Ok("integration".to_string())
    );
    // …and a genuinely different branch still refuses in either spelling.
    assert_eq!(
        validate_target("integration", "main", Some("refs/heads/other"), None),
        Err(TargetRefusal::BaseNotTarget)
    );
    // The value returned is always the plain `base`, never the caller's
    // spelling — the refspec is built from this string.
    assert_eq!(
        validate_target("integration", "main", Some("refs/heads/integration"), Some("integration")),
        Ok("integration".to_string())
    );

    // An empty recorded target means "no target established yet" (§4), not a
    // mismatch: the first successful enqueue is what establishes one.
    assert_eq!(validate_target("integration", "main", Some(""), None), Ok("integration".to_string()));
    // But it never rescues the default-branch refusal.
    assert_eq!(validate_target("main", "main", Some(""), None), Err(TargetRefusal::BaseIsDefault));
}

/// **A remote-qualified default still trips the constraint-7 refusal** (rev-157
/// NB3).
///
/// `usable_for_comparison` accepts `origin/main` — nothing about it is
/// unlandable — so if `same_branch` normalized only `refs/heads/`, a `default`
/// of `origin/main` against a `base` of `main` would **not match**, the refusal
/// would not fire, and the queue would push to the default branch. Exactly the
/// bypass the `refs/heads/` normalization fixed, one spelling over.
///
/// Latent rather than live today — `resolve_default_branch` reads
/// `.defaultBranchRef.name`, always a short name — and pinned here anyway,
/// because "the only producer returns short names" is a discipline the next
/// slice to source `default` from a git read, a config value or a cached target
/// would break silently, with a push to the default branch as the cost. A test
/// is cheaper than that discipline holding forever.
#[test]
fn a_remote_qualified_default_still_trips_the_constraint_7_refusal() {
    for spelling in ["main", "refs/heads/main", "origin/main"] {
        assert_eq!(
            validate_target("main", spelling, None, None),
            Err(TargetRefusal::BaseIsDefault),
            "a default spelled {spelling:?} names the same branch as base \"main\""
        );
    }
    // The same three spellings on the recorded-target and assertion arms.
    for spelling in ["integration", "refs/heads/integration", "origin/integration"] {
        assert_eq!(
            validate_target("integration", "main", Some(spelling), None),
            Ok("integration".to_string())
        );
        assert_eq!(
            validate_target("integration", "main", None, Some(spelling)),
            Ok("integration".to_string())
        );
    }

    // Over-matching is the SAFE direction and this is what it costs: a branch
    // literally named `origin/x` compares equal to `x`, so it is refused as if
    // it were the default. A false refusal is a batch that does not land —
    // recoverable and loud. The opposite error pushes to the default branch.
    assert_eq!(
        validate_target("origin/main", "main", None, None),
        Err(TargetRefusal::BaseIsDefault)
    );

    // `refs/remotes/origin/main` needs no normalization arm: it refuses earlier,
    // as unverifiable, because `landable` rejects a still-`refs/`-prefixed name
    // after the one optional `refs/heads/` strip. Three spellings, two
    // mechanisms, no gap.
    assert_eq!(
        validate_target("main", "refs/remotes/origin/main", None, None),
        Err(TargetRefusal::BaseUnverifiable)
    );
}

/// A **corrupt default** refuses rather than merely failing to match. The
/// `refs/heads/` allowance is the only way the default's check is looser than
/// the base's; everything a branch name cannot contain is still rejected, and it
/// is rejected as `base-unverifiable` rather than being compared. Failing to
/// match a garbage default would be failing to fire the constraint-7 refusal at
/// all — a silent hole, where this is a loud refusal.
#[test]
fn a_corrupt_default_branch_answer_refuses_instead_of_failing_to_match() {
    for corrupt in [
        "main:refs/heads/x",
        "--force",
        "-main",
        "ma in",
        "ma..in",
        "main*",
        "main^",
        "main~1",
        "main\nintegration",
        "HEAD",
        "refs/heads/HEAD",
        "refs/tags/v1",
        "",
        "   ",
    ] {
        assert_eq!(
            validate_target("integration", corrupt, None, None),
            Err(TargetRefusal::BaseUnverifiable),
            "a default of {corrupt:?} is a corrupt answer, not a branch that happens not to match"
        );
    }
    // The two spellings that ARE answers still compare, and still refuse.
    assert_eq!(validate_target("integration", "integration", None, None), Err(TargetRefusal::BaseIsDefault));
    assert_eq!(
        validate_target("integration", "refs/heads/integration", None, None),
        Err(TargetRefusal::BaseIsDefault)
    );
}

/// **Refusal 2 (§7.2, batch build).** A target that was legal at enqueue and has
/// since *become* the repository default aborts the batch. Same function, later
/// moment, different live answer.
#[test]
fn refusal_2_a_target_that_became_the_default_aborts_the_batch_build() {
    // At enqueue: base `integration`, default `main` — allowed, and it is the
    // string the refspec would later be built from.
    let f = Fake::new()
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("repo view", 0, "main\n", "");
    assert_eq!(
        resolve_and_validate_target(&f, 612, None, None).map(|(t, _)| t),
        Ok("integration".to_string())
    );

    // At batch build, the repo default is now `integration` itself (renamed, or
    // the default pointer moved). The batch must abort.
    let f = Fake::new()
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("repo view", 0, "integration\n", "");
    assert_eq!(
        resolve_and_validate_target(&f, 612, Some("integration"), None),
        Err(TargetRefusal::BaseIsDefault),
        "constraint 7 outranks an already-established target"
    );
}

/// **Refusal 3 (§7.3, the adversarial case).** The default branch is renamed to
/// the queue target's name **between batch build and landing**. The landing
/// function re-resolves inside itself, so the batch it was about to fast-forward
/// is refused at the moment of submit — and **nothing is pushed**.
///
/// This is the case a design that resolved the target once, earlier, and carried
/// the string could not catch, which is why §7.3 puts the lookups and the
/// refspec construction in one function.
#[test]
fn refusal_3_the_default_renamed_to_the_target_is_refused_at_the_moment_of_submit() {
    let f = Fake::new()
        // The world at landing time: the default IS now `integration`.
        .gh("repo view", 0, "integration\n", "")
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "");

    let got = land_batch(
        &f,
        SCRATCH,
        // The batch was built against `integration` and recorded it. The record
        // is an assertion, never the authority.
        "integration",
        &[612],
        &one_reviewer_gate(),
        &|_| verdict_map(PR_HEAD, "b"),
    );
    assert_eq!(got, Err(LandRefusal::Target { pr: 612, refusal: TargetRefusal::BaseIsDefault }));

    // The proof that matters: no push of any kind was attempted.
    assert!(
        !f.calls().iter().any(|c| c.starts_with("git push")),
        "a refused landing must push nothing; calls: {:?}",
        f.calls()
    );
}

/// **Refusal 4 (§7.1).** A failed lookup refuses rather than defaulting.
/// Mirrors the shim's `unverifiable-base` posture (`mod.rs:761-763`): unknown is
/// never treated as safe.
///
/// Four independent ways the answer can be missing, because the failure mode
/// this guards against is a *plausible-looking* value flowing onward — which is
/// exactly why `git::default_base_ref` is not the authority here: it falls
/// through to the literal string `HEAD`, a branch-shaped non-answer that would
/// never equal a real base and so would never trip the default-branch refusal.
#[test]
fn refusal_4_a_failed_lookup_refuses_rather_than_defaulting() {
    // (a) `gh repo view` exits non-zero.
    let f = Fake::new()
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("repo view", 1, "", "could not resolve repository");
    assert_eq!(resolve_default_branch(&f), Err(TargetRefusal::BaseUnverifiable));
    assert_eq!(
        resolve_and_validate_target(&f, 612, None, None),
        Err(TargetRefusal::BaseUnverifiable)
    );

    // (b) `gh repo view` succeeds with an empty answer.
    let f = Fake::new()
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("repo view", 0, "\n", "");
    assert_eq!(
        resolve_and_validate_target(&f, 612, None, None),
        Err(TargetRefusal::BaseUnverifiable)
    );

    // (c) `gh pr view` fails, or answers with an unparseable body.
    let f = Fake::new().gh("pr view 612", 1, "", "no pull requests found");
    assert_eq!(resolve_pr(&f, 612), Err(TargetRefusal::BaseUnverifiable));
    let f = Fake::new().gh("pr view 612", 0, "not json", "");
    assert_eq!(resolve_pr(&f, 612), Err(TargetRefusal::BaseUnverifiable));

    // (d) `gh` is not installed at all (§10's `gh-not-found` sentinel).
    let f = Fake::new().spawn_fail("gh");
    assert_eq!(resolve_default_branch(&f), Err(TargetRefusal::BaseUnverifiable));

    // And the non-answers a local-git fallback would produce are refused as
    // values, not merely as failures — `HEAD` being the exact string
    // `git::default_base_ref` degrades to.
    assert_eq!(validate_target("HEAD", "main", None, None), Err(TargetRefusal::BaseUnverifiable));
    assert_eq!(validate_target("integration", "HEAD", None, None), Err(TargetRefusal::BaseUnverifiable));
    assert_eq!(validate_target("", "main", None, None), Err(TargetRefusal::BaseUnverifiable));
    assert_eq!(validate_target("integration", "", None, None), Err(TargetRefusal::BaseUnverifiable));
    assert_eq!(TargetRefusal::BaseUnverifiable.code(), "base-unverifiable");
}

/// **Refusal 5 (§7.5).** The landing push's argv is exactly
/// `git push origin <tested-sha>:refs/heads/<target>` — fast-forward only, the
/// tested SHA, the validated target, and **nothing else**.
///
/// Asserted element by element rather than as a joined string: the absence of
/// `--force` and of a leading `+` on the refspec is the entire §7.4 guarantee,
/// and the Bors invariant (§8) is the claim that the SHA in that refspec is the
/// SHA CI judged.
#[test]
fn refusal_5_the_landing_refspec_is_exactly_the_tested_sha_onto_the_validated_target() {
    let f = Fake::new()
        .gh("repo view", 0, "main\n", "")
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "")
        .git("push", 0, "", "");

    let landed = land_batch(
        &f,
        SCRATCH,
        "integration",
        &[612],
        &one_reviewer_gate(),
        &|_| verdict_map(PR_HEAD, "b"),
    )
    .expect("a green, gated batch onto a non-default target lands");
    assert_eq!(landed.target, "integration");
    assert_eq!(landed.scratch_sha, SCRATCH);

    // THE assertion.
    assert_eq!(
        f.argv("git", 0),
        vec![
            "push".to_string(),
            "origin".to_string(),
            format!("{SCRATCH}:refs/heads/integration"),
        ],
        "the landing verb is a plain fast-forward push of the tested SHA"
    );
    // Restated as properties, so a future edit that keeps the shape but changes
    // the meaning still fails: ff-only, and no default-branch name anywhere.
    let argv = f.argv("git", 0);
    assert!(!argv.iter().any(|a| a.contains("--force")), "never --force (§7.4)");
    assert!(!argv.iter().any(|a| a.starts_with('+')), "never a force refspec");
    assert!(!argv.iter().any(|a| a.contains("--force-with-lease")), "the landing push takes no lease");
    assert!(
        !argv.iter().any(|a| a.contains("main")),
        "no code path builds a landing refspec from the default branch name"
    );
    // And the pure builder agrees with what the call site emitted — one shape,
    // one definition.
    assert_eq!(land_push_argv(SCRATCH, "integration"), argv);
}

// ════════════════════════════════════════════════════════════════════════════
// §4 — the create-only scratch push, asserted at the argv level.
// ════════════════════════════════════════════════════════════════════════════

/// **The trailing colon is the whole mechanism.** `--force-with-lease=<ref>:`
/// with an *empty* expect value means "expect this ref not to exist"; every
/// other spelling degrades to a silently successful ordinary push, which is why
/// this is asserted on the argument vector and not on a resulting ref (§4).
#[test]
fn the_scratch_push_is_create_only_by_primitive_and_the_argv_proves_it() {
    let branch = "loomux/mq/g1-mq-7f3a0000";
    let f = Fake::new().git("push", 0, "", "");
    push_scratch(&f, SCRATCH, branch).expect("a create-only push onto a free ref succeeds");

    let argv = f.argv("git", 0);
    assert_eq!(
        argv,
        vec![
            "push".to_string(),
            format!("--force-with-lease=refs/heads/{branch}:"),
            "origin".to_string(),
            format!("{SCRATCH}:refs/heads/{branch}"),
        ]
    );

    // The lease argument, character for character. Each of these is a real way
    // to write it wrong, and each one degrades to a push that SUCCEEDS.
    let lease = &argv[1];
    assert!(lease.ends_with(':'), "the empty expect value — dropping this colon makes it an ordinary force push");
    assert_eq!(lease, "--force-with-lease=refs/heads/loomux/mq/g1-mq-7f3a0000:");
    assert_ne!(lease, "--force-with-lease", "a bare lease expects the remote-tracking ref, not absence");
    assert_ne!(lease, "--force-with-lease=refs/heads/loomux/mq/g1-mq-7f3a0000");
    assert!(!lease.contains("::"), "the expect value is EMPTY, not a literal");
    // A plain push is not create-only: a leaked ancestor ref fast-forwards
    // silently. So the argv must never be the plain three-token shape.
    assert_ne!(
        argv,
        vec!["push".to_string(), "origin".to_string(), format!("{SCRATCH}:refs/heads/{branch}")],
        "a plain push fast-forwards onto a leaked ANCESTOR ref, silently"
    );
    assert_eq!(scratch_push_argv(SCRATCH, branch), argv);
}

/// The push refuses inputs that would reach an argument position as something
/// other than what they claim to be, before any argv is built.
#[test]
fn the_scratch_push_refuses_a_non_object_sha_and_an_out_of_namespace_branch() {
    let f = Fake::new().git("push", 0, "", "");
    assert!(push_scratch(&f, "--force", "loomux/mq/g1-mq-1").is_err());
    assert!(push_scratch(&f, "not-hex-at-all", "loomux/mq/g1-mq-1").is_err());
    assert!(push_scratch(&f, "abc", "loomux/mq/g1-mq-1").is_err(), "too short to be an object name");
    assert!(push_scratch(&f, SCRATCH, "main").is_err(), "outside loomux/mq/* (§11.4)");
    assert!(push_scratch(&f, SCRATCH, "refs/heads/main").is_err());
    // None of those reached `git` at all.
    assert!(f.calls().is_empty(), "refused inputs must not spawn anything; calls: {:?}", f.calls());
}

/// §4: refuse to mint onto an existing scratch ref, bounded at three attempts,
/// and **never delete to make room**.
#[test]
fn minting_refuses_an_existing_scratch_ref_is_bounded_and_never_deletes() {
    // Happy path: the remote does not have it (`ls-remote --exit-code` exits 2).
    let f = Fake::new().git("ls-remote", 2, "", "");
    let m = mint_scratch(&f, "g1", 1_700_000_000_000).expect("a free name mints");
    assert_eq!(m.attempts, 1);
    assert!(m.branch.starts_with("loomux/mq/g1-mq-"), "{}", m.branch);
    assert_eq!(ls_remote_argv(&m.branch), f.argv("git", 0));
    assert_eq!(f.argv("git", 0)[0], "ls-remote");
    assert_eq!(f.argv("git", 0)[1], "--exit-code");

    // Every candidate already exists → bounded failure, loudly.
    let f = Fake::new().git("ls-remote", 0, "deadbeef\trefs/heads/loomux/mq/x\n", "");
    assert_eq!(
        mint_scratch(&f, "g1", 1_700_000_000_000),
        Err(MintError::Collision { attempts: MINT_ATTEMPTS })
    );
    assert_eq!(f.calls().len(), MINT_ATTEMPTS, "bounded at {MINT_ATTEMPTS}, no retry loop");
    // The colliding ref is never deleted to make room, and never pushed onto.
    assert!(
        !f.calls().iter().any(|c| c.contains("--delete") || c.contains("push")),
        "a ref loomux cannot account for is not one it may overwrite; calls: {:?}",
        f.calls()
    );

    // Each attempt asks about a DIFFERENT name — a mint that re-rolled to the
    // same id would burn the bound without ever trying anything new.
    let names: Vec<String> = f.calls();
    assert_ne!(names[0], names[1]);
    assert_ne!(names[1], names[2]);
}

/// A remote that cannot be *asked* is a refusal, not an absence. Reading a
/// network failure as "the ref is free" is the one way a fresh batch pushes onto
/// a leaked scratch ref and ends up testing an object it did not construct (§4).
#[test]
fn an_unanswerable_collision_check_refuses_instead_of_assuming_the_ref_is_free() {
    for (code, stdout, stderr) in [
        (128, "", "fatal: could not read from remote repository"),
        (1, "", "ssh: connect: network is unreachable"),
        // `--exit-code` promises non-zero when nothing matched, so a zero exit
        // with no listing is a contradiction — not an absence.
        (0, "", ""),
    ] {
        let f = Fake::new().git("ls-remote", code, stdout, stderr);
        assert!(
            scratch_exists(&f, "loomux/mq/g1-mq-1").is_err(),
            "exit {code} must not read as 'the ref is free'"
        );
        // A fresh fake, so the call count below is the mint's alone.
        let f = Fake::new().git("ls-remote", code, stdout, stderr);
        assert!(matches!(mint_scratch(&f, "g1", 7), Err(MintError::Lookup(_))));
        assert_eq!(f.calls().len(), 1, "an unanswerable question is not retried into an answer");
    }
    // …and the two answers that ARE answers.
    let f = Fake::new().git("ls-remote", 2, "", "");
    assert_eq!(scratch_exists(&f, "loomux/mq/g1-mq-1"), Ok(false));
    let f = Fake::new().git("ls-remote", 0, "abc\trefs/heads/loomux/mq/g1-mq-1\n", "");
    assert_eq!(scratch_exists(&f, "loomux/mq/g1-mq-1"), Ok(true));

    // A group id `mergeq::scratch_branch` refuses to build a name from is
    // rejected outright, not rewritten into a different name (§11.4).
    let f = Fake::new().git("ls-remote", 2, "", "");
    assert_eq!(mint_scratch(&f, "../..", 7), Err(MintError::BadName));
    assert!(f.calls().is_empty());
}

// ════════════════════════════════════════════════════════════════════════════
// §6 — the gate's second enforcement point (the #532 rule).
// ════════════════════════════════════════════════════════════════════════════

/// A gate that held when the batch was built and does **not** hold at the moment
/// of submit refuses the landing, and pushes nothing. Between those two points
/// there is a full CI cycle — tens of minutes in which a PR can be rebased.
#[test]
fn the_gate_is_re_enforced_at_landing_and_a_rebase_since_the_build_refuses() {
    let rebased_head = "1111111111111111111111111111111111111111";
    let f = Fake::new()
        .gh("repo view", 0, "main\n", "")
        // The PR's LIVE head has moved since the verdict was recorded.
        .gh("pr view 612", 0, &pr_json("integration", rebased_head, "b"), "")
        .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "")
        .git("push", 0, "", "");

    let got = land_batch(
        &f,
        SCRATCH,
        "integration",
        &[612],
        &one_reviewer_gate(),
        // The verdict binds to the OLD head.
        &|_| verdict_map(PR_HEAD, "b"),
    );
    match got {
        Err(LandRefusal::Gate { pr, .. }) => assert_eq!(pr, 612),
        other => panic!("a stale pass must refuse the landing, got {other:?}"),
    }
    assert!(
        !f.calls().iter().any(|c| c.starts_with("git push")),
        "a gate-refused landing pushes nothing; calls: {:?}",
        f.calls()
    );
}

/// No gate configured is a **refusal**, not a pass (§6). `evaluate_merge_gate`
/// with no gate returns *allowed*, which is right for the shim and wrong for the
/// queue: it would mean the backend pushing approved-by-nobody PRs onto a branch
/// under its own authority.
#[test]
fn landing_with_no_gate_configured_refuses_rather_than_passes() {
    let f = Fake::new()
        .gh("repo view", 0, "main\n", "")
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "")
        .git("push", 0, "", "");

    let got = land_batch(&f, SCRATCH, "integration", &[612], &GateSpec::Absent, &|_| {
        BTreeMap::new()
    });
    match got {
        Err(LandRefusal::Gate { pr, ref recheck }) => {
            assert_eq!(pr, 612);
            assert_eq!(recheck.refusal_code(), Some("gate-not-configured"));
        }
        other => panic!("an ungated repo must not land through the queue, got {other:?}"),
    }
    assert!(!f.calls().iter().any(|c| c.starts_with("git push")));
}

/// A batch whose sub-PRs disagree about their base refuses rather than landing
/// on whichever one happened to be resolved first.
#[test]
fn a_batch_whose_prs_disagree_about_their_base_refuses() {
    let f = Fake::new()
        .gh("repo view", 0, "main\n", "")
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("pr view 613", 0, &pr_json("some-other-branch", PR_HEAD, "b"), "")
        .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "")
        .git("push", 0, "", "");

    assert_eq!(
        land_batch(&f, SCRATCH, "integration", &[612, 613], &one_reviewer_gate(), &|_| {
            verdict_map(PR_HEAD, "b")
        }),
        Err(LandRefusal::Target { pr: 613, refusal: TargetRefusal::BaseNotTarget })
    );
    assert!(!f.calls().iter().any(|c| c.starts_with("git push")));
    assert_eq!(TargetRefusal::BaseNotTarget.code(), "base-not-target");
}

/// A **corrupt recorded target** refuses before any lookup, rather than letting
/// every sub-PR validate against nothing but the default and the batch push to
/// whatever the last one's base happened to be.
///
/// This test exists because mutation run C found the gap: the case was defended
/// by threading each validated sibling into the next iteration, and removing
/// that threading reddened nothing, because no test ever supplied a record that
/// was not already a good branch name. The guard replaced the threading; this is
/// the case that now holds it.
#[test]
fn a_corrupt_recorded_target_refuses_before_anything_is_resolved() {
    for corrupt in ["", "   ", "--force", "refs/heads/integration", "a:b", "HEAD"] {
        let f = Fake::new()
            .gh("repo view", 0, "main\n", "")
            .gh("pr view", 0, &pr_json("integration", PR_HEAD, "b"), "")
            .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "")
            .git("push", 0, "", "");
        assert_eq!(
            land_batch(&f, SCRATCH, corrupt, &[612], &one_reviewer_gate(), &|_| {
                verdict_map(PR_HEAD, "b")
            }),
            Err(LandRefusal::Target { pr: 0, refusal: TargetRefusal::BaseUnverifiable }),
            "a recorded target of {corrupt:?} is a corrupt record, not a branch"
        );
        // Refused before ANY lookup — a corrupt record is not worth a round-trip,
        // and nothing is pushed.
        assert!(
            f.calls().is_empty(),
            "a corrupt record must refuse before resolving anything; calls: {:?}",
            f.calls()
        );
    }
}

/// The recorded target is an **assertion, not a selection** (§4): it can only
/// ever narrow the outcome, so a record that disagrees with every PR's live base
/// refuses rather than retargeting the batch onto the record's branch.
#[test]
fn the_recorded_target_can_only_narrow_never_select() {
    let f = Fake::new()
        .gh("repo view", 0, "main\n", "")
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "")
        .git("push", 0, "", "");

    assert_eq!(
        land_batch(&f, SCRATCH, "a-branch-nobody-reviewed-against", &[612], &one_reviewer_gate(), &|_| {
            verdict_map(PR_HEAD, "b")
        }),
        Err(LandRefusal::Target { pr: 612, refusal: TargetRefusal::BaseNotTarget })
    );
    assert!(!f.calls().iter().any(|c| c.starts_with("git push")));

    // Same posture in the pure core, for `queue_merge`'s optional argument.
    assert_eq!(validate_target("integration", "main", None, Some("integration")), Ok("integration".into()));
    assert_eq!(
        validate_target("integration", "main", None, Some("something-else")),
        Err(TargetRefusal::BaseNotTarget)
    );
    // An assertion can never widen: asserting the default does not make the
    // default landable.
    assert_eq!(validate_target("main", "main", None, Some("main")), Err(TargetRefusal::BaseIsDefault));
}

/// A branch name the queue will not build a refspec from is refused as
/// unverifiable. `validate_target`'s output is interpolated straight into
/// `<sha>:refs/heads/<target>`, so a name carrying a `:` would split the refspec
/// and land the batch on a different ref, and a name starting with `-` would be
/// read as a flag.
#[test]
fn a_target_that_would_not_survive_becoming_a_refspec_is_refused() {
    for hostile in [
        "integration:refs/heads/main",
        "--force",
        "-x",
        "refs/heads/integration",
        "integration branch",
        "in..tegration",
        "integration\nmain",
        "integration^",
        "integration~1",
        "feat/*",
        "integration@{0}",
        "/integration",
        "integration/",
        "integration.lock",
        "HEAD",
        "",
    ] {
        assert_eq!(
            validate_target(hostile, "main", None, None),
            Err(TargetRefusal::BaseUnverifiable),
            "{hostile:?} must never reach a refspec"
        );
    }
    // The ordinary shapes still pass.
    for ok in ["integration", "feat/integration-batch-2", "release-1.2", "a_b-c/d"] {
        assert_eq!(validate_target(ok, "main", None, None), Ok(ok.to_string()));
    }
}

// ════════════════════════════════════════════════════════════════════════════
// §5 — the checks adapter. `Met` is not green.
// ════════════════════════════════════════════════════════════════════════════

/// The correction the adapter exists for: `notify::pr_checks_result` returns
/// `PollResult::Met` for a **failing** run as well as a passing one, because for
/// a *watch* "the checks resolved" is the event. A queue that read `Met` as
/// success would land a red batch.
#[test]
fn a_failing_run_is_terminal_but_never_green() {
    let red = checks_json(&[("build", "SUCCESS"), ("test (windows)", "FAILURE")]);
    // The shared helper says Met — terminal — for this exact input.
    assert!(matches!(
        loomux_lib::orchestration::notify::pr_checks_result(Ok(&red)),
        loomux_lib::orchestration::notify::PollResult::Met { .. }
    ));
    // The adapter narrows it to Red, naming the failing check.
    assert_eq!(
        classify_checks(Ok(&red)),
        BatchVerification::Red { failing: vec!["test (windows)".into()] }
    );
    assert!(!classify_checks(Ok(&red)).is_green());

    // All-terminal, none failing → Green.
    assert_eq!(
        classify_checks(Ok(&checks_json(&[("build", "SUCCESS"), ("lint", "SUCCESS")]))),
        BatchVerification::Green
    );
    // GitHub's non-failing terminal states (#290) stay non-failing, because the
    // adapter uses `notify::check_is_failing` rather than its own opinion.
    assert_eq!(
        classify_checks(Ok(&checks_json(&[("build", "SUCCESS"), ("deploy", "SKIPPED"), ("x", "NEUTRAL")]))),
        BatchVerification::Green
    );
    // …and an undocumented conclusion is failing, never silently passing.
    assert_eq!(
        classify_checks(Ok(&checks_json(&[("build", "SOMETHING_NEW")]))),
        BatchVerification::Red { failing: vec!["build".into()] }
    );
}

/// An empty check list is **pending**, not success — the property §5 says
/// matters most, inherited from the shared helper rather than re-derived. And a
/// just-pushed PR's "no checks reported" stderr is pending too, so the queue
/// never fires an instant bogus green the moment a draft PR opens.
#[test]
fn an_empty_or_absent_check_list_is_pending_and_never_green() {
    assert_eq!(classify_checks(Ok("[]")), BatchVerification::Pending);
    assert_eq!(
        classify_checks(Err("no checks reported on the 'loomux/mq/g1-mq-1' branch")),
        BatchVerification::Pending
    );
    assert_eq!(
        classify_checks(Ok(&checks_json(&[("build", "IN_PROGRESS"), ("lint", "SUCCESS")]))),
        BatchVerification::Pending
    );
    // Everything the adapter cannot classify is Unavailable — and Unavailable is
    // not green: nothing lands on it (§5).
    assert!(matches!(classify_checks(Err("gh: not logged in")), BatchVerification::Unavailable { .. }));
    assert!(matches!(classify_checks(Ok("{not json")), BatchVerification::Unavailable { .. }));
    for v in [
        classify_checks(Ok("[]")),
        classify_checks(Err("gh: not logged in")),
        classify_checks(Ok("{not json")),
    ] {
        assert!(!v.is_green());
    }
}

/// `also: [ci-green]` means the **sub-PR's own** checks (§6) — pending and
/// unavailable both read as `None`, which `recheck_gate` turns into a refusal,
/// mirroring the shim's `ci-not-green` arm.
#[test]
fn a_sub_prs_own_ci_is_read_from_its_own_checks_and_unknown_refuses() {
    let f = Fake::new().gh("pr checks 612", 0, &checks_json(&[("build", "SUCCESS")]), "");
    assert_eq!(pr_ci_green(&f, 612), Some(true));
    assert_eq!(f.argv("gh", 0), pr_checks_argv(612));
    assert_eq!(f.argv("gh", 0)[2], "612", "the PR asked about is the sub-PR, not the batch");

    let f = Fake::new().gh("pr checks 612", 0, &checks_json(&[("build", "FAILURE")]), "");
    assert_eq!(pr_ci_green(&f, 612), Some(false));

    // Pending / unreadable / gh missing are all `None` = "cannot tell" = refuse.
    let f = Fake::new().gh("pr checks 612", 0, "[]", "");
    assert_eq!(pr_ci_green(&f, 612), None);
    let f = Fake::new().gh("pr checks 612", 1, "", "gh: not logged in");
    assert_eq!(pr_ci_green(&f, 612), None);
    let f = Fake::new().spawn_fail("gh");
    assert_eq!(pr_ci_green(&f, 612), None);
}

/// The gate's `also: [ci-green]` clause refuses a landing when the sub-PR's own
/// checks are not green — even though the *batch* is green. §6: the batch's
/// checks are an additional signal, never a substitute for the per-PR one.
#[test]
fn a_green_batch_does_not_substitute_for_a_sub_prs_own_red_ci() {
    let gate = GateSpec::Declared(
        parse_gate_file("require all-pass\nreviewer rev-a\nalso ci-green\n").expect("gate parses"),
    );
    let f = Fake::new()
        .gh("repo view", 0, "main\n", "")
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("pr checks 612", 0, &checks_json(&[("test", "FAILURE")]), "")
        .git("push", 0, "", "");

    match land_batch(&f, SCRATCH, "integration", &[612], &gate, &|_| verdict_map(PR_HEAD, "b")) {
        Err(LandRefusal::Gate { pr, .. }) => assert_eq!(pr, 612),
        other => panic!("the sub-PR's own red CI must refuse, got {other:?}"),
    }
    assert!(!f.calls().iter().any(|c| c.starts_with("git push")));
}

// ════════════════════════════════════════════════════════════════════════════
// §10 — the failure table's driver-side rows.
// ════════════════════════════════════════════════════════════════════════════

/// A target that moved under an in-flight batch makes the **fast-forward push
/// fail** rather than overwriting anything (§10). There is no retry arm.
#[test]
fn a_target_that_moved_makes_the_landing_push_fail_and_is_not_retried() {
    let f = Fake::new()
        .gh("repo view", 0, "main\n", "")
        .gh("pr view 612", 0, &pr_json("integration", PR_HEAD, "b"), "")
        .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "")
        .git("push", 1, "", "! [rejected] integration -> integration (non-fast-forward)");

    match land_batch(&f, SCRATCH, "integration", &[612], &one_reviewer_gate(), &|_| {
        verdict_map(PR_HEAD, "b")
    }) {
        Err(LandRefusal::PushFailed(why)) => assert!(why.contains("non-fast-forward"), "{why}"),
        other => panic!("expected the ff push to fail, got {other:?}"),
    }
    assert_eq!(
        f.calls().iter().filter(|c| c.starts_with("git push")).count(),
        1,
        "no retry loop (§3): one attempt per state transition"
    );
}

/// Cleanup deletes **only** the exact ref this batch minted, rebuilt here from
/// the group and batch ids — never a pattern, never a glob, never
/// "delete branches matching" (§10).
#[test]
fn cleanup_deletes_only_the_exact_ref_it_minted() {
    let f = Fake::new().gh("pr close", 0, "", "").git("push", 0, "", "");
    assert_eq!(cleanup_scratch(&f, "g1", "mq-7f3a0000", Some(640)), vec![]);

    assert_eq!(f.argv("gh", 0), close_draft_argv(640));
    assert_eq!(f.argv("gh", 0), vec!["pr", "close", "640"]);
    assert!(
        !f.argv("gh", 0).iter().any(|a| a == "--delete-branch"),
        "the ref deletion is its own auditable step, not a side effect of closing"
    );

    let argv = f.argv("git", 0);
    assert_eq!(argv, delete_scratch_argv("loomux/mq/g1-mq-7f3a0000"));
    assert_eq!(argv, vec!["push", REMOTE, "--delete", "refs/heads/loomux/mq/g1-mq-7f3a0000"]);
    // No wildcard could ever reach the remote.
    let joined = argv.join(" ");
    assert!(!joined.contains('*') && !joined.contains('?') && !joined.contains("--prune"));
    assert!(joined.contains("refs/heads/loomux/mq/"), "namespace-scoped by exact name (§11.4)");

    // A name `mergeq::scratch_branch` refuses to build is not deleted by some
    // other spelling — it is reported as a failure and nothing is pushed.
    let f = Fake::new().gh("pr close", 0, "", "").git("push", 0, "", "");
    let fails = cleanup_scratch(&f, "../..", "mq-1", None);
    assert_eq!(fails.len(), 1);
    assert_eq!(fails[0].step, "delete-scratch");
    assert!(!f.calls().iter().any(|c| c.starts_with("git push")), "calls: {:?}", f.calls());
}

/// Cleanup failure never fails a batch: it reports what went wrong and leaves
/// the ref behind for the next reconcile to see (§10). A leaked scratch ref is
/// cheap; a batch held hostage by a failed `git push --delete` is not.
#[test]
fn cleanup_failure_is_reported_and_never_raised() {
    let f = Fake::new()
        .gh("pr close", 1, "", "could not close pull request")
        .git("push", 1, "", "remote ref does not exist");
    let fails = cleanup_scratch(&f, "g1", "mq-7f3a0000", Some(640));
    assert_eq!(fails.len(), 2, "both steps ran; the first failure did not abort the second");
    assert_eq!(fails[0].step, "close-draft");
    assert_eq!(fails[1].step, "delete-scratch");
    assert!(fails[1].why.contains("remote ref does not exist"));

    // The second step runs even when the first could not spawn at all.
    let f = Fake::new().spawn_fail("gh").git("push", 0, "", "");
    let fails = cleanup_scratch(&f, "g1", "mq-7f3a0000", Some(640));
    assert_eq!(fails.len(), 1);
    assert_eq!(fails[0].step, "close-draft");
    assert!(fails[0].why.contains("gh-not-found"), "{}", fails[0].why);
    assert!(f.calls().iter().any(|c| c.starts_with("git push")), "the ref is still deleted");
}

/// The argv builders are pinned in isolation too, so a change to the shape is a
/// visible test change rather than something only an end-to-end path notices.
#[test]
fn the_lookup_argvs_are_the_shims_own_two_lookups() {
    assert_eq!(
        default_branch_argv(),
        vec!["repo", "view", "--json", "defaultBranchRef", "--jq", ".defaultBranchRef.name"]
    );
    assert_eq!(
        pr_facts_argv(612),
        vec!["pr", "view", "612", "--json", "baseRefName,headRefOid,body"]
    );
    // One round trip for base + head + body: a second call would be a second
    // moment, and §6's whole point is that the gate is re-verified at ONE.
    assert_eq!(pr_facts_argv(612).len(), 5);
    assert_eq!(ls_remote_argv("loomux/mq/g1-mq-1"), vec![
        "ls-remote",
        "--exit-code",
        REMOTE,
        "refs/heads/loomux/mq/g1-mq-1"
    ]);
    // Slice C's schema constant is what D1 builds against — a mismatch here
    // would mean the two slices disagree about the file they share.
    assert_eq!(MERGE_QUEUE_VERSION, 1);
}
