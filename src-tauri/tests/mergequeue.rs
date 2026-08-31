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

use loomux_lib::orchestration::mergeq::{
    scratch_branch, valid_id_component, BatchRecord, EntryState, GateSpec, MergeQueueState,
    QueueEntry, MERGE_QUEUE_VERSION,
};
use loomux_lib::orchestration::mqloop::{
    advance_entry, batch_fetch_argv, batch_pr_body, batch_pr_title, bisect_action, build_scratch,
    cancel, cleanup_worktree, culprit_comment, culprit_notice, draft_pr_argv, drive, enqueue,
    finish_batch, load_state, observe_batch, parse_created_pr, pr_comment_argv, pr_state_argv,
    prune_terminal, reconcile_batch, record_batch, requeue_survivors, scratch_worktree_path,
    state_path, status_view, store_state, unverifiable_notice, walk_bisect, worktree_remove_argv,
    BatchBuildError, BatchOutcome, BisectAction, CancelOutcome, DriveConfig, EnqueueOutcome,
    StateError, MAX_TERMINAL_RETAINED,
};
use loomux_lib::orchestration::mqdriver::{
    base_check_runs_argv, base_ci_green, base_status_argv, classify_checks, cleanup_scratch,
    close_draft_argv, default_branch_argv, delete_scratch_argv,
    land_batch, land_push_argv, ls_remote_argv, mint_scratch, pr_checks_argv, pr_ci_green,
    pr_facts_argv, push_scratch, resolve_and_validate_target, resolve_default_branch, resolve_pr,
    scratch_exists, scratch_push_argv, validate_target, BatchVerification, CmdOut, LandRefusal,
    MintError, MqRunner, TargetRefusal, MINT_ATTEMPTS, REMOTE,
};
use loomux_lib::orchestration::workflow::{
    body_digest, parse_gate_file, BlockId, ReviewVerdict, Verdict, BASE_CHECK_RUNS_JQ,
    BASE_STATUS_JQ,
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
    /// The contents of every `--body-file` argument, read **at the moment of
    /// the call** — which is the only chance: the driver deletes the file as
    /// soon as `gh` returns, so a test that looked afterwards would find
    /// nothing. These are the exact bytes that would reach GitHub, which is
    /// what §8's "the queue's own text never carries a closing keyword" has to
    /// be pinned against.
    bodies: Mutex<Vec<String>>,
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
            bodies: Mutex::new(Vec::new()),
            spawn_fails: Vec::new(),
        }
    }

    fn git(mut self, matches: &'static str, code: i32, stdout: &str, stderr: &str) -> Fake {
        self.git_replies.push(Reply { matches, out: out(code, stdout, stderr) });
        self
    }

    /// Like [`Fake::git`], but **ahead of** everything already scripted.
    ///
    /// Replies are first-match-wins, so a builder that only appends cannot
    /// override a reply a shared fixture already answers — the "override" is
    /// simply never reached, the fixture's own answer stands, and the test
    /// passes or fails for a reason that has nothing to do with what it was
    /// written to check. That is not hypothetical: the first cut of the
    /// batch-abort and merge-conflict tests below appended, so both ran against
    /// a *successful* build and failed on an assertion three steps later.
    fn git_first(mut self, matches: &'static str, code: i32, stdout: &str, stderr: &str) -> Fake {
        self.git_replies.insert(0, Reply { matches, out: out(code, stdout, stderr) });
        self
    }

    fn gh(mut self, matches: &'static str, code: i32, stdout: &str, stderr: &str) -> Fake {
        self.gh_replies.push(Reply { matches, out: out(code, stdout, stderr) });
        self
    }

    /// Like [`Fake::gh`], but **ahead of** everything already scripted — see
    /// [`Fake::git_first`] for why appending cannot override.
    fn gh_first(mut self, matches: &'static str, code: i32, stdout: &str, stderr: &str) -> Fake {
        self.gh_replies.insert(0, Reply { matches, out: out(code, stdout, stderr) });
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
    /// The one argv to `bin` containing `needle`, for argv-level assertions in
    /// a sequence whose call order is not fixed by the test. Panics if there is
    /// not exactly one — an assertion aimed at "the push" must not silently
    /// pick the first of two.
    fn argv_containing(&self, bin: &str, needle: &str) -> Vec<String> {
        let hits: Vec<Vec<String>> = self
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(b, a)| *b == bin && a.join(" ").contains(needle))
            .map(|(_, a)| a.clone())
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one {bin} call containing {needle:?}, saw {hits:?} of {:?}",
            self.calls()
        );
        hits.into_iter().next().unwrap()
    }

    /// Every `--body-file` body handed to `gh`, in call order.
    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }

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
        if let Some(i) = args.iter().position(|a| *a == "--body-file") {
            if let Some(p) = args.get(i + 1) {
                if let Ok(t) = std::fs::read_to_string(p) {
                    self.bodies.lock().unwrap().push(t);
                }
            }
        }
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
        vec!["pr", "view", "612", "--json", "baseRefName,headRefOid,body,additions,deletions"]
    );
    // One round trip for base + head + body + SIZE (#1174): a second call would
    // be a second moment, and §6's whole point is that the gate is re-verified
    // at ONE. The size rides here rather than behind a `declares_…` branch
    // precisely because it costs nothing extra in a call already being made.
    assert_eq!(pr_facts_argv(612).len(), 5);
    // #1174's base-green lookups. TWO endpoints, and the pin says why: the
    // combined-status API sees only legacy statuses and the check-runs API only
    // check runs, so a repo using either alone reports nothing from the other.
    assert_eq!(
        base_check_runs_argv("integration/x")[..2],
        [
            "api".to_string(),
            "repos/{owner}/{repo}/commits/integration/x/check-runs?per_page=100".to_string()
        ],
        "the API maximum page size — a MITIGATION for pagination, never the guard (#1181)"
    );
    assert_eq!(
        base_status_argv("integration/x")[..2],
        ["api".to_string(), "repos/{owner}/{repo}/commits/integration/x/status".to_string()]
    );
    // `--jq` reduces each to ONE word in gh, not in Rust — the shim has to do
    // exactly this in shell with no JSON parser, and the two halves of the
    // contract are easiest to keep identical when they are the same expression.
    for argv in [base_check_runs_argv("m"), base_status_argv("m")] {
        assert_eq!(argv[2], "--jq", "{argv:?}");
        assert!(argv[3].contains("\"green\"") && argv[3].contains("\"none\""), "{argv:?}");
    }
    // Green is an ALLOW-list of conclusions, so a conclusion GitHub adds
    // tomorrow reads as red rather than as green.
    let runs_jq = base_check_runs_argv("m").remove(3);
    for good in ["success", "neutral", "skipped"] {
        assert!(runs_jq.contains(good), "{runs_jq}");
    }
    assert!(!runs_jq.contains("failure"), "an allow-list must not be spelled as a deny-list: {runs_jq}");
    // The argv carries the SHARED constant, not a copy that matches today.
    assert_eq!(runs_jq, BASE_CHECK_RUNS_JQ);
    assert_eq!(base_status_argv("m").remove(3), BASE_STATUS_JQ);
    // #1181: the paginated endpoint's guard is a comparison against total_count,
    // and it is what a per_page bump can never replace.
    assert!(
        runs_jq.contains("total_count") && runs_jq.contains("truncated"),
        "a page that does not carry every run must not be able to answer green: {runs_jq}"
    );
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

