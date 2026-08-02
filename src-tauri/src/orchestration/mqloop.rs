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
//! - this file (slices D2 and D3) — the **loop that sequences them**: build the
//!   scratch, open the draft PR, observe the checks under a bound, bisect a red
//!   batch and attribute it, requeue the survivors, reconcile after a crash, and
//!   persist every step — plus [`drive`], the one-step-per-call tick that
//!   decides which of those happens next (§13, #698).
//!
//! `mod.rs` gets registry wiring only — a module declaration, three fields, the
//! two `merge_queue_reconcile*` methods and the `mq_drive_group_with` /
//! `mq_driver_tick` pair, all of which resolve paths and delegate. No decision
//! in this feature lives there.
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
    bisect_step, plan_batch, recheck_gate, scratch_branch, valid_id_component, BatchPlan,
    BatchRecord, BisectSearch, BisectStep, EntryState, GateSpec, InvalidTransition,
    MergeQueueState, PrObservation, QueueEntry, MAX_ENTRIES,
};
use super::mergeqview::MERGE_QUEUE_FILE;
use super::mqdriver::{
    as_args, classify_checks, cleanup_scratch, land_batch, mint_scratch, pr_checks_argv,
    pr_ci_green, push_scratch, resolve_and_validate_target, resolve_and_validate_target_detailed,
    scratch_exists, BatchVerification, LandRefusal, MintError, MqRunner, REMOTE,
};
use super::notify::sanitize_gh_text;
use super::workflow::{body_digest, BlockId, ReviewVerdict};
use serde::Deserialize;
use serde_json::{json, Value};
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
///
/// **`None` for an id this module will not build a name from** — the same
/// `mergeq::valid_id_component` `scratch_branch` applies, enforced *here* rather
/// than by whoever calls it (rev-183).
///
/// The distinction is not pedantry. `drive`'s top-of-function guard rejects an
/// unusable record before any path is built, and that was true of every path the
/// *driver* builds — but `merge_queue_reconcile_with` runs **before** that guard
/// in the same tick, and `reconcile_batch` hands `cleanup_worktree` a batch id
/// straight off disk. So the guard's protection was **positional**: it held for
/// the callers that happened to sit after it, and a caller added earlier (or a
/// path reached by a route nobody re-checked) got nothing. Validating inside the
/// builder makes it structural — there is no argument, and no call order, that
/// produces a path from a name this module rejects.
pub fn scratch_worktree_path(batch_id: &str) -> Option<PathBuf> {
    // RED WITNESS (temporary, restored by the next commit): the builder does
    // NOT validate, so the guarantee is only as good as whether `drive`'s guard
    // ran first — which for the reconcile path it did not.
    Some(std::env::temp_dir().join(worktree_dir_name(batch_id.trim())))
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

    // Unreachable in practice — `mint_scratch` only ever hands back an id that
    // already built a ref name — and refused rather than unwrapped, because the
    // guarantee this function offers must not depend on which caller reached it.
    let Some(wt) = scratch_worktree_path(batch_id) else {
        return ScratchBuild::failed(BatchBuildError::Git(format!(
            "refusing to build a worktree path from batch id {:?}",
            quote(batch_id)
        )));
    };
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
/// **This is the caller the builder's own check exists for** (rev-183):
/// `reconcile_batch` reaches it with a batch id straight off disk, and reconcile
/// runs *before* `drive`'s record guard in the same tick. An unusable id is
/// reported as a cleanup failure — the channel this function already has for
/// "there is a leftover somebody has to look at" — rather than becoming a path.
pub fn cleanup_worktree(r: &dyn MqRunner, batch_id: &str) -> Option<String> {
    let Some(wt) = scratch_worktree_path(batch_id) else {
        return Some(format!(
            "refusing to build a worktree path from batch id {:?}",
            quote(batch_id)
        ));
    };
    remove_worktree(r, &wt)
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
    /// A batch that could not be constructed or could not be observed, and was
    /// abandoned with its entries returned to `queued` (§10's "batch aborts,
    /// entries return to queued" rows).
    ///
    /// **Added to §11.5's vocabulary by the #698 driver, deliberately rather
    /// than by reusing `mq-batch-built` with a falsy field.** §11.5 requires an
    /// audit action to name what actually happened, and "built: false" on a
    /// `built` event is the exact mislabel #461 catalogues — a filter looking
    /// for batches that were built would match a batch that was not.
    pub const BATCH_ABORTED: &str = "mq-batch-aborted";
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
/// of these eight **policy** strings, `NOT_QUEUED`, or one of the loomux-fault
/// strings below — nothing constructs a string outside this set, and nothing
/// returns a free-text reason, because an open vocabulary is one a caller
/// cannot branch on and a human cannot grep for.
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

    // ── loomux faults, distinct from policy refusals (rev-163 NB) ───────────
    //
    // The eight above are **policy**: they say the queue considered the request
    // and declined it, and each names something the caller can act on. These
    // four say loomux itself failed, and conflating the two is the defect this
    // module otherwise exists to prevent — an orchestrator told `queue-disabled`
    // concludes the repo never opted in and stops, which is precisely the wrong
    // move when the truth is a torn state file. A wrong label does not merely
    // under-inform; it actively sends the reader somewhere else.
    //
    // Shape borrowed from `queue_orphans`' `no-app-handle` / `registry-not-shared`,
    // which the tool description already flags as "should never appear in a
    // running build, so treat one as a loomux defect worth reporting to the
    // human". Same posture here, and the tool descriptions say so.

    /// `merge_queue.json` exists and could not be read or parsed — a torn write,
    /// a hand edit, or a file written by a newer schema. **Not** "nothing is
    /// queued": loomux cannot tell what is queued, which is the distinction §4's
    /// loud reconcile is built around.
    pub const STATE_UNREADABLE: &str = "queue-state-unreadable";
    /// The change was computed but could not be persisted. Reported as a
    /// **failure** rather than a success, because an enqueue the next restart
    /// forgets is not an enqueue.
    pub const STATE_UNWRITABLE: &str = "queue-state-unwritable";
    /// The group could not be resolved at all. Should never appear in a running
    /// build; if it does, it is a loomux defect, not a queue state.
    pub const QUEUE_UNAVAILABLE: &str = "queue-unavailable";
    /// The `merge_gate` file is on disk but an I/O error (permissions, a
    /// transient read failure, non-UTF-8 bytes) kept loomux from reading it —
    /// **not** [`GATE_NOT_CONFIGURED`] (the file genuinely absent, a policy
    /// choice) and **not** [`GATE_NOT_MET`] (the file read fine and
    /// `GateSpec::Malformed` rejected its contents). Collapsing this into
    /// `gate-not-configured` via `Result::ok()` is the exact defect #681 fixed:
    /// an orchestrator told "no gate covers this target" has no reason to
    /// suspect a torn or permission-denied file instead.
    pub const GATE_UNREADABLE: &str = "gate-unreadable";

    /// Whether a refusal string names a **loomux fault** rather than a policy
    /// decision. Exposed so a caller (and a test) can branch on the distinction
    /// without re-listing the strings and getting the list wrong later.
    pub fn is_loomux_fault(reason: &str) -> bool {
        matches!(reason, STATE_UNREADABLE | STATE_UNWRITABLE | QUEUE_UNAVAILABLE | GATE_UNREADABLE)
    }
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
    if !enabled {
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
    let Some(was) = state.entry(pr).map(|e| e.state()) else {
        return CancelOutcome::Refused { reason: refusal::NOT_QUEUED };
    };
    if was.is_terminal() {
        return CancelOutcome::Refused { reason: refusal::NOT_QUEUED };
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

// ── the driver tick (#698) ───────────────────────────────────────────────────
//
// Everything above this line was reachable only from a tool call or a restart.
// `queue_merge` enqueued, `merge_queue_status` read, `cancel_queued_merge`
// cancelled, `merge_queue_reconcile` recovered — and nothing anywhere started a
// batch when entries were queued and the target was idle. The whole
// queued → batching → ci-wait → landing lifecycle had no production caller
// (#698): every slice test drove the seams directly, so everything was green
// while no path connected them. This section is that path.
//
// # One step per call, and why the shape is a step rather than a loop
//
// [`drive`] performs **at most one state advance**. It never loops waiting for
// anything, never sleeps, and never retries an external call in place. It is
// called from the unified `gh` poll loop (#406/#652), which is the one thread in
// this process that makes `gh` calls, and that loop's per-tick budget (#656) is
// a shared resource: a driver that blocked it would stop every `notify_when`
// watch in the fleet from firing, which is the deadlock #590 is about, one layer
// down.
//
// So the sequencing that a synchronous implementation would express as a loop is
// expressed as **state**: the batch record on `merge_queue.json` says where the
// batch is, each tick reads it, does the one next thing, and writes it back. A
// bisect that needs three CI runs is three ticks-worth of transitions spread over
// however long those runs take, not a `walk_bisect` call blocking a thread for
// forty minutes. [`walk_bisect`] remains the executable statement of the search's
// *shape* — and the property its test pins, that the search terminates within
// ceil(log2 k) runs, is the property this state machine has to match — but the
// production search is [`bisect_step`] applied once per tick, because its
// `reproduces` closure can only ever be a full build-push-observe cycle.
//
// # The invariant that keeps a crash recoverable
//
// **An entry is in an in-flight state only while `state.batch` is `Some`.** Every
// path here that clears the batch record first moves its entries out of the
// in-flight states, and every path that puts entries into one records a batch in
// the same call. That is what makes `reconcile_batch`'s two rules — resume only
// when the world matches, strand an in-flight entry with no batch record — a
// statement about a *crash* rather than a statement about a tick that was midway
// through its work. It is why a bisect probe carries a batch record of its own
// (`BatchRecord::new_probe`) instead of the search running between batches.

/// How many `queued` entries one build attempt examines from the head of the
/// queue before it stops looking.
///
/// Bounded because each examined entry costs two or three `gh` round-trips (§7's
/// live lookups plus the gate's `ci-green` clause), and the queue holds up to
/// [`MAX_ENTRIES`] — an unbounded refresh pass would put 190 round-trips inside
/// one poll tick, which is the fan-out #656 exists to stop.
///
/// The window deliberately includes entries that are **already blocked**: §4's
/// `blocked_reason` is "refreshed at every batch build" and becomes eligible
/// again the instant a re-review covers the new head, so an entry loomux never
/// re-examines is an entry that can never unblock. The cost of that is a
/// truncation, which is **stated** in the `mq-batch-built` / `mq-batch-aborted`
/// detail rather than silent (`.loomux/lessons.md`: no silent caps).
pub const MAX_EXAMINED_PER_BUILD: usize = 8;

/// One audit event the driver decided on, for the registry to emit.
///
/// Returned rather than emitted here for the same reason
/// [`ReconcileReport::notices`] is: this module owns no registry, and a driver
/// that reached for one would put the queue's decisions back inside `mod.rs`.
#[derive(Clone, Debug, PartialEq)]
pub struct DriveAudit {
    /// One of §11.5's actions — always a constant from [`audit_action`] or
    /// `mqdriver::audit_action`, never a literal built at the call site.
    pub action: &'static str,
    pub detail: Value,
}

/// What one driver step decided.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DriveReport {
    /// Audit events, in the order they happened.
    pub audits: Vec<DriveAudit>,
    /// Orchestrator notices. Handed back rather than sent, so the caller can
    /// deliver them outside whatever lock it holds — the #467/#468 two-phase
    /// posture `reconcile_batch` already takes.
    pub notices: Vec<String>,
    /// Whether `state` was mutated and must be persisted. A tick that only
    /// observed a still-pending batch changes nothing, and must not rewrite the
    /// file for it.
    pub changed: bool,
    /// Whether this tick reached a state whose next attempt should be held off
    /// rather than retried on the next wake.
    ///
    /// Two shapes set it, for the same underlying reason — the next attempt
    /// would cost the same external calls and reach the same answer:
    ///
    /// - **An external failure.** §10's failure rows all end "entries return to
    ///   `queued`", so the very next tick tries the same thing — one notice and
    ///   one audit line per tick, forever, against a remote that is simply down.
    /// - **Nothing eligible to batch.** Establishing that costs two `gh`
    ///   round-trips per examined entry, and a queue can sit blocked on a
    ///   re-review for hours.
    ///
    /// The bound itself lives in the caller (a per-group floor, the
    /// `due_intake_polls` idiom); this flag is how the driver says which
    /// outcomes deserve it. It is deliberately **not** a retry counter on disk:
    /// the condition is about the world, not about the queue, and a counter that
    /// survived a restart would keep punishing a batch for a network that has
    /// since come back.
    pub backoff: bool,
}

impl DriveReport {
    fn audit(&mut self, action: &'static str, detail: Value) {
        self.audits.push(DriveAudit { action, detail });
    }
}

/// The per-group facts one driver step needs and this module will not resolve
/// for itself: which group, the repo's policy (§11.2), and the clock.
///
/// `now_ms` is an argument rather than a clock read for the same reason
/// [`observe_batch`]'s is — the `checks_timeout_minutes` backstop has to be
/// testable without waiting for it.
#[derive(Clone, Copy, Debug)]
pub struct DriveConfig<'a> {
    pub group: &'a str,
    pub max_batch: u32,
    pub checks_timeout_minutes: u32,
    pub now_ms: u64,
}

/// **One driver step for one group** (#698).
///
/// Two branches and no third:
///
/// 1. **A batch is in flight** — observe its draft PR once and advance it: land
///    on green, bisect or attribute on red, requeue on unverifiable. A cancelled
///    or vanished member abandons it first (§10).
/// 2. **No batch is in flight** — refresh the head of the queue against the
///    world, cut a batch from what is still eligible, build it, push it
///    create-only, and open its draft PR.
///
/// Both branches are bounded: a fixed number of `gh`/`git` calls, no loop that
/// waits on anything, and at most one advance. Nothing here retries an external
/// call in place — §3's "no unbounded retry" arm, which is why every failure
/// path below ends in a state change plus an audit rather than in a second
/// attempt.
///
/// `verdicts` is a closure rather than a map because the batch paths need one
/// PR's verdicts at a time and the files live in the group dir, which is the
/// registry's business — the same separation [`land_batch`] already takes.
pub fn drive(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    cfg: &DriveConfig<'_>,
    gate: &GateSpec,
    verdicts: &dyn Fn(u64) -> BTreeMap<BlockId, ReviewVerdict>,
) -> DriveReport {
    let mut rep = DriveReport::default();
    // §4: `merge_queue.json` is a file, and a torn write or a hand edit can put
    // anything in it. **Every string this tick interpolates into a path or an
    // argv is validated here, once, before any of them is built** — so a branch
    // added later cannot skip the check by taking a different route to the same
    // string.
    //
    // Two strings qualify, and they get different predicates because they are
    // different kinds of name:
    //
    // - **the target** reaches a refspec — the batch fetch's
    //   `+refs/heads/<target>:…` runs long before `land_batch`'s own submit-time
    //   guard does — so it gets `landable`, the same predicate the landing path
    //   uses. Observed, not theorised: with this guard absent an empty target
    //   fetched `+refs/heads/:refs/remotes/loomux-mq/target` and ran
    //   `gh pr create --base ` with an empty base.
    // - **the batch id** reaches three places — the scratch ref name, the temp
    //   worktree path, and the body file — and gets `valid_id_component`, the
    //   *same* predicate `scratch_branch` applies, named rather than inlined so
    //   the three cannot drift apart.
    //
    // **This guard is an early, loud stop, not the guarantee** (rev-183). All
    // three builders enforce `valid_id_component` themselves and return `None`
    // on a name they will not build, because a check that lives only here is
    // *positional*: it holds for the callers that happen to sit after it, and
    // `merge_queue_reconcile_with` runs **before** it in the same tick with a
    // batch id straight off disk. What this guard buys is that the whole tick
    // stops on one audit line rather than each builder refusing separately
    // further in.
    //
    // Refusing the tick rather than repairing the record is the conservative
    // half of §4: a record loomux will not build a name from is also one it
    // cannot clean up by name, so acting on it would leak whatever the real
    // batch was. It audits every time and backs off, which is a durable, loud,
    // rate-bounded "a human has to look at this file".
    let live = state.entries.iter().any(|e| !e.state().is_terminal());
    let unusable = if live && !super::mqdriver::landable(state.target.trim()) {
        Some(("target", quote(&state.target)))
    } else {
        state
            .batch
            .as_ref()
            .filter(|b| !valid_id_component(&b.id))
            .map(|b| ("batch-id", quote(&b.id)))
    };
    if let Some((field, value)) = unusable {
        rep.backoff = true;
        rep.audit(
            audit_action::STRANDED,
            json!({ "reason": "merge_queue.json holds a value loomux will not build a name from",
                    "field": field, "value": value }),
        );
        return rep;
    }
    match state.batch.clone() {
        Some(batch) => advance_in_flight(r, state, cfg, gate, verdicts, batch, &mut rep),
        None => start_batch(r, state, cfg, gate, verdicts, &mut rep),
    }
    rep
}

// ── branch 1: a batch is in flight ──────────────────────────────────────────

fn advance_in_flight(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    cfg: &DriveConfig<'_>,
    gate: &GateSpec,
    verdicts: &dyn Fn(u64) -> BTreeMap<BlockId, ReviewVerdict>,
    batch: BatchRecord,
    rep: &mut DriveReport,
) {
    // §10: "Entry cancelled while batching/ci-wait — the in-flight batch is
    // abandoned and rebuilt without it." `cancel` deliberately does not touch
    // the batch record (it cannot clean up a scratch ref or close a draft PR),
    // so noticing is this loop's job and this is the first thing it does: there
    // is no point spending a `gh pr checks` on a batch that is already void.
    let gone: Vec<u64> = batch
        .prs
        .iter()
        .copied()
        .filter(|pr| state.entry(*pr).map(|e| e.state().is_terminal()).unwrap_or(true))
        .collect();
    if !gone.is_empty() {
        let mut moves = Vec::new();
        for pr in &batch.prs {
            requeue(state, &mut moves, *pr);
            // Cleared even for the member that could not move — a cancelled
            // entry still pointing at a batch that no longer exists is a record
            // saying something untrue, and `requeue` deliberately leaves an
            // entry it did not move alone.
            set_batch_tag(state, *pr, None);
        }
        teardown(r, cfg.group, &batch, rep);
        state.batch = None;
        rep.changed = true;
        rep.audit(
            audit_action::BATCH_ABORTED,
            json!({ "batch": batch.id, "why": "a member left the queue", "left": gone,
                    "transitions": moves_json(&moves) }),
        );
        return;
    }

    // A batch past construction always has a draft PR — this driver records the
    // batch and its draft PR in the same call. One without is therefore a file a
    // crash or a hand edit produced, and §4's posture on those is loud failure
    // rather than a guess at what was meant.
    let Some(draft_pr) = batch.draft_pr else {
        let mut moves = Vec::new();
        for pr in &batch.prs {
            mv(state, &mut moves, *pr, EntryState::KickedBack);
            set_batch_tag(state, *pr, None);
        }
        teardown(r, cfg.group, &batch, rep);
        state.batch = None;
        rep.changed = true;
        rep.audit(
            audit_action::STRANDED,
            json!({ "batch": batch.id, "why": "an in-flight batch with no draft PR",
                    "transitions": moves_json(&moves) }),
        );
        rep.notices.push(format!(
            "[loomux] merge queue: batch {} has no draft PR on record and cannot be observed. \
             {} entr{} kicked back; nothing landed.",
            quote(&batch.id),
            batch.prs.len(),
            if batch.prs.len() == 1 { "y" } else { "ies" }
        ));
        return;
    };

    let probe = batch.is_probe();
    match observe_batch(r, draft_pr, batch.started_ms, cfg.now_ms, cfg.checks_timeout_minutes) {
        // Nothing to do and nothing to write. A pending batch is the steady
        // state, and rewriting the file on every tick of it would be churn on a
        // value that did not change.
        BatchOutcome::Pending { .. } => {}
        BatchOutcome::Green => {
            rep.audit(
                audit_action::CHECKS_GREEN,
                json!({ "batch": batch.id, "draft_pr": draft_pr, "prs": batch.prs,
                        "probe": probe }),
            );
            if probe {
                // §9: a probe never lands. Its green says only that this half of
                // the red set is innocent — the culprit is in the other half.
                narrow_search(r, state, cfg, &batch, false, rep);
            } else {
                land(r, state, cfg, gate, verdicts, &batch, rep);
            }
        }
        BatchOutcome::Red { failing } => {
            rep.audit(
                audit_action::CHECKS_RED,
                json!({ "batch": batch.id, "draft_pr": draft_pr, "prs": batch.prs,
                        "failing": failing, "probe": probe }),
            );
            let mut opened = batch.clone();
            if !probe {
                // The whole batch is the red set, and the search opens here.
                // Both facts it records are ones a later tick cannot recompute —
                // see `mergeq::BisectSearch`.
                opened.bisect = Some(BisectSearch::new(batch.prs.clone(), failing));
            }
            narrow_search(r, state, cfg, &opened, true, rep);
        }
        BatchOutcome::Unverifiable { why } => {
            // §5: unverifiable is **not** red and no PR is implicated — nothing
            // lands, everything requeues, and the whole search (if this was one)
            // is abandoned rather than continued on an answer loomux never got.
            let mut moves = Vec::new();
            for pr in search_set(state, &batch) {
                requeue(state, &mut moves, pr);
            }
            teardown(r, cfg.group, &batch, rep);
            state.batch = None;
            rep.changed = true;
            rep.backoff = true;
            rep.audit(
                audit_action::CHECKS_UNVERIFIABLE,
                json!({ "batch": batch.id, "draft_pr": draft_pr, "why": quote(&why),
                        "transitions": moves_json(&moves) }),
            );
            rep.notices.push(unverifiable_notice(&batch.id, Some(draft_pr), &why));
        }
    }
}

/// Land a green batch (§7.3, §8) and clear it.
fn land(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    cfg: &DriveConfig<'_>,
    gate: &GateSpec,
    verdicts: &dyn Fn(u64) -> BTreeMap<BlockId, ReviewVerdict>,
    batch: &BatchRecord,
    rep: &mut DriveReport,
) {
    let mut moves = Vec::new();
    for pr in &batch.prs {
        mv(state, &mut moves, *pr, EntryState::Landing);
    }
    rep.changed = true;
    let target = state.target.clone();
    // §6's second enforcement point and §7's third layer both live inside
    // `land_batch`, which re-resolves the default branch and every sub-PR's base
    // at the moment of submit. Nothing is resolved here and carried in.
    match land_batch(r, &batch.scratch_sha, &target, &batch.prs, gate, verdicts) {
        Ok(landed) => {
            for pr in &batch.prs {
                mv(state, &mut moves, *pr, EntryState::Landed);
                set_batch_tag(state, *pr, None);
            }
            teardown(r, cfg.group, batch, rep);
            state.batch = None;
            finish_batch(state);
            prune_terminal(state);
            rep.audit(
                super::mqdriver::audit_action::LANDED,
                json!({ "batch": batch.id, "target": landed.target,
                        "scratch_sha": landed.scratch_sha, "prs": batch.prs,
                        "transitions": moves_json(&moves) }),
            );
            rep.notices.push(landed_notice(&batch.id, &landed.target, &batch.prs));
        }
        Err(refusal) => {
            // One bad entry refuses the whole landing before anything is pushed
            // (`land_batch` re-checks every sub-PR first), so nothing partial
            // reached the target and the survivors are genuinely untouched.
            let culprit = match &refusal {
                LandRefusal::Gate { pr, .. } => Some(*pr),
                LandRefusal::Target { pr, .. } if *pr != 0 => Some(*pr),
                _ => None,
            };
            for pr in &batch.prs {
                if Some(*pr) == culprit {
                    mv(state, &mut moves, *pr, EntryState::KickedBack);
                } else {
                    requeue(state, &mut moves, *pr);
                }
                set_batch_tag(state, *pr, None);
            }
            teardown(r, cfg.group, batch, rep);
            state.batch = None;
            finish_batch(state);
            prune_terminal(state);
            let why = land_refusal_text(&refusal);
            // A push that failed because the target moved is a fact about the
            // world; a gate that no longer holds is a fact about one PR. Only
            // the first should slow the group down.
            rep.backoff = culprit.is_none();
            rep.audit(
                super::mqdriver::audit_action::LAND_REFUSED,
                json!({ "batch": batch.id, "target": target, "why": why, "pr": culprit,
                        "transitions": moves_json(&moves) }),
            );
            rep.notices.push(land_refused_notice(&batch.id, culprit, &why, batch.prs.len()));
        }
    }
}

/// Advance the bisect: narrow the red set by this probe's answer, then either
/// attribute a culprit or build the next probe (§9).
///
/// `reproduced` is what the observation said about **this probe's** subset —
/// red means the culprit is inside it, green means the culprit is in what the
/// probe left out. Either way the entries on the exonerated side leave the
/// search immediately: §9 requeues survivors the moment they are cleared rather
/// than making them wait for the search to finish.
fn narrow_search(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    cfg: &DriveConfig<'_>,
    batch: &BatchRecord,
    reproduced: bool,
    rep: &mut DriveReport,
) {
    let search = batch.bisect.clone().unwrap_or_else(|| BisectSearch::new(batch.prs.clone(), Vec::new()));
    let universe = search_set(state, batch);
    let red: Vec<u64> = if reproduced {
        // The tested subset reproduces: the culprit is in it. (For a batch that
        // has only just gone red, the subset IS the whole batch.)
        universe.iter().copied().filter(|p| batch.prs.contains(p)).collect()
    } else {
        universe.iter().copied().filter(|p| !batch.prs.contains(p)).collect()
    };
    let exonerated: Vec<u64> = universe.iter().copied().filter(|p| !red.contains(p)).collect();

    let mut moves = Vec::new();
    // Everything in the batch (and the wider search) that is not in the new red
    // set has to leave the in-flight states before the batch record goes, or the
    // invariant this module rests on breaks.
    for pr in &exonerated {
        // Straight from ci-wait/bisecting back to queued, then re-ordered to the
        // front below — §9 gives survivors their original queue order back
        // rather than the order the search happened to clear them in.
        requeue(state, &mut moves, *pr);
    }
    requeue_survivors(state, &exonerated);
    // Everything still suspected enters (or stays in) `bisecting`, so the batch
    // record can be replaced without any entry ever sitting in `ci-wait` with no
    // batch behind it. On the round that opens a search this is the whole batch;
    // on later rounds the entries are already there and nothing moves.
    for pr in &red {
        if state.entry(*pr).map(|e| e.state()) == Some(EntryState::CiWait) {
            mv(state, &mut moves, *pr, EntryState::Bisecting);
        }
    }
    teardown(r, cfg.group, batch, rep);
    state.batch = None;
    rep.changed = true;

    match bisect_action(&red) {
        // An empty red set means the search has nothing to attribute, and §9 is
        // explicit that it must not invent something. Requeue what is left and
        // say so.
        BisectAction::Abort => {
            finish_batch(state);
            rep.audit(
                audit_action::BISECT_STEP,
                json!({ "batch": batch.id, "outcome": "abort",
                        "transitions": moves_json(&moves) }),
            );
            rep.notices.push(format!(
                "[loomux] merge queue: batch {}'s search ended with nothing to attribute. \
                 Entries requeued; no PR is implicated.",
                quote(&batch.id)
            ));
        }
        BisectAction::Attribute { culprit, .. } => {
            attribute(r, state, batch, &search, culprit, &mut moves, rep);
        }
        BisectAction::Test { subset, rest } => {
            rep.audit(
                audit_action::BISECT_STEP,
                json!({ "batch": batch.id, "outcome": "split", "red": red,
                        "test": subset, "rest": rest, "exonerated": exonerated,
                        "transitions": moves_json(&moves) }),
            );
            // The next probe is built now, in this same tick, so the search is
            // never in the "entries bisecting, no batch record" shape that
            // `reconcile_batch` (correctly) strands. If the build fails, the
            // search is abandoned loudly instead — never left half-shaped.
            build_probe(r, state, cfg, batch, &search, &subset, &red, rep);
        }
    }
}

/// The search this batch belongs to: every entry currently inside it.
///
/// For a fresh red batch that is the batch itself; for a probe it is the wider
/// set of entries still in `bisecting`, which is where the halves the search has
/// not yet reached are kept. Deliberately derived from the entries rather than
/// stored on the batch: the entries are the durable record of who is still under
/// suspicion, and a second copy on the batch record could disagree with it.
fn search_set(state: &MergeQueueState, batch: &BatchRecord) -> Vec<u64> {
    let mut set: Vec<u64> = state
        .entries
        .iter()
        .filter(|e| e.state() == EntryState::Bisecting)
        .map(|e| e.pr)
        .collect();
    for pr in &batch.prs {
        if !set.contains(pr) && state.entry(*pr).map(|e| !e.state().is_terminal()).unwrap_or(false)
        {
            set.push(*pr);
        }
    }
    // Queue order, which §9 requires the survivors to keep.
    let order: Vec<u64> = state.entries.iter().map(|e| e.pr).collect();
    set.sort_by_key(|p| order.iter().position(|q| q == p).unwrap_or(usize::MAX));
    set
}

/// Name a culprit, kick it back, and tell the two audiences §9 names: a durable
/// comment on the PR, and one decision-grade notice to the orchestrator.
///
/// The siblings named are the **original batch's**, not the last round's — a
/// pairwise interaction is only visible if the reader can see everyone the
/// culprit was combined with, which is the whole reason
/// [`BisectSearch::origin_prs`] is carried.
fn attribute(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    batch: &BatchRecord,
    search: &BisectSearch,
    culprit: u64,
    moves: &mut Vec<Transition>,
    rep: &mut DriveReport,
) {
    let survivors: Vec<u64> = search.origin_prs.iter().copied().filter(|p| *p != culprit).collect();
    mv(state, moves, culprit, EntryState::KickedBack);
    set_batch_tag(state, culprit, None);
    finish_batch(state);
    prune_terminal(state);

    // §9's honest limit, checked rather than assumed: a batch still red at k = 1
    // while that PR's OWN checks are green is an infrastructure/flake case, and
    // saying "this PR broke the batch" there would be a confident half-truth.
    let own_ci = pr_ci_green(r, culprit);
    // `run_url` is `None` on purpose: §5's classification adapter narrows
    // `gh pr checks` to names and a verdict, and the queue does not keep a
    // second parse of that JSON just to carry a link. The batch id and the
    // sibling set are the handles the comment gives instead. An invented or
    // repurposed URL — the batch PR's, say, under a field labelled `run` —
    // would be the mislabel §11.5 forbids one layer over.
    let comment = culprit_comment(&batch.id, &search.failing, None, &survivors);
    let posted = post_comment(r, &batch.id, culprit, &comment);
    if let Err(why) = &posted {
        rep.audit(
            super::mqdriver::audit_action::CLEANUP_FAILED,
            json!({ "batch": batch.id, "step": "culprit-comment", "pr": culprit,
                    "detail": quote(why) }),
        );
    }
    rep.audit(
        audit_action::CULPRIT,
        json!({ "batch": batch.id, "pr": culprit, "survivors": survivors,
                "failing": search.failing, "own_checks_green": own_ci,
                "comment_posted": posted.is_ok(), "transitions": moves_json(moves) }),
    );
    rep.audit(
        audit_action::KICKED_BACK,
        json!({ "batch": batch.id, "pr": culprit, "why": "isolated as the batch's culprit" }),
    );
    rep.notices.push(if own_ci == Some(true) {
        flake_notice(culprit, &batch.id, &search.failing, survivors.len())
    } else {
        culprit_notice(culprit, &batch.id, &search.failing, survivors.len())
    });
}

/// Build, push and open the next probe of a search (§9), leaving the state with
/// a batch record for it.
#[allow(clippy::too_many_arguments)]
fn build_probe(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    cfg: &DriveConfig<'_>,
    prev: &BatchRecord,
    search: &BisectSearch,
    subset: &[u64],
    red: &[u64],
    rep: &mut DriveReport,
) {
    let heads: Vec<(u64, String)> = subset
        .iter()
        .filter_map(|pr| state.entry(*pr).map(|e| (*pr, e.head.clone())))
        .collect();
    let target = state.target.clone();
    let mut moves = Vec::new();
    match construct(r, state, cfg, &heads, &target, Some(search.clone()), &mut moves, rep) {
        Some(rec) => {
            for pr in subset {
                set_batch_tag(state, *pr, Some(&rec.id));
            }
            record_batch(state, rec);
            rep.changed = true;
        }
        None => {
            // Abandoning the search is the bounded answer: every entry still
            // under suspicion goes back to `queued` and a later ordinary batch
            // re-derives the whole question. Nothing is attributed on a probe
            // that never ran — that would be blaming a PR for a `git` failure.
            for pr in red {
                requeue(state, &mut moves, *pr);
            }
            finish_batch(state);
            rep.changed = true;
            rep.audit(
                audit_action::BATCH_ABORTED,
                json!({ "batch": prev.id, "why": "the next bisect probe could not be built",
                        "red": red, "transitions": moves_json(&moves) }),
            );
            rep.notices.push(format!(
                "[loomux] merge queue: batch {}'s search was abandoned - the next probe could \
                 not be built. {} entr{} requeued; nothing attributed.",
                quote(&prev.id),
                red.len(),
                if red.len() == 1 { "y" } else { "ies" }
            ));
        }
    }
}

// ── branch 2: cut a batch ───────────────────────────────────────────────────

fn start_batch(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    cfg: &DriveConfig<'_>,
    gate: &GateSpec,
    verdicts: &dyn Fn(u64) -> BTreeMap<BlockId, ReviewVerdict>,
    rep: &mut DriveReport,
) {
    // The single-in-flight discipline (§4), held on the entries as well as on
    // the batch record — `plan_batch` refuses on either, and an inconsistent
    // file must never be the reason a second batch races the first. Nothing here
    // repairs that shape: it is `reconcile_batch`'s, once, at startup.
    if !matches!(plan_batch(state, cfg.max_batch), BatchPlan::Idle | BatchPlan::Build(_)) {
        return;
    }
    if !state.entries.iter().any(|e| e.state() == EntryState::Queued) {
        return;
    }
    let target = state.target.clone();
    let (selected, examined, truncated) = refresh_and_select(r, state, cfg, gate, verdicts, rep);
    if selected.is_empty() {
        // Two ways to get here and both want the same answer. Either the pass
        // stalled on the seam, in which case it has already audited and set
        // `backoff`; or everything at the head of the queue is blocked — a real
        // state (§4) and not a failure, since the reasons are on the entries and
        // visible in `merge_queue_status`. In that second case the refresh that
        // established it costs two
        // `gh` round-trips per examined entry, and a queue can sit blocked for
        // hours waiting on a re-review. Retrying that on every 30-second wake
        // would be ~700 `gh` calls an hour for a group doing nothing, which is
        // the fan-out this tick's whole design is about not producing. So the
        // group backs off, and an unblocking re-review is picked up within the
        // backoff rather than within a wake. Nothing is *waiting* on that
        // latency: the human action that unblocks it takes longer than the
        // backoff does.
        rep.backoff = true;
        return;
    }

    let heads: Vec<(u64, String)> = selected
        .iter()
        .filter_map(|pr| state.entry(*pr).map(|e| (*pr, e.head.clone())))
        .collect();
    let mut moves = Vec::new();
    match construct(r, state, cfg, &heads, &target, None, &mut moves, rep) {
        Some(rec) => {
            for pr in &selected {
                mv(state, &mut moves, *pr, EntryState::CiWait);
                set_batch_tag(state, *pr, Some(&rec.id));
            }
            rep.audit(
                audit_action::BATCH_BUILT,
                json!({ "batch": rec.id, "target": target, "prs": rec.prs,
                        "scratch_sha": rec.scratch_sha, "draft_pr": rec.draft_pr,
                        "examined": examined, "examination_truncated": truncated,
                        "transitions": moves_json(&moves) }),
            );
            record_batch(state, rec);
            rep.changed = true;
        }
        None => {
            // `construct` has already audited exactly what failed and returned
            // the entries to `queued`; there is nothing to add here.
        }
    }
}

/// Refresh §4's live eligibility predicate over the head of the queue and return
/// what may go into a batch.
///
/// This is §7 layer 2 and §6's **first** enforcement point, and both are done
/// per entry at build time rather than once for the batch: a PR that was
/// approved when it was enqueued may have been rebased, re-reviewed, or
/// retargeted since, and the queue is only ever as strong as the moment it last
/// asked.
///
/// A refusal does not remove the entry — it records the reason and leaves it
/// `queued` (§4: "paused" is a predicate, not a ninth state), so a re-review that
/// covers the new head makes it eligible again with no agent action at all.
///
/// Returns `(selected, examined, truncated)`; the last two exist so the audit
/// event can say the window was capped rather than implying the whole queue was
/// considered.
fn refresh_and_select(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    cfg: &DriveConfig<'_>,
    gate: &GateSpec,
    verdicts: &dyn Fn(u64) -> BTreeMap<BlockId, ReviewVerdict>,
    rep: &mut DriveReport,
) -> (Vec<u64>, usize, bool) {
    let queued: Vec<u64> =
        state.entries.iter().filter(|e| e.state() == EntryState::Queued).map(|e| e.pr).collect();
    let truncated = queued.len() > MAX_EXAMINED_PER_BUILD;
    let window: Vec<u64> = queued.into_iter().take(MAX_EXAMINED_PER_BUILD).collect();
    let target = state.target.clone();

    let mut selected: Vec<u64> = Vec::new();
    let mut examined = 0usize;
    for pr in &window {
        if selected.len() >= cfg.max_batch as usize {
            break;
        }
        examined += 1;
        // §7 layer 2: live, through the real `gh`, and re-checked against the
        // recorded target — a PR whose base moved under the queue is refused
        // here rather than landed somewhere nobody reviewed it for.
        //
        // **A runner failure ends the pass.** It is a fact about the world, not
        // about this PR: the next entry costs another `MQ_CMD_TIMEOUT` for the
        // same reason and answers nothing. Carrying on would put
        // `MAX_EXAMINED_PER_BUILD` timeouts back to back inside the one loop
        // that also delivers every `notify_when` notice in the fleet — the
        // counts are bounded, but the clock is what the fleet actually feels.
        // Stopping here bounds the phase at **one** timed-out call, and the
        // group backs off rather than re-deriving the same stall next wake.
        // Same line `land()` draws with `backoff = culprit.is_none()`.
        let reason = match resolve_and_validate_target_detailed(r, *pr, Some(&target), None) {
            Err(f) if f.is_runner() => return stall(rep, examined, truncated, *pr, &f),
            Err(f) => Some(f.into_refusal().code().to_string()),
            Ok((_, facts)) => {
                // Fetched only when the gate declares the clause — the same
                // predicate `land_batch` uses, which fails *toward* fetching
                // (an absent or malformed gate answers yes), so an unreadable
                // gate can never be the reason a check was skipped. Reused
                // rather than re-decided here because the build and the landing
                // must not disagree about when the sub-PR's own CI matters, and
                // because this runs once per examined entry inside a shared
                // poll tick.
                let ci_green = if super::mqdriver::declares_ci_green(gate) {
                    match super::mqdriver::pr_ci_green_detailed(r, *pr) {
                        // Same reasoning as the target lookup, one call later: a
                        // seam that could not run is the world, and handling it
                        // here is why the bound holds whichever of the two
                        // lookups stalls first.
                        Err(e) => {
                            return stall(
                                rep,
                                examined,
                                truncated,
                                *pr,
                                &super::mqdriver::ResolveFailure::Runner(e),
                            )
                        }
                        Ok(v) => v,
                    }
                } else {
                    None
                };
                let observed =
                    PrObservation { body_digest: Some(body_digest(&facts.body)), ci_green };
                let recheck =
                    recheck_gate(gate, &verdicts(*pr), Some(facts.head.as_str()), &observed);
                if recheck.passed() {
                    // The head is refreshed from the live answer, so the batch
                    // is built on the object the gate was just checked against.
                    if set_head(state, *pr, &facts.head) {
                        rep.changed = true;
                    }
                    None
                } else {
                    Some(recheck.refusal_code().unwrap_or(refusal::GATE_NOT_MET).to_string())
                }
            }
        };
        if set_blocked(state, *pr, reason.clone()) {
            rep.changed = true;
        }
        if reason.is_none() {
            selected.push(*pr);
        }
    }
    (selected, examined, truncated)
}

/// End a selection pass because the seam stalled, not because a PR was refused.
///
/// Returns an empty selection (so no batch is cut on half a picture), reports
/// `backoff` (so the next wake is five minutes away rather than 30 seconds), and
/// audits what happened with the entry it stalled on — an operator looking at a
/// queue that stopped moving needs the `git`/`gh` message, and `mq-batch-aborted`
/// is §11.5's action for "a batch was not built and here is why".
fn stall(
    rep: &mut DriveReport,
    examined: usize,
    truncated: bool,
    pr: u64,
    why: &super::mqdriver::ResolveFailure,
) -> (Vec<u64>, usize, bool) {
    let detail = match why {
        super::mqdriver::ResolveFailure::Runner(e) => quote(e),
        // Unreachable — `stall` is only called on a runner failure — and still
        // rendered rather than unwrapped, because an audit line that says
        // nothing is worse than one that says something unexpected.
        other => format!("{other:?}"),
    };
    rep.backoff = true;
    rep.audit(
        audit_action::BATCH_ABORTED,
        json!({ "why": "a live lookup could not be completed; the selection pass stopped rather \
                        than spending one timeout per entry on the same stall",
                "pr": pr, "detail": detail,
                "examined": examined, "examination_truncated": truncated }),
    );
    (Vec::new(), examined, truncated)
}

/// Mint, build, push and open the draft PR — the whole construction half of a
/// batch, shared by an ordinary batch and a bisect probe because every step of
/// it and every failure of it is identical between them.
///
/// Returns the batch record on success. On failure the selected entries have
/// been returned to `queued`, the audit event has been recorded and the notice
/// (if the failure deserves one) is on the report — so a caller has nothing left
/// to clean up, which is what keeps the two call sites from each having to get
/// six failure paths right.
#[allow(clippy::too_many_arguments)]
fn construct(
    r: &dyn MqRunner,
    state: &mut MergeQueueState,
    cfg: &DriveConfig<'_>,
    heads: &[(u64, String)],
    target: &str,
    search: Option<BisectSearch>,
    moves: &mut Vec<Transition>,
    rep: &mut DriveReport,
) -> Option<BatchRecord> {
    let prs: Vec<u64> = heads.iter().map(|(p, _)| *p).collect();
    let probe = search.is_some();
    if !probe {
        // An ordinary batch's entries enter `batching` before any external call,
        // so a crash anywhere below leaves a shape `reconcile_batch` recognises.
        // A probe's entries are already `bisecting` and stay there.
        for pr in &prs {
            mv(state, moves, *pr, EntryState::Batching);
        }
        rep.changed = true;
    }

    let abort = |state: &mut MergeQueueState, rep: &mut DriveReport, why: String, backoff: bool| {
        let mut back = Vec::new();
        if !probe {
            for pr in &prs {
                requeue(state, &mut back, *pr);
            }
        }
        rep.changed = true;
        rep.backoff = rep.backoff || backoff;
        rep.audit(
            audit_action::BATCH_ABORTED,
            json!({ "prs": prs, "why": quote(&why), "probe": probe,
                    "transitions": moves_json(&back) }),
        );
        why
    };

    let minted = match mint_scratch(r, cfg.group, cfg.now_ms) {
        Ok(m) => m,
        Err(MintError::Collision { attempts }) => {
            let why = abort(
                state,
                rep,
                format!("{attempts} scratch-ref names in a row already existed on the remote"),
                true,
            );
            rep.audit(
                super::mqdriver::audit_action::SCRATCH_COLLISION,
                json!({ "prs": prs, "attempts": attempts }),
            );
            rep.notices.push(batch_aborted_notice(&prs, &why));
            return None;
        }
        Err(MintError::Lookup(e)) => {
            let why = abort(state, rep, format!("the remote could not be asked: {e}"), true);
            rep.notices.push(batch_aborted_notice(&prs, &why));
            return None;
        }
        Err(MintError::BadName) => {
            // The group id will not build a ref name, and will not on any retry
            // — so this is a permanent refusal, not a transient one.
            let why = abort(
                state,
                rep,
                format!("no scratch ref name can be built for group {:?}", cfg.group),
                true,
            );
            rep.notices.push(batch_aborted_notice(&prs, &why));
            return None;
        }
    };

    let build = build_scratch(r, &minted.batch_id, &minted.branch, target, heads);
    if let Some(why) = build.cleanup_failed {
        // §10: cleanup failure never fails a batch. Audited, not acted on.
        rep.audit(
            super::mqdriver::audit_action::CLEANUP_FAILED,
            json!({ "batch": minted.batch_id, "step": "remove-worktree", "detail": quote(&why) }),
        );
    }
    let scratch = match build.result {
        Ok(s) => s,
        // §8: a conflict costs no CI. The entry kicks back immediately, before
        // anything is pushed, and the batch rebuilds without it on the next tick
        // — a tick rather than a retry here, because "one advance per call" is
        // what keeps this bounded inside the shared poll loop.
        Err(BatchBuildError::Conflict { pr }) => {
            kick_back_one(state, rep, &prs, pr, "the speculative merge conflicted", probe);
            return None;
        }
        // A head that moved has lost its verdicts (§6), which is a better reason
        // to kick back than a conflict is.
        Err(BatchBuildError::HeadMoved { pr, expected, actual }) => {
            kick_back_one(
                state,
                rep,
                &prs,
                pr,
                &format!(
                    "its head moved under the queue ({} -> {})",
                    quote(&expected),
                    quote(&actual)
                ),
                probe,
            );
            return None;
        }
        Err(BatchBuildError::NoTargetHead(e)) => {
            let why = abort(state, rep, format!("the target {target:?} could not be resolved: {e}"), true);
            rep.notices.push(batch_aborted_notice(&prs, &why));
            return None;
        }
        Err(BatchBuildError::Git(e)) => {
            let why = abort(state, rep, format!("git failed while building the batch: {e}"), true);
            rep.notices.push(batch_aborted_notice(&prs, &why));
            return None;
        }
    };

    // §4: create-only by primitive. A lease rejection here means a ref this
    // batch does not own is sitting on the name, which is the collision the mint
    // check exists for — and it is not deleted to make room.
    if let Err(e) = push_scratch(r, &scratch.sha, &minted.branch) {
        let why = abort(state, rep, format!("the scratch push was rejected: {e}"), true);
        rep.audit(
            super::mqdriver::audit_action::SCRATCH_COLLISION,
            json!({ "batch": minted.batch_id, "branch": minted.branch, "detail": quote(&e) }),
        );
        rep.notices.push(batch_aborted_notice(&prs, &why));
        return None;
    }
    rep.audit(
        super::mqdriver::audit_action::BATCH_PUSHED,
        json!({ "batch": minted.batch_id, "branch": minted.branch, "sha": scratch.sha,
                "target_head": scratch.target_head, "mint_attempts": minted.attempts }),
    );

    let title = batch_pr_title(&minted.batch_id, &prs);
    let body = batch_pr_body(&minted.batch_id, target, &scratch.sha, &prs);
    match open_draft_pr(r, &minted.batch_id, &minted.branch, target, &title, &body) {
        Ok(draft_pr) => {
            let mut rec = match search {
                Some(s) => BatchRecord::new_probe(&minted.batch_id, prs.clone(), cfg.now_ms, s),
                None => BatchRecord::new(&minted.batch_id, prs.clone(), cfg.now_ms),
            };
            rec.scratch_sha = scratch.sha;
            rec.draft_pr = Some(draft_pr);
            if !probe {
                // batching -> ci-wait: the scratch is pushed and the draft PR is
                // open, which is exactly what that state means (§4).
                let _ = rec.advance(EntryState::CiWait);
            }
            Some(rec)
        }
        Err(e) => {
            // The scratch ref is already on the remote at this point, so cleanup
            // has something real to do. `cleanup_scratch` is given no draft PR
            // because there is none to close — or, in the unparseable-number
            // case, none loomux can name, which the notice says out loud rather
            // than leaving a stray draft PR nobody knows about.
            for f in cleanup_scratch(r, cfg.group, &minted.batch_id, None) {
                rep.audit(
                    super::mqdriver::audit_action::CLEANUP_FAILED,
                    json!({ "batch": minted.batch_id, "step": f.step, "detail": quote(&f.why) }),
                );
            }
            let why = abort(state, rep, format!("the batch draft PR could not be opened: {e}"), true);
            rep.notices.push(batch_aborted_notice(&prs, &why));
            None
        }
    }
}

/// Kick one entry out of a batch under construction and return the rest to
/// `queued` (§8's conflict path, §6's moved-head path).
fn kick_back_one(
    state: &mut MergeQueueState,
    rep: &mut DriveReport,
    batch: &[u64],
    pr: u64,
    why: &str,
    probe: bool,
) {
    let mut moves = Vec::new();
    mv(state, &mut moves, pr, EntryState::KickedBack);
    set_batch_tag(state, pr, None);
    for other in batch {
        if *other != pr {
            requeue(state, &mut moves, *other);
        }
    }
    if probe {
        // A probe whose construction kicked one of its own members back cannot
        // continue the search — the set under test has changed underneath it.
        // Everything still suspected goes back to `queued`; a later ordinary
        // batch re-derives the question from scratch.
        let still: Vec<u64> = state
            .entries
            .iter()
            .filter(|e| e.state() == EntryState::Bisecting)
            .map(|e| e.pr)
            .collect();
        for other in still {
            requeue(state, &mut moves, other);
        }
    }
    // Releases the target if that kick-back emptied the queue (§4: a target is a
    // property of the work in it). The batch record is already `None` on both
    // call paths, so this is the release and nothing else.
    finish_batch(state);
    prune_terminal(state);
    rep.changed = true;
    rep.audit(
        audit_action::KICKED_BACK,
        json!({ "pr": pr, "why": why, "batch_prs": batch, "probe": probe,
                "transitions": moves_json(&moves) }),
    );
    rep.notices.push(format!(
        "[loomux] merge queue: #{pr} was kicked back before any CI was spent - {}. \
         The batch rebuilds without it.",
        quote(why)
    ));
}

// ── the driver's small mutations, in one place ──────────────────────────────

/// Apply one transition and record it for the audit. `false` when the entry is
/// gone or the state machine refused — refusals are not silently rendered as
/// successes, they simply produce no transition to report.
fn mv(
    state: &mut MergeQueueState,
    moves: &mut Vec<Transition>,
    pr: u64,
    to: EntryState,
) -> bool {
    match advance_entry(state, pr, to) {
        Ok(Some(t)) => {
            moves.push(t);
            true
        }
        _ => false,
    }
}

/// Return one entry to `queued` and detach it from whatever batch it was in.
fn requeue(state: &mut MergeQueueState, moves: &mut Vec<Transition>, pr: u64) -> bool {
    let moved = mv(state, moves, pr, EntryState::Queued);
    if moved {
        set_batch_tag(state, pr, None);
    }
    moved
}

fn set_batch_tag(state: &mut MergeQueueState, pr: u64, batch: Option<&str>) {
    if let Some(e) = state.entries.iter_mut().find(|e| e.pr == pr) {
        e.batch = batch.map(|b| b.to_string());
    }
}

fn set_blocked(state: &mut MergeQueueState, pr: u64, reason: Option<String>) -> bool {
    match state.entries.iter_mut().find(|e| e.pr == pr) {
        Some(e) if e.blocked_reason != reason => {
            e.blocked_reason = reason;
            true
        }
        _ => false,
    }
}

fn set_head(state: &mut MergeQueueState, pr: u64, head: &str) -> bool {
    match state.entries.iter_mut().find(|e| e.pr == pr) {
        Some(e) if e.head != head => {
            e.head = head.to_string();
            true
        }
        _ => false,
    }
}

fn moves_json(moves: &[Transition]) -> Value {
    Value::Array(
        moves
            .iter()
            .map(|t| json!({ "pr": t.pr, "from": t.from.as_str(), "to": t.to.as_str() }))
            .collect(),
    )
}

/// §10's "cleanup runs on every exit path": close the draft PR and delete the
/// scratch ref by exact name, plus the temp worktree in case a crash left one.
/// Every failure is audited and none of them fails anything.
fn teardown(r: &dyn MqRunner, group: &str, batch: &BatchRecord, rep: &mut DriveReport) {
    for f in cleanup_scratch(r, group, &batch.id, batch.draft_pr) {
        rep.audit(
            super::mqdriver::audit_action::CLEANUP_FAILED,
            json!({ "batch": batch.id, "step": f.step, "detail": quote(&f.why) }),
        );
    }
    if let Some(why) = cleanup_worktree(r, &batch.id) {
        rep.audit(
            super::mqdriver::audit_action::CLEANUP_FAILED,
            json!({ "batch": batch.id, "step": "remove-worktree", "detail": quote(&why) }),
        );
    }
}

// ── the two `gh` writes the driver performs ─────────────────────────────────

/// Where a body file for one `gh` write lives.
///
/// The OS temp dir, under a name built from the **batch id** — which is
/// `RandomState`-derived (§11.4), so two agents, two groups or two batches on
/// one machine cannot pick the same path. That is not a theoretical concern:
/// `/tmp/body.md` is the obvious name, everybody picks it, and two writers
/// seconds apart published one PR's body under the other's text (#625). A path
/// only this batch can name is the fix.
/// **`None` for an id this module will not build a name from**, enforced here
/// rather than by the caller — see [`scratch_worktree_path`] for why the
/// difference between "the guard runs first" and "the builder refuses" is the
/// difference between a positional guarantee and a structural one (rev-183).
fn body_file_path(batch_id: &str, kind: &str) -> Option<PathBuf> {
    // RED WITNESS (temporary, restored by the next commit) — see
    // `scratch_worktree_path`.
    Some(std::env::temp_dir().join(format!("loomux-mq-{}-{kind}.md", batch_id.trim())))
}

/// Write a body file, run `f` with its path, and remove it whatever happened.
///
/// `--body-file` rather than `--body` because a batch body is multi-line and a
/// culprit comment is longer still; the arg-vector rule makes a long argument
/// safe but not portable, and `gh` reads a file the same way on every platform.
fn with_body_file<T>(
    batch_id: &str,
    kind: &str,
    text: &str,
    f: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, String> {
    let path = body_file_path(batch_id, kind)
        .ok_or_else(|| format!("refusing to build a body-file path from batch id {batch_id:?}"))?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    let out = f(&path.display().to_string());
    // Best effort: a leftover body file in the OS temp dir is inert, and failing
    // a landed batch over one would be the tail wagging the dog.
    let _ = std::fs::remove_file(&path);
    out
}

/// The PR number `gh pr create` just reported.
///
/// `gh` prints the new PR's URL on stdout, possibly after other chatter, so the
/// **last** line carrying `/pull/<n>` wins. Anything else is an error rather
/// than a guess: a draft PR whose number loomux cannot name is one it can never
/// observe, close, or clean up, and inventing a number would attach the batch to
/// somebody else's PR.
pub fn parse_created_pr(stdout: &str) -> Option<u64> {
    stdout.lines().rev().find_map(|line| {
        let (_, tail) = line.trim().rsplit_once("/pull/")?;
        tail.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    })
}

/// Open the batch's draft PR and return its number (§5).
fn open_draft_pr(
    r: &dyn MqRunner,
    batch_id: &str,
    branch: &str,
    target: &str,
    title: &str,
    body: &str,
) -> Result<u64, String> {
    with_body_file(batch_id, "batch", body, |file| {
        let argv = draft_pr_argv(branch, target, title, file);
        let out = r.gh(&as_args(&argv))?;
        if !out.ok() {
            return Err(if out.stderr.is_empty() {
                format!("gh pr create: exit {:?}", out.code)
            } else {
                out.stderr.clone()
            });
        }
        parse_created_pr(&out.stdout).ok_or_else(|| {
            format!(
                "gh pr create reported no PR number for {branch}; a draft PR may be open on \
                 that branch and will need closing by hand"
            )
        })
    })
}

/// Post the culprit comment (§9).
fn post_comment(r: &dyn MqRunner, batch_id: &str, pr: u64, body: &str) -> Result<(), String> {
    with_body_file(batch_id, "culprit", body, |file| {
        let out = r.gh(&as_args(&pr_comment_argv(pr, file)))?;
        if out.ok() {
            Ok(())
        } else if out.stderr.is_empty() {
            Err(format!("gh pr comment {pr}: exit {:?}", out.code))
        } else {
            Err(out.stderr.clone())
        }
    })
}

// ── the driver's notices ────────────────────────────────────────────────────

/// A batch landed. Decision-grade: what is on the target now, and what the
/// orchestrator no longer has to chase.
///
/// The sub-PRs are named as bare `#N` references. This text is loomux-authored
/// and it reaches an orchestrator pane rather than GitHub, so the closing-keyword
/// scan cannot see it — but the rule that loomux's own text never carries the
/// pattern is a habit worth keeping uniform, because a notice's wording is
/// exactly the kind of thing that later gets pasted into a PR body.
pub fn landed_notice(batch_id: &str, target: &str, prs: &[u64]) -> String {
    let named: Vec<String> =
        prs.iter().take(MAX_SIBLINGS_LISTED).map(|p| format!("#{p}")).collect();
    let more = if prs.len() > MAX_SIBLINGS_LISTED {
        format!(" +{} more", prs.len() - MAX_SIBLINGS_LISTED)
    } else {
        String::new()
    };
    format!(
        "[loomux] merge queue: batch {} landed on {} - {}{}. The tested object is the target head.",
        quote(batch_id),
        quote(target),
        named.join(", "),
        more
    )
}

/// A green batch that was refused at the moment of submit (§6, §10).
pub fn land_refused_notice(
    batch_id: &str,
    culprit: Option<u64>,
    why: &str,
    batch_size: usize,
) -> String {
    match culprit {
        Some(pr) => format!(
            "[loomux] merge queue: batch {} was REFUSED at landing on #{pr} ({}). \
             #{pr} kicked back; {} sibling{} requeued; nothing landed.",
            quote(batch_id),
            quote(why),
            batch_size.saturating_sub(1),
            if batch_size == 2 { "" } else { "s" }
        ),
        None => format!(
            "[loomux] merge queue: batch {} could NOT land ({}). All {batch_size} entries \
             requeued; nothing landed and no PR is implicated.",
            quote(batch_id),
            quote(why)
        ),
    }
}

/// A batch that could not be constructed at all (§10's abort rows).
pub fn batch_aborted_notice(prs: &[u64], why: &str) -> String {
    format!(
        "[loomux] merge queue: a batch of {} entr{} was ABORTED before CI - {}. \
         Entries requeued; nothing landed.",
        prs.len(),
        if prs.len() == 1 { "y" } else { "ies" },
        quote(why)
    )
}

/// §9's infrastructure/flake case: the batch is still red at k = 1 while that
/// PR's **own** checks are green.
///
/// Distinct from [`culprit_notice`] because it asks for something different.
/// "This PR broke the batch" and "this PR looks fine on its own and the batch
/// still fails" send a reader to different places, and collapsing them would
/// route somebody to re-review a diff when the thing to look at is CI. §9 is
/// explicit that this is surfaced, not looped on — the entry is still kicked
/// back, exactly once.
pub fn flake_notice(culprit: u64, batch_id: &str, failing: &[String], survivors: usize) -> String {
    let check = failing.first().map(|f| format!(" ({})", quote(f))).unwrap_or_default();
    format!(
        "[loomux] merge queue: batch {} is still red at k=1 on #{culprit}{check}, but #{culprit}'s \
         OWN checks are green - treat this as infrastructure/flake, not a bad diff. \
         #{culprit} kicked back; {survivors} sibling{} requeued.",
        quote(batch_id),
        if survivors == 1 { "" } else { "s" }
    )
}

/// One line naming what a [`LandRefusal`] actually was — for the audit detail
/// and the notice, which §11.5 requires to name what happened.
fn land_refusal_text(refusal: &LandRefusal) -> String {
    match refusal {
        LandRefusal::Target { pr, refusal } => {
            format!("the constraint-7 re-check refused at submit on #{pr}: {}", refusal.clone().code())
        }
        LandRefusal::Gate { pr, recheck } => format!(
            "the merge gate no longer holds for #{pr}: {}",
            recheck.refusal_code().unwrap_or("gate-not-met")
        ),
        LandRefusal::BadScratch => "the recorded scratch sha is not an object name".to_string(),
        LandRefusal::PushFailed(e) => format!("the fast-forward push failed: {}", quote(e)),
    }
}
