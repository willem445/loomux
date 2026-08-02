//! The bisecting merge queue — **the driver loop** (#581 slice D2).
//!
//! Design note: `doc/design/merge-queue.md`. §4, §8, §9, §10 and §11.3 are the
//! spec this file implements.
//!
//! # The three layers, and why this is the third file
//!
//! - `mergeq.rs` (slice C) — the **pure core**. Which entries batch, which half
//!   bisects next, whether the gate holds. No I/O at all.
//! - `mqdriver.rs` (slice D1) — the **write primitives and their gates**. The
//!   `MqRunner` seam, the live lookups, the constraint-7 refusal core, the
//!   create-only scratch push, the landing function, cleanup.
//! - this file (slice D2) — the **loop that sequences them**: build the scratch,
//!   open the draft PR, observe the checks under a bound, bisect a red batch and
//!   attribute it, requeue the survivors, reconcile after a crash, and persist
//!   every step.
//!
//! `mod.rs` gets registry wiring only — a module declaration, a guard field, and
//! the two `merge_queue_reconcile*` methods, which resolve paths and delegate.
//! No decision in this feature lives there. **`mcp.rs` is untouched by this
//! slice**: §11.1's three agent-callable tools are slice E's, so D2 adds no
//! agent-reachable surface at all (said explicitly because "no role gate found"
//! and "no role gate needed" look identical from a diff).
//!
//! # Why `build_scratch` uses a temporary worktree
//!
//! §8 records the *shape* of batch construction — the current integration head
//! plus one merge commit per queued PR — and names [`build_scratch`] as the
//! one-function reversal seam. It does not fix the git mechanism, and the two
//! candidates are not equivalent:
//!
//! - `git merge-tree --write-tree` + `git commit-tree` needs no worktree and is
//!   the tidier plumbing — but `--write-tree` arrived in **git 2.38**, so
//!   depending on it silently raises this product's git floor. That is an
//!   operator-setup assumption, which CLAUDE.md constraint 8 forbids, and it is
//!   not hypothetical: the machine this was written on has a git that rejects
//!   the flag outright.
//! - `git worktree add` + `git merge` is portable to every git this project
//!   already supports, and it is a primitive the repo **already depends on** —
//!   `git.rs::create_worktree` is how every agent workspace is made. No new
//!   floor, no new failure class the codebase has not already met.
//!
//! So: a detached worktree under the OS temp dir, merged in queue order, read
//! back with `rev-parse`, and removed on every exit path. The cost is disk and a
//! cleanup obligation; the benefit is that the queue works on the git the user
//! already has. Reversing to squash replay is still a rewrite of this one
//! function **plus the landing verb**, exactly as §8 warns.

use super::mergeq::{
    bisect_step, recheck_gate, scratch_branch, BatchRecord, BisectStep, EntryState, GateSpec,
    InvalidTransition, MergeQueueState, PrObservation, QueueEntry, MAX_ENTRIES,
};
use super::mergeqview::MERGE_QUEUE_FILE;
use super::mqdriver::{
    as_args, classify_checks, pr_checks_argv, pr_ci_green, resolve_and_validate_target,
    scratch_exists, BatchVerification, MqRunner, REMOTE,
};
use super::notify::sanitize_gh_text;
use super::workflow::{body_digest, BlockId, ReviewVerdict};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Cap on any single gh-sourced string this module formats into a PR comment or
/// a notice — a check name, a `git` stderr line. `notify.rs` caps its notices
/// the same way and for the same reason: the text crosses a trust boundary.
const MAX_QUOTED: usize = 200;

/// How many sub-PRs a batch comment will name before it says it truncated.
/// Bounded because the sibling list goes into a comment on someone else's PR,
/// and **stated** because a capped list that reads as complete is the failure
/// `.loomux/lessons.md` files under "no silent caps".
const MAX_SIBLINGS_LISTED: usize = 16;

// ── persistence (§11.3) ─────────────────────────────────────────────────────

/// Why `merge_queue.json` could not be turned into state this build may act on.
///
/// All three are **loud**: §4's reconcile refuses to guess, and the driver's
/// contract on [`StateError::Unsupported`] is to leave the file **untouched**,
/// which is the only way "an older build does not destroy what a newer one
/// wrote" survives a version bump that changes meanings rather than adding keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateError {
    /// Present and unparseable. Never silently replaced with a fresh queue —
    /// that would drop entries a human believes are queued.
    Malformed(String),
    /// A schema this build does not understand. Do not operate; do not write.
    Unsupported(u32),
    /// The file is there and could not be read at all.
    Io(String),
}

/// The group's queue file.
pub fn state_path(group_dir: &Path) -> PathBuf {
    group_dir.join(MERGE_QUEUE_FILE)
}

/// Read the group's queue state.
///
/// **An absent file is an empty queue, not an error** — that is the product
/// default (§12: no `merge_queue:` block, nothing ever enqueued). Every other
/// failure is a [`StateError`], because the difference between "nothing is
/// queued" and "loomux cannot tell what is queued" is the whole point of §4's
/// loud reconcile, and `mergeqview.rs` already draws exactly that distinction
/// for the read-only chrome.
pub fn load_state(group_dir: &Path) -> Result<MergeQueueState, StateError> {
    let text = match std::fs::read_to_string(state_path(group_dir)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MergeQueueState::default())
        }
        Err(e) => return Err(StateError::Io(e.to_string())),
    };
    let state: MergeQueueState =
        serde_json::from_str(&text).map_err(|e| StateError::Malformed(e.to_string()))?;
    if !state.version_supported() {
        return Err(StateError::Unsupported(state.version));
    }
    Ok(state)
}

/// Write the queue state atomically, reusing `mod.rs`'s `atomic_write` — the
/// #133-hardened writer (same-directory temp, `sync_all` before the rename, a
/// fallback that keeps the temp on failure).
///
/// Deliberately not a fresh `fs::write` here: a disk-full `fs::write` is what
/// truncated `tasks.json` and destroyed a live board in #133, and this file has
/// exactly the same "losing it loses queued work" property. One hardened writer,
/// not two.
pub fn store_state(group_dir: &Path, state: &MergeQueueState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    super::atomic_write(&state_path(group_dir), &bytes).map_err(|e| e.to_string())
}

// ── batch construction (§8) — THE REVERSAL SEAM ─────────────────────────────

/// A constructed scratch: the branch name it will be pushed to, and the SHA CI
/// will judge — and, on green, the exact object that lands (§8's Bors
/// invariant).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScratchRef {
    pub branch: String,
    pub sha: String,
    /// The target head the batch was built on. Recorded so a later landing can
    /// say what moved when the fast-forward push fails.
    pub target_head: String,
}