/// Is a real `jq` available to EXECUTE the reductions? (Preinstalled on all
/// three GitHub-hosted runner images; frequently absent on a dev box.) Skipped
/// rather than failed when missing — the `have_sh()` precedent.
fn have_jq() -> bool {
    std::process::Command::new("jq")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// **The reductions, EXECUTED** (#1181 rev-lead NB1 — and the blocking finding).
///
/// `BASE_CHECK_RUNS_JQ`/`BASE_STATUS_JQ` *are* the `base-green` decision, and
/// until this test nothing ran them: the queue's `Fake` and the shim harness's
/// fake `gh` both return the already-reduced word, and the only other pins were
/// string containment. So every test agreed about a string none of them had
/// evaluated — which is how a suite green on three platforms coexisted with a
/// truncated page reducing to `green` and a red base merging.
///
/// **Limit, stated rather than implied:** the shipped consumer is `gh`'s built-in
/// **gojq**, and this runs the same expression under **jq**. The constructs used
/// (`any/2`, `length`, `>`, string equality, `if`/`elif`) are ones the two
/// implement identically; nothing here proves gojq parity in general, and a
/// reduction that reached for a jq-only builtin would need a different harness.
#[test]
fn the_base_green_reductions_reduce_real_payloads_to_the_right_word() {
    if !have_jq() {
        eprintln!("SKIP the_base_green_reductions_reduce_real_payloads_to_the_right_word: no jq");
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basegreen");
    let reduce = |jq: &str, fixture: &str| -> String {
        let json = std::fs::read_to_string(dir.join(fixture))
            .unwrap_or_else(|e| panic!("{fixture}: {e}"));
        let mut c = std::process::Command::new("jq")
            .args(["-r", jq])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn jq");
        use std::io::Write;
        c.stdin.as_mut().unwrap().write_all(json.as_bytes()).unwrap();
        let out = c.wait_with_output().expect("run jq");
        assert!(
            out.status.success(),
            "{fixture}: jq refused the expression: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    for (fixture, want) in [
        ("checkruns-green.json", "green"),
        // Every green-ish conclusion in the allow-list, and nothing else.
        ("checkruns-red-at-tail.json", "red"),
        // THE REGRESSION. Same six runs, page cut before the three failures:
        // `any(.check_runs[]; …)` can only see page 1, so before #1181 this
        // reduced to `green` and the merge onto a red base was allowed.
        ("checkruns-truncated.json", "truncated"),
        // A run still in progress is PENDING, never red — `conclusion: null`
        // would satisfy the conclusion allow-list's negation, so the red clause
        // is guarded on `.status == "completed"`. A refusal that called a
        // still-building base "RED" would be a false sentence.
        ("checkruns-pending.json", "pending"),
        ("checkruns-none.json", "none"),
        // #1181 rev-lead NB5: the truncation clause rests on `total_count`, and
        // jq sorts `null` BELOW every number — so an absent key makes `null > N`
        // false, skips that clause and falls through to `green`. Round one's
        // defect in different clothing, which is why the shape is checked and
        // not assumed.
        ("checkruns-no-total-count.json", "truncated"),
        // …and the negative control: a payload can be shape-broken AND carry a
        // visible failure, and `red` still wins — a failure we can actually see
        // is the more actionable answer.
        ("checkruns-no-total-count-with-failure.json", "red"),
    ] {
        assert_eq!(reduce(BASE_CHECK_RUNS_JQ, fixture), want, "check-runs: {fixture}");
    }
    for (fixture, want) in [
        ("status-green.json", "green"),
        ("status-red.json", "red"),
        // `.state` is `pending` both for a pending context and for NO statuses,
        // so the count is read first — otherwise a repo with no legacy statuses
        // would hold every merge forever.
        ("status-none.json", "none"),
        ("status-pending.json", "pending"),
        // The same guard over THIS reduction's inputs — not asked for by the
        // review, required by the repo's own rule that a guard reads every one of
        // its inputs by one rule. `null | length` is 0 rather than an error, so an
        // absent `statuses` would read as the definite claim "no legacy statuses
        // exist"; an absent `state` would fall to the `else` and report `red`,
        // refusing while saying something false about the base.
        ("status-no-statuses.json", "truncated"),
        ("status-no-state.json", "truncated"),
    ] {
        assert_eq!(reduce(BASE_STATUS_JQ, fixture), want, "status: {fixture}");
    }

    // ── the two defects, EXECUTED rather than described ────────────────────
    //
    // Both review rounds found this expression failing OPEN on an assumption it
    // never checked, and a green suite saw neither. So the superseded
    // expressions are kept as literals and run against the same fixtures: the
    // reds that justify the guards are part of the suite permanently, instead
    // of living on a scratch branch someone has to go and find.
    //
    // These strings are HISTORY, deliberately NOT derived from the live
    // constants — that is the whole point. Editing the constants must not
    // touch them.
    const BEFORE_ROUND_1: &str = "if (.check_runs|length) == 0 then \"none\" elif any(.check_runs[]; .status != \"completed\") then \"pending\" elif any(.check_runs[]; .conclusion != \"success\" and .conclusion != \"neutral\" and .conclusion != \"skipped\") then \"red\" else \"green\" end";
    assert_eq!(
        reduce(BEFORE_ROUND_1, "checkruns-truncated.json"),
        "green",
        "the pre-round-1 reduction called a TRUNCATED page green: six runs, three failing, page cut before them — the merge onto a red base this clause exists to stop"
    );
    const BEFORE_ROUND_2: &str = "if any(.check_runs[]; .status == \"completed\" and .conclusion != \"success\" and .conclusion != \"neutral\" and .conclusion != \"skipped\") then \"red\" elif (.total_count > (.check_runs|length)) then \"truncated\" elif any(.check_runs[]; .status != \"completed\") then \"pending\" elif (.check_runs|length) == 0 then \"none\" else \"green\" end";
    assert_eq!(
        reduce(BEFORE_ROUND_2, "checkruns-no-total-count.json"),
        "green",
        "round 1 guarded with a field it never checked for: jq sorts null below every number, so `null > N` is false and an absent total_count fell through to green — the same fail-open one clause further in"
    );
    // …and both fixtures answer `truncated` under the SHIPPED reduction, in the
    // table above. Old expression green, new expression refuses: the before and
    // after, executed on every run.
}

/// #1174: how the two base-check answers COMBINE. Driven through the real
/// runner seam, because the combination is the part a reader gets wrong.
#[test]
fn base_ci_green_combines_two_surfaces_and_treats_silence_as_unknown() {
    let ask = |runs: &str, status: &str| {
        let f = Fake::new()
            .gh("check-runs", 0, runs, "")
            .gh("/status", 0, status, "");
        base_ci_green(&f, "integration/x")
    };
    // Green needs BOTH surfaces quiet-or-green AND at least one of them to have
    // said something. A repo on Actions alone reports nothing from the legacy
    // status API, and a repo on statuses alone reports no check runs — neither
    // is thereby unknown.
    assert_eq!(ask("green", "green"), Some(true));
    assert_eq!(ask("green", "none"), Some(true));
    assert_eq!(ask("none", "green"), Some(true));
    // Red from either surface is red.
    assert_eq!(ask("red", "green"), Some(false));
    assert_eq!(ask("green", "red"), Some(false));
    assert_eq!(ask("red", "none"), Some(false));
    // Red OUTRANKS pending: the worse answer wins, so a base that is both
    // broken and still building is reported broken.
    assert_eq!(ask("red", "pending"), Some(false));
    // #1181: a truncated page is UNKNOWN, from either surface, and it outranks
    // pending/none the way the shim's chain does. The queue lands ONTO this
    // branch, so "we could not see all of its checks" is never "it is fine".
    assert_eq!(ask("truncated", "green"), None);
    assert_eq!(ask("green", "truncated"), None);
    assert_eq!(ask("truncated", "none"), None);
    // …but a run we CAN see failing still reports red, which is the more
    // actionable answer and the reason the reduction tests red first.
    assert_eq!(ask("red", "truncated"), Some(false));

    // Unknown — `None`, which the gate refuses on. Still running, silent on
    // both surfaces, or an answer this build cannot read at all.
    assert_eq!(ask("pending", "green"), None);
    assert_eq!(ask("green", "pending"), None);
    assert_eq!(ask("none", "none"), None, "nothing said this commit is healthy");
    assert_eq!(ask("wat", "green"), None, "an unreadable answer is not a green one");
    // A non-zero exit from gh is unknown too, never green.
    let failing = Fake::new().gh("check-runs", 1, "", "boom").gh("/status", 0, "green", "");
    assert_eq!(base_ci_green(&failing, "integration/x"), None);
}

// ════════════════════════════════════════════════════════════════════════════
// Slice D2 — the driver LOOP.
//
// Same `Fake` runner, same constraint-3 posture: no real `git`, no real `gh`,
// no network. The only real filesystem touched is a scratch dir under the OS
// temp root, following `tests/orchestration.rs::scratch_dir` (std, not
// `tempfile` — constraint 2 keeps getrandom out of this crate).
// ════════════════════════════════════════════════════════════════════════════

/// Std-based scratch dir keyed by tag + pid so parallel runs never collide.
/// Same pattern and same rationale as `tests/orchestration.rs::scratch_dir`.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("loomux-mqloop-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TARGET_HEAD: &str = "cccccccccccccccccccccccccccccccccccccccc";
const MERGED: &str = "dddddddddddddddddddddddddddddddddddddddd";

// ── persistence (§11.3) ─────────────────────────────────────────────────────

/// **An absent file is an empty queue, not an error** — that is the product
/// default (§12: no `merge_queue:` block, nothing ever enqueued). Every *other*
/// failure is loud, because "nothing is queued" and "loomux cannot tell what is
/// queued" are the distinction §4's reconcile exists for.
#[test]
fn an_absent_queue_file_is_an_empty_queue_and_every_other_failure_is_loud() {
    let dir = scratch_dir("persist");

    // Absent → empty, and specifically NOT an error.
    let s = load_state(&dir).expect("an absent file is the product default, not a failure");
    assert!(s.entries.is_empty());
    assert!(s.batch.is_none());
    assert_eq!(s.version, MERGE_QUEUE_VERSION);

    // Malformed → loud. Never silently replaced with a fresh queue, which would
    // drop entries a human believes are queued.
    std::fs::write(state_path(&dir), "{ not json").unwrap();
    assert!(matches!(load_state(&dir), Err(StateError::Malformed(_))));

    // A future schema → Unsupported, so the driver refuses to operate AND
    // refuses to write. The file must survive untouched: that is the only way
    // §11.2's "an older build does not destroy what a newer one wrote" holds
    // for a version bump that changes meanings rather than adding keys.
    let future = r#"{"version":99,"target":"integration","entries":[]}"#;
    std::fs::write(state_path(&dir), future).unwrap();
    assert_eq!(load_state(&dir), Err(StateError::Unsupported(99)));
    assert_eq!(std::fs::read_to_string(state_path(&dir)).unwrap(), future, "left untouched");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A store/load round trip preserves what a **newer** build wrote. Slice C pins
/// the type's serde behaviour; this pins that D2's file I/O does not lose it —
/// a field ignored on read is lost on the next write, and the whole promise is
/// read *and rewrite*.
#[test]
fn the_queue_file_round_trips_a_newer_builds_fields_through_disk() {
    let dir = scratch_dir("roundtrip");
    let newer = r#"{"version":1,"target":"integration",
        "entries":[{"pr":612,"head":"abc","state":"queued","enqueued_ms":7,"priority":"high"}],
        "batch":null,"second_target":"feat/other"}"#;
    std::fs::write(state_path(&dir), newer).unwrap();

    let s = load_state(&dir).expect("a newer file with known version still loads");
    store_state(&dir, &s).expect("and stores");

    let back: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(state_path(&dir)).unwrap()).unwrap();
    assert_eq!(back["second_target"], "feat/other", "a file-level unknown survived disk");
    assert_eq!(back["entries"][0]["priority"], "high", "an entry-level unknown survived disk");
    assert_eq!(back["target"], "integration");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── batch construction (§8) ─────────────────────────────────────────────────

/// A runner that answers the whole build sequence, so each test only overrides
/// the one reply it is about.
fn build_fake() -> Fake {
    Fake::new()
        .git("fetch", 0, "", "")
        .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, HEAD_A, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-613", 0, HEAD_B, "")
        .git("worktree add", 0, "", "")
        .git("merge --no-ff", 0, "", "")
        .git("rev-parse HEAD", 0, MERGED, "")
        .git("worktree remove", 0, "", "")
}

/// §8's shape, at the argv level: one fetch of the target plus each PR's **pull
/// ref**, a detached worktree at the target head, one `--no-ff` merge per PR
/// **in queue order**, and the resulting SHA read back.
#[test]
fn the_scratch_is_the_target_head_plus_one_merge_per_pr_in_queue_order() {
    let f = build_fake();
    let built = build_scratch(
        &f,
        "mq-7f3a0000",
        "loomux/mq/g1-mq-7f3a0000",
        "integration",
        &[(612, HEAD_A.into()), (613, HEAD_B.into())],
    );
    let scratch = built.result.expect("a clean build succeeds");
    assert_eq!(scratch.sha, MERGED, "the SHA CI will judge is what rev-parse HEAD said");
    assert_eq!(scratch.target_head, TARGET_HEAD);
    assert_eq!(scratch.branch, "loomux/mq/g1-mq-7f3a0000");

    // One fetch, pull refs, into a namespace of our own so the user's
    // refs/remotes/origin/* is undisturbed.
    assert_eq!(
        f.argv("git", 0),
        vec![
            "fetch".to_string(),
            "--no-tags".into(),
            "origin".into(),
            "+refs/heads/integration:refs/remotes/loomux-mq/target".into(),
            "+refs/pull/612/head:refs/remotes/loomux-mq/pr-612".into(),
            "+refs/pull/613/head:refs/remotes/loomux-mq/pr-613".into(),
        ]
    );
    assert_eq!(batch_fetch_argv("integration", &[612, 613]), f.argv("git", 0));

    let calls = f.calls();
    // Detached, at the target head — never a branch checkout in the user's clone.
    assert!(
        calls.iter().any(|c| c.contains("worktree add --detach") && c.contains(TARGET_HEAD)),
        "calls: {calls:?}"
    );
    // Queue order: 612's merge precedes 613's.
    let m612 = calls.iter().position(|c| c.contains("merge --no-ff") && c.contains(HEAD_A));
    let m613 = calls.iter().position(|c| c.contains("merge --no-ff") && c.contains(HEAD_B));
    assert!(m612.is_some() && m613.is_some() && m612 < m613, "queue order is contractual: {calls:?}");
    // `--no-ff` so every queued PR gets its own merge commit, per §8's loop.
    assert_eq!(calls.iter().filter(|c| c.contains("merge --no-ff")).count(), 2);
    // Nothing here is a landing or a push.
    assert!(!calls.iter().any(|c| c.contains("git push")), "construction pushes nothing");
}

/// **A conflict kicks that entry back with no CI spent** (§8), and the worktree
/// still goes away.
#[test]
fn a_conflicted_merge_kicks_that_entry_back_and_spends_no_ci() {
    // Built explicitly rather than from `build_fake()`: `Fake` returns the FIRST
    // matching reply, so the failing `merge` has to be the registered one.
    let f = Fake::new()
        .git("fetch", 0, "", "")
        .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, HEAD_A, "")
        .git("worktree add", 0, "", "")
        .git("merge --no-ff", 1, "", "CONFLICT (content): Merge conflict in a.rs")
        .git("worktree remove", 0, "", "");
    let built = build_scratch(&f, "mq-1", "loomux/mq/g1-mq-1", "integration", &[(612, HEAD_A.into())]);
    assert_eq!(built.result, Err(BatchBuildError::Conflict { pr: 612 }));
    // No push, and no draft PR: a conflict costs zero CI.
    assert!(!f.calls().iter().any(|c| c.contains("push") || c.contains("pr create")));
}

/// A PR rebased since the entry recorded its head is kicked back too — its
/// verdicts died with the rebase (§6), so merging the new head would build an
/// object nobody approved.
#[test]
fn a_pr_whose_head_moved_since_the_entry_recorded_it_is_kicked_back() {
    let f = Fake::new()
        .git("fetch", 0, "", "")
        .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, HEAD_B, "")
        .git("worktree add", 0, "", "")
        .git("worktree remove", 0, "", "");
    let built =
        build_scratch(&f, "mq-1", "loomux/mq/g1-mq-1", "integration", &[(612, HEAD_A.into())]);
    match built.result {
        Err(BatchBuildError::HeadMoved { pr, expected, actual }) => {
            assert_eq!(pr, 612);
            assert_eq!(expected, HEAD_A);
            assert_eq!(actual, HEAD_B);
        }
        other => panic!("a moved head must kick back, got {other:?}"),
    }
    assert!(!f.calls().iter().any(|c| c.contains("merge --no-ff")), "never merged the wrong object");

    // An entry whose head loomux never resolved reads as UNKNOWN, and unknown is
    // never "unbound, therefore fine" — same fail-closed posture as an empty
    // head in the gate re-check.
    let f = Fake::new()
        .git("fetch", 0, "", "")
        .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, HEAD_A, "")
        .git("worktree add", 0, "", "")
        .git("worktree remove", 0, "", "");
    let built = build_scratch(&f, "mq-1", "loomux/mq/g1-mq-1", "integration", &[(612, String::new())]);
    assert!(matches!(built.result, Err(BatchBuildError::HeadMoved { .. })));
}

