//! The bisecting merge queue — **the driver's write primitives** (#581 slice D1).
//!
//! Design note: `doc/design/merge-queue.md`. Section references below (§4, §5,
//! §6, §7, §8, §10, §11) point into it; that note is the spec this file
//! implements and the argument for every choice made here.
//!
//! # Where the seam is, and why there is one at all
//!
//! `mergeq.rs` (slice C) is the queue's **pure core**: it decides which entries
//! go into a batch, which half a bisect tests next, and whether the merge gate
//! still holds — all as functions over values, with no I/O anywhere. This module
//! is the other half: it is the only place in the queue that talks to `git` and
//! `gh`, and every one of those calls goes through the [`MqRunner`] trait.
//!
//! That trait is the test seam, and it is a **trait parameter** rather than the
//! thread-local override `sessions.rs` uses, because the thing under test here
//! is the **argument vector**, not a directory: a `&dyn MqRunner` a test
//! supplies can record every argv it is handed and hand back canned output, so
//! `tests/mergequeue.rs` pins the exact bytes that would reach the real binaries
//! without a network, a remote, or a real `gh` (CLAUDE.md constraint 3). §8's
//! own sketch (`build_scratch(…, git: &impl GitRunner)`) asks for this shape.
//!
//! **Argv-level assertions are not a stylistic preference here.** §4 spells out
//! why for the scratch push in particular: *every* way of getting create-only
//! wrong degrades to a **silently successful ordinary push**, so an outcome-only
//! test ("did the ref end up at the right SHA?") passes in exactly the cases the
//! check exists to prevent. The same posture covers the landing refspec (§7.5).
//!
//! # What lives here (slice D1) and what is deliberately still missing
//!
//! Here: the [`MqRunner`] seam and its real process implementation; the **live**
//! default-branch and PR lookups §7 requires; the [`validate_target`] refusal
//! core those three enforcement points share; scratch-ref minting with §4's
//! remote collision check; the **create-only** scratch push; the **landing**
//! function, which re-resolves at the moment of submit and is the only thing
//! that ever builds a landing refspec; the [`BatchVerification`] adapter over
//! `notify.rs`'s classification (§5); and namespace-exact cleanup (§10).
//!
//! Not here, and in [`crate::mqloop`] (D2) instead: §8's
//! batch construction (the merge
//! commits onto the scratch), the draft PR and its body builder, the bounded
//! observation loop (`checks_timeout_minutes`), §9's bisect and culprit
//! attribution, §4's crash reconcile, and `merge_queue.json` persistence — plus,
//! since #698 (D3), the **driver tick** that sequences all of it from the
//! unified `gh` poll loop. Everything in this module is reached from a running
//! loomux through that tick; §13 of the design note is the path.
//!
//! # The two properties this module exists to hold
//!
//! 1. **The queue is structurally incapable of targeting the default branch**
//!    (§7). Every branch name that reaches a refspec has come out of
//!    [`validate_target`], which is fed **live** lookups — never
//!    `git::default_base_ref`, never a stored `Task.pr_base`, never a caller's
//!    string. See [`validate_target`]'s own comment for why the local helper is
//!    the wrong authority.
//! 2. **The backend writes only what it can name.** The scratch push is
//!    create-only by *primitive*; the landing push is fast-forward by *verb*;
//!    cleanup deletes only the exact ref this batch minted. No pattern, no
//!    glob, no `--force`.
//!
//! # Why it is in this crate (#888 slice A3 batch 12a)
//!
//! Nothing here is desktop-specific: `std::process` plus [`crate::subproc`]'s
//! bounded capture, and every outbound edge — [`crate::mergeq`],
//! [`crate::notify`], [`crate::workflow`] — was already on this side. The queue
//! runs on the same single `gh` poll loop a headless daemon would run, so the
//! driver belongs with the core it drives rather than with the window.
//!
//! **Three items are `pub(crate)`, and that is the whole of their reach**:
//! [`as_args`], [`landable`] and [`declares_ci_green`]. They were `pub(super)`
//! in `src-tauri/src/orchestration/mqdriver.rs` — visible within
//! `orchestration` — and batch 12a had to widen them to `pub` for exactly one
//! reason: their only caller, `mqloop`, was still on the other side of the
//! crate boundary, and no visibility narrower than `pub` crosses one. Batch 12b
//! moved `mqloop` here, so that reason expired and the keyword went back down.
//! `pub(crate)` is the faithful translation of the old `pub(super)`, not a new
//! narrowing invented for this batch: the scope that used to be "the
//! `orchestration` module" is now "this crate".
//!
//! It is also load-bearing, which is why it was worth reverting rather than
//! leaving as harmless surplus. [`landable`] is **half** of the constraint-7
//! refusal — the refspec-shape predicate — and [`validate_target`] is the whole
//! of it, ordering the unverifiable / default-branch / target / assertion
//! refusals so an unreadable answer can never *fail to match* the default and
//! read as safe. While the item was `pub`, "reach the half instead" was
//! reachable from anywhere in `src-tauri` (which depends on this crate directly
//! and already spells `loomux_engine::…` in `gh.rs` and `obs.rs`), and no
//! re-export shape could have prevented it — batch 12a's header said so
//! explicitly and could do nothing about it. Now the compiler prevents it. If
//! you are writing a new branch-name guard: [`validate_target`], and from
//! inside this crate you have no other option.

use crate::mergeq::{new_batch_id, scratch_branch, GateRecheck, GateSpec, PrObservation};
use crate::notify::{self, PollResult};
use crate::workflow::{body_digest, BlockId, ReviewVerdict};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The remote the queue reads and writes. Fixed, not configurable: the whole
/// point of §3's bounded new authority is that the set of objects the backend
/// can touch is enumerable from the code, and a configurable remote name is one
/// more input that would have to be validated at every call site.
pub const REMOTE: &str = "origin";

/// How many times [`mint_scratch`] will re-roll a batch id whose scratch ref
/// already exists on the remote before giving up (§4/§10). Bounded because
/// "keep trying" is the failure mode `.loomux/lessons.md` records over and
/// over; on exhaustion the batch fails **loudly** as `mq-scratch-collision`.
pub const MINT_ATTEMPTS: usize = 3;

// ── the seam (§3, §8) ───────────────────────────────────────────────────────

/// One completed external command: what it wrote, and how it exited.
///
/// The exit **code** is carried rather than collapsed into `Result` because the
/// queue has at least one call whose whole meaning is its code:
/// `git ls-remote --exit-code` answers "does this ref exist" as `0` vs `2`, and
/// a `Result<String, String>` seam would make "no such ref" and "the network is
/// down" the same value — which is precisely the confusion §4's collision check
/// must not have, since one of them means *push* and the other means *refuse*.
///
/// Mirrors `gh.rs::gh_output`'s contract: a non-zero exit is data, not an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmdOut {
    /// `None` when the process was killed by a signal (Unix) — never treated as
    /// success by anything in this module.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOut {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout with surrounding whitespace removed — every `gh --jq` and
    /// `git rev-parse` answer this module reads is a single line.
    pub fn line(&self) -> &str {
        self.stdout.trim()
    }
}