/// Why a batch could not be constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchBuildError {
    /// The speculative merge conflicted on this PR. That entry kicks back
    /// **immediately**, before anything is pushed, and the batch rebuilds
    /// without it — §8's "conflicts cost no CI".
    Conflict { pr: u64 },
    /// A PR's live head is not the head the entry recorded: it was rebased
    /// while the batch was being built, so its verdicts are already dead (§6).
    /// Kicked back the same way a conflict is, and for a better reason.
    HeadMoved { pr: u64, expected: String, actual: String },
    /// The target branch could not be resolved on the remote.
    NoTargetHead(String),
    /// Any other `git` failure. Aborts the batch; entries return to `queued`.
    Git(String),
}

/// The `mq-`-prefixed directory name a batch's temp worktree uses. Prefixed so
/// the directory is identifiable as this feature's at a glance — in a `git
/// worktree list`, in the OS temp dir, or in an incident.
fn worktree_dir_name(batch_id: &str) -> String {
    format!("loomux-mq-{batch_id}")
}

/// Where a batch's temporary worktree lives: **the OS temp dir, under an `mq-`
/// bearing name built from the batch id** — which is `RandomState`-derived
/// (§11.4), so two concurrent builds cannot collide on it.
///
/// **Never inside the human's clone.** A stray directory there shows up in their
/// `git status`, in every editor's file tree, and in their next `git clean`; the
/// merge queue's speculative objects are loomux's business and belong nowhere a
/// human is reading their own working tree. The group dir would be the other
/// defensible home, and OS temp wins only because a batch worktree is genuinely
/// ephemeral — it does not survive the batch, so it does not want to live beside
/// the state that does.
///
/// The path is a pure function of the batch id, which is what lets every
/// teardown ([`remove_worktree`]) name it exactly rather than search for it.
pub fn scratch_worktree_path(batch_id: &str) -> PathBuf {
    std::env::temp_dir().join(worktree_dir_name(batch_id))
}

/// The one fetch a batch build performs: the target branch, plus every queued
/// PR's head, into remote-tracking refs this module owns.
///
/// `refs/pull/<n>/head` rather than the PR's branch name: the branch may live in
/// a fork loomux has no remote for, while the pull ref is always present on the
/// upstream repository. Fetched into `refs/remotes/loomux-mq/*` so nothing here
/// disturbs the user's own `refs/remotes/origin/*`.
pub fn batch_fetch_argv(target: &str, prs: &[u64]) -> Vec<String> {
    let mut v = vec!["fetch".to_string(), "--no-tags".into(), REMOTE.into()];
    v.push(format!("+refs/heads/{target}:refs/remotes/loomux-mq/target"));
    for pr in prs {
        v.push(format!("+refs/pull/{pr}/head:refs/remotes/loomux-mq/pr-{pr}"));
    }
    v
}

/// **Build the batch's scratch object** (§8). The reversal seam: nothing outside
/// this function knows how the scratch was built, and every downstream stage
/// takes a SHA.
///
/// ```text
/// scratch = target_head
/// for entry in batch:                        # queue order, deterministic
///     scratch = merge(scratch, entry.pr_head)
/// ```
///
/// Queue order is part of the contract, not an implementation detail: §9
/// requeues bisect survivors preserving it, so two builds of the same entry set
/// must produce the same sequence of merges.
///
/// `heads` is `(pr, the head the entry recorded)`. Each fetched pull ref is
/// checked against it before merging — a PR rebased since the batch was planned
/// has lost its verdicts (§6), so merging its new head would build an object
/// nobody approved. That is [`BatchBuildError::HeadMoved`], and it kicks the
/// entry back rather than aborting the batch.
///
/// The temp worktree is torn down on **every** exit path — success, conflict,
/// moved head, and `git` failure alike — and whether that teardown worked is
/// **returned**, never swallowed. See [`ScratchBuild`].
pub fn build_scratch(
    r: &dyn MqRunner,
    batch_id: &str,
    branch: &str,
    target: &str,
    heads: &[(u64, String)],
) -> ScratchBuild {
    let prs: Vec<u64> = heads.iter().map(|(p, _)| *p).collect();
    let fetch = batch_fetch_argv(target, &prs);
    match r.git(&as_args(&fetch)) {
        // Both of these return before `worktree add` runs, so there is nothing
        // to tear down and `cleanup_failed` is genuinely None rather than
        // unchecked.
        Err(e) => return ScratchBuild::failed(BatchBuildError::Git(e)),
        Ok(o) if !o.ok() => {
            return ScratchBuild::failed(BatchBuildError::NoTargetHead(quote(&o.stderr)))
        }
        Ok(_) => {}
    }

    let wt = scratch_worktree_path(batch_id);
    let wt_s = wt.display().to_string();
    // Bound to a local rather than `?`-propagated: the teardown below has to run
    // between the build and the return, on every one of the build's outcomes.
    let result = build_in_worktree(r, &wt_s, branch, heads);
    let cleanup_failed = remove_worktree(r, &wt);
    ScratchBuild { result, cleanup_failed }
}

/// What a batch build produced, **and** whether its temp worktree went away.
///
/// The two are separate fields because §10 makes them separate outcomes:
/// **cleanup failure never fails a batch**. A leaked worktree is cheap; a green
/// batch held hostage by a failed `git worktree remove` is not. So the caller
/// lands or bisects on `result` and audits `mq-cleanup-failed` on
/// `cleanup_failed`, and it cannot do the second by forgetting — the field is
/// there in the value it already has to destructure.
///
/// An earlier cut dropped this on the floor while its comment claimed "the
/// caller audits it on the next sweep". There was no sweep and no channel for
/// the caller to learn: a claim with no code behind it, which is the one defect
/// class `.loomux/lessons.md` opens with. Returning it is what makes the comment
/// true.
#[derive(Debug)]
pub struct ScratchBuild {
    pub result: Result<ScratchRef, BatchBuildError>,
    /// `Some(why)` when the temp worktree could not be removed. Audit it; do not
    /// act on it.
    pub cleanup_failed: Option<String>,
}

impl ScratchBuild {
    fn failed(e: BatchBuildError) -> ScratchBuild {
        ScratchBuild { result: Err(e), cleanup_failed: None }
    }
}