/// The recorded-vs-fetched head comparison is **one-directional and
/// fail-closed** (rev-163 N1).
///
/// Two properties, neither of which the earlier implementation had despite its
/// doc claiming the first:
///
/// - **Only the RECORDED head may be an abbreviation.** The earlier code
///   compared `min(recorded.len(), fetched.len())` bytes of both, which is
///   symmetric — so a *fetched* value shorter than the record would have
///   matched a full recorded head. A prefix comparison only means something
///   when the short side is the stored one.
/// - **A non-hex head returns `false`, it does not panic.** `&str` indexing
///   panics off a char boundary, so the old byte-slice was total only by the
///   accident that oids are ASCII — and nothing enforced that, since
///   `PrFacts.head` comes straight from `headRefOid.trim()`.
///
/// Exercised through `build_scratch`, because `same_object` is private — which
/// is the right shape anyway: what matters is that a mismatch **kicks the entry
/// back** rather than building an object nobody approved.
#[test]
fn only_the_recorded_head_may_be_abbreviated_and_a_malformed_one_fails_closed() {
    // A fetched value SHORTER than the recorded head is not a match, even
    // though it is a prefix of it.
    let f = Fake::new()
        .git("fetch", 0, "", "")
        .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, &HEAD_A[..8], "")
        .git("worktree add", 0, "", "")
        .git("worktree remove", 0, "", "");
    let built =
        build_scratch(&f, "mq-1", "loomux/mq/g1-mq-1", "integration", &[(612, HEAD_A.into())]);
    assert!(
        matches!(built.result, Err(BatchBuildError::HeadMoved { .. })),
        "a short FETCH must not satisfy a full recorded head, got {:?}",
        built.result
    );
    assert!(!f.calls().iter().any(|c| c.contains("merge --no-ff")), "nothing was merged");

    // The supported direction still works: a recorded abbreviation of the
    // fetched full oid IS the same object.
    let f = Fake::new()
        .git("fetch", 0, "", "")
        .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, HEAD_A, "")
        .git("worktree add", 0, "", "")
        .git("merge --no-ff", 0, "", "")
        .git("rev-parse HEAD", 0, MERGED, "")
        .git("worktree remove", 0, "", "");
    let built = build_scratch(
        &f,
        "mq-1",
        "loomux/mq/g1-mq-1",
        "integration",
        &[(612, HEAD_A[..10].to_string())],
    );
    assert!(built.result.is_ok(), "a recorded abbreviation matches: {:?}", built.result);

    // Malformed heads fail closed rather than panicking. The multi-byte case is
    // the one the old `&str` slice would have panicked on.
    for bad in ["not-hex-at-all", "aaé", "aaaaaaé_head", "abc"] {
        let f = Fake::new()
            .git("fetch", 0, "", "")
            .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
            .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, HEAD_A, "")
            .git("worktree add", 0, "", "")
            .git("worktree remove", 0, "", "");
        let built =
            build_scratch(&f, "mq-1", "loomux/mq/g1-mq-1", "integration", &[(612, bad.to_string())]);
        assert!(
            matches!(built.result, Err(BatchBuildError::HeadMoved { .. })),
            "a malformed recorded head {bad:?} must kick back, got {:?}",
            built.result
        );
    }
}

/// **The temp worktree is torn down on every exit path, and the outcome is
/// RETURNED rather than swallowed** (§10: cleanup failure never fails a batch).
///
/// The teardown only runs when the directory actually exists — otherwise a build
/// that failed before `worktree add` would report a cleanup failure for a
/// worktree that was never created. So this test creates the real directory.
#[test]
fn the_temp_worktree_is_torn_down_on_every_exit_path_and_the_outcome_is_returned() {
    // Success path: real directory present → removal attempted, by exact path.
    let wt = scratch_worktree_path("mq-teardown").expect("a well-formed id builds a path");
    std::fs::create_dir_all(&wt).unwrap();
    let f = build_fake();
    let built =
        build_scratch(&f, "mq-teardown", "loomux/mq/g1-mq-teardown", "integration", &[(612, HEAD_A.into())]);
    assert!(built.result.is_ok());
    assert_eq!(built.cleanup_failed, None);
    let removal = f.calls().into_iter().find(|c| c.contains("worktree remove")).expect("torn down");
    assert!(removal.contains(&wt.display().to_string()), "by exact path: {removal}");
    assert_eq!(worktree_remove_argv(&wt.display().to_string())[0..3], ["worktree", "remove", "--force"]);

    // Failure path: the worktree is STILL torn down, and a failed teardown comes
    // back in `cleanup_failed` rather than failing the batch or vanishing.
    std::fs::create_dir_all(&wt).unwrap();
    let f = Fake::new()
        .git("fetch", 0, "", "")
        .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, HEAD_A, "")
        .git("worktree add", 0, "", "")
        .git("merge --no-ff", 1, "", "CONFLICT")
        .git("worktree remove", 1, "", "fatal: validation failed, cannot remove working tree");
    let built =
        build_scratch(&f, "mq-teardown", "loomux/mq/g1-mq-teardown", "integration", &[(612, HEAD_A.into())]);
    assert_eq!(built.result, Err(BatchBuildError::Conflict { pr: 612 }), "the batch outcome is unchanged");
    assert!(
        built.cleanup_failed.as_deref().unwrap_or("").contains("cannot remove working tree"),
        "a failed teardown is REPORTED, not swallowed: {:?}",
        built.cleanup_failed
    );

    // **Never a prune.** Prune is a sweep over the shared `.git/worktrees` dir,
    // so a peer worktree that is momentarily unreachable is one prune forgets —
    // the shared-`.git` hazard that made `git stash` a banned verb here (#299).
    assert!(
        !f.calls().iter().any(|c| c.contains("prune")),
        "no pattern sweep, ever: {:?}",
        f.calls()
    );
    let _ = std::fs::remove_dir_all(&wt);
}

/// A worktree that was never created reports no cleanup failure — the existence
/// check, rather than matching `git`'s "is not a working tree" stderr, which is
/// a message and not a contract.
#[test]
fn a_worktree_that_was_never_created_is_not_a_cleanup_failure() {
    let f = Fake::new().git("worktree remove", 1, "", "fatal: is not a working tree");
    assert_eq!(cleanup_worktree(&f, "mq-never-existed"), None);
    assert!(f.calls().is_empty(), "nothing to remove means nothing is spawned");
}

// ── loomux-authored text (§8, §9) ───────────────────────────────────────────

/// **loomux's own text must never contain GitHub's closing pattern** (§8).
/// The scan is textual and context-blind — it fires from inside a blockquote, a
/// caveat, or a sentence asking a human to close something by hand (#569 was
/// auto-closed twice, the second time by a PR arguing against doing so). So
/// sub-PRs are listed as **bare `#N`**, and this is a test rather than a habit.
#[test]
fn loomux_authored_batch_text_never_carries_a_closing_keyword() {
    let body = batch_pr_body("mq-7f3a0000", "integration", MERGED, &[612, 613]);
    let comment = culprit_comment("mq-7f3a0000", &["build (windows)".into()], Some("http://x/run/1"), &[613]);
    let title = batch_pr_title("mq-7f3a0000", &[612, 613]);

    for text in [&body, &comment, &title] {
        let lower = text.to_lowercase();
        for kw in ["close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved"] {
            for pr in ["#612", "#613", "#581"] {
                assert!(
                    !lower.contains(&format!("{kw} {pr}")) && !lower.contains(&format!("{kw}{pr}")),
                    "loomux-authored text must not carry {kw:?} next to {pr:?}: {text}"
                );
            }
        }
    }
    // …and the sub-PRs really are listed, as bare references.
    assert!(body.contains("- #612") && body.contains("- #613"));
    assert!(comment.contains("#613"), "the sibling set is named (§9)");
}

/// gh-sourced text is sanitized, and both lists **state their own truncation**
/// rather than reading as complete — `.loomux/lessons.md`'s "no silent caps".
#[test]
fn gh_sourced_text_is_sanitized_and_truncation_is_stated() {
    // A check name carrying a newline and the `[orrerix]` marker must not survive
    // either: `sanitize_gh_text` strips control chars and neutralizes brackets.
    let comment = culprit_comment(
        "mq-1",
        &["evil\ncheck [orrerix] spoof".into()],
        None,
        &[613],
    );
    assert!(!comment.contains("evil\ncheck"), "control chars stripped");
    assert!(!comment.contains("[orrerix]"), "the marker is neutralized: {comment}");

    // 40 siblings and 40 failing checks: the caps fire and SAY they fired.
    let many: Vec<u64> = (600..640).collect();
    let checks: Vec<String> = (0..40).map(|i| format!("check-{i}")).collect();
    let comment = culprit_comment("mq-1", &checks, None, &many);
    assert!(comment.contains("further failing checks not listed"), "{comment}");
    assert!(comment.contains("further siblings not listed"), "{comment}");

    let body = batch_pr_body("mq-1", "integration", MERGED, &many);
    assert!(body.contains("more not listed here"), "{body}");
}

/// §9's honesty requirement, in the text itself: bisect finds **a** culprit, not
/// necessarily **the** culprit, and loomux has briefed nobody.
#[test]
fn the_culprit_comment_states_the_honest_limit_and_disclaims_routing() {
    let c = culprit_comment("mq-1", &["build".into()], None, &[613, 614]);
    let lower = c.to_lowercase();
    assert!(lower.contains("not necessarily"), "the pairwise-interaction limit is stated: {c}");
    assert!(lower.contains("pairwise"), "{c}");
    assert!(
        lower.contains("orchestrator") && lower.contains("not briefed") || lower.contains("has not briefed"),
        "§9: attribution is mechanical, routing is the orchestrator's call: {c}"
    );
}

/// The draft batch PR is opened **as a draft, into the target, from the scratch
/// ref** (§5) — the shape that reliably triggers PR-triggered CI and gives
/// `gh pr checks` a handle.
#[test]
fn the_batch_pr_is_opened_as_a_draft_from_the_scratch_ref_into_the_target() {
    let argv = draft_pr_argv("loomux/mq/g1-mq-1", "integration", "t", "./b.md");
    assert_eq!(argv[0..3], ["pr", "create", "--draft"]);
    assert!(argv.contains(&"--base".to_string()) && argv.contains(&"integration".to_string()));
    assert!(argv.contains(&"--head".to_string()) && argv.contains(&"loomux/mq/g1-mq-1".to_string()));
    // Body via file, never `--body` — a batch body carries newlines and
    // arbitrary sub-PR text.
    assert!(argv.contains(&"--body-file".to_string()));
    assert!(!argv.contains(&"--body".to_string()));
    assert_eq!(pr_comment_argv(612, "./c.md")[0..3], ["pr", "comment", "612"]);
}

// ── the bisect walk (§9) ────────────────────────────────────────────────────

/// Larger half first, and **survivors keep their original queue order** (§9) —
/// not the order the search happened to discard halves in.
#[test]
fn the_search_isolates_a_culprit_and_requeues_survivors_in_queue_order() {
    // k = 1: attributed with no further CI.
    assert_eq!(
        bisect_action(&[612]),
        BisectAction::Attribute { culprit: 612, survivors: vec![] }
    );
    assert_eq!(bisect_action(&[]), BisectAction::Abort);
    // k = 3 splits 2/1 — larger half first.
    assert_eq!(
        bisect_action(&[612, 613, 614]),
        BisectAction::Test { subset: vec![612, 613], rest: vec![614] }
    );

    // A full walk: 614 is the culprit; the survivors come back in the batch's
    // order (612, 613), not in the order the halves were discarded.
    let batch = [612, 613, 614, 615];
    let action = walk_bisect(&batch, |subset| subset.contains(&614));
    assert_eq!(
        action,
        BisectAction::Attribute { culprit: 614, survivors: vec![612, 613, 615] }
    );

    // Every position, every k, terminates and isolates the right entry.
    for k in 1..=8usize {
        let batch: Vec<u64> = (0..k as u64).collect();
        for culprit in &batch {
            let mut runs = 0;
            let action = walk_bisect(&batch, |s| {
                runs += 1;
                s.contains(culprit)
            });
            match action {
                BisectAction::Attribute { culprit: got, survivors } => {
                    assert_eq!(got, *culprit, "k={k}");
                    assert_eq!(
                        survivors,
                        batch.iter().copied().filter(|p| p != culprit).collect::<Vec<_>>(),
                        "survivors keep queue order"
                    );
                }
                other => panic!("k={k} culprit={culprit}: {other:?}"),
            }
            let bound = (usize::BITS - (k.max(1) - 1).leading_zeros()) as usize;
            assert!(runs <= bound + 1, "k={k} took {runs} observations, bound ~{bound}");
        }
    }
}

// ── the bounded observation loop (§5) ───────────────────────────────────────

/// Terminal answers win immediately; **`Pending` past the bound becomes
/// `Unverifiable`, never a forever-wait** (§5). Unverifiable is not green.
#[test]
fn the_observation_is_bounded_and_unverifiable_is_never_green() {
    let green = checks_json(&[("build", "SUCCESS")]);
    let red = checks_json(&[("build", "SUCCESS"), ("test", "FAILURE")]);

    // Terminal wins regardless of elapsed time — evidence beats the clock.
    let f = Fake::new().gh("pr checks", 0, &green, "");
    assert_eq!(observe_batch(&f, 700, 0, 999_999_999, 60), BatchOutcome::Green);
    let f = Fake::new().gh("pr checks", 0, &red, "");
    assert_eq!(
        observe_batch(&f, 700, 0, 0, 60),
        BatchOutcome::Red { failing: vec!["test".into()] }
    );

    // Pending inside the bound stays pending, and carries the elapsed time so a
    // caller cannot mistake it for a steady state.
    let f = Fake::new().gh("pr checks", 0, "[]", "");
    assert_eq!(observe_batch(&f, 700, 0, 59 * 60_000, 60), BatchOutcome::Pending { elapsed_ms: 59 * 60_000 });

    // At the bound: UNVERIFIABLE, loudly, naming the batch PR. This is the
    // repo-with-no-CI case that would otherwise pend forever.
    let f = Fake::new().gh("pr checks", 0, "[]", "");
    match observe_batch(&f, 700, 0, 60 * 60_000, 60) {
        BatchOutcome::Unverifiable { why } => {
            assert!(why.contains("#700") && why.contains("60 minutes"), "{why}");
        }
        other => panic!("the bound must fire, got {other:?}"),
    }
    // A just-pushed PR's "no checks reported" is Pending, not a bogus instant
    // anything — inherited from the shared classifier, not re-derived.
    let f = Fake::new().gh("pr checks", 1, "", "no checks reported on the 'x' branch");
    assert!(matches!(observe_batch(&f, 700, 0, 0, 60), BatchOutcome::Pending { .. }));
    // `gh` missing entirely → unavailable, never green (§10).
    let f = Fake::new().spawn_fail("gh");
    assert!(matches!(observe_batch(&f, 700, 0, 0, 60), BatchOutcome::Unverifiable { .. }));

    // Only Green may land.
    assert!(BatchOutcome::Green.may_land());
    for o in [
        BatchOutcome::Pending { elapsed_ms: 0 },
        BatchOutcome::Red { failing: vec![] },
        BatchOutcome::Unverifiable { why: "x".into() },
    ] {
        assert!(!o.may_land(), "{o:?} must never land");
    }
}