/// **The single seam between the merge queue and the outside world.**
///
/// Both methods take an **arg vector**, never a shell string — the `gh.rs` /
/// `git.rs` house rule, which makes shell injection impossible regardless of
/// what a branch name or PR body contains. `Err` means the command could not be
/// *run at all* (§10: `gh` not installed surfaces as `gh-not-found`); a command
/// that ran and failed comes back as `Ok(CmdOut)` with a non-zero code, because
/// which non-zero code it was is frequently the answer.
///
/// Implementors are `Send + Sync` so a future D2 driver thread can hold one.
pub trait MqRunner: Send + Sync {
    /// Run `git` in the repository this runner is bound to.
    fn git(&self, args: &[&str]) -> Result<CmdOut, String>;
    /// Run `gh` in the repository this runner is bound to.
    fn gh(&self, args: &[&str]) -> Result<CmdOut, String>;
}

/// The real implementation: spawn the binaries, in the repo root, with the same
/// hardening `gh.rs::gh_output` and `git.rs::run_git` already apply —
/// `current_dir` validated, arg vectors, `NO_COLOR`/`GH_PAGER`/
/// `GH_PROMPT_DISABLE`, `GIT_TERMINAL_PROMPT=0`, and `CREATE_NO_WINDOW` on
/// Windows so a backend-initiated call never flashes a console.
///
/// `GIT_TERMINAL_PROMPT=0` matters more here than it does in `git.rs`: this is
/// the first git **write** loomux performs without a human having pressed
/// anything, and a push that blocks on a credential prompt in a headless
/// backend would be an unbounded wait with nobody watching it.
pub struct ProcessRunner {
    repo: PathBuf,
}

impl ProcessRunner {
    pub fn new(repo: impl Into<PathBuf>) -> ProcessRunner {
        ProcessRunner { repo: repo.into() }
    }

    fn run(&self, bin: &str, args: &[&str], extra_env: &[(&str, &str)]) -> Result<CmdOut, String> {
        use std::process::Command;
        if !self.repo.is_dir() {
            return Err(format!("no such directory: {}", self.repo.display()));
        }
        let mut cmd = Command::new(bin);
        cmd.current_dir(&self.repo).args(args);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        // **Bounded** (#656, applied here by #698). These calls run inside the
        // one `gh` poll loop, so a `git fetch` parked on a stalled connection
        // would stop every `notify_when` notice in the fleet from firing — the
        // exact failure #656 closed for `gh_capture`, and the one the fleet's
        // "register the watch, end the turn" discipline rests on. Reusing that
        // primitive rather than writing a second one is deliberate: every arm
        // of it (null stdin, both pipes drained concurrently, the kill's own
        // bounded reap, the abandoned-reader ceiling) exists because of a
        // specific way an unbounded wait bites, and a copy would drift.
        match crate::subproc::capture_raw_with_timeout(cmd, MQ_CMD_TIMEOUT) {
            Ok((status, stdout, stderr)) => Ok(CmdOut {
                code: status.code(),
                stdout,
                stderr: stderr.trim().to_string(),
            }),
            // The spawn-failure sentinel §10 reports the queue **unavailable**
            // on. `capture_raw_with_timeout` flattens the spawn error to a
            // string, so the not-found case is recognised by its text rather
            // than by `ErrorKind` — matched on the two spellings the std error
            // uses across platforms, and falling through to the raw message
            // rather than guessing when it is something else.
            Err(e) if is_not_found(&e) => Err(format!("{bin}-not-found")),
            Err(e) => Err(e),
        }
    }
}

/// How long one merge-queue `git`/`gh` call may run before it is killed (#656's
/// bound, this feature's value).
///
/// Longer than `GH_CAPTURE_TIMEOUT`'s 20s because the work is different in kind:
/// a `gh pr list` is a single API read, while a batch build is a `git fetch` of
/// several pull refs plus one merge per sub-PR against a real working tree.
/// Short enough that the worst case still matters: this runs on the shared poll
/// loop, so a hung call parks every watch notice for its duration. Sixty seconds
/// is the trade — a fetch that has not finished in a minute is not going to, and
/// a batch that trips this aborts loudly and backs the group off rather than
/// retrying into the same stall.
pub const MQ_CMD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a spawn error means the binary is not installed.
///
/// Kept to the two spellings `std::io::Error`'s `NotFound` produces on the
/// platforms this ships on, rather than a substring like "not found" that a
/// *repository* path in the message could also satisfy — mislabeling a missing
/// directory as a missing `git` would send a reader looking for the wrong thing.
fn is_not_found(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    e.contains("no such file or directory")
        || e.contains("the system cannot find the file specified")
        || e.contains("program not found")
}

impl MqRunner for ProcessRunner {
    fn git(&self, args: &[&str]) -> Result<CmdOut, String> {
        self.run("git", args, &[("GIT_TERMINAL_PROMPT", "0")])
    }

    fn gh(&self, args: &[&str]) -> Result<CmdOut, String> {
        self.run(
            "gh",
            args,
            &[("NO_COLOR", "1"), ("GH_PAGER", ""), ("GH_PROMPT_DISABLED", "1")],
        )
    }
}

// ── the live lookups (§7) ───────────────────────────────────────────────────

/// The argv for the repo default-branch lookup — **the shim's own lookup**
/// (`orchestration/mod.rs:759`), in Rust.
///
/// Note #294 by construction: the shim passes the repo **positionally** to
/// `gh repo view` (`-R` is not a flag it accepts), and this call passes no repo
/// at all — `gh` infers it from the runner's `current_dir`, the same trust
/// boundary `gh.rs` documents. There is therefore no repo string to get wrong.
pub fn default_branch_argv() -> Vec<String> {
    vec![
        "repo".into(),
        "view".into(),
        "--json".into(),
        "defaultBranchRef".into(),
        "--jq".into(),
        ".defaultBranchRef.name".into(),
    ]
}

/// The argv for the per-PR lookup: the base branch, the live head, and the body
/// (the last two feed the gate re-check, §6).
///
/// One call for three facts, like the shim's `pr view --json baseRefName,number`
/// — a second round-trip would be a second moment, and §6's whole point is that
/// the gate is re-verified at **one** moment.
pub fn pr_facts_argv(pr: u64) -> Vec<String> {
    vec![
        "pr".into(),
        "view".into(),
        pr.to_string(),
        "--json".into(),
        "baseRefName,headRefOid,body".into(),
    ]
}

/// The argv for one PR's **own** checks (§6: `also: [ci-green]` means the
/// sub-PR's checks, never the batch's). Field set pinned to what
/// `notify::pr_checks_result` parses.
pub fn pr_checks_argv(pr: u64) -> Vec<String> {
    vec![
        "pr".into(),
        "checks".into(),
        pr.to_string(),
        "--json".into(),
        "state,name,link".into(),
    ]
}

/// What one `gh pr view` round-trip tells the queue about a sub-PR.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrFacts {
    pub base: String,
    pub head: String,
    pub body: String,
}

#[derive(Deserialize)]
struct RawPrFacts {
    #[serde(rename = "baseRefName", default)]
    base_ref_name: String,
    #[serde(rename = "headRefOid", default)]
    head_ref_oid: String,
    #[serde(default)]
    body: String,
}

pub(crate) fn as_args(v: &[String]) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