/// Remove one batch's temp worktree, by the exact path built from its batch id.
///
/// `None` when there is nothing to remove or the removal worked; `Some(why)`
/// when a directory is left behind.
///
/// **Existence is checked first**, so a build that failed before `worktree add`
/// ever ran does not report a spurious cleanup failure for a worktree that was
/// never created — the alternative is matching `git`'s "is not a working tree"
/// stderr, which is a message string, not a contract.
///
/// **No `git worktree prune`, ever.** Prune is a *sweep*: it walks
/// `.git/worktrees/` and drops every admin entry whose directory it cannot see.
/// That directory is shared with every other loomux worktree on this machine —
/// the same shared-`.git` hazard that made `git stash` a banned verb here after
/// #299 — so a peer's workspace that is momentarily unreachable (a disconnected
/// network path, a volume not yet mounted) is a peer's workspace prune will
/// happily forget. This is §10's "namespace only, by exact name, never a pattern
/// sweep" applied to worktrees instead of refs. A leaked admin entry is cheap
/// and inert; a pruned live workspace is somebody's work.
fn remove_worktree(r: &dyn MqRunner, wt: &Path) -> Option<String> {
    if !wt.exists() {
        return None;
    }
    let wt_s = wt.display().to_string();
    match r.git(&as_args(&worktree_remove_argv(&wt_s))) {
        Ok(o) if o.ok() => None,
        Ok(o) => Some(quote(&o.stderr)),
        Err(e) => Some(quote(&e)),
    }
}

fn build_in_worktree(
    r: &dyn MqRunner,
    wt: &str,
    branch: &str,
    heads: &[(u64, String)],
) -> Result<ScratchRef, BatchBuildError> {
    let target_head = rev_parse(r, "refs/remotes/loomux-mq/target")?;
    let add = vec![
        "worktree".to_string(),
        "add".into(),
        "--detach".into(),
        wt.to_string(),
        target_head.clone(),
    ];
    let out = r.git(&as_args(&add)).map_err(BatchBuildError::Git)?;
    if !out.ok() {
        return Err(BatchBuildError::Git(quote(&out.stderr)));
    }

    for (pr, expected) in heads {
        let fetched = rev_parse(r, &format!("refs/remotes/loomux-mq/pr-{pr}"))?;
        if !same_object(expected, &fetched) {
            return Err(BatchBuildError::HeadMoved {
                pr: *pr,
                expected: expected.trim().to_string(),
                actual: fetched,
            });
        }
        let merge = vec![
            "-C".to_string(),
            wt.to_string(),
            "merge".into(),
            "--no-ff".into(),
            "--no-edit".into(),
            "-m".into(),
            format!("loomux merge queue: #{pr} into {branch}"),
            fetched,
        ];
        let out = r.git(&as_args(&merge)).map_err(BatchBuildError::Git)?;
        if !out.ok() {
            // A conflicted merge leaves the worktree mid-merge; the teardown
            // removes it wholesale, so no `merge --abort` is needed.
            return Err(BatchBuildError::Conflict { pr: *pr });
        }
    }

    let sha = {
        let argv = vec!["-C".to_string(), wt.to_string(), "rev-parse".into(), "HEAD".into()];
        let out = r.git(&as_args(&argv)).map_err(BatchBuildError::Git)?;
        if !out.ok() {
            return Err(BatchBuildError::Git(quote(&out.stderr)));
        }
        out.line().to_string()
    };
    Ok(ScratchRef { branch: branch.to_string(), sha, target_head })
}

/// Whether a recorded head and a freshly fetched one name the same object.
///
/// Abbreviation tolerance runs in **one direction, and the code enforces it**:
/// the *recorded* head may be an abbreviation of the *fetched* full oid (that is
/// how a short sha comes to be stored), so `recorded.len() <= fetched.len()` is
/// required. A fetched value shorter than the record is **not** a match — the
/// shorter string has to be the recorded one for a prefix comparison to mean
/// anything, and accepting the reverse would let a truncated *fetch* satisfy a
/// full recorded head.
///
/// An earlier cut documented exactly that rule and implemented
/// `min(recorded.len(), fetched.len())`, which compares both prefixes and is
/// **symmetric** — the guarantee in the doc was not the guarantee in the code
/// (rev-163 N1). No live path differed, because `fetched` always arrives from
/// `rev_parse` as a full oid; it is fixed rather than re-documented because this
/// is the head-equality check standing between an approved head and what gets
/// built into a batch.
///
/// **Both sides must look like object names**, and the comparison is over
/// **bytes**. `&str` indexing panics on a non-char-boundary, so the previous
/// `recorded[..short]` was total only by the accident that oids are ASCII —
/// nothing enforced it, and `PrFacts.head` is taken straight from
/// `headRefOid.trim()`. A malformed value now returns `false` (fail closed)
/// rather than panicking on a char boundary.
///
/// **An empty recorded head is not a match.** It reads as *unknown*, and unknown
/// is never "unbound, therefore fine" — the same fail-closed posture
/// `mergeq::recheck_gate` takes on an empty head and an empty body digest. An
/// entry whose head loomux never resolved has verdicts bound to nothing, so
/// building it into a batch would test an object nobody approved.
fn same_object(recorded: &str, fetched: &str) -> bool {
    let (recorded, fetched) = (recorded.trim(), fetched.trim());
    // Hex-only, so the byte comparison below is also a character comparison and
    // a non-ASCII value can never reach the slice.
    let hexish = |s: &str| !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if !hexish(recorded) || !hexish(fetched) {
        return false;
    }
    // A truncated record must not match half the repository, and the record is
    // the side allowed to be short.
    if recorded.len() < 7 || recorded.len() > fetched.len() {
        return false;
    }
    recorded.as_bytes().eq_ignore_ascii_case(&fetched.as_bytes()[..recorded.len()])
}

fn rev_parse(r: &dyn MqRunner, rev: &str) -> Result<String, BatchBuildError> {
    let argv = vec!["rev-parse".to_string(), rev.to_string()];
    let out = r.git(&as_args(&argv)).map_err(BatchBuildError::Git)?;
    if !out.ok() || out.line().is_empty() {
        return Err(BatchBuildError::NoTargetHead(format!("cannot resolve {rev}")));
    }
    Ok(out.line().to_string())
}

/// Remove a batch's temporary worktree, by the exact path built from its id.
pub fn worktree_remove_argv(path: &str) -> Vec<String> {
    vec!["worktree".into(), "remove".into(), "--force".into(), path.to_string()]
}

/// Teardown for a worktree an **earlier** build left behind — the crash-reconcile
/// and cancel paths, where no `build_scratch` call is in scope to have returned a
/// [`ScratchBuild`].
///
/// Same discipline as the in-build teardown, and deliberately the same function
/// underneath ([`remove_worktree`]): exact path from the batch id, existence
/// checked first, never a prune, never a wildcard, and the failure **returned**
/// for the caller to audit as `mq-cleanup-failed` rather than raised. §10:
/// cleanup failure never blocks landing and never fails a batch.
pub fn cleanup_worktree(r: &dyn MqRunner, batch_id: &str) -> Option<String> {
    remove_worktree(r, &scratch_worktree_path(batch_id))
}