/// A clock that stepped backwards saturates to zero elapsed rather than
/// underflowing into a huge one: a backward step can extend the wait, never
/// fabricate an instant timeout that strands a healthy batch.
#[test]
fn a_backward_clock_step_cannot_fabricate_a_timeout() {
    let f = Fake::new().gh("pr checks", 0, "[]", "");
    assert_eq!(
        observe_batch(&f, 700, 1_000_000, 0, 60),
        BatchOutcome::Pending { elapsed_ms: 0 }
    );
}

// ── crash reconcile (§4) ────────────────────────────────────────────────────

fn state_with_batch(draft: Option<u64>) -> MergeQueueState {
    let mut s = MergeQueueState {
        target: "integration".into(),
        entries: vec![QueueEntry::new(612, HEAD_A, 0), QueueEntry::new(613, HEAD_B, 0)],
        ..Default::default()
    };
    for e in s.entries.iter_mut() {
        e.advance(EntryState::Batching).unwrap();
        e.advance(EntryState::CiWait).unwrap();
    }
    let mut rec = BatchRecord::new("mq-7f3a0000", vec![612, 613], 0);
    rec.draft_pr = draft;
    rec.advance(EntryState::CiWait).unwrap();
    s.batch = Some(rec);
    s
}

/// Resume **only** when the world matches the record: the scratch ref still on
/// the remote AND the draft PR still open.
#[test]
fn a_batch_resumes_only_when_the_scratch_ref_and_draft_pr_both_still_exist() {
    let f = Fake::new()
        .git("ls-remote", 0, "abc\trefs/heads/loomux/mq/g1-mq-7f3a0000\n", "")
        .gh("pr view", 0, r#"{"state":"OPEN"}"#, "");
    let mut s = state_with_batch(Some(640));
    let report = reconcile_batch(&f, &mut s, "g1");
    assert!(report.resumed);
    assert!(report.stranded.is_empty());
    assert!(s.batch.is_some(), "the record survives a resume");
    assert_eq!(s.entries[0].state(), EntryState::CiWait, "entries are left where they were");
    // Phase 1 collects notices; it never sends them (§4 — the lock is not
    // reentrant, which is what #467/#468 taught).
    assert_eq!(report.notices.len(), 1);
    assert!(report.notices[0].contains("resumed batch"));
    assert_eq!(pr_state_argv(640), vec!["pr", "view", "640", "--json", "state"]);
}

/// Every way the world can fail to match strands the batch **loudly** — and the
/// two "cannot tell" cases fail **closed**, because resuming onto a scratch ref
/// loomux cannot confirm is how the Bors invariant fails at the one point §8
/// does not guard.
#[test]
fn a_world_that_does_not_match_the_record_strands_the_batch_loudly() {
    let cases: Vec<(&str, Fake)> = vec![
        (
            "the scratch ref is gone",
            Fake::new().git("ls-remote", 2, "", "").gh("pr view", 0, r#"{"state":"OPEN"}"#, ""),
        ),
        (
            "the draft PR is closed",
            Fake::new()
                .git("ls-remote", 0, "abc\trefs/heads/x\n", "")
                .gh("pr view", 0, r#"{"state":"CLOSED"}"#, ""),
        ),
        (
            "the remote could not be asked (fail closed)",
            Fake::new()
                .git("ls-remote", 128, "", "fatal: could not read from remote")
                .gh("pr view", 0, r#"{"state":"OPEN"}"#, ""),
        ),
        (
            "the PR could not be read (fail closed)",
            Fake::new()
                .git("ls-remote", 0, "abc\trefs/heads/x\n", "")
                .gh("pr view", 1, "", "gh: not logged in"),
        ),
    ];
    for (label, f) in cases {
        let f = f.git("worktree remove", 0, "", "");
        let mut s = state_with_batch(Some(640));
        let report = reconcile_batch(&f, &mut s, "g1");
        assert!(!report.resumed, "{label} must not resume");
        assert_eq!(report.stranded.len(), 2, "{label}: both entries stranded");
        assert_eq!(report.transitions.len(), 2, "{label}: both transitions reported for audit");
        for t in &report.transitions {
            assert_eq!(t.to, EntryState::KickedBack, "{label}");
        }
        assert!(s.batch.is_none(), "{label}: the dead record is cleared");
        assert_eq!(report.notices.len(), 1, "{label}: one notice, collected not sent");
        assert!(report.notices[0].contains("could NOT be resumed"), "{label}");
        // Nothing landed, and nothing was pushed to recover.
        assert!(!f.calls().iter().any(|c| c.contains("git push origin ")), "{label}");
    }
}

/// Entries sitting in an in-flight state with **no batch record** are the crash
/// shape `plan_batch` refuses to dispatch on — so they are stranded rather than
/// left to block the queue forever.
#[test]
fn in_flight_entries_with_no_batch_record_are_stranded_not_left_to_block_the_queue() {
    let f = Fake::new();
    let mut s = state_with_batch(Some(640));
    s.batch = None; // the record was lost; the entries still say ci-wait
    let report = reconcile_batch(&f, &mut s, "g1");
    assert!(!report.resumed);
    assert_eq!(report.stranded.len(), 2);
    for (_, why) in &report.stranded {
        assert!(why.contains("no batch record"), "{why}");
    }
    assert!(f.calls().is_empty(), "no record means nothing to check on the remote");

    // A queue with nothing in flight reconciles to a no-op.
    let mut s = MergeQueueState {
        entries: vec![QueueEntry::new(612, HEAD_A, 0)],
        ..Default::default()
    };
    let report = reconcile_batch(&Fake::new(), &mut s, "g1");
    assert_eq!(report, Default::default());
    assert_eq!(s.entries[0].state(), EntryState::Queued);
}

// ── audited transitions and requeueing (§9, §11.5) ──────────────────────────

/// `advance_entry` is the audited wrapper over `mergeq`'s only sanctioned
/// mutation path: it reports the fact, and it refuses exactly what §4 refuses.
#[test]
fn advancing_an_entry_reports_the_fact_and_refuses_what_the_core_refuses() {
    let mut s = MergeQueueState {
        entries: vec![QueueEntry::new(612, HEAD_A, 0)],
        ..Default::default()
    };
    let t = advance_entry(&mut s, 612, EntryState::Batching).unwrap().expect("moved");
    assert_eq!((t.pr, t.from, t.to), (612, EntryState::Queued, EntryState::Batching));

    // A transition the design note does not enumerate is refused, and the entry
    // does not move.
    assert!(advance_entry(&mut s, 612, EntryState::Landed).is_err());
    assert_eq!(s.entries[0].state(), EntryState::Batching);

    // An entry that is not in the queue is not an error and is not invented.
    assert_eq!(advance_entry(&mut s, 999, EntryState::Cancelled), Ok(None));
}

/// Survivors requeue **at the front, in their original order** (§9): they were
/// never implicated, and making them wait behind newly-enqueued work would
/// punish them for a neighbour's failure.
#[test]
fn survivors_requeue_at_the_front_preserving_their_original_order() {
    let mut s = MergeQueueState {
        entries: vec![
            QueueEntry::new(612, HEAD_A, 0),
            QueueEntry::new(613, HEAD_B, 0),
            QueueEntry::new(614, HEAD_A, 0),
            QueueEntry::new(700, HEAD_B, 0), // enqueued later, must stay behind
        ],
        ..Default::default()
    };
    requeue_survivors(&mut s, &[613, 614]);
    assert_eq!(
        s.entries.iter().map(|e| e.pr).collect::<Vec<_>>(),
        vec![613, 614, 612, 700],
        "survivors first, in their own order; everyone else keeps their relative order"
    );
}

/// The target is a property of the work in the queue, not a setting: it is
/// released when the queue drains (§4), and held while any entry is live.
#[test]
fn the_target_is_released_only_when_the_queue_drains() {
    let mut s = MergeQueueState {
        target: "integration".into(),
        entries: vec![QueueEntry::new(612, HEAD_A, 0), QueueEntry::new(613, HEAD_B, 0)],
        batch: Some(BatchRecord::new("mq-1", vec![612], 0)),
        ..Default::default()
    };
    // One entry terminal, one still queued → the target is held.
    advance_entry(&mut s, 612, EntryState::Cancelled).unwrap();
    finish_batch(&mut s);
    assert!(s.batch.is_none());
    assert_eq!(s.target, "integration", "a live entry holds the target");

    // Everything terminal → released, and the campaign's corpses go with the
    // target: both are properties of the work that was in the queue, and a
    // finished campaign keeps neither (the pane counts `entries`, not just the
    // live ones — see `a_drained_queue_leaves_no_terminal_entries_behind`).
    advance_entry(&mut s, 613, EntryState::Cancelled).unwrap();
    finish_batch(&mut s);
    assert_eq!(s.target, "", "a drained queue releases its target");
    assert!(s.entries.is_empty(), "…and leaves no corpses: {:?}", s.entries);
    assert_eq!(prune_terminal(&mut s), 0, "there is nothing left to prune");

    // Pruning itself still drops only terminal entries, keeping order — pinned
    // on a queue that has NOT drained, which is the only place it now has work
    // to do.
    let mut live = MergeQueueState {
        target: "integration".into(),
        entries: vec![
            QueueEntry::new(700, HEAD_A, 0),
            QueueEntry::new(701, HEAD_B, 0),
            QueueEntry::new(702, HEAD_A, 0),
        ],
        ..Default::default()
    };
    advance_entry(&mut live, 700, EntryState::Cancelled).unwrap();
    advance_entry(&mut live, 702, EntryState::Cancelled).unwrap();
    assert_eq!(prune_terminal(&mut live), 2);
    assert_eq!(live.entries.iter().map(|e| e.pr).collect::<Vec<_>>(), vec![701]);

    // `record_batch` is the one place a batch record is installed.
    record_batch(&mut s, BatchRecord::new("mq-2", vec![700], 0));
    assert_eq!(s.batch.as_ref().unwrap().id, "mq-2");
}

// ── #710: the target does not outlive the work that established it ──────────

/// The two branches #710 is about, plus the default, answered live.
fn drain_fake() -> Fake {
    Fake::new()
        .gh("repo view", 0, "main\n", "")
        .gh("pr view 612", 0, &pr_json("integration/batch3", HEAD_A, "b"), "")
        .gh("pr view 705", 0, &pr_json("integration/batch4", HEAD_B, "b"), "")
        // Read unconditionally by `enqueue`; this gate declares no `ci-green`
        // clause, so the answer only has to exist.
        .gh("pr checks", 0, &checks_json(&[("build", "SUCCESS")]), "")
}

/// The live #710 sequence, up to the point each test is about: enqueue 612
/// (establishing `integration/batch3`), then cancel it — leaving a queue with
/// no live entry, no batch, and whatever the target does next.
///
/// Shared so the two properties below can each fail on their **own** assertion
/// rather than one of them tripping over the other's: a single test asserting
/// both would panic at the first and never execute the second, and a property
/// that never ran is a property nobody has seen fail.
fn cancelled_to_empty(f: &Fake, gate: &GateSpec) -> MergeQueueState {
    let mut s = MergeQueueState { version: MERGE_QUEUE_VERSION, ..Default::default() };
    assert_eq!(
        enqueue(f, &mut s, 612, None, true, gate, &verdict_map(HEAD_A, "b"), 0, false),
        EnqueueOutcome::Queued { position: 1 },
        "the first enqueue establishes the target, as it always has"
    );
    assert_eq!(s.target, "integration/batch3");
    // While that entry is live the drain-first refusal is exactly what it was —
    // asserted here so both tests below start from a queue that has genuinely
    // refused, not merely from one that was never asked.
    assert_eq!(
        enqueue(f, &mut s, 705, None, true, gate, &verdict_map(HEAD_B, "b"), 0, false),
        EnqueueOutcome::Refused { reason: TargetRefusal::BaseNotTarget.code() },
        "a live entry was approved against the old target and must not be retargeted"
    );
    assert_eq!(s.target, "integration/batch3", "a refused enqueue changes nothing");
    assert_eq!(cancel(&mut s, 612), CancelOutcome::Cancelled { was: EntryState::Queued });
    s
}

/// **#710, the wedge itself.** A queue that has drained takes the next
/// enqueue's target.
///
/// The refusal this relaxes is §4's drain-first rule, and it protects
/// *entries* — work already approved against the old target. With none left
/// there is nobody to protect, and the refusal becomes "this queue is already
/// landing elsewhere, drain it first" said about a queue that IS drained. Found
/// live: `queue_merge(705, "integration/batch4")` refused `base-not-target`
/// against `{"entries":[],"target":"integration/batch3"}` — a branch deleted
/// when that batch closed. `merge_queue.json` is persistent by design, so no
/// restart clears it: one cancelled batch wedged the group permanently.
#[test]
fn a_drained_queue_takes_the_next_enqueues_target() {
    let f = drain_fake();
    let gate = one_reviewer_gate();
    let mut s = cancelled_to_empty(&f, &gate);

    // The assertion form too, which must narrow what happens rather than be
    // contradicted by a residue nobody asserted anything about.
    let asserted = Some("integration/batch4");
    assert_eq!(
        enqueue(&f, &mut s, 705, asserted, true, &gate, &verdict_map(HEAD_B, "b"), 0, false),
        EnqueueOutcome::Queued { position: 1 },
        "a drained queue has no entry left to protect, so this establishes"
    );
    assert_eq!(s.target, "integration/batch4");
    assert_eq!(status_view(&s, true, 7)["target"], serde_json::json!("integration/batch4"));
}

/// **#710's other half:** the drained queue *says* it is drained.
///
/// Separate from the enqueue property on purpose — this is what a reader (an
/// orchestrator, or the chrome) sees, and a status that keeps naming a branch
/// the queue is not landing on is what sent a live orchestrator looking for a
/// queue to drain. It is also the state the empty-target readers have to
/// tolerate: `mergeqview::absent` already publishes `"target": ""` and
/// `src/mergequeue.ts` already guards for it, so this is not a new wire shape.
#[test]
fn a_drained_queue_reports_that_it_is_landing_nowhere() {
    let f = drain_fake();
    let gate = one_reviewer_gate();
    let s = cancelled_to_empty(&f, &gate);

    assert_eq!(s.target, "", "nothing is queued, so the queue is landing nowhere");
    let view = status_view(&s, true, 7);
    assert_eq!(view["target"], serde_json::json!(""));
    assert_eq!(view["entries"], serde_json::json!([]), "a terminal entry is not queued work");
    assert_eq!(view["enabled"], serde_json::json!(true));
    assert!(view.get("batch").is_none(), "no batch record, no batch key");
}

/// **A drained queue leaves no corpses** — reported live: the pane read
/// `merge queue · → integration/batch3 · 4 entries` against a queue whose four
/// entries were all cancelled and whose campaign was over.
///
/// Asserted through `mergeqview::project`, not only through `status_view`,
/// because the two views disagree about terminal entries and **only the second
/// one was ever clean**: `status_view` filters them (so the MCP tool never
/// showed this), while `project` renders `state.entries` and reports
/// `entries_total = state.entries.len()`, which is where the "4 entries" the
/// human read came from. A fix pinned only at `status_view` would assert the
/// surface that was already correct.
#[test]
fn a_drained_queue_leaves_no_terminal_entries_behind() {
    let f = drain_fake();
    let gate = one_reviewer_gate();

    // Four entries against batch-3, every one of them cancelled — the campaign
    // that wedged the group in the field.
    let mut s = MergeQueueState {
        version: MERGE_QUEUE_VERSION,
        target: "integration/batch3".into(),
        entries: (0..4).map(|i| QueueEntry::new(600 + i, HEAD_A, 0)).collect(),
        ..Default::default()
    };
    for i in 0..4 {
        assert_eq!(
            cancel(&mut s, 600 + i),
            CancelOutcome::Cancelled { was: EntryState::Queued }
        );
    }

    // The drain took the target AND the corpses with it.
    assert!(s.entries.is_empty(), "a finished campaign leaves nothing behind: {:?}", s.entries);
    assert_eq!(s.target, "");

    // The pane the human actually read.
    let view = loomux_lib::orchestration::mergeqview::project(
        &serde_json::to_string(&s).expect("state serializes"),
    );
    assert_eq!(view["status"], serde_json::json!("ok"));
    assert_eq!(view["entries_total"], serde_json::json!(0), "the header's count: {view}");
    assert_eq!(view["entries"], serde_json::json!([]));
    assert_eq!(view["target"], serde_json::json!(""));

    // …and re-establishing a target starts from an empty queue rather than
    // inheriting the old campaign's rows.
    assert_eq!(
        enqueue(&f, &mut s, 705, None, true, &gate, &verdict_map(HEAD_B, "b"), 0, false),
        EnqueueOutcome::Queued { position: 1 }
    );
    assert_eq!(s.target, "integration/batch4");
    assert_eq!(s.entries.len(), 1, "only the new campaign's entry: {:?}", s.entries);
}

/// **The drain keeps `kicked-back`**, and that asymmetry is deliberate.
///
/// A `cancelled` entry is a request that has been honoured; a `kicked-back` one
/// is the queue telling an owner something they have not acted on yet, and the
/// drain is exactly when someone goes to look. Deleting it there would leave
/// §4's "strand loudly" alive only in the audit log — and a pane showing nothing
/// is not loud. Pinned because it is the one place the corpse purge deliberately
/// does *less* than "drop everything terminal", so nothing but a test stops a
/// later tidy-up from collapsing the three states into one.
#[test]
fn the_drain_keeps_the_conversation_it_has_not_finished() {
    let mut s = MergeQueueState {
        version: MERGE_QUEUE_VERSION,
        target: "integration/batch3".into(),
        entries: vec![
            QueueEntry::new(612, HEAD_A, 0),
            QueueEntry::new(613, HEAD_B, 0),
            QueueEntry::new(614, HEAD_A, 0),
        ],
        ..Default::default()
    };
    // 613 bounced — through the transition the state machine actually allows
    // (`queued → batching → kicked-back`, §8's "a conflict costs no CI"), not by
    // teleporting an entry into a terminal state the table would refuse.
    advance_entry(&mut s, 613, EntryState::Batching).unwrap();
    advance_entry(&mut s, 613, EntryState::KickedBack).unwrap();
    assert_eq!(cancel(&mut s, 612), CancelOutcome::Cancelled { was: EntryState::Queued });
    assert_eq!(cancel(&mut s, 614), CancelOutcome::Cancelled { was: EntryState::Queued });

    assert_eq!(s.target, "", "the campaign is over either way");
    assert_eq!(
        s.entries.iter().map(|e| (e.pr, e.state())).collect::<Vec<_>>(),
        vec![(613, EntryState::KickedBack)],
        "the cancels are gone; the unanswered kick-back stays"
    );

    // And it is visible where the human looks — the pane, which counts every
    // entry rather than only the live ones.
    let view = loomux_lib::orchestration::mergeqview::project(
        &serde_json::to_string(&s).expect("state serializes"),
    );
    assert_eq!(view["entries_total"], serde_json::json!(1), "{view}");
    assert_eq!(view["entries"][0]["state"], serde_json::json!("kicked-back"), "{view}");
}

/// A **live** campaign keeps its corpses — but a bounded number of them.
///
/// Zero would drop the one durable row that tells a human *which* PR bounced;
/// unbounded lets terminal entries push live ones out of `project`'s
/// `VIEW_ENTRY_LIMIT` window, since it renders in file order and corpses sit at
/// the front. `MAX_ENTRIES` does not bound this — it counts only non-terminal
/// entries.
#[test]
fn a_live_campaign_bounds_the_corpses_it_keeps() {
    let f = drain_fake();
    let gate = one_reviewer_gate();

    // One live entry FIRST — so the queue never drains while the rest are
    // cancelled, and the release-on-drain path is not what is under test here.
    let mut s = MergeQueueState {
        version: MERGE_QUEUE_VERSION,
        target: "integration/batch3".into(),
        entries: std::iter::once(QueueEntry::new(612, HEAD_A, 0))
            .chain((0..20).map(|i| QueueEntry::new(800 + i, HEAD_A, 0)))
            .collect(),
        ..Default::default()
    };
    for i in 0..20 {
        assert_eq!(
            cancel(&mut s, 800 + i),
            CancelOutcome::Cancelled { was: EntryState::Queued }
        );
    }
    assert_eq!(s.entries.len(), 21, "612 holds the queue open, so nothing was released");
    assert_eq!(s.target, "integration/batch3");

    // A cheap refusal writes nothing — the trim happens on the path that was
    // already rewriting the file, not on every touch.
    assert_eq!(
        enqueue(&f, &mut s, 612, None, true, &gate, &verdict_map(HEAD_A, "b"), 0, false),
        EnqueueOutcome::Refused { reason: loomux_lib::orchestration::mqloop::refusal::ALREADY_QUEUED },
        "the live 612 is still queued"
    );
    assert_eq!(s.entries.len(), 21, "a cheap refusal leaves the file alone");

    // A real enqueue against the SAME target tidies as it writes.
    let f2 = drain_fake().gh("pr view 613", 0, &pr_json("integration/batch3", HEAD_B, "b"), "");
    assert_eq!(
        enqueue(&f2, &mut s, 613, None, true, &gate, &verdict_map(HEAD_B, "b"), 0, false),
        EnqueueOutcome::Queued { position: 2 },
        "612 and 613 are the live entries; the corpses are not queued work"
    );

    let terminal: Vec<u64> =
        s.entries.iter().filter(|e| e.state().is_terminal()).map(|e| e.pr).collect();
    assert_eq!(terminal.len(), MAX_TERMINAL_RETAINED, "bounded: {terminal:?}");
    assert_eq!(
        terminal,
        (12..20).map(|i| 800 + i).collect::<Vec<u64>>(),
        "the OLDEST are the ones dropped"
    );
    // The live work is still there, and still visible.
    assert!(s.entries.iter().any(|e| e.pr == 612 && !e.state().is_terminal()));
    assert!(s.entries.iter().any(|e| e.pr == 613 && !e.state().is_terminal()));
}

/// **A refused enqueue releases the stale target too** (rev N2).
///
/// The release is asked *before* the decision and does not depend on it, so the
/// first touch of a drained-but-stale queue stops it naming a branch it is not
/// landing on — rather than leaving that to the next restart's reconcile or to
/// whichever later enqueue happens to succeed. The refusal here is
/// constraint 7's (`base-is-default`), which fires before a target could be
/// established, so nothing writes the target back afterwards.
#[test]
fn a_refused_enqueue_still_releases_a_stale_target() {
    let f = drain_fake().gh("pr view 800", 0, &pr_json("main", HEAD_A, "b"), "");
    let mut s = MergeQueueState {
        version: MERGE_QUEUE_VERSION,
        target: "integration/batch3".into(),
        ..Default::default()
    };

    assert_eq!(
        enqueue(&f, &mut s, 800, None, true, &one_reviewer_gate(), &verdict_map(HEAD_A, "b"), 0, false),
        EnqueueOutcome::Refused { reason: TargetRefusal::BaseIsDefault.code() },
        "constraint 7 outranks everything and is unaffected by the release"
    );
    assert_eq!(s.target, "", "the queue stops naming a branch it is not landing on");
    assert_eq!(status_view(&s, true, 7)["target"], serde_json::json!(""));

    // A *disabled* queue is still byte-for-byte untouched (§12) — the release
    // sits after that refusal, not before it.
    let mut s = MergeQueueState {
        version: MERGE_QUEUE_VERSION,
        target: "integration/batch3".into(),
        ..Default::default()
    };
    let before = s.clone();
    assert_eq!(
        enqueue(&f, &mut s, 800, None, false, &one_reviewer_gate(), &verdict_map(HEAD_A, "b"), 0, false),
        EnqueueOutcome::Refused {
            reason: loomux_lib::orchestration::mqloop::refusal::QUEUE_DISABLED
        }
    );
    assert_eq!(s, before, "a repo that never opted in must see no state change at all");
}

/// #1778 §8.1's mutual refusal, the queue's half — **and the fixture
/// discriminates**, which is the whole of what makes it a pin.
///
/// The two calls below differ in exactly one bit: whether the review driver
/// holds this PR. Everything else — the state, the PR number, the gate, the
/// verdicts, the clock — is identical, so `in-review-drive` on the first and
/// something else on the second is a statement about that bit and not about a
/// fixture that could never have been enqueued anyway.
///
/// Until #1778 S4, `mqloop::refusal` had no name for this at all and `enqueue`
/// made no such check, so §8.1's sentence described a mechanism the queue did
/// not implement. Its opposite number is `rddrive::refusal::IN_MERGE_QUEUE`;
/// neither is spelled `already-…`, because `already-queued` is taken by the row
/// above for a different subject and a refusal string has to read correctly
/// without knowing which tool the caller called.
#[test]
fn the_queue_refuses_a_pr_the_review_driver_is_holding() {
    use loomux_lib::orchestration::mqloop::refusal;
    let f = drain_fake();
    let gate = one_reviewer_gate();
    let verdicts = verdict_map(HEAD_A, "b");

    let mut driven_state = MergeQueueState::default();
    let out_driven =
        enqueue(&f, &mut driven_state, 800, None, true, &gate, &verdicts, 0, true);
    assert_eq!(
        out_driven,
        EnqueueOutcome::Refused { reason: refusal::IN_REVIEW_DRIVE },
        "a PR the driver holds must be refused BY NAME, not merely not queued"
    );
    assert_eq!(
        driven_state,
        MergeQueueState::default(),
        "and refused before anything is written"
    );

    // The discriminating half: the same call with that one bit cleared does NOT
    // give this refusal. Whatever it answers, it is not this one — so the
    // assertion above is about the drive rather than about a fixture that could
    // never be enqueued.
    let mut free_state = MergeQueueState::default();
    let out_free = enqueue(&f, &mut free_state, 800, None, true, &gate, &verdicts, 0, false);
    assert_ne!(
        out_free,
        EnqueueOutcome::Refused { reason: refusal::IN_REVIEW_DRIVE },
        "the refusal must turn on the drive, not on the rest of the fixture: {out_free:?}"
    );
}

/// The other side of the same rule: **drained means both halves of §4** — no
/// non-terminal entry *and* no batch in flight.
///
/// The second half is the one a narrower fix would miss. `cancel` can take the
/// last live entry out from under an in-flight batch, and §10 abandons that
/// batch on the *next* driver tick, not synchronously — so between the two
/// there is a window with a scratch ref and a draft PR built for the old target
/// still on the remote. Retargeting there would leave the driver about to land
/// an object built from one branch onto another.
#[test]
fn a_queue_that_has_not_drained_still_refuses_a_different_branch() {
    let f = drain_fake();
    let gate = one_reviewer_gate();

    // (1) One entry terminal, one still queued — a partial drain is not a drain.
    let mut s = MergeQueueState {
        version: MERGE_QUEUE_VERSION,
        target: "integration/batch3".into(),
        entries: vec![QueueEntry::new(612, HEAD_A, 0), QueueEntry::new(613, HEAD_B, 0)],
        ..Default::default()
    };
    assert_eq!(cancel(&mut s, 612), CancelOutcome::Cancelled { was: EntryState::Queued });
    assert_eq!(s.target, "integration/batch3", "613 is still live and holds the target");
    assert_eq!(
        enqueue(&f, &mut s, 705, None, true, &gate, &verdict_map(HEAD_B, "b"), 0, false),
        EnqueueOutcome::Refused { reason: TargetRefusal::BaseNotTarget.code() }
    );

    // (2) Every entry terminal, but a batch still in flight.
    let mut s = MergeQueueState {
        version: MERGE_QUEUE_VERSION,
        target: "integration/batch3".into(),
        entries: vec![QueueEntry::new(612, HEAD_A, 0)],
        batch: Some(BatchRecord::new("mq-710", vec![612], 0)),
        ..Default::default()
    };
    assert_eq!(cancel(&mut s, 612), CancelOutcome::Cancelled { was: EntryState::Queued });
    assert_eq!(
        s.target, "integration/batch3",
        "an in-flight batch was built against this target and has not been abandoned yet"
    );
    assert_eq!(
        enqueue(&f, &mut s, 705, None, true, &gate, &verdict_map(HEAD_B, "b"), 0, false),
        EnqueueOutcome::Refused { reason: TargetRefusal::BaseNotTarget.code() },
        "the landing path reads state.target: retargeting under a live batch would \
         land an object built from one branch onto another"
    );
}

// ── the two notices (§5, §9) ────────────────────────────────────────────────

/// One decision-grade notice per event, and the two say **different** things:
/// collapsing them would tell an orchestrator a PR is at fault when none is.
#[test]
fn the_two_notices_carry_one_fact_each_and_are_not_interchangeable() {
    let culprit = culprit_notice(612, "mq-7f3a0000", &["build (windows)".into()], 2);
    assert!(culprit.contains("#612") && culprit.contains("mq-7f3a0000"));
    assert!(culprit.contains("build (windows)"), "the failing check is named: {culprit}");
    assert!(culprit.contains("2 siblings requeued"), "{culprit}");
    // Terse: a pointer, not a narration (§9).
    assert!(culprit.len() < 300, "decision-grade means short: {} chars", culprit.len());

    let unver = unverifiable_notice("mq-7f3a0000", Some(700), "no terminal checks within 60 minutes");
    assert!(unver.contains("UNVERIFIABLE"), "{unver}");
    assert!(
        unver.to_lowercase().contains("no pr is implicated"),
        "an unverifiable batch blames nobody: {unver}"
    );
    assert!(unver.contains("#700"), "points at the batch PR, not a sub-PR: {unver}");
    // The two must not be confusable.
    assert!(!unver.contains("culprit"));
    assert!(!culprit.contains("UNVERIFIABLE"));

    // Singular/plural is not a lie either way.
    assert!(culprit_notice(1, "b", &[], 1).contains("1 sibling requeued"));
    assert!(culprit_notice(1, "b", &[], 0).contains("0 siblings requeued"));
}

// ════════════════════════════════════════════════════════════════════════════
// The driver tick (#698) — the production caller the pipeline never had.
//
// Everything above this line tested a seam. #698 is what happens when every
// seam is green and nothing calls them: four approved PRs sat `queued` for 51
// minutes with zero driver actions in the audit log. These tests drive
// `mqloop::drive`, which is the function `gh_poll_tick` now calls; the wiring
// from the poll loop down to it is pinned in `tests/orchestration.rs`, because
// a test of the decision half alone is exactly what #698 already had.
// ════════════════════════════════════════════════════════════════════════════

const HEAD_C: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// The live head each fixture PR reports, so the recorded head, the gate's
/// verdict and the fetched pull ref all agree — a disagreement between any two
/// of those is a *different* test's subject.
fn head_of(pr: u64) -> &'static str {
    match pr {
        612 => HEAD_A,
        613 => HEAD_B,
        614 => HEAD_C,
        _ => PR_HEAD,
    }
}

/// A queue of `queued` entries on `integration`, which is the state the driver
/// is supposed to act on and #698 says it never did.
fn queued_state(prs: &[u64]) -> MergeQueueState {
    MergeQueueState {
        version: MERGE_QUEUE_VERSION,
        target: "integration".into(),
        entries: prs.iter().map(|p| QueueEntry::new(*p, head_of(*p), 0)).collect(),
        ..Default::default()
    }
}

/// A queue whose entries are already inside an in-flight batch at `draft_pr`.
///
/// **`batch_id` must be unique per test**, and that is not bookkeeping — the
/// driver's body files are named from the batch id (`loomux-mq-<id>-culprit.md`
/// in the OS temp dir, so two batches on one machine cannot collide, #625).
/// `cargo test` runs these in parallel, so two tests sharing a hardcoded id
/// share that path: one truncates it while the other's `gh` is reading, and the
/// body arrives empty. The first cut of these tests did exactly that and went
/// red on macOS only — the race, not the driver.
fn in_flight_state(batch_id: &str, prs: &[u64], draft_pr: u64) -> MergeQueueState {
    let mut s = queued_state(prs);
    let mut rec = BatchRecord::new(batch_id, prs.to_vec(), 0);
    rec.scratch_sha = MERGED.into();
    rec.draft_pr = Some(draft_pr);
    rec.advance(EntryState::CiWait).unwrap();
    for e in s.entries.iter_mut() {
        e.advance(EntryState::Batching).unwrap();
        e.advance(EntryState::CiWait).unwrap();
        e.batch = Some(batch_id.to_string());
    }
    s.batch = Some(rec);
    s
}

fn cfg(max_batch: u32, now_ms: u64) -> DriveConfig<'static> {
    DriveConfig { group: "g1", max_batch, checks_timeout_minutes: 60, now_ms }
}

/// A live `pass` from `rev-a` against each PR's own head.
fn live_verdicts(pr: u64) -> BTreeMap<BlockId, ReviewVerdict> {
    verdict_map(head_of(pr), "b")
}

/// **The invariant every path in the driver has to keep**: an entry is in an
/// in-flight state only while a batch record exists.
///
/// It is what makes `reconcile_batch`'s two rules a statement about a *crash*
/// rather than about a tick that was midway through its work — strand an
/// in-flight entry with no batch record, resume only when the world matches. A
/// driver that left the search between batch records would have every restart
/// kick back a set of innocent PRs.
fn assert_no_orphan_in_flight(s: &MergeQueueState, when: &str) {
    if s.batch.is_some() {
        return;
    }
    let orphans: Vec<u64> =
        s.entries.iter().filter(|e| e.state().in_flight()).map(|e| e.pr).collect();
    assert!(orphans.is_empty(), "{when}: {orphans:?} left in flight with no batch record");
}

/// Answers the whole build-observe-land sequence, so each test overrides only
/// the reply it is about. Constraint 3: no `git`, no `gh`, no network.
fn drive_fake() -> Fake {
    Fake::new()
        // §7's two live lookups, per examined entry and again at landing.
        .gh("repo view", 0, "main\n", "")
        .gh("pr view 612", 0, &pr_json("integration", HEAD_A, "b"), "")
        .gh("pr view 613", 0, &pr_json("integration", HEAD_B, "b"), "")
        .gh("pr view 614", 0, &pr_json("integration", HEAD_C, "b"), "")
        // §4's mint check: the name is free.
        .git("ls-remote", 2, "", "")
        // §8's construction.
        .git("fetch", 0, "", "")
        .git("rev-parse refs/remotes/loomux-mq/target", 0, TARGET_HEAD, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-612", 0, HEAD_A, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-613", 0, HEAD_B, "")
        .git("rev-parse refs/remotes/loomux-mq/pr-614", 0, HEAD_C, "")
        .git("worktree add", 0, "", "")
        .git("merge --no-ff", 0, "", "")
        .git("rev-parse HEAD", 0, MERGED, "")
        .git("worktree remove", 0, "", "")
        // The create-only scratch push, then the draft PR (§4, §5).
        .git("push --force-with-lease", 0, "", "")
        .gh("pr create", 0, "https://github.com/o/r/pull/641\n", "")
        // §10's cleanup. Matched BEFORE the landing push, whose argv is also a
        // `push origin`.
        .git("--delete", 0, "", "")
        .gh("pr close", 0, "", "")
        .gh("pr comment", 0, "", "")
        // §7.4's only landing verb.
        .git("push origin", 0, "", "")
}

/// **#698's headline: a non-empty queue with an idle target cuts a batch.**
///
/// This is the assertion whose absence is the whole issue. Everything it checks
/// was individually green while `merge_queue.json` sat at four `queued` entries
/// for 51 minutes, because nothing joined the seams up.
#[test]
fn the_driver_cuts_a_batch_from_the_queue_head_and_opens_its_draft_pr() {
    let f = drive_fake();
    let mut s = queued_state(&[612, 613, 614, 615]);
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    assert!(rep.changed, "cutting a batch is a state change that must be persisted");
    let b = s.batch.clone().expect("a queued, unblocked, idle-target queue must produce a batch");
    assert_eq!(b.prs, vec![612, 613, 614], "queue order, capped at max_batch");
    assert_eq!(b.scratch_sha, MERGED, "the SHA CI will judge is the one construction produced");
    assert_eq!(b.draft_pr, Some(641), "the draft PR number is read back from `gh pr create`");
    assert_eq!(b.state(), EntryState::CiWait);
    assert!(!b.is_probe(), "an ordinary batch is not a bisect probe");
    for pr in [612, 613, 614] {
        assert_eq!(s.entry(pr).unwrap().state(), EntryState::CiWait, "#{pr}");
        assert_eq!(s.entry(pr).unwrap().batch.as_deref(), Some(b.id.as_str()), "#{pr}");
    }
    assert_eq!(
        s.entry(615).unwrap().state(),
        EntryState::Queued,
        "beyond max_batch, and therefore never examined"
    );
    assert_no_orphan_in_flight(&s, "after a batch is cut");

    // §4's create-only push, at the argv level — the level that matters,
    // because every way of getting it wrong degrades to a silently successful
    // ordinary push.
    let branch = scratch_branch("g1", &b.id).expect("the batch id builds a ref name");
    assert_eq!(f.argv_containing("git", "--force-with-lease"), scratch_push_argv(MERGED, &branch));

    // §5's observation handle: a DRAFT PR from the scratch into the target.
    let create = f.argv_containing("gh", "pr create");
    assert_eq!(create[..4], ["pr", "create", "--draft", "--base"]);
    assert_eq!(create[4], "integration");
    assert_eq!(create[5], "--head");
    assert_eq!(create[6], branch);
    assert_eq!(create[8], batch_pr_title(&b.id, &b.prs));

    // The body that actually reached `gh` — read at the moment of the call,
    // since the driver deletes the file straight after.
    let body = f.bodies().into_iter().next().expect("the draft PR was given a body file");
    for pr in [612, 613, 614] {
        assert!(body.contains(&format!("- #{pr}")), "sub-PRs are listed: {body}");
    }
    // §8: loomux's own text must never carry GitHub's closing pattern.
    for kw in ["close #", "closes #", "fix #", "fixes #", "resolve #", "resolves #"] {
        assert!(!body.to_lowercase().contains(kw), "the batch body must not carry {kw:?}: {body}");
    }

    assert!(
        rep.audits.iter().any(|a| a.action == "mq-batch-built"),
        "every transition is audited; saw {:?}",
        rep.audits.iter().map(|a| a.action).collect::<Vec<_>>()
    );
}

/// **The gate is re-checked per entry at build time** (§6's first enforcement
/// point), and a refusal *blocks* the entry rather than removing it (§4:
/// "paused" is a live predicate, not a ninth state).
#[test]
fn a_build_time_gate_refusal_blocks_that_entry_and_the_batch_forms_without_it() {
    let f = drive_fake();
    let mut s = queued_state(&[612, 613, 614]);
    // 613's reviewer passed an EARLIER revision: the branch moved under them,
    // so what they approved is not what would land.
    let verdicts = |pr: u64| {
        if pr == 613 {
            verdict_map("an-older-head", "b")
        } else {
            live_verdicts(pr)
        }
    };
    drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &verdicts);

    let b = s.batch.as_ref().expect("the other two still batch");
    assert_eq!(b.prs, vec![612, 614], "a blocked entry is skipped, not fatal to the batch");
    let stale = s.entry(613).unwrap();
    assert_eq!(stale.state(), EntryState::Queued, "still queued — there is no ninth state");
    assert_eq!(
        stale.blocked_reason.as_deref(),
        Some("gate-not-met"),
        "the reason is recorded, from the closed refusal vocabulary, so a human can see it"
    );
    assert_eq!(s.entry(612).unwrap().blocked_reason, None, "an eligible entry carries no reason");
}

/// **A corrupt recorded target never reaches `git`** (§4, §7).
///
/// `state.target` comes off disk and is interpolated into a refspec by *every*
/// branch of the tick — the batch fetch's `+refs/heads/<target>:…` runs long
/// before `land_batch`'s own guard. A hand-edited or torn record that is not a
/// plain branch name has to fail loudly rather than be handed to `git` to find
/// out, which is the same posture §4 takes on every other unusable record.
#[test]
fn a_recorded_target_that_is_not_a_branch_name_stops_the_tick_before_any_call() {
    for bad in ["", "  ", "a:refs/heads/main", "-x", "refs/heads/main", "HEAD", "a..b", "a b"] {
        let f = drive_fake();
        let mut s = queued_state(&[612]);
        s.target = bad.to_string();
        let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

        assert!(f.calls().is_empty(), "target {bad:?} must reach no argv at all: {:?}", f.calls());
        assert!(s.batch.is_none(), "target {bad:?}");
        assert!(!rep.changed, "a corrupt record is not rewritten on the way past ({bad:?})");
        assert!(rep.backoff, "target {bad:?}");
        assert!(
            rep.audits.iter().any(|a| a.action == "mq-stranded"),
            "target {bad:?} must be audited loudly, saw {:?}",
            rep.audits.iter().map(|a| a.action).collect::<Vec<_>>()
        );
    }
    // …and the guard is scoped to a queue that has live work: a drained queue
    // has released its target (§4), and that empty string is not a corruption.
    let f = drive_fake();
    let mut s = queued_state(&[]);
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);
    assert!(rep.audits.is_empty() && !rep.backoff, "a drained queue is not a corrupt one");
}