/// Resolve the repository's default branch **live**, the way the shim does.
///
/// Any failure — `gh` missing, a non-zero exit, an empty answer — is
/// [`TargetRefusal::BaseUnverifiable`], never a default and never an empty
/// string that flows onward. This mirrors the shim's `unverifiable-base` posture
/// at `orchestration/mod.rs:761-763`: **unknown is never treated as safe.**
pub fn resolve_default_branch(r: &dyn MqRunner) -> Result<String, TargetRefusal> {
    resolve_default_branch_detailed(r).map_err(ResolveFailure::into_refusal)
}

/// Why a live lookup did not produce a usable answer, split by **who failed**.
///
/// [`TargetRefusal`] deliberately collapses both into `base-unverifiable`,
/// because §11.1's refusal vocabulary is closed and, for a caller deciding
/// whether *this PR* may be queued, "unknown" is one answer however it arose.
/// A caller deciding **whether to keep going** needs the split:
///
/// - [`ResolveFailure::Refused`] — the remote answered, and the answer is one
///   the queue will not act on (the base is the default branch; the base is not
///   the target). A fact about **that PR**: the next entry is a different
///   question and worth asking.
/// - [`ResolveFailure::Runner`] — the call did not complete at all: `gh`
///   missing, or a remote answering slowly enough to burn `MQ_CMD_TIMEOUT`. A
///   fact about **the world**: the next entry costs the same again, for the
///   same reason, and none of them will answer.
///
/// The distinction is not cosmetic, and it is not new to this file — `land()`
/// already draws exactly this line (`backoff = culprit.is_none()`). It matters
/// here because the selection pass runs inside the one shared `gh` poll loop:
/// examining `MAX_EXAMINED_PER_BUILD` entries against a slow-but-answering
/// remote would be that many timeouts back to back, a slice of the loop that
/// delivers every `notify_when` notice in the fleet — which is #656's point,
/// undone by fan-out the counts alone do not bound. Treating the first runner
/// failure as terminal for the pass bounds it at **one** timed-out call.
///
/// Same posture as `capture_raw_with_timeout` one layer down: one implementation
/// of the fallible thing, two readings of its result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveFailure {
    /// The seam itself failed. Carries `git`/`gh`'s own message for the audit.
    Runner(String),
    /// The remote answered and the queue refuses that answer.
    Refused(TargetRefusal),
}

impl ResolveFailure {
    /// The closed-vocabulary refusal an MCP caller sees. A runner failure is
    /// `base-unverifiable` — unknown is never treated as safe (§7.1).
    pub fn into_refusal(self) -> TargetRefusal {
        match self {
            ResolveFailure::Runner(_) => TargetRefusal::BaseUnverifiable,
            ResolveFailure::Refused(t) => t,
        }
    }

    /// Whether the world failed rather than this PR.
    pub fn is_runner(&self) -> bool {
        matches!(self, ResolveFailure::Runner(_))
    }
}

/// [`resolve_default_branch`] keeping the who-failed split.
pub fn resolve_default_branch_detailed(r: &dyn MqRunner) -> Result<String, ResolveFailure> {
    let out = r.gh(&as_args(&default_branch_argv())).map_err(ResolveFailure::Runner)?;
    if !out.ok() {
        // The process ran and `gh` said no. That is an answer, not a stall —
        // an unauthenticated or renamed repo refuses every entry equally, and
        // the caller's own loop decides what to do about that.
        return Err(ResolveFailure::Refused(TargetRefusal::BaseUnverifiable));
    }
    let name = out.line().to_string();
    if name.is_empty() {
        return Err(ResolveFailure::Refused(TargetRefusal::BaseUnverifiable));
    }
    Ok(name)
}

/// Resolve one PR's base, head and body **live**. Same refusal posture as
/// [`resolve_default_branch`]: an unreadable PR is unverifiable, not "probably
/// fine".
///
/// An empty `headRefOid` is left empty rather than being an error here, because
/// `mergeq::recheck_gate` already treats an empty head as **no** head
/// (`UnknownRevision`, its rev-157 filter) — one refusal, in the module that
/// owns the gate decision, rather than two that could drift apart.
pub fn resolve_pr(r: &dyn MqRunner, pr: u64) -> Result<PrFacts, TargetRefusal> {
    resolve_pr_detailed(r, pr).map_err(ResolveFailure::into_refusal)
}

/// [`resolve_pr`] keeping the who-failed split — see [`ResolveFailure`].
pub fn resolve_pr_detailed(r: &dyn MqRunner, pr: u64) -> Result<PrFacts, ResolveFailure> {
    let out = r.gh(&as_args(&pr_facts_argv(pr))).map_err(ResolveFailure::Runner)?;
    if !out.ok() {
        return Err(ResolveFailure::Refused(TargetRefusal::BaseUnverifiable));
    }
    let raw: RawPrFacts = serde_json::from_str(out.line())
        .map_err(|_| ResolveFailure::Refused(TargetRefusal::BaseUnverifiable))?;
    Ok(PrFacts {
        base: raw.base_ref_name.trim().to_string(),
        head: raw.head_ref_oid.trim().to_string(),
        body: raw.body,
    })
}

// ── the refusal core (§7) ───────────────────────────────────────────────────

/// Why a branch may not become, or may not remain, the queue's target.
///
/// The spellings are §11.1's closed refusal vocabulary; nothing here invents a
/// reason outside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetRefusal {
    /// The PR's base **is** the repository default branch. Constraint 7.
    BaseIsDefault,
    /// A lookup failed, or came back empty, or came back as something the queue
    /// will not build a refspec from. The shim's `unverifiable-base` arm.
    BaseUnverifiable,
    /// The queue already has a target and this base is not it (§4: never
    /// silently retargeted), or the caller's optional `target` assertion did not
    /// match what the base resolved to.
    BaseNotTarget,
}

impl TargetRefusal {
    /// The §11.1 code an MCP caller sees.
    pub fn code(self) -> &'static str {
        match self {
            TargetRefusal::BaseIsDefault => "base-is-default",
            TargetRefusal::BaseUnverifiable => "base-unverifiable",
            TargetRefusal::BaseNotTarget => "base-not-target",
        }
    }
}

/// Whether the queue is willing to build a **refspec** out of this name.
///
/// Deliberately much stricter than "git would accept it". Two independent jobs:
///
/// - **It closes the argv surface.** The validated target is interpolated into
///   `<sha>:refs/heads/<target>` and handed to `git push` as one argument. A
///   name containing `:` would silently split the refspec and land the batch on
///   a *different* ref; a name starting with `-` would be read as a flag. Arg
///   vectors make shell injection impossible, and this makes *refspec* injection
///   impossible, which is the analogous hazard one layer down.
/// - **It rejects the non-answers.** The literal `HEAD` is what
///   `git::default_base_ref` falls through to when it cannot resolve anything,
///   and it is exactly the kind of plausible-looking string a security refusal
///   must not accept — see [`validate_target`].
pub(crate) fn landable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name.is_ascii()
        && !name.chars().any(|c| c.is_control() || c.is_whitespace())
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && !name.chars().any(|c| matches!(c, ':' | '?' | '*' | '[' | '\\' | '~' | '^'))
        && !name.starts_with("refs/")
        && name != "HEAD"
}