// ── the batch draft PR (§5, §8) ─────────────────────────────────────────────

/// The argv that opens the batch's **draft** PR from the scratch ref into the
/// target (§5: this is what reliably triggers PR-triggered CI, gives
/// `gh pr checks` a handle, and gives the human a URL).
pub fn draft_pr_argv(branch: &str, target: &str, title: &str, body_file: &str) -> Vec<String> {
    vec![
        "pr".into(),
        "create".into(),
        "--draft".into(),
        "--base".into(),
        target.to_string(),
        "--head".into(),
        branch.to_string(),
        "--title".into(),
        title.to_string(),
        "--body-file".into(),
        body_file.to_string(),
    ]
}

/// The batch PR's title.
pub fn batch_pr_title(batch_id: &str, prs: &[u64]) -> String {
    format!("loomux merge queue: batch {batch_id} ({} PRs)", prs.len())
}

/// The batch PR's body.
///
/// **This text is loomux-authored, and it must never contain GitHub's closing
/// pattern** — `close`/`fix`/`resolve` in any inflection immediately followed by
/// `#N`. §8 makes that a rule the body-builder's tests pin rather than a habit,
/// because the scan is textual and context-blind: it fires from inside a
/// blockquote, a caveat, or a sentence asking a human to close something by
/// hand. Sub-PRs are therefore listed as **bare `#N` references** with no
/// keyword in front of them.
///
/// The queue does not scrub the sub-PRs' own bodies (§8) — rewriting
/// agent- or human-authored text to defuse a keyword is loomux editing the
/// record, which is worse than the problem. This constrains only what loomux
/// itself writes.
pub fn batch_pr_body(batch_id: &str, target: &str, scratch_sha: &str, prs: &[u64]) -> String {
    let mut s = String::new();
    s.push_str("Speculative merge-queue batch opened by loomux. **Do not merge by hand.**\n\n");
    s.push_str(&format!("- batch: `{batch_id}`\n"));
    s.push_str(&format!("- target: `{target}`\n"));
    s.push_str(&format!("- tested object: `{scratch_sha}`\n\n"));
    s.push_str("Sub-PRs in this batch, in queue order:\n\n");
    for pr in prs.iter().take(MAX_SIBLINGS_LISTED) {
        s.push_str(&format!("- #{pr}\n"));
    }
    if prs.len() > MAX_SIBLINGS_LISTED {
        s.push_str(&format!(
            "- …and {} more not listed here\n",
            prs.len() - MAX_SIBLINGS_LISTED
        ));
    }
    s.push_str(
        "\nThis PR exists so the repository's own CI judges the combination. \
         On green the tested object is fast-forwarded onto the target unchanged \
         (the commit that was tested is the commit that lands); on red the batch \
         is bisected and the culprit is kicked back. loomux closes this PR and \
         deletes the scratch ref on every exit path.\n",
    );
    s
}

// ── attribution (§9) ────────────────────────────────────────────────────────

/// The comment loomux leaves on a culprit PR (§9) — the durable record, where a
/// human or the owning worker will actually look.
///
/// Two honesty requirements from §9, both of which the tests pin:
///
/// - **It says bisect finds _a_ culprit, not necessarily _the_ culprit.** A
///   genuine pairwise interaction — A fine alone, B fine alone, red together —
///   attributes to whichever entry the search isolates, which depends on the
///   split. Overclaiming here would be the "a claim is a deliverable" failure;
///   the sibling set is named so the reader can see the interaction instead of
///   being told a half-truth confidently.
/// - **No closing keyword**, same rule and same reason as [`batch_pr_body`].
///
/// Every gh-sourced fragment (check names, the run link) goes through
/// `notify::sanitize_gh_text`, the sanitizer every crossing-text boundary in
/// this codebase uses.
pub fn culprit_comment(
    batch_id: &str,
    failing: &[String],
    run_url: Option<&str>,
    siblings: &[u64],
) -> String {
    let mut s = String::new();
    s.push_str("**loomux merge queue: this PR was isolated as the batch's culprit.**\n\n");
    s.push_str(&format!("- batch: `{}`\n", quote(batch_id)));
    if failing.is_empty() {
        s.push_str("- failing checks: none reported by name\n");
    } else {
        let named: Vec<String> =
            failing.iter().take(MAX_SIBLINGS_LISTED).map(|f| quote(f)).collect();
        s.push_str(&format!("- failing checks: {}\n", named.join(", ")));
        if failing.len() > MAX_SIBLINGS_LISTED {
            s.push_str(&format!(
                "- (and {} further failing checks not listed)\n",
                failing.len() - MAX_SIBLINGS_LISTED
            ));
        }
    }
    if let Some(u) = run_url {
        s.push_str(&format!("- run: {}\n", quote(u)));
    }
    if siblings.is_empty() {
        s.push_str("- batched alone\n");
    } else {
        let named: Vec<String> = siblings
            .iter()
            .take(MAX_SIBLINGS_LISTED)
            .map(|p| format!("#{p}"))
            .collect();
        s.push_str(&format!("- batched with: {}\n", named.join(", ")));
        if siblings.len() > MAX_SIBLINGS_LISTED {
            s.push_str(&format!(
                "- (and {} further siblings not listed)\n",
                siblings.len() - MAX_SIBLINGS_LISTED
            ));
        }
    }
    s.push_str(
        "\nThe search isolates **a** culprit, not necessarily **the** culprit: a genuine \
         pairwise interaction (each change fine alone, red together) attributes to whichever \
         entry the split isolated. The sibling set above is listed so that case is visible \
         rather than hidden behind a confident single name.\n\n\
         This PR has been kicked back out of the queue. Its siblings were exonerated and \
         re-queued automatically. loomux has not briefed anyone about this — routing is the \
         orchestrator's call.\n",
    );
    s
}

/// The argv that posts the culprit comment.
pub fn pr_comment_argv(pr: u64, body_file: &str) -> Vec<String> {
    vec!["pr".into(), "comment".into(), pr.to_string(), "--body-file".into(), body_file.to_string()]
}

/// Sanitize one gh-sourced or otherwise untrusted fragment for inclusion in
/// loomux-authored text.
fn quote(s: &str) -> String {
    sanitize_gh_text(s, MAX_QUOTED)
}

// ── the bisect walk (§9) ────────────────────────────────────────────────────