/// **A batch id off disk is validated everywhere it lands, not just where it
/// becomes a ref** (rev-lead finding 1).
///
/// `scratch_branch` always validated it, so the ref namespace was closed. But
/// the same string off the same file also names the temp worktree
/// `git worktree add` creates and the body file `fs::write` fills, and those
/// took it raw — a record carrying `../…` picked both locations. The record is
/// now validated as a whole, before any path or argv is built from it.
#[test]
fn a_batch_id_that_is_not_a_name_stops_the_tick_before_any_path_is_built() {
    for bad in ["../../evil", "a/b", "", "  ", "-x", "id with spaces", "a\nb"] {
        // The batch's own checks are stubbed even though a guarded tick never
        // reaches them: without that reply the *fixture* panics on the first
        // unstubbed call, so the guard's absence would be reported as "no
        // canned reply" and this test's own assertions would never run. A red
        // that fires in the harness is not evidence that the assertion works —
        // exactly what the first witness of this test showed.
        let f = drive_fake().gh("pr checks 641", 0, "[]", "");
        let mut s = in_flight_state("mq-placehold", &[612], 641);
        s.batch.as_mut().unwrap().id = bad.to_string();
        let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

        assert!(
            f.calls().is_empty(),
            "batch id {bad:?} must reach no argv and no path at all: {:?}",
            f.calls()
        );
        assert!(!rep.changed, "a corrupt record is not rewritten on the way past ({bad:?})");
        assert!(rep.backoff, "batch id {bad:?}");
        assert!(
            rep.audits.iter().any(|a| a.action == "mq-stranded"),
            "batch id {bad:?} must be audited loudly"
        );
        // And the path builders refuse the name themselves, so the guarantee
        // does not depend on this guard having run first (rev-183).
        assert_eq!(
            scratch_worktree_path(bad),
            None,
            "batch id {bad:?} must not build a worktree path at all"
        );
    }
    // The predicate is the one `scratch_branch` already applied — named now, so
    // the three interpolations cannot drift apart.
    assert!(valid_id_component("mq-7f3a0000") && valid_id_component("g1"));
    let too_long = "x".repeat(65);
    for bad in ["../..", "a/b", "", "-g1", "g 1", too_long.as_str()] {
        assert!(!valid_id_component(bad), "{bad:?}");
        assert_eq!(scratch_branch("g1", bad), None, "{bad:?} as a batch id");
        assert_eq!(scratch_branch(bad, "mq-1"), None, "{bad:?} as a group id");
    }
}