/// Whether a name is usable as the **right-hand side of a comparison** — the
/// default branch, the recorded target, a caller's assertion.
///
/// Deliberately looser than [`landable`] in exactly one way: it accepts a
/// `refs/heads/`-qualified spelling, which [`same_branch`] then normalizes.
/// **The asymmetry is the point, and it is a security property, not a
/// convenience.** `base` becomes a refspec component, so it must be a plain
/// branch name or nothing. `default` is *only ever compared* — it never reaches
/// an argument position — so applying `landable` to it would mean that a `gh`
/// which one day answered `refs/heads/main` made the default-branch refusal stop
/// firing on the comparison and start refusing everything as unverifiable
/// instead. Fail-closed either way, but the first is a refusal that says the
/// wrong thing, and #581's whole §7 rests on this comparison landing.
///
/// Defined as [`landable`] applied after an optional `refs/heads/` is stripped,
/// rather than as its own looser list — so the `refs/heads/` allowance is the
/// *only* difference between the two, provably, instead of being the difference
/// a reader has to diff two predicates to find.
///
/// Everything else [`landable`] refuses is still refused here, and that
/// direction matters: a `default` carrying a `:` or a `*` is not a branch git
/// would let exist, so it is a **corrupt answer**, and a corrupt answer must
/// refuse rather than merely fail to match the base — failing to match is
/// failing to fire the constraint-7 refusal. The literal `HEAD` that
/// `git::default_base_ref` degrades to is refused in both spellings.
fn usable_for_comparison(name: &str) -> bool {
    let name = name.trim();
    landable(name.strip_prefix("refs/heads/").unwrap_or(name))
}

/// Compare two branch names the way a refusal must: exactly, after normalizing
/// the one shape `gh` and `git` disagree about.
///
/// `gh repo view --jq .defaultBranchRef.name` returns a short name (`main`), and
/// so does `gh pr view --json baseRefName` — but a ref read from `git`, or a
/// target recorded by another build, may arrive as `refs/heads/main`, and a
/// refusal that missed on that spelling would be a refusal that does not fire.
///
/// Nothing is *rewritten* here; this only decides whether two strings name the
/// same branch. Note where the normalization can actually bite: `base` has
/// already been through [`landable`], which rejects a `refs/`-qualified name
/// outright, so the live-vs-live half is normalized on the **default's** side —
/// see [`usable_for_comparison`] for why that side is deliberately looser.
///
/// # Both qualified spellings, and why over-matching is the safe direction
///
/// A branch identity can arrive here in three forms: bare (`main`),
/// ref-qualified (`refs/heads/main`), and **remote-qualified** (`origin/main`).
/// All three are normalized, because this comparison's job is to *catch* a
/// match, and the two failure directions are not symmetric:
///
/// - **Under-matching fails open.** A `default` of `origin/main` compared
///   naively against a `base` of `main` does not match, so the constraint-7
///   refusal never fires and the queue pushes to the **default branch**. That is
///   the same shape as the `refs/heads/` bypass, one spelling over.
/// - **Over-matching fails closed.** A branch *literally named* `origin/x`
///   compares equal to `x` and is refused as if it were the default. That is a
///   false refusal on a pathological name, and a false refusal is a batch that
///   does not land — recoverable, loud, and nobody's data.
///
/// So the remote prefix is stripped too. This is deliberately **not** left to
/// "the only producer is `resolve_default_branch`, which returns short names":
/// that is true today and is a discipline a future slice sourcing `default` from
/// a git read, a config value or a cached target would silently break, with a
/// push to the default branch as the cost.
fn same_branch(a: &str, b: &str) -> bool {
    fn short(s: &str) -> &str {
        let s = s.trim();
        let s = s.strip_prefix("refs/heads/").unwrap_or(s);
        // Derived from `REMOTE` rather than a second `"origin/"` literal, and
        // the separate `/` step is what keeps `originalthing` from being read
        // as the remote-qualified `althing`.
        s.strip_prefix(REMOTE).and_then(|r| r.strip_prefix('/')).unwrap_or(s)
    }
    // `refs/remotes/origin/main` needs no arm here: `usable_for_comparison` is
    // `landable` after one optional `refs/heads/` strip, and `landable` rejects
    // anything still starting with `refs/` — so that spelling refuses as
    // `base-unverifiable` before it can reach this comparison. Three spellings
    // in, two mechanisms, no gap.
    !short(a).is_empty() && short(a) == short(b)
}

/// **The one place a branch becomes a legal queue target** — §7 layers 1, 2 and
/// 3 all funnel through this function, so the three enforcement points cannot
/// drift apart into three slightly different opinions.
///
/// # Which lookup is authoritative
///
/// `base` and `default` must be **live** answers from the real `gh` — the same
/// two lookups the shim makes at `orchestration/mod.rs:750` and `:759`. They are
/// deliberately **not** `git::default_base_ref:631`, despite that being the
/// obvious reuse: that helper answers "what does local git think the default
/// base is", derived from local refs after a best-effort fetch, and it falls
/// through to the literal string `"HEAD"` when it can resolve nothing. Local
/// refs are not an authority a security refusal may rest on, and a fallback that
/// produces a plausible branch-shaped string on failure is the worst possible
/// input to a comparison whose whole job is to catch a match. Reuse is a virtue
/// right up to the point where it changes what a check means.
///
/// For the same reason `Task.pr_base` (slice A) never reaches here: it is
/// agent-writable board data, a display hint, and its own doc comment says so.
///
/// # Order of refusals, and why it is this order
///
/// 1. **Unverifiable first.** If either name is missing, or is a string the
///    queue will not build a refspec from ([`landable`]), nothing downstream can
///    be decided — including whether it is the default branch. Refusing here
///    means an unreadable answer can never *fail to match* the default and thus
///    read as safe.
/// 2. **Default second.** Constraint 7 outranks every other consideration: a
///    base equal to the default is refused even if it is also the current target
///    and even if the caller asserted it. This is the arm that catches the
///    adversarial §7.3 case — the default branch **renamed to the target's name**
///    between batch build and landing — because at landing this function is
///    called again with a freshly resolved `default`, and the target it was
///    about to write to now *is* that default.
/// 3. **Target third.** An established target is never silently retargeted (§4):
///    the entries already queued were approved against a different branch.
/// 4. **The caller's assertion last.** §4: `queue_merge`'s optional `target`
///    argument is an **assertion, not a selection** — it can narrow what happens,
///    never widen it. It is checked after everything else precisely so that it
///    can only ever turn an allow into a refusal.
///
/// Returns the validated branch name — the **only** string in this module that
/// any refspec is ever built from.
pub fn validate_target(
    base: &str,
    default: &str,
    current_target: Option<&str>,
    asserted: Option<&str>,
) -> Result<String, TargetRefusal> {
    let base = base.trim();
    let default = default.trim();
    // Two different tests, on purpose — see `usable_for_comparison`. `base` is
    // the string a refspec gets built from, so it must be a plain branch name;
    // `default` is only ever compared, so a qualified spelling of it must still
    // be able to trip the refusal below rather than being turned away here.
    if !landable(base) || !usable_for_comparison(default) {
        return Err(TargetRefusal::BaseUnverifiable);
    }
    if same_branch(base, default) {
        return Err(TargetRefusal::BaseIsDefault);
    }
    if let Some(t) = current_target.map(str::trim).filter(|t| !t.is_empty()) {
        if !same_branch(base, t) {
            return Err(TargetRefusal::BaseNotTarget);
        }
    }
    if let Some(a) = asserted.map(str::trim).filter(|a| !a.is_empty()) {
        if !same_branch(base, a) {
            return Err(TargetRefusal::BaseNotTarget);
        }
    }
    Ok(base.to_string())
}