/// One step the driver should take on a red batch, decided by `mergeq`'s pure
/// splitter and shaped for the loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BisectAction {
    /// Attribute this PR, exonerate `survivors`, spend **no further CI** (§9).
    Attribute { culprit: u64, survivors: Vec<u64> },
    /// Build and test this subset next; `rest` is what stays unexamined for now.
    Test { subset: Vec<u64>, rest: Vec<u64> },
    /// Nothing to attribute. The driver aborts rather than blaming a PR (§9).
    Abort,
}

/// Drive `mergeq::bisect_step` — **larger half first**, which is the core's own
/// contract — and turn its shape into the loop's next move.
///
/// `red` is the set currently known to reproduce the failure. This is a pure
/// function of it: the CI observation that decides which half to recurse into is
/// the caller's, so the search itself stays deterministic and test-pinned.
pub fn bisect_action(red: &[u64]) -> BisectAction {
    match bisect_step(red) {
        BisectStep::Nothing => BisectAction::Abort,
        BisectStep::Culprit(pr) => {
            BisectAction::Attribute { culprit: pr, survivors: Vec::new() }
        }
        BisectStep::Split { test, rest } => BisectAction::Test { subset: test, rest },
    }
}

/// The whole search, as a sequence of subsets to test — driven by a caller that
/// answers "is this subset red?".
///
/// Written as a driver over a closure rather than as a loop inside the observer
/// so the *search* is testable with no CI at all: `reproduces` stands in for a
/// full build-push-observe cycle. §9 bounds the depth by `max_batch`, so this
/// terminates; the `guard` is a belt against a future splitter that stopped
/// halving, and it surfaces rather than spinning.
pub fn walk_bisect(
    batch: &[u64],
    mut reproduces: impl FnMut(&[u64]) -> bool,
) -> BisectAction {
    let mut set: Vec<u64> = batch.to_vec();
    // One step per halving, plus slack; §9's bound is ceil(log2 k).
    let guard = batch.len().saturating_add(2);
    for _ in 0..guard {
        match bisect_action(&set) {
            BisectAction::Abort => return BisectAction::Abort,
            BisectAction::Attribute { culprit, .. } => {
                // Survivors are "the batch, minus the culprit", derived from
                // `batch` rather than accumulated as the search exonerates —
                // §9 requires their ORIGINAL queue order, and the order the
                // halves happen to be discarded in is not that order.
                let survivors: Vec<u64> =
                    batch.iter().copied().filter(|p| *p != culprit).collect();
                return BisectAction::Attribute { culprit, survivors };
            }
            BisectAction::Test { subset, rest } => {
                set = if reproduces(&subset) { subset } else { rest };
            }
        }
    }
    BisectAction::Abort
}

// ── state transitions, audited (§11.5) ──────────────────────────────────────

/// The audit actions this slice emits. Constants rather than literals at each
/// call site: §11.5 fixes the vocabulary, and a typo'd action is an event no
/// filter will ever match.
pub mod audit_action {
    pub const ENQUEUED: &str = "mq-enqueued";
    pub const BATCH_BUILT: &str = "mq-batch-built";
    pub const CHECKS_GREEN: &str = "mq-checks-green";
    pub const CHECKS_RED: &str = "mq-checks-red";
    pub const CHECKS_UNVERIFIABLE: &str = "mq-checks-unverifiable";
    pub const BISECT_STEP: &str = "mq-bisect-step";
    pub const CULPRIT: &str = "mq-culprit";
    pub const KICKED_BACK: &str = "mq-kicked-back";
    pub const CANCELLED: &str = "mq-cancelled";
    pub const RECOVERED: &str = "mq-recovered";
    pub const STRANDED: &str = "mq-stranded";
}

/// Move one entry and say what happened, so the caller has exactly one place to
/// audit from and cannot move an entry without producing an audit-shaped fact.
///
/// `advance` is `mergeq`'s only sanctioned mutation path and refuses anything
/// §4 did not enumerate; this wraps it rather than replacing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub pr: u64,
    pub from: EntryState,
    pub to: EntryState,
}

/// Apply a transition to the entry for `pr`, returning the fact to audit.
///
/// `Ok(None)` means there is no such entry — not an error, because a cancel can
/// race a batch that already finished, and inventing an entry to move would be
/// worse than doing nothing.
pub fn advance_entry(
    state: &mut MergeQueueState,
    pr: u64,
    to: EntryState,
) -> Result<Option<Transition>, InvalidTransition> {
    let Some(e) = state.entries.iter_mut().find(|e| e.pr == pr) else {
        return Ok(None);
    };
    let from = e.state();
    e.advance(to)?;
    Ok(Some(Transition { pr, from, to }))
}

/// Requeue the survivors of a bisect **at the front, preserving their original
/// order** (§9).
///
/// They were never implicated; making them wait behind newly-enqueued work would
/// punish them for a neighbour's failure. Order is taken from `survivors` (which
/// `walk_bisect` derives from the batch's own queue order), and entries not in
/// that list keep their relative order behind them.
pub fn requeue_survivors(state: &mut MergeQueueState, survivors: &[u64]) {
    let mut front: Vec<QueueEntry> = Vec::new();
    for pr in survivors {
        if let Some(i) = state.entries.iter().position(|e| e.pr == *pr) {
            front.push(state.entries.remove(i));
        }
    }
    for (i, e) in front.into_iter().enumerate() {
        state.entries.insert(i, e);
    }
}

/// Clear the in-flight batch record once it is finished, and release the target
/// when the queue has drained (§4: a target is a property of the work in the
/// queue, never a configured setting).
pub fn finish_batch(state: &mut MergeQueueState) {
    state.batch = None;
    let live = state.entries.iter().any(|e| !e.state().is_terminal());
    if !live {
        state.target.clear();
    }
}

/// Drop terminal entries so the file stays bounded, keeping the queue's order.
/// Terminal entries have no outgoing transition at all (§4) — a kicked-back PR
/// that gets fixed comes back through a fresh `queue_merge`, as a new entry.
pub fn prune_terminal(state: &mut MergeQueueState) -> usize {
    let before = state.entries.len();
    state.entries.retain(|e| !e.state().is_terminal());
    before - state.entries.len()
}

/// Record a freshly built batch on the state.
pub fn record_batch(state: &mut MergeQueueState, rec: BatchRecord) {
    state.batch = Some(rec);
}

// ── the bounded observation loop (§5) ────────────────────────────────────────