/// **The path builders refuse a bad id themselves, so RECONCILE is covered too**
/// (rev-183).
///
/// `drive`'s record guard rejects an unusable batch id before the driver builds
/// any path from it — but `merge_queue_reconcile_with` runs **before** that guard
/// in the same tick, and `reconcile_batch` hands `cleanup_worktree` a batch id
/// straight off disk. So the guard's protection was *positional*: true for the
/// callers that happened to sit after it, and worth nothing to one that sits
/// before it. This drives the reconcile path directly, which is the one the
/// guard cannot reach.
#[test]
fn reconcile_refuses_to_build_a_path_from_a_batch_id_the_guard_never_saw() {
    let f = Fake::new()
        // `ls-remote` is never reached: `scratch_branch` refuses the name first.
        .gh("pr view", 0, r#"{"state":"OPEN"}"#, "");
    let mut s = in_flight_state("mq-placehold", &[612], 640);
    s.batch.as_mut().unwrap().id = "../../evil".into();

    let report = reconcile_batch(&f, &mut s, "g1");

    assert_eq!(
        report.cleanup_failed.as_deref(),
        Some("refusing to build a worktree path from batch id \"../../evil\""),
        "the refusal is REPORTED, on the channel §10 already has for leftovers"
    );
    assert!(
        !f.calls().iter().any(|c| c.contains("worktree")),
        "and no worktree path was handed to git at all: {:?}",
        f.calls()
    );
    // The entries are still stranded loudly — refusing to name a path is not a
    // reason to leave a batch in flight (§4).
    assert_eq!(s.entry(612).unwrap().state(), EntryState::KickedBack);
    assert!(s.batch.is_none());
}

/// **A stalled seam ends the selection pass; a refused PR does not**
/// (rev-lead finding 2).
///
/// The counts were bounded and the clock was not: `MAX_EXAMINED_PER_BUILD`
/// entries against a slow-but-answering remote is that many `MQ_CMD_TIMEOUT`s
/// back to back, inside the one loop that also delivers every `notify_when`
/// notice in the fleet — #656's point undone by a fan-out the counts do not
/// bound. The fix is the distinction `land()` already draws: a **runner**
/// failure is a fact about the world and the next entry answers nothing, while
/// a **refusal** is a fact about that PR and the next entry is a fresh question.
#[test]
fn a_stalled_lookup_stops_the_selection_pass_but_a_refused_pr_does_not() {
    // ── the world stalls: one lookup, then nothing.
    let f = Fake::new().spawn_fail("gh");
    let mut s = queued_state(&[612, 613, 614]);
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    assert_eq!(
        f.calls().len(),
        1,
        "the pass must stop on the FIRST unrunnable call, not spend one per entry: {:?}",
        f.calls()
    );
    assert!(s.batch.is_none(), "nothing is batched on half a picture");
    assert!(rep.backoff, "a stalled world is not re-derived on the next 30-second wake");
    assert!(
        rep.audits.iter().any(|a| a.action == "mq-batch-aborted"),
        "and it says so, with the runner's own message: {:?}",
        rep.audits
    );

    // ── one PR is refused: the remote answered, so the others are still worth
    //    asking about, and the batch forms without it.
    let f = drive_fake().gh_first("pr view 613", 0, &pr_json("some-other-branch", HEAD_B, "b"), "");
    let mut s = queued_state(&[612, 613, 614]);
    drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    let b = s.batch.as_ref().expect("a refusal is not a stall — the pass keeps going");
    assert_eq!(b.prs, vec![612, 614]);
    assert_eq!(
        s.entry(613).unwrap().blocked_reason.as_deref(),
        Some("base-not-target"),
        "the refusal the remote actually answered with, from the closed vocabulary"
    );
}

/// **A queue where nothing is eligible backs the group off.**
///
/// Establishing "everything is blocked" costs two `gh` round-trips per examined
/// entry, and a queue can sit blocked on a re-review for hours. Re-deriving that
/// on every 30-second wake would be hundreds of `gh` calls an hour for a group
/// doing nothing — the fan-out the whole tick design exists to avoid — so the
/// answer has to be "hold off", not "look again in 30 seconds".
#[test]
fn a_queue_with_nothing_eligible_holds_the_group_off_instead_of_re_asking() {
    let f = drive_fake();
    let mut s = queued_state(&[612, 613]);
    // Every reviewer passed an earlier revision, so no entry is batchable.
    let stale = |_pr: u64| verdict_map("an-older-head", "b");
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &stale);

    assert!(s.batch.is_none(), "an empty selection must not produce an empty batch");
    assert!(
        rep.backoff,
        "a fully-blocked queue re-asks the same two lookups per entry; that has to be rated"
    );
    for pr in [612, 613] {
        assert_eq!(s.entry(pr).unwrap().blocked_reason.as_deref(), Some("gate-not-met"), "#{pr}");
        assert_eq!(s.entry(pr).unwrap().state(), EntryState::Queued, "#{pr}");
    }
    assert!(rep.changed, "the reasons themselves are a state change worth persisting");
    // …and it did not go on to spend a build on nothing.
    assert!(
        !f.calls().iter().any(|c| c.contains("push") || c.contains("ls-remote")),
        "nothing was minted or pushed: {:?}",
        f.calls()
    );
}