/// [`validate_target`] with the two live lookups performed for you — the whole
/// of §7 layer 1 (enqueue) and layer 2 (batch build) in one call.
///
/// The landing layer does **not** use this: it needs the lookups and the refspec
/// construction inside a single function so nothing can be resolved early and
/// carried (see [`land_batch`]).
pub fn resolve_and_validate_target(
    r: &dyn MqRunner,
    pr: u64,
    current_target: Option<&str>,
    asserted: Option<&str>,
) -> Result<(String, PrFacts), TargetRefusal> {
    resolve_and_validate_target_detailed(r, pr, current_target, asserted)
        .map_err(ResolveFailure::into_refusal)
}

/// [`resolve_and_validate_target`] keeping the who-failed split — see
/// [`ResolveFailure`]. `validate_target`'s own refusals are always `Refused`:
/// they are decisions about answers loomux already has.
pub fn resolve_and_validate_target_detailed(
    r: &dyn MqRunner,
    pr: u64,
    current_target: Option<&str>,
    asserted: Option<&str>,
) -> Result<(String, PrFacts), ResolveFailure> {
    let facts = resolve_pr_detailed(r, pr)?;
    let default = resolve_default_branch_detailed(r)?;
    let target = validate_target(&facts.base, &default, current_target, asserted)
        .map_err(ResolveFailure::Refused)?;
    Ok((target, facts))
}

// ── minting a scratch ref (§4, §11.4) ───────────────────────────────────────

/// The argv that asks the remote whether a scratch ref already exists (§4).
///
/// `--exit-code` is what makes the answer a **code** rather than an inference
/// from empty stdout: `0` found, `2` not found, anything else a real failure.
/// [`scratch_exists`] refuses on that third case rather than reading it as
/// absent — the whole reason [`CmdOut`] carries the code at all.
pub fn ls_remote_argv(branch: &str) -> Vec<String> {
    vec![
        "ls-remote".into(),
        "--exit-code".into(),
        REMOTE.into(),
        format!("refs/heads/{branch}"),
    ]
}

/// Does this scratch ref already exist on the remote?
///
/// `Err` means the question could not be answered, and every caller treats that
/// as a refusal. That distinction is load-bearing: reading a network failure as
/// "absent" would push a fresh batch onto a **leaked** scratch ref from a
/// crashed earlier batch, which is the one way this design can end up testing an
/// object it did not construct (§4) — the Bors invariant failing at the one
/// point §8 does not guard.
pub fn scratch_exists(r: &dyn MqRunner, branch: &str) -> Result<bool, String> {
    let out = r.git(&as_args(&ls_remote_argv(branch)))?;
    match out.code {
        Some(0) if !out.line().is_empty() => Ok(true),
        // `--exit-code` promises a non-zero exit when nothing matched, so a
        // zero exit with no output is a contradiction, not an absence. Refuse.
        Some(0) => Err(format!("git ls-remote {branch}: exit 0 with no ref listed")),
        Some(2) => Ok(false),
        _ => Err(if out.stderr.is_empty() {
            format!("git ls-remote {branch}: exit {:?}", out.code)
        } else {
            out.stderr.clone()
        }),
    }
}

/// A minted scratch ref: the batch id, its branch name, and how many ids had to
/// be rolled to get one the remote did not already have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Minted {
    pub batch_id: String,
    pub branch: String,
    /// 1 on the first-try common case. Reported rather than discarded so the
    /// `mq-batch-built` audit event can say a collision happened and was
    /// survived — a silent retry is a fact nobody can see afterwards.
    pub attempts: usize,
}

/// Why a mint failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MintError {
    /// [`MINT_ATTEMPTS`] ids in a row already existed on the remote. Audited as
    /// `mq-scratch-collision` and the batch fails **loudly** (§10).
    Collision { attempts: usize },
    /// The remote could not be asked. Refuse rather than assume.
    Lookup(String),
    /// `mergeq::scratch_branch` refused to build a name from this group id —
    /// rejected, never rewritten into a different one (§11.4).
    BadName,
}

/// Mint a scratch ref this batch may create (§4).
///
/// Three properties, and each is a thing a plausible driver would get wrong:
///
/// - **Refuse to mint onto an existing ref.** Re-roll the id instead.
/// - **Bounded.** [`MINT_ATTEMPTS`] and then a loud failure — no "keep trying"
///   arm, the rule `.loomux/lessons.md` records for every fallible signal.
/// - **Never delete to make room.** A ref loomux cannot account for is not a ref
///   it gets to overwrite, and blind deletion is exactly the sweep hazard §10
///   forbids. There is no code path in this module that deletes an unowned ref;
///   [`cleanup_scratch`] deletes only a name it rebuilt from this batch's own id.
///
/// The check is **not** load-bearing on its own — a check-then-act leaves a
/// window, so the act itself is the enforcement: [`push_scratch`] is create-only
/// by primitive (§4, and see its own comment).
pub fn mint_scratch(r: &dyn MqRunner, group: &str, now_ms: u64) -> Result<Minted, MintError> {
    for attempt in 1..=MINT_ATTEMPTS {
        let batch_id = new_batch_id(now_ms);
        let Some(branch) = scratch_branch(group, &batch_id) else {
            // The group id, not the generated batch id, is what can be
            // unbuildable here — and it will be unbuildable on every retry, so
            // this returns rather than burning the attempt budget.
            return Err(MintError::BadName);
        };
        match scratch_exists(r, &branch) {
            Ok(false) => return Ok(Minted { batch_id, branch, attempts: attempt }),
            Ok(true) => continue,
            // A remote that cannot be asked does not get retried into an
            // answer: an unanswerable question is a refusal, immediately.
            Err(e) => return Err(MintError::Lookup(e)),
        }
    }
    Err(MintError::Collision { attempts: MINT_ATTEMPTS })
}

// ── the create-only scratch push (§4) ───────────────────────────────────────