/// What one observation of the batch's draft PR concluded.
///
/// Note what is **not** here: there is no "keep waiting forever" arm.
/// [`BatchOutcome::Pending`] carries the elapsed time precisely so a caller
/// cannot treat it as a steady state — the bound is applied inside
/// [`observe_batch`], and pending past the bound comes back as
/// [`BatchOutcome::Unverifiable`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchOutcome {
    /// Checks are still running and the bound has not expired.
    Pending { elapsed_ms: u64 },
    /// Every check terminal, none failing. The tested object may land.
    Green,
    /// Terminal with failures. Into the search (§9) when k > 1.
    Red { failing: Vec<String> },
    /// **Not green, and nothing lands.** Either the bound expired with no
    /// terminal verdict, or `gh` could not answer at all. Surfaced to the
    /// orchestrator explicitly rather than sitting pending in silence (§5).
    Unverifiable { why: String },
}

impl BatchOutcome {
    /// Only [`BatchOutcome::Green`] may land. A method rather than a `matches!`
    /// at each call site, because "may this batch land" is exactly the question
    /// a future fifth variant must not be able to answer by accident.
    pub fn may_land(&self) -> bool {
        matches!(self, BatchOutcome::Green)
    }
}

/// Observe the batch's draft PR once, under §5's bound.
///
/// The primary release is **the checks going terminal**; `checks_timeout_minutes`
/// is a *backstop*, not the mechanism — `.loomux/lessons.md` records that
/// releasing on evidence beats releasing on elapsed time, and, beside it, that
/// any suppression driven by a fallible signal must still be bounded. Both hold
/// here: a terminal answer wins immediately, and a repo with no CI at all (which
/// classifies as `Pending` forever) surfaces as **unverifiable** at the bound
/// instead of pinning a batch indefinitely.
///
/// **Unverifiable is not green.** Nothing lands on it; the caller requeues the
/// entries and tells the orchestrator. That is why this returns a four-way
/// outcome rather than an `Option<bool>`.
///
/// `now_ms`/`started_ms` are arguments rather than clock reads, so the bound is
/// testable without waiting for one. A clock that stepped backwards saturates to
/// zero elapsed rather than underflowing: a backward step can extend the wait,
/// never fabricate an instant timeout.
pub fn observe_batch(
    r: &dyn MqRunner,
    draft_pr: u64,
    started_ms: u64,
    now_ms: u64,
    checks_timeout_minutes: u32,
) -> BatchOutcome {
    let argv = pr_checks_argv(draft_pr);
    let verdict = match r.gh(&as_args(&argv)) {
        Ok(o) if o.ok() => classify_checks(Ok(o.stdout.as_str())),
        Ok(o) => classify_checks(Err(o.stderr.as_str())),
        // `gh` could not be run at all (§10's `gh-not-found`): the queue reports
        // itself unavailable rather than silently doing nothing.
        Err(e) => BatchVerification::Unavailable { why: e },
    };
    let elapsed_ms = now_ms.saturating_sub(started_ms);
    match verdict {
        BatchVerification::Green => BatchOutcome::Green,
        BatchVerification::Red { failing } => BatchOutcome::Red { failing },
        BatchVerification::Unavailable { why } => BatchOutcome::Unverifiable { why: quote(&why) },
        BatchVerification::Pending => {
            let bound_ms = u64::from(checks_timeout_minutes).saturating_mul(60_000);
            if elapsed_ms >= bound_ms {
                BatchOutcome::Unverifiable {
                    why: format!(
                        "no terminal checks on batch PR #{draft_pr} within \
                         {checks_timeout_minutes} minutes"
                    ),
                }
            } else {
                BatchOutcome::Pending { elapsed_ms }
            }
        }
    }
}

// ── crash reconcile (§4) ─────────────────────────────────────────────────────

/// The argv that asks whether the batch's draft PR is still open.
pub fn pr_state_argv(pr: u64) -> Vec<String> {
    vec!["pr".into(), "view".into(), pr.to_string(), "--json".into(), "state".into()]
}

#[derive(Deserialize)]
struct RawPrState {
    #[serde(default)]
    state: String,
}

/// Is the batch's draft PR still open? `None` when that could not be determined
/// — which reconcile treats as "the world does not match", never as "probably
/// fine".
fn draft_pr_open(r: &dyn MqRunner, pr: u64) -> Option<bool> {
    let out = r.gh(&as_args(&pr_state_argv(pr))).ok()?;
    if !out.ok() {
        return None;
    }
    let parsed: RawPrState = serde_json::from_str(out.line()).ok()?;
    Some(parsed.state.eq_ignore_ascii_case("OPEN"))
}

/// What a restart found, and what it did about it.
///
/// **The two-phase contract (§4, the #467/#468 pattern).** Phase 1 — this
/// function — reads the world, classifies, and mutates the state. It must not
/// deliver anything: the registry's lock is not reentrant, and the delivery
/// queue learned that the hard way. Every notice it wants sent is *collected*
/// here and returned, for the caller to send in phase 2 once the guard has
/// dropped. That is why `notices` is a field rather than a callback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// The in-flight batch matched the world and may continue.
    pub resumed: bool,
    /// `(pr, why)` for every entry failed **loudly** because the world did not
    /// match the record. Audited as `mq-stranded`.
    pub stranded: Vec<(u64, String)>,
    /// Transitions applied, for the caller to audit — one per entry moved.
    pub transitions: Vec<Transition>,
    /// Phase-2 notices. Sent by the caller after the guard drops, never here.
    pub notices: Vec<String>,
    /// A leftover temp worktree that would not go away. Audit as
    /// `mq-cleanup-failed`; never a reason to fail anything.
    pub cleanup_failed: Option<String>,
}