/// **A green batch lands the tested object itself** (§8's Bors invariant) via
/// the one landing verb §7.4 permits, and clears the queue.
#[test]
fn a_green_batch_lands_the_tested_object_and_releases_the_target() {
    let f = drive_fake()
        .gh("pr checks 641", 0, &checks_json(&[("build", "SUCCESS"), ("test", "SKIPPED")]), "");
    let mut s = in_flight_state("mq-land0001", &[612, 613], 641);
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    // The refspec is exactly `<tested-sha>:refs/heads/<target>` — no `--force`,
    // no `+`, and built from the validated target and nothing else.
    assert_eq!(
        f.argv_containing("git", ":refs/heads/integration"),
        land_push_argv(MERGED, "integration")
    );
    assert!(s.batch.is_none(), "the batch record is cleared on landing");
    assert!(s.entry(612).is_none() && s.entry(613).is_none(), "landed entries are pruned");
    assert_eq!(s.target, "", "a drained queue releases its target");
    assert_no_orphan_in_flight(&s, "after landing");
    assert!(rep.audits.iter().any(|a| a.action == "mq-landed"));
    assert!(!rep.backoff, "a clean landing is not a reason to hold the group off");
    assert!(
        rep.notices.iter().any(|n| n.contains("landed on") && n.contains("#612")),
        "{:?}",
        rep.notices
    );

    // §10: cleanup runs on the green path too.
    let calls = f.calls();
    assert!(calls.iter().any(|c| c.contains("pr close 641")), "{calls:?}");
    assert!(calls.iter().any(|c| c.contains("--delete refs/heads/loomux/mq/")), "{calls:?}");
}