/// **The one place a scratch-push argv is built** (§4).
///
/// ```text
/// git push --force-with-lease=refs/heads/<branch>: origin <sha>:refs/heads/<branch>
///                                                ^
///                            the empty expect value — this colon IS the mechanism
/// ```
///
/// `--force-with-lease=<ref>:<expect>` with an **empty** `<expect>` means
/// *expect this ref not to exist*, so the push is rejected outright if it does.
/// The trailing colon is the whole thing; dropping it turns the lease into an
/// ordinary force push, which is the opposite of what is wanted.
///
/// **Why not a plain push.** "Create-only" is a named primitive, not a property
/// a plain push has. `git push origin <sha>:refs/heads/<b>` does *not* provide
/// it: if a leaked ref of the same name happens to be an **ancestor** of the new
/// scratch, the push is a fast-forward and succeeds **silently** — verified
/// empirically in review of the design note, and it is the exact case the
/// collision check exists for, since a leaked scratch built on an older head of
/// the same target is a plausible ancestor rather than an unrelated object.
/// Non-fast-forward rejection is therefore not the guarantee; it only catches
/// the *divergent* half of the failure.
///
/// **Why the tests assert this argv rather than the resulting ref.** Every way
/// of getting this wrong — no lease, a lease with a non-empty expect, a lease
/// with no colon, a plain push — degrades to a *silently successful* ordinary
/// push. An outcome-only test ("did the ref end up at the right SHA?") passes in
/// exactly the cases this function exists to prevent (§4). The alternative
/// primitive the note permits, `POST /repos/{o}/{r}/git/refs` with its
/// server-side 422, is not used here only because it would need a second
/// credential path; if it ever is, it replaces this function whole.
pub fn scratch_push_argv(sha: &str, branch: &str) -> Vec<String> {
    vec![
        "push".into(),
        format!("--force-with-lease=refs/heads/{branch}:"),
        REMOTE.into(),
        format!("{sha}:refs/heads/{branch}"),
    ]
}

/// Push the scratch ref, create-only. `Err` carries `git`'s own stderr — the
/// lease rejection reads as a normal non-fast-forward refusal, which is what the
/// caller audits as `mq-scratch-collision`.
///
/// A SHA that is not a plain object name is refused before any argv is built:
/// the scratch sha reaches an argument position, and `--force` is a valid
/// "branch name" to a shell-free arg vector but not to this queue.
pub fn push_scratch(r: &dyn MqRunner, sha: &str, branch: &str) -> Result<(), String> {
    let sha = sha.trim();
    if !is_object_name(sha) {
        return Err(format!("refusing to push a non-object-name scratch sha: {sha:?}"));
    }
    if !branch.starts_with("loomux/mq/") {
        // Belt and braces over `scratch_branch`: this module never pushes a
        // scratch outside the reserved namespace (§11.4), whatever it is handed.
        return Err(format!("refusing to push outside loomux/mq/*: {branch:?}"));
    }
    let out = r.git(&as_args(&scratch_push_argv(sha, branch)))?;
    if out.ok() {
        Ok(())
    } else if out.stderr.is_empty() {
        Err(format!("git push {branch}: exit {:?}", out.code))
    } else {
        Err(out.stderr)
    }
}

/// A full or abbreviated git object name: 7–64 lowercase-or-uppercase hex
/// characters and nothing else. Same posture as `workflow::sanitize_digest` —
/// keep it to what the thing can actually be, so a truncated read or a stray
/// flag can never reach an argument position.
fn is_object_name(s: &str) -> bool {
    let s = s.trim();
    (7..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

// ── observing the batch's checks (§5) ───────────────────────────────────────

/// The queue-shaped verdict on a set of checks (§5).
///
/// This is an **adapter** over `notify::pr_checks_result`, not a fork of it and
/// not a third classifier. §5 is explicit that terminal-state logic already
/// exists twice (`notify.rs` and `intake.rs::parse_pr_list`) and that a third
/// would be a defect; what the queue needs is a *narrowing*, because a watch and
/// a gate want different things out of the same answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchVerification {
    /// Still running — **including an empty check list and the just-pushed "no
    /// checks reported" case**, which the shared helper already gets right and
    /// which is the property §5 says matters most.
    Pending,
    /// Every check terminal and none failing.
    Green,
    /// Terminal with at least one failing check, named.
    Red { failing: Vec<String> },
    /// `gh` itself failed, the PR is conflicting, or the answer was a shape this
    /// build cannot classify. **Never green.** Nothing lands on this.
    Unavailable { why: String },
}

impl BatchVerification {
    pub fn is_green(&self) -> bool {
        matches!(self, BatchVerification::Green)
    }
}

/// One element of `gh pr checks --json state,name,link`.
///
/// A second *deserialization* struct, not a second *classification*: the
/// pass/fail/pending decisions below are `notify::check_is_failing` and the
/// shared helper, which are the one definition. `notify.rs`'s own `RawCheck` is
/// private to that module, and making it public to save eight lines would widen
/// a parsing detail into a cross-module contract for no gain.
#[derive(Deserialize)]
struct RawCheck {
    name: String,
    state: String,
}

/// Classify a `gh pr checks --json state,name,link` poll for the queue (§5).
///
/// `raw` is `Ok(stdout)` on a zero exit and `Err(stderr)` on a non-zero one —
/// exactly `notify::pr_checks_result`'s input, so "no checks reported on the
/// '<branch>' branch" (a just-pushed PR) is handled in the one place that
/// already handles it.
///
/// # `Met` is not green — this is the correction that makes the adapter exist
///
/// `pr_checks_result` returns `PollResult::Met` for a **failing** run as well as
/// a passing one, because for a *watch* "the checks resolved" is the event. A
/// queue that read `Met` as success would land a red batch. So `Met` is used
/// only for the fact it actually asserts — *the checks are terminal* — and the
/// success/failure split is then taken from the same rows using
/// `notify::check_is_failing`, the shared predicate that already encodes
/// GitHub's `SKIPPED`/`NEUTRAL` semantics (#290) and the rule that an
/// undocumented conclusion must never read as passing.
///
/// The summary string is deliberately **not** parsed for this. It is
/// human-facing notice text; deriving a gate decision by matching on its
/// wording would make a copy-edit a behavior change.
pub fn classify_checks(raw: Result<&str, &str>) -> BatchVerification {
    match notify::pr_checks_result(raw) {
        PollResult::Pending => BatchVerification::Pending,
        PollResult::Failed { why } => BatchVerification::Unavailable { why },
        PollResult::Conflicting => BatchVerification::Unavailable {
            why: "the PR is CONFLICTING, so GitHub will never create a check suite for it".into(),
        },
        PollResult::Met { .. } => {
            // Terminal — now decide WHICH terminal, from the rows themselves.
            let Ok(json) = raw else {
                // Unreachable via `pr_checks_result` (an Err input cannot yield
                // `Met`), and still handled rather than unwrapped: an
                // unclassifiable answer is Unavailable, never green.
                return BatchVerification::Unavailable {
                    why: "checks reported terminal with no output to classify".into(),
                };
            };
            let Ok(checks) = serde_json::from_str::<Vec<RawCheck>>(json) else {
                return BatchVerification::Unavailable {
                    why: "gh pr checks: bad JSON".into(),
                };
            };
            let failing: Vec<String> = checks
                .iter()
                .filter(|c| notify::check_is_failing(&c.state))
                .map(|c| c.name.clone())
                .collect();
            if failing.is_empty() {
                BatchVerification::Green
            } else {
                BatchVerification::Red { failing }
            }
        }
    }
}

/// Read one PR's **own** checks through the seam and classify them (§6:
/// `also: [ci-green]` is about the sub-PR, never the batch).
///
/// Returns the `Option<bool>` shape `mergeq::PrObservation::ci_green` wants:
/// `Some(true)` only on green, `Some(false)` on red, and **`None` for pending or
/// unavailable** — which `recheck_gate` turns into a refusal, mirroring the
/// shim's `ci-not-green` arm that treats failing, still-running and
/// no-checks-reported alike.
pub fn pr_ci_green(r: &dyn MqRunner, pr: u64) -> Option<bool> {
    pr_ci_green_detailed(r, pr).unwrap_or(None)
}

/// [`pr_ci_green`] keeping the who-failed split — see [`ResolveFailure`].
///
/// `Err` is the seam failing; `Ok(None)` is the shim's own `ci-not-green` arm,
/// which treats failing, still-running and no-checks-reported alike. The two
/// refuse identically at the gate, and the caller that must not spend another
/// `MQ_CMD_TIMEOUT` on the next entry needs them apart.
pub fn pr_ci_green_detailed(r: &dyn MqRunner, pr: u64) -> Result<Option<bool>, String> {
    let out = r.gh(&as_args(&pr_checks_argv(pr)))?;
    let raw = if out.ok() { Ok(out.stdout.as_str()) } else { Err(out.stderr.as_str()) };
    Ok(match classify_checks(raw) {
        BatchVerification::Green => Some(true),
        BatchVerification::Red { .. } => Some(false),
        BatchVerification::Pending | BatchVerification::Unavailable { .. } => None,
    })
}

// ── landing (§7.3, §7.4, §8) ────────────────────────────────────────────────

/// **The only landing refspec this codebase constructs**, and it takes the
/// validated target as its only branch input (§7.3).
///
/// There is no argument that could make it write elsewhere: `target` has come
/// out of [`validate_target`], which refuses anything containing a `:` (so the
/// refspec cannot be split), anything that is the default branch, and anything
/// that is not a plain branch name at all. And **no code path anywhere
/// constructs a push refspec from the default-branch name** — the default is
/// read in this module for exactly one purpose, comparison.
fn land_refspec(scratch_sha: &str, target: &str) -> String {
    format!("{scratch_sha}:refs/heads/{target}")
}

/// The argv for the landing push — §7.4's **only landing verb**.
///
/// Fast-forward only, by construction: no `--force`, no `+` on the refspec, no
/// lease. A target that moved under the batch therefore makes the push **fail**
/// rather than overwrite (§10), and the queue never calls `gh pr merge` at all,
/// so it cannot reach the shim's default-branch arms and the per-PR human grant
/// path (`orchestration/mod.rs:943-960`) is untouched.
pub fn land_push_argv(scratch_sha: &str, target: &str) -> Vec<String> {
    vec!["push".into(), REMOTE.into(), land_refspec(scratch_sha, target)]
}

/// Whether this gate names the `ci-green` clause, and therefore whether the
/// sub-PR's own checks have to be read at all.
///
/// **Fails toward fetching.** `Absent` and `Malformed` both answer `true`, even
/// though both refuse the landing outright a moment later: the alternative
/// couples "did we look at CI" to "could we read the gate file", and a future
/// edit that made an unreadable gate reachable would silently also have made it
/// unobserved. The cheap direction is the safe one.
pub(crate) fn declares_ci_green(spec: &GateSpec) -> bool {
    match spec {
        GateSpec::Declared(g) => g.also.iter().any(|c| c == "ci-green"),
        GateSpec::Absent | GateSpec::Malformed => true,
    }
}

/// Why a landing was refused. Every variant is a designed path in §10's table.
#[derive(Clone, Debug, PartialEq)]
pub enum LandRefusal {
    /// The constraint-7 re-check refused at the moment of submit — including the
    /// adversarial case where the default branch was **renamed to the target's
    /// name** between batch build and landing (§7.3).
    Target { pr: u64, refusal: TargetRefusal },
    /// The merge gate no longer holds for this sub-PR (§6's second enforcement
    /// point). That entry kicks back; survivors requeue.
    Gate { pr: u64, recheck: GateRecheck },
    /// The scratch sha is not an object name — refused before any argv is built.
    BadScratch,
    /// The fast-forward push itself failed: the target moved, or auth failed
    /// (§10). **No retry loop.**
    PushFailed(String),
}

/// What landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landed {
    /// The target as re-resolved **at submit** — not as it was recorded when the
    /// batch was built.
    pub target: String,
    /// The SHA that was tested and is now the target head. The Bors invariant
    /// (§8): the object CI judged *is* the object that landed, byte for byte.
    pub scratch_sha: String,
}