/// **Reconcile an in-flight batch against reality after a restart** (§4).
///
/// The file says what loomux *intended*. The truth is whether the scratch ref
/// still exists and whether the draft PR is still open. Resume **only** when the
/// world matches; otherwise fail the entries loudly — audit, notice, terminal
/// state. Never silently drop, never silently retry.
///
/// Both existence checks fail **closed**: an unanswerable `ls-remote` or an
/// unreadable PR strands the batch rather than resuming it, because resuming
/// onto a scratch ref loomux cannot confirm is exactly how the Bors invariant
/// (§8) fails at the one point §8 does not guard.
///
/// The snapshot is **not deleted** here — the caller rewrites it alongside live
/// entries, so recovery is re-runnable across N restarts rather than a one-shot
/// that a crash mid-recovery can lose.
pub fn reconcile_batch(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    group: &str,
) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    let Some(batch) = state.batch.clone() else {
        // Nothing was in flight. Entries sitting in an in-flight state with no
        // batch record are themselves the inconsistency §4 reconciles, and
        // `plan_batch` already refuses to dispatch on exactly that signal — so
        // strand them rather than leaving them to block the queue forever.
        let orphans: Vec<u64> =
            state.entries.iter().filter(|e| e.state().in_flight()).map(|e| e.pr).collect();
        for pr in orphans {
            strand(state, &mut report, pr, "in-flight with no batch record on disk");
        }
        return report;
    };

    let scratch_ok = match scratch_branch(group, &batch.id) {
        None => Some(false),
        Some(branch) => scratch_exists(r, &branch).ok(),
    };
    let draft_ok = match batch.draft_pr {
        Some(pr) => draft_pr_open(r, pr),
        // A batch recorded past `batching` with no draft PR never got one, so
        // there is nothing to resume onto.
        None => Some(false),
    };

    let why = match (scratch_ok, draft_ok) {
        (Some(true), Some(true)) => None,
        (None, _) => Some("the scratch ref could not be checked on the remote".to_string()),
        (_, None) => Some("the batch's draft PR could not be read".to_string()),
        (Some(false), _) => Some("the scratch ref is gone from the remote".to_string()),
        (_, Some(false)) => Some("the batch's draft PR is no longer open".to_string()),
    };

    match why {
        None => {
            report.resumed = true;
            report.notices.push(format!(
                "[loomux] merge queue: resumed batch {} after a restart - scratch ref and draft PR #{} both still present.",
                quote(&batch.id),
                batch.draft_pr.unwrap_or(0)
            ));
        }
        Some(why) => {
            for pr in batch.prs.clone() {
                strand(state, &mut report, pr, &why);
            }
            report.cleanup_failed = cleanup_worktree(r, &batch.id);
            state.batch = None;
            report.notices.push(format!(
                "[loomux] merge queue: batch {} could NOT be resumed after a restart ({}). {} entr{} kicked back; nothing landed.",
                quote(&batch.id),
                quote(&why),
                batch.prs.len(),
                if batch.prs.len() == 1 { "y" } else { "ies" }
            ));
        }
    }
    report
}

/// Fail one entry loudly: move it to `kicked-back`, record the transition for
/// the audit, and record why.
///
/// A **refused** transition is itself reported rather than swallowed — an entry
/// the state machine will not move is a fact the audit needs, and §11.5 requires
/// an audit action to name what actually happened.
fn strand(state: &mut MergeQueueState, report: &mut ReconcileReport, pr: u64, why: &str) {
    match advance_entry(state, pr, EntryState::KickedBack) {
        Ok(Some(t)) => {
            report.transitions.push(t);
            report.stranded.push((pr, why.to_string()));
        }
        Ok(None) => {}
        Err(e) => {
            report.stranded.push((pr, format!("{why}; and the entry would not move: {e}")));
        }
    }
}

// ── the one orchestrator notice (§9) ─────────────────────────────────────────

/// **One decision-grade notice** on a culprit (§9) — "one fact that changes what
/// it does next, plus the PR link, not a narration".
///
/// Deliberately terse, and deliberately *not* a briefing. §9 is explicit that
/// the backend produces the **fact** and the orchestrator makes the **call**:
/// worker liveness, resume-versus-fresh-spawn, and folding this in with whatever
/// else is pending are judgment calls, and the board mapping that equips them
/// belongs to the orchestrator. So this names the culprit, the batch, one
/// failing check and the survivor count — and says nothing about who should do
/// what next.
///
/// The durable record is the PR comment ([`culprit_comment`]); this is the nudge
/// that makes someone read it.
pub fn culprit_notice(culprit: u64, batch_id: &str, failing: &[String], survivors: usize) -> String {
    let check = failing.first().map(|f| format!(" ({})", quote(f))).unwrap_or_default();
    let more =
        if failing.len() > 1 { format!(" +{} more failing", failing.len() - 1) } else { String::new() };
    format!(
        "[loomux] merge queue: #{culprit} isolated as batch {}'s culprit{check}{more}. \
         {survivors} sibling{} requeued; details on #{culprit}.",
        quote(batch_id),
        if survivors == 1 { "" } else { "s" }
    )
}

/// The notice for a batch that could not be verified within the bound (§5).
///
/// Distinct from the culprit notice because it means something different and
/// asks for something different: **nothing is attributable**, nothing landed,
/// and the thing to look at is the repo's CI rather than any one PR. Collapsing
/// the two would tell an orchestrator a PR is implicated when none is.
pub fn unverifiable_notice(batch_id: &str, draft_pr: Option<u64>, why: &str) -> String {
    let where_ = draft_pr.map(|p| format!(" (batch PR #{p})")).unwrap_or_default();
    format!(
        "[loomux] merge queue: batch {}{where_} is UNVERIFIABLE - {}. Nothing landed; \
         entries requeued. This is not a red batch and no PR is implicated.",
        quote(batch_id),
        quote(why)
    )
}

// ── the three queue operations behind §11.1's MCP tools (slice E) ───────────
//
// The MCP layer is a JSON shim and the registry resolves paths; the decisions
// live here, so `queue_merge`'s refusal set is one closed vocabulary in one
// place rather than a set of `if`s spread across `mcp.rs`.

/// §11.1's **closed** refusal vocabulary. Every refusal an agent can see is one
/// of these eight strings — nothing constructs a ninth, and nothing returns a
/// free-text reason, because an open vocabulary is one a caller cannot branch on
/// and a human cannot grep for.
pub mod refusal {
    pub const BASE_IS_DEFAULT: &str = "base-is-default";
    pub const BASE_UNVERIFIABLE: &str = "base-unverifiable";
    pub const BASE_NOT_TARGET: &str = "base-not-target";
    pub const GATE_NOT_MET: &str = "gate-not-met";
    pub const GATE_NOT_CONFIGURED: &str = "gate-not-configured";
    pub const ALREADY_QUEUED: &str = "already-queued";
    pub const QUEUE_FULL: &str = "queue-full";
    pub const QUEUE_DISABLED: &str = "queue-disabled";
    /// `cancel_queued_merge` only: the PR is not in the queue, or is already
    /// terminal. Not in §11.1's `queue_merge` list because it cannot arise
    /// there — kept beside them so the whole vocabulary is one place.
    pub const NOT_QUEUED: &str = "not-queued";
}

/// What `queue_merge` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Queued, with its 1-based position among the entries that are still live.
    Queued { position: usize },
    /// Refused, with one of [`refusal`]'s strings.
    Refused { reason: &'static str },
}