/// **A red batch of one is the culprit, with no further CI** (§9), and the
/// durable record is a comment on that PR.
#[test]
fn a_red_batch_of_one_attributes_the_culprit_and_leaves_the_comment() {
    let f = drive_fake()
        .gh("pr checks 641", 0, &checks_json(&[("build (windows)", "FAILURE")]), "")
        // The culprit's OWN checks are red too, so this is an ordinary
        // attribution rather than §9's infrastructure/flake case.
        .gh("pr checks 612", 0, &checks_json(&[("build (windows)", "FAILURE")]), "");
    let mut s = in_flight_state("mq-culprit1", &[612], 641);
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    assert!(s.entry(612).is_none(), "the culprit is kicked back and pruned");
    assert!(s.batch.is_none());
    assert_no_orphan_in_flight(&s, "after attribution");
    // §9 spends no further CI at k = 1: nothing is rebuilt or re-pushed.
    assert!(
        !f.calls().iter().any(|c| c.contains("--force-with-lease")),
        "k = 1 must not build another scratch: {:?}",
        f.calls()
    );

    let comment = f.bodies().into_iter().next().expect("a comment body was written");
    assert!(comment.contains("build (windows)"), "the failing check is named: {comment}");
    assert!(comment.contains("batched alone"), "{comment}");
    assert!(comment.contains("**a** culprit"), "the honest limit is stated: {comment}");
    for kw in ["close #", "closes #", "fix #", "fixes #", "resolve #", "resolves #"] {
        assert!(!comment.to_lowercase().contains(kw), "no closing keyword: {comment}");
    }
    assert!(rep.audits.iter().any(|a| a.action == "mq-culprit"));
    assert!(rep.notices.iter().any(|n| n.contains("isolated as batch")), "{:?}", rep.notices);
}

/// **§9's infrastructure/flake case**: still red at k = 1 while that PR's own
/// checks are green. Surfaced as what it is, and not looped on.
#[test]
fn a_red_batch_of_one_whose_pr_is_green_alone_is_surfaced_as_infrastructure() {
    let f = drive_fake()
        .gh("pr checks 641", 0, &checks_json(&[("build (windows)", "FAILURE")]), "")
        .gh("pr checks 612", 0, &checks_json(&[("build (windows)", "SUCCESS")]), "");
    let mut s = in_flight_state("mq-flake001", &[612], 641);
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    let notice = rep.notices.join("\n");
    assert!(
        notice.contains("infrastructure/flake"),
        "a green-alone PR must not be reported as a bad diff: {notice}"
    );
    assert!(notice.contains("OWN checks are green"), "{notice}");
    // It is still kicked back, exactly once — surfaced, not looped on.
    assert!(s.entry(612).is_none());
    assert!(s.batch.is_none());
}

/// **The bisect runs across ticks, and attributes from a GREEN probe** (§9).
///
/// This is the case that decides whether the search can be a state machine at
/// all. A probe that exonerates its half leaves the culprit in the other half —
/// so the tick that names the culprit has no failing checks of its own to
/// report, and by then every sibling has already gone back to `queued` and is
/// indistinguishable from newly enqueued work. Both facts have to have been
/// carried from the original red batch (`mergeq::BisectSearch`), or the comment
/// §9 requires is a comment that names neither the failure nor the combination.
#[test]
fn the_search_runs_across_ticks_and_names_the_culprit_from_a_green_probe() {
    let f = drive_fake()
        .gh("pr checks 640", 0, &checks_json(&[("build (windows)", "FAILURE")]), "")
        .gh("pr checks 641", 0, &checks_json(&[("build (windows)", "SUCCESS")]), "")
        .gh("pr checks 614", 0, &checks_json(&[("build (windows)", "FAILURE")]), "");
    let mut s = in_flight_state("mq-search01", &[612, 613, 614], 640);

    // ── tick 1: the batch goes red, the search opens, the first probe is built
    let rep1 = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);
    let probe = s.batch.clone().expect("a red batch of three opens a search, in the same tick");
    assert!(probe.is_probe(), "the record says it is a probe, so no green of its can land");
    assert_eq!(probe.prs, vec![612, 613], "larger half first, so k=3 splits 2/1");
    let search = probe.bisect.clone().unwrap();
    assert_eq!(search.origin_prs, vec![612, 613, 614], "the whole batch is the sibling set");
    assert_eq!(search.failing, vec!["build (windows)".to_string()]);
    for pr in [612, 613, 614] {
        assert_eq!(s.entry(pr).unwrap().state(), EntryState::Bisecting, "#{pr}");
    }
    assert_no_orphan_in_flight(&s, "mid-search");
    assert!(rep1.audits.iter().any(|a| a.action == "mq-bisect-step"));

    // ── tick 2: the probe is GREEN, so 612/613 are exonerated and 614 is it
    let rep2 = drive(&f, &mut s, &cfg(3, 2_000), &one_reviewer_gate(), &live_verdicts);
    assert!(s.batch.is_none(), "attribution ends the search");
    assert!(s.entry(614).is_none(), "the culprit is kicked back and pruned");
    for pr in [612, 613] {
        let e = s.entry(pr).unwrap();
        assert_eq!(e.state(), EntryState::Queued, "#{pr} was exonerated and requeued");
        assert_eq!(e.batch, None, "#{pr} is no longer in any batch");
    }
    assert_eq!(
        s.entries.iter().map(|e| e.pr).collect::<Vec<_>>(),
        vec![612, 613],
        "survivors keep their ORIGINAL queue order, not the order the search cleared them in"
    );
    assert_no_orphan_in_flight(&s, "after the search");

    // The comment carries what only the carried search could supply: the
    // original failing check, and the full sibling set.
    let comment = f.bodies().last().cloned().expect("a culprit comment was written");
    assert!(
        comment.contains("build (windows)"),
        "the ORIGINAL red batch's failing check, on a tick whose own observation was green: \
         {comment}"
    );
    assert!(
        comment.contains("batched with: #612, #613"),
        "the full sibling set, so a pairwise interaction is visible: {comment}"
    );
    assert!(rep2.audits.iter().any(|a| a.action == "mq-culprit"));
    assert!(rep2.notices.iter().any(|n| n.contains("#614")), "{:?}", rep2.notices);
}

/// **Unverifiable is not red, and nothing lands** (§5). The bound is the
/// backstop, not the mechanism: the checks never went terminal.
#[test]
fn an_unverifiable_batch_requeues_everything_and_implicates_nobody() {
    // An empty check list is PENDING, not success — the property §5 says
    // matters most — so this batch is still pending when the bound expires.
    let f = drive_fake().gh("pr checks 641", 0, "[]", "");
    let mut s = in_flight_state("mq-unver001", &[612, 613], 641);
    // 61 minutes past `started_ms`, against the default 60-minute bound.
    let rep = drive(&f, &mut s, &cfg(3, 61 * 60_000), &one_reviewer_gate(), &live_verdicts);

    assert!(s.batch.is_none());
    for pr in [612, 613] {
        assert_eq!(s.entry(pr).unwrap().state(), EntryState::Queued, "#{pr} requeued");
    }
    assert_eq!(s.target, "integration", "a requeued queue keeps its target");
    assert!(
        !f.calls().iter().any(|c| c.contains(":refs/heads/integration")),
        "nothing lands on an answer loomux never got: {:?}",
        f.calls()
    );
    assert!(rep.audits.iter().any(|a| a.action == "mq-checks-unverifiable"));
    assert!(rep.backoff, "an unverifiable batch holds the group off rather than retrying at once");
    let notice = rep.notices.join("\n");
    assert!(notice.contains("UNVERIFIABLE") && notice.contains("no PR is implicated"), "{notice}");
}

/// **A pending batch is the steady state**: nothing moves, nothing is written,
/// nothing is said.
///
/// The `changed` half is the one with teeth — a driver that rewrote
/// `merge_queue.json` on every 30-second poll of a 40-minute CI run would churn
/// the file ~80 times per batch for no state change at all.
#[test]
fn a_pending_batch_changes_nothing_and_says_nothing() {
    let f = drive_fake().gh("pr checks 641", 0, &checks_json(&[("build", "IN_PROGRESS")]), "");
    let mut s = in_flight_state("mq-pending1", &[612, 613], 641);
    let before = s.clone();
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    assert_eq!(s, before, "a pending observation moves nothing");
    assert!(!rep.changed, "…and must not trigger a write");
    assert!(rep.audits.is_empty() && rep.notices.is_empty(), "…or any noise");
    assert!(!rep.backoff);
}

/// **A conflict costs no CI** (§8): the entry kicks back before anything is
/// pushed, and the batch rebuilds without it on a later tick.
#[test]
fn a_conflicting_entry_kicks_back_before_any_push() {
    let f = drive_fake().git_first("merge --no-ff", 1, "", "CONFLICT (content): merge conflict");
    let mut s = queued_state(&[612, 613]);
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    assert!(s.batch.is_none(), "no batch was recorded");
    assert!(s.entry(612).is_none(), "the conflicting entry is kicked back and pruned");
    assert_eq!(s.entry(613).unwrap().state(), EntryState::Queued, "its sibling did nothing wrong");
    assert_no_orphan_in_flight(&s, "after a conflict");
    assert!(
        !f.calls().iter().any(|c| c.contains("push")),
        "nothing is pushed and no CI is spent: {:?}",
        f.calls()
    );
    assert!(rep.audits.iter().any(|a| a.action == "mq-kicked-back"));
}

/// **A batch that cannot be constructed aborts loudly and backs the group off**
/// (§10) — entries return to `queued`, and nothing is left half-shaped.
#[test]
fn a_batch_that_cannot_be_built_aborts_and_returns_its_entries_to_queued() {
    let f = drive_fake().git_first("fetch", 1, "", "fatal: could not read from remote repository");
    let mut s = queued_state(&[612, 613]);
    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);

    assert!(s.batch.is_none());
    for pr in [612, 613] {
        assert_eq!(s.entry(pr).unwrap().state(), EntryState::Queued, "#{pr} did nothing wrong");
    }
    assert_no_orphan_in_flight(&s, "after an aborted build");
    assert!(
        rep.audits.iter().any(|a| a.action == "mq-batch-aborted"),
        "the audit action names what happened — not a `built` event with a falsy field"
    );
    assert!(rep.backoff, "§10's abort rows requeue, so the rate has to be bounded somewhere");
    assert!(rep.notices.iter().any(|n| n.contains("ABORTED")), "{:?}", rep.notices);
}

/// **One in-flight batch per target** (§4), held on the entries alone when the
/// batch record is missing — the crash shape `reconcile_batch` owns. The driver
/// must not race a second batch onto a target it cannot reason about.
#[test]
fn the_driver_never_dispatches_a_second_batch_over_an_inconsistent_file() {
    let f = drive_fake();
    let mut s = queued_state(&[612, 613]);
    s.entries[0].advance(EntryState::Batching).unwrap();
    s.entries[0].advance(EntryState::CiWait).unwrap();
    assert!(s.batch.is_none(), "precondition: the crash shape");

    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);
    assert!(s.batch.is_none(), "no second batch was dispatched");
    assert!(!rep.changed);
    assert!(f.calls().is_empty(), "and nothing was spawned to find that out: {:?}", f.calls());
}

/// A cancelled member abandons the batch rather than landing without it (§10).
#[test]
fn a_cancelled_member_abandons_the_in_flight_batch() {
    let f = drive_fake();
    let mut s = in_flight_state("mq-cancel01", &[612, 613], 641);
    s.entries[0].advance(EntryState::Cancelled).unwrap();

    let rep = drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);
    assert!(s.batch.is_none(), "the batch is abandoned");
    assert_eq!(s.entry(613).unwrap().state(), EntryState::Queued, "the survivor requeues");
    assert!(
        !f.calls().iter().any(|c| c.contains("pr checks")),
        "a void batch is not worth a `gh pr checks`: {:?}",
        f.calls()
    );
    assert!(rep.audits.iter().any(|a| a.action == "mq-batch-aborted"));
    assert_no_orphan_in_flight(&s, "after a cancel abandoned the batch");
    assert_eq!(s.target, "integration", "613 requeued, so the target is still held");

    // …and when the cancels took the last live entry with them, abandoning the
    // batch is also the transition that drains the queue. That path cleared the
    // batch record without asking §4's release question, which is how #710's
    // wedge was reached in the field: a cancelled batch, an empty queue, and a
    // target nothing would ever clear.
    let f = drive_fake();
    let mut s = in_flight_state("mq-cancel02", &[612], 641);
    s.entries[0].advance(EntryState::Cancelled).unwrap();
    drive(&f, &mut s, &cfg(3, 1_000), &one_reviewer_gate(), &live_verdicts);
    assert!(s.batch.is_none(), "the batch is abandoned");
    assert_eq!(s.target, "", "a queue drained by cancellation is landing nowhere");
}

/// The draft PR's number comes back from `gh pr create`'s URL, and an answer
/// this parser cannot read is an error rather than a guess — a batch attached
/// to the wrong PR number would observe, and close, somebody else's work.
#[test]
fn the_draft_pr_number_is_parsed_from_ghs_url_or_refused() {
    assert_eq!(parse_created_pr("https://github.com/o/r/pull/641\n"), Some(641));
    // Chatter before the URL, and the LAST such line wins.
    assert_eq!(
        parse_created_pr(
            "Warning: something\nhttps://github.com/o/r/pull/12\nhttps://github.com/o/r/pull/34\n"
        ),
        Some(34)
    );
    assert_eq!(parse_created_pr("https://github.example.com/o/r/pull/7?foo=1"), Some(7));
    for bad in
        ["", "created\n", "https://github.com/o/r/issues/641", "https://github.com/o/r/pull/x"]
    {
        assert_eq!(parse_created_pr(bad), None, "{bad:?} must not become a PR number");
    }
}