/// **Land a green batch** (§7.3, §7.4, §8).
///
/// This function performs the live lookups, the gate re-check and the refspec
/// construction **itself, in this order, with no intervening caller** — that
/// containment is the whole of §7 layer 3. Splitting it (resolving the target in
/// a caller and passing the string in) would reintroduce the window the layer
/// exists to close: the default branch renamed to the target's name between
/// build and landing, which a stale resolution would not see.
///
/// `recorded_target` is what `merge_queue.json` says the target is. It is an
/// **assertion, not a selection** — the target is re-derived from each sub-PR's
/// live base, and `recorded_target` only ever narrows the result (§4's posture
/// on `queue_merge`'s optional argument, applied one layer down).
///
/// Every sub-PR is re-checked before **anything** is pushed, so a batch with one
/// bad entry refuses without a partial write. §6's two enforcement points are
/// build and landing, and this is the second: between them there is a full CI
/// cycle, tens of minutes in which a reviewer can record a `fail`, a PR can be
/// rebased, or a body can change.
///
/// `verdicts` is supplied by the caller rather than read here because the
/// verdict files live in the group dir, which is the registry's business and not
/// this module's — the same separation that keeps `orchestration/mod.rs`
/// wiring-only.
pub fn land_batch(
    r: &dyn MqRunner,
    scratch_sha: &str,
    recorded_target: &str,
    prs: &[u64],
    gate: &GateSpec,
    verdicts: &dyn Fn(u64) -> BTreeMap<BlockId, ReviewVerdict>,
) -> Result<Landed, LandRefusal> {
    let scratch_sha = scratch_sha.trim();
    if !is_object_name(scratch_sha) {
        return Err(LandRefusal::BadScratch);
    }
    // A record that is not a branch name is a **corrupt record**, and §4's
    // reconcile posture for those is to fail loudly rather than proceed. It has
    // to be checked here rather than left to the per-PR comparison below,
    // because `validate_target` treats an empty `current_target` as "no target
    // established yet" (§4, correct at enqueue) — which at landing would mean
    // every PR validating against nothing but the default, and the batch pushing
    // to whatever the LAST sub-PR's base happened to be.
    //
    // An earlier cut defended that by threading each validated sibling into the
    // next iteration. Mutation run C (#668, M11) showed the threading never
    // changed an outcome: with any usable record the assertion below already
    // refuses, and with an unusable one this guard now refuses first. It was a
    // construct whose only remaining justification was "if this guard were
    // removed" — unfalsifiable by design, and its comment credited it for a
    // refusal the assertion actually produced. Removed rather than kept as
    // untestable belt-and-braces.
    if !landable(recorded_target.trim()) {
        return Err(LandRefusal::Target { pr: 0, refusal: TargetRefusal::BaseUnverifiable });
    }
    // Resolved ONCE, here, at the moment of submit — not carried in.
    let default = resolve_default_branch(r).map_err(|refusal| LandRefusal::Target {
        pr: prs.first().copied().unwrap_or(0),
        refusal,
    })?;

    let mut target: Option<String> = None;
    for &pr in prs {
        let facts = resolve_pr(r, pr).map_err(|refusal| LandRefusal::Target { pr, refusal })?;
        // Layer 3. Every sub-PR's live base must be the recorded target, so a
        // batch whose PRs disagree about their base refuses rather than landing
        // on whichever one was resolved first.
        let validated = validate_target(&facts.base, &default, Some(recorded_target), None)
            .map_err(|refusal| LandRefusal::Target { pr, refusal })?;

        // §6, second enforcement point. `head` is the PR's LIVE head, so a
        // rebase since the batch was built disarms the entry here.
        //
        // `ci_green` is fetched only when the gate actually declares the clause.
        // Not an optimization for its own sake: an unconditional `gh pr checks`
        // per sub-PR is a round-trip whose answer is discarded by every gate
        // that does not name `ci-green`, and D2's bisect walks this path
        // repeatedly. Fetching is the DEFAULT — `declares_ci_green` returns true
        // for a malformed gate too, so an unreadable gate file cannot be the
        // reason a check was skipped.
        let observed = PrObservation {
            body_digest: Some(body_digest(&facts.body)),
            ci_green: if declares_ci_green(gate) { pr_ci_green(r, pr) } else { None },
        };
        let recheck = crate::mergeq::recheck_gate(
            gate,
            &verdicts(pr),
            Some(facts.head.as_str()),
            &observed,
        );
        if !recheck.passed() {
            return Err(LandRefusal::Gate { pr, recheck });
        }
        target = Some(validated);
    }

    // An empty batch has no validated target and must never fall back to the
    // recorded string: that string has been through no live check in this call.
    let Some(target) = target else {
        return Err(LandRefusal::Target {
            pr: 0,
            refusal: TargetRefusal::BaseUnverifiable,
        });
    };

    let out = r
        .git(&as_args(&land_push_argv(scratch_sha, &target)))
        .map_err(LandRefusal::PushFailed)?;
    if !out.ok() {
        return Err(LandRefusal::PushFailed(if out.stderr.is_empty() {
            format!("git push {target}: exit {:?}", out.code)
        } else {
            out.stderr
        }));
    }
    Ok(Landed { target, scratch_sha: scratch_sha.to_string() })
}