/// **Enqueue a PR** (§11.1, §7 layer 1).
///
/// Refusal order is deliberate, and it is cheap-and-local before
/// expensive-and-remote — but that is the *secondary* reason. The primary one is
/// that each check answers a question the next one presupposes:
///
/// 1. **`queue-disabled`** — an absent `merge_queue:` block means the feature is
///    off and behavior is byte-for-byte unchanged (§12). Nothing else is even
///    meaningful.
/// 2. **`already-queued`** — a second enqueue of the same PR is a caller
///    mistake, not a state change, and answering it needs no lookups.
/// 3. **`queue-full`** — the §10 cap, so `merge_queue.json` stays bounded under
///    an enqueue storm.
/// 4. **The constraint-7 refusals**, from **live** lookups (§7.1): the PR's base
///    and the repo default, resolved through the real `gh`. A failed lookup
///    refuses (`base-unverifiable`) — unknown is never treated as safe.
/// 5. **The gate** (§6). Last because it is the most expensive and because a PR
///    whose base is wrong should not have its reviewers reported on.
///
/// `asserted_target` is §4's **assertion, not a selection**: present, it must
/// equal what the base resolves to, and a mismatch refuses. It can narrow the
/// outcome, never widen it.
///
/// The target is **established by the first successful enqueue** from that PR's
/// live base (§4) — never configured, never inferred from a caller's argument.
#[allow(clippy::too_many_arguments)]
pub fn enqueue(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    pr: u64,
    asserted_target: Option<&str>,
    enabled: bool,
    gate: &GateSpec,
    verdicts: &BTreeMap<BlockId, ReviewVerdict>,
    now_ms: u64,
) -> EnqueueOutcome {
    let refuse = |reason| EnqueueOutcome::Refused { reason };
    // MUTATION K2 (evidence, NOT shipped): the opt-in ignored, so the queue is
    // live in a repo that never asked for it.
    if false && !enabled {
        return refuse(refusal::QUEUE_DISABLED);
    }
    if state.entry(pr).map(|e| !e.state().is_terminal()).unwrap_or(false) {
        return refuse(refusal::ALREADY_QUEUED);
    }
    if state.entries.iter().filter(|e| !e.state().is_terminal()).count() >= MAX_ENTRIES {
        return refuse(refusal::QUEUE_FULL);
    }

    // §7.1: live, through the real `gh`. `current_target` is whatever the queue
    // has established so far — empty until the first successful enqueue.
    let current = (!state.target.trim().is_empty()).then(|| state.target.clone());
    let (target, facts) =
        match resolve_and_validate_target(r, pr, current.as_deref(), asserted_target) {
            Ok(v) => v,
            Err(e) => return refuse(e.code()),
        };

    // §6. `ci_green` is read unconditionally here, unlike at landing: enqueue
    // happens once per PR rather than once per sub-PR per batch, so the
    // round-trip is not worth a branch that could get the condition wrong.
    let observed = PrObservation {
        body_digest: Some(body_digest(&facts.body)),
        ci_green: pr_ci_green(r, pr),
    };
    let recheck = recheck_gate(gate, verdicts, Some(facts.head.as_str()), &observed);
    if let Some(code) = recheck.refusal_code() {
        return refuse(match code {
            "gate-not-configured" => refusal::GATE_NOT_CONFIGURED,
            _ => refusal::GATE_NOT_MET,
        });
    }

    // Established here, and only here (§4).
    state.target = target;
    // A prior terminal entry for this PR is replaced rather than accumulated:
    // §4 is explicit that a kicked-back PR comes back as a NEW entry, so its
    // corpse must not sit in the file forever.
    state.entries.retain(|e| e.pr != pr || !e.state().is_terminal());
    state.entries.push(QueueEntry::new(pr, &facts.head, now_ms));
    let position = state.entries.iter().filter(|e| !e.state().is_terminal()).count();
    EnqueueOutcome::Queued { position }
}

/// What `cancel_queued_merge` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled { was: EntryState },
    Refused { reason: &'static str },
}

/// **Cancel a queued PR** (§11.1).
///
/// A cancel reaches any **non-terminal** entry (§4) — including one inside an
/// in-flight batch, which §10 says abandons and rebuilds that batch. Cancelling
/// something already terminal is `not-queued` rather than a silent success: an
/// agent that thinks it stopped a landing which had already happened would be
/// told the wrong thing.
///
/// The batch record is deliberately **not** touched here. Whether an in-flight
/// batch is abandoned is the driver loop's call (it must also clean up the
/// scratch ref and the draft PR), and doing half of it from a tool call would
/// leave the world and the record disagreeing.
pub fn cancel(state: &mut MergeQueueState, pr: u64) -> CancelOutcome {
    // MUTATION K3 (evidence, NOT shipped): a PR that was never queued reports a
    // successful cancel — telling an agent it stopped a landing that never was.
    let Some(was) = state.entry(pr).map(|e| e.state()) else {
        return CancelOutcome::Cancelled { was: EntryState::Queued };
    };
    if was.is_terminal() {
        return CancelOutcome::Cancelled { was };
    }
    match advance_entry(state, pr, EntryState::Cancelled) {
        Ok(Some(_)) => CancelOutcome::Cancelled { was },
        // Unreachable while `transition` allows every non-terminal → cancelled,
        // and still handled: a refusal the state machine invents is reported,
        // never rendered as a success.
        _ => CancelOutcome::Refused { reason: refusal::NOT_QUEUED },
    }
}

/// **`merge_queue_status()`'s wire shape** (§11.1).
///
/// Deliberately not `mergeqview::project`: that is slice F's projection for the
/// *chrome*, with its own closed `status` vocabulary for rendering. This is the
/// agent-facing contract §11.1 fixes, and collapsing the two would make a UI
/// tweak a change to a tool's output.
///
/// `since_ms` is an **age**, not a timestamp: an agent reasoning about "has this
/// been sitting too long" should not have to know what clock the host is on.
pub fn status_view(state: &MergeQueueState, enabled: bool, now_ms: u64) -> Value {
    let entries: Vec<Value> = state
        .entries
        .iter()
        .filter(|e| !e.state().is_terminal())
        .map(|e| {
            let mut o = serde_json::Map::new();
            o.insert("pr".into(), Value::from(e.pr));
            o.insert("state".into(), Value::from(e.state().as_str()));
            o.insert("since_ms".into(), Value::from(now_ms.saturating_sub(e.enqueued_ms)));
            if let Some(why) = &e.blocked_reason {
                o.insert("blocked_reason".into(), Value::from(quote(why)));
            }
            Value::Object(o)
        })
        .collect();
    let mut out = serde_json::Map::new();
    out.insert("enabled".into(), Value::from(enabled));
    out.insert("target".into(), Value::from(state.target.clone()));
    out.insert("entries".into(), Value::Array(entries));
    if let Some(b) = &state.batch {
        out.insert(
            "batch".into(),
            serde_json::json!({
                "id": quote(&b.id),
                "prs": b.prs,
                "state": b.state().as_str(),
                "draft_pr": b.draft_pr,
                "scratch_sha": quote(&b.scratch_sha),
            }),
        );
    }
    Value::Object(out)
}