// ── cleanup (§10) ───────────────────────────────────────────────────────────

/// The argv that deletes one scratch ref, **by exact name**.
///
/// Never a pattern, never a glob, never "delete branches matching". A cleanup
/// routine that can be talked into a wildcard is a data-loss bug waiting for its
/// input (§10), so the name is not even a parameter a caller chooses — see
/// [`cleanup_scratch`], which rebuilds it from the batch id.
pub fn delete_scratch_argv(branch: &str) -> Vec<String> {
    vec![
        "push".into(),
        REMOTE.into(),
        "--delete".into(),
        format!("refs/heads/{branch}"),
    ]
}

/// The argv that closes a batch's draft PR.
///
/// Deliberately **without** `--delete-branch`: that flag would delete the head
/// ref as a side effect of closing, which is a deletion loomux did not name and
/// could not audit separately. The ref goes through [`delete_scratch_argv`],
/// by exact name, as its own auditable step.
pub fn close_draft_argv(draft_pr: u64) -> Vec<String> {
    vec!["pr".into(), "close".into(), draft_pr.to_string()]
}

/// One cleanup step that did not succeed. Audited as `mq-cleanup-failed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupFailure {
    /// `"close-draft"` or `"delete-scratch"`.
    pub step: &'static str,
    pub why: String,
}

/// Cleanup for one batch — **runs on every exit path** (§10: green, red,
/// conflict, timeout, cancel, crash-reconcile, abort).
///
/// Two rules, both structural rather than advisory:
///
/// - **Namespace only, by exact name.** The branch is rebuilt here from the
///   group and batch ids via `mergeq::scratch_branch`; there is no parameter a
///   caller could pass to point this at another ref, and a name that module
///   refuses to build is one this function will not delete.
/// - **Cleanup failure never blocks landing and never fails a batch.** It
///   returns what failed for the caller to audit and leaves the ref behind,
///   which the next reconcile can see. A leaked scratch ref is cheap; a batch
///   held hostage by a failed `git push --delete` is not. That is why the return
///   type is a list of failures rather than a `Result`.
pub fn cleanup_scratch(
    r: &dyn MqRunner,
    group: &str,
    batch_id: &str,
    draft_pr: Option<u64>,
) -> Vec<CleanupFailure> {
    let mut failures = Vec::new();
    if let Some(pr) = draft_pr {
        match r.gh(&as_args(&close_draft_argv(pr))) {
            Ok(out) if out.ok() => {}
            Ok(out) => failures.push(CleanupFailure {
                step: "close-draft",
                why: if out.stderr.is_empty() {
                    format!("gh pr close {pr}: exit {:?}", out.code)
                } else {
                    out.stderr
                },
            }),
            Err(e) => failures.push(CleanupFailure { step: "close-draft", why: e }),
        }
    }
    let Some(branch) = scratch_branch(group, batch_id) else {
        failures.push(CleanupFailure {
            step: "delete-scratch",
            why: format!("no scratch ref name for group {group:?} batch {batch_id:?}"),
        });
        return failures;
    };
    match r.git(&as_args(&delete_scratch_argv(&branch))) {
        Ok(out) if out.ok() => {}
        Ok(out) => failures.push(CleanupFailure {
            step: "delete-scratch",
            why: if out.stderr.is_empty() {
                format!("git push --delete {branch}: exit {:?}", out.code)
            } else {
                out.stderr
            },
        }),
        Err(e) => failures.push(CleanupFailure { step: "delete-scratch", why: e }),
    }
    failures
}

// ── audit vocabulary (§11.5) ────────────────────────────────────────────────

/// The audit actions slice D1 emits, as constants rather than string literals at
/// each call site — §11.5 fixes the vocabulary, and a typo'd action is an event
/// nobody's filter will ever match.
///
/// The remaining §11.5 actions (`mq-enqueued`, `mq-checks-*`, `mq-bisect-step`,
/// `mq-culprit`, `mq-recovered`, `mq-stranded`, …) arrive with the code that
/// emits them in D2; declaring them here with no emitter would be a claim the
/// code does not back.
pub mod audit_action {
    pub const ENQUEUE_REFUSED: &str = "mq-enqueue-refused";
    pub const BATCH_PUSHED: &str = "mq-batch-pushed";
    pub const LANDED: &str = "mq-landed";
    pub const LAND_REFUSED: &str = "mq-land-refused";
    pub const CLEANUP_FAILED: &str = "mq-cleanup-failed";
    pub const SCRATCH_COLLISION: &str = "mq-scratch-collision";
}

/// The repo root a [`ProcessRunner`] should be bound to, given a group's
/// workspace path. Trivial today and named anyway, so D2's wiring has one
/// obvious place to change if the queue ever runs somewhere other than the
/// group's repo root.
pub fn runner_for(repo_root: &Path) -> ProcessRunner {
    ProcessRunner::new(repo_root)
}
