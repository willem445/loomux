//! Pure core of the idle-tick intake gate (#332): host-side, zero-token
//! detection of label/PR-check deltas since the last observation, and the
//! pure decision of whether an idle tick that has cleared its quiet-window
//! threshold should actually wake the orchestrator or skip quietly. Mirrors
//! `notify.rs`'s split exactly: no `gh`, no lock, everything here is a plain
//! function over plain data (most of it over `gh --json` output already
//! captured as a string), so it is unit-testable with canned fixtures. See
//! `OrchRegistry::poll_intake`/`idle_tick_tick` (mod.rs) for the impure half
//! and `doc/design/orchestration.md`'s "Idle-tick intake gate" section for
//! the design rationale.

use super::notify;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

/// Labels that count as new intake for the gate — the same two
/// `orchestrator.md`'s "Label signals" section already documents. The real
/// GitHub label is `agent-investigation` (confirmed against the repo's label
/// list); the poller must match it exactly or it silently never fires.
pub const INTAKE_LABELS: [&str; 2] = ["agent-ready", "agent-investigation"];

/// Cap on how many individual signals `intake_wake_summary` will name before
/// it stops and states what it dropped — a poll that catches a large batch
/// (a relabeling sweep, many PRs finishing CI around the same time) must
/// never grow the wake notice unboundedly.
pub const MAX_SIGNALS_IN_SUMMARY: usize = 8;

/// How many open issues one intake poll asks `gh` for.
///
/// **`gh issue list` defaults to 30, newest first** — so without an explicit
/// `--limit` the poller sees only the 30 newest open issues and everything
/// older is invisible to it, permanently and silently (measured on this repo
/// mid-review: 30 of 94 open issues returned). That is wrong for the label
/// diff, which has always silently missed a label added to an older issue,
/// and fatal for the full-autonomy eligibility signal, whose entire claim is
/// that it announces *the backlog*.
///
/// 300 is a stated bound, not a guess: it is ~3× this repo's open-issue count
/// with room to grow, and it costs `gh` three API pages (it pages at 100
/// internally) inside a call that is already wall-clock-bounded by
/// `GH_CAPTURE_TIMEOUT` — so the #656 posture (a bounded amount of `gh` work
/// per tick, never an unbounded one) still holds, at one call per group per
/// interval exactly as before.
///
/// **A bound that silently truncates is the same defect with a bigger
/// number**, so exceeding it is never silent: [`OpenIssueList::from_fetch`]
/// marks such a fetch incomplete, which both suppresses the "this issue
/// stopped being eligible" inference (see [`eligible_deltas`]) and adds a
/// stated caveat to the wake summary.
pub const MAX_INTAKE_ISSUES: usize = 300;

/// How many open PRs one intake poll asks `gh` for.
///
/// `gh pr list` has [`MAX_INTAKE_ISSUES`]' defect with a different default:
/// **30, newest first**, so without an explicit `--limit` the repo-wide
/// check-state sweep silently sees only the 30 newest open PRs. That is worse
/// here than plain truncation, because [`pr_check_deltas`] prunes on absence:
/// a PR evicted from the window is *forgotten*, and when it re-enters, the
/// terminal check state already reported for it reads as a fresh transition —
/// a "checks SUCCESS" wake for a PR that has been green for days.
///
/// **200, not [`MAX_INTAKE_ISSUES`]' 300** — the bound is sized to what it
/// bounds, not copied. Open issues are a *backlog*: they accumulate for as
/// long as nobody closes them (94 on this repo against 9 open PRs, measured
/// the same day). Open PRs are *in-flight work*, structurally capped by how
/// much a repo can review at once, so 200 is already ~20× this repo's count
/// with room for a far busier one. It is also the heavier of the two calls
/// per item — `statusCheckRollup` is a nested check-run array per PR, where
/// the issue listing carries a flat label list — so 200 is two of `gh`'s
/// internal 100-item pages, holding the #656 posture (a bounded amount of
/// `gh` work per tick) at one call per group per interval exactly as before.
///
/// Exceeding it is never silent, on the same rule [`MAX_INTAKE_ISSUES`]
/// states: [`OpenPrList::from_fetch`] marks such a fetch incomplete, which
/// suppresses the absence-means-merged inference in [`pr_check_deltas`] and
/// adds a stated caveat to the wake summary.
pub const MAX_INTAKE_PRS: usize = 200;

// ---------------------------------------------------------------------------
// Label deltas
// ---------------------------------------------------------------------------

/// One open issue, reduced from `gh issue list --json number,title,labels` to
/// what the gate needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawIssue {
    pub number: u64,
    pub title: String,
    pub labels: Vec<String>,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
struct RawIssueJson {
    number: u64,
    title: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
}

/// Parse `gh issue list --json number,title,labels` output. `None` on
/// malformed JSON — the caller treats that exactly like a `gh` failure (skip
/// this poll, retry next interval; never crash the poller over one bad
/// response).
pub fn parse_issue_list(json: &str) -> Option<Vec<RawIssue>> {
    let raw: Vec<RawIssueJson> = serde_json::from_str(json).ok()?;
    Some(
        raw.into_iter()
            .map(|i| RawIssue { number: i.number, title: i.title, labels: i.labels.into_iter().map(|l| l.name).collect() })
            .collect(),
    )
}

/// One issue whose intake-labeled set gained a label since the last poll —
/// a brand-new issue with the label, or a label added to one loomux has seen
/// before (including a label that was removed and then re-added: this
/// function doesn't remember "used to have it and lost it", only "has it
/// now, didn't at the last observation").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelSignal {
    pub number: u64,
    pub title: String,
    pub label: String,
}

/// Diff `current` against `last_seen` (issue number -> the intake-watched
/// labels observed at the last poll) and return one [`LabelSignal`] per
/// (issue, watched label) pair present now but absent at the last
/// observation. `last_seen` is updated in place — unconditionally, even for
/// issues that fire nothing — so a restart-then-first-poll (an empty
/// `last_seen`) fires once on everything currently labeled and never again
/// for the same state, satisfying "a restart may re-fire once, but must not
/// re-fire on every poll" without any special-casing.
pub fn label_deltas(last_seen: &mut HashMap<u64, HashSet<String>>, current: &[RawIssue]) -> Vec<LabelSignal> {
    let mut signals = Vec::new();
    for issue in current {
        let watched: HashSet<String> =
            issue.labels.iter().filter(|l| INTAKE_LABELS.contains(&l.as_str())).cloned().collect();
        let seen = last_seen.entry(issue.number).or_default();
        for label in &watched {
            if !seen.contains(label) {
                signals.push(LabelSignal { number: issue.number, title: issue.title.clone(), label: label.clone() });
            }
        }
        *seen = watched;
    }
    signals
}

// ---------------------------------------------------------------------------
// PR check-state transitions
// ---------------------------------------------------------------------------

/// Coarse rollup of an open PR's checks — the same three-way classification
/// `notify::pr_checks_result` uses for a single watched PR, applied here to
/// every open PR in one `gh pr list` call instead of one `gh pr checks` per
/// PR (the whole point: a repo-wide sweep in O(1) calls, not O(open PRs)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrCheckState {
    /// No checks reported yet, or at least one is still running/queued.
    Pending,
    /// Every check that ran reached a passing terminal state.
    Success,
    /// At least one check reached a non-passing terminal state.
    Failure,
}

impl PrCheckState {
    pub fn label(self) -> &'static str {
        match self {
            PrCheckState::Pending => "PENDING",
            PrCheckState::Success => "SUCCESS",
            PrCheckState::Failure => "FAILURE",
        }
    }
}

#[derive(Deserialize)]
struct RawRollupEntry {
    /// `StatusContext` nodes (a third-party status check) report state here.
    #[serde(default)]
    state: Option<String>,
    /// `CheckRun` nodes (a GitHub Actions job) report `status` (QUEUED /
    /// IN_PROGRESS / COMPLETED) and, once completed, `conclusion`.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
}

/// One rollup entry's state, in the same vocabulary `notify::check_is_pending`
/// / `check_is_failing` already classify (`gh pr checks`'s `state` field) —
/// `gh pr list`'s nested `statusCheckRollup` shape is different (a `CheckRun`
/// carries `status`+`conclusion`, a `StatusContext` carries `state` directly)
/// but resolves to the identical vocabulary once normalized here.
fn rollup_entry_state(e: &RawRollupEntry) -> &str {
    if let Some(s) = &e.state {
        return s;
    }
    if let Some(status) = &e.status {
        if status != "COMPLETED" {
            return "IN_PROGRESS";
        }
    }
    e.conclusion.as_deref().unwrap_or("PENDING")
}

/// One issue-comment node from `gh pr list --json comments` (#864). Only the
/// timestamp is read: the body is skipped by serde without ever being
/// allocated (unknown fields are ignored, not materialized into a `Value`),
/// which is what keeps a comments-bearing poll a bounded parse however long
/// the discussion on a PR has grown.
#[derive(Deserialize)]
struct RawCommentJson {
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
}

/// One review node from `gh pr list --json reviews` (#864). A review lands as
/// its own node rather than as a `comments` entry, so a poll that read only
/// `comments` would miss the single most decision-relevant thing that happens
/// on a PR the orchestrator is waiting on. `submitted_at` is null for a
/// PENDING (unsubmitted) review — filtered, never treated as activity.
#[derive(Deserialize)]
struct RawReviewJson {
    #[serde(default, rename = "submittedAt")]
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
struct RawPrJson {
    number: u64,
    title: String,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Vec<RawRollupEntry>,
    #[serde(default)]
    comments: Vec<RawCommentJson>,
    #[serde(default)]
    reviews: Vec<RawReviewJson>,
}

/// One open PR, reduced from `gh pr list --json
/// number,title,statusCheckRollup,comments,reviews` to its coarse
/// [`PrCheckState`] plus its newest discussion timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPr {
    pub number: u64,
    pub title: String,
    pub state: PrCheckState,
    /// Newest comment/review timestamp on this PR as GitHub reported it
    /// (RFC-3339, always UTC `Z`), or `None` for a PR nobody has said
    /// anything on yet. Compared as a **string**: GitHub's timestamps are
    /// fixed-width, zero-padded and UTC-normalized, so lexicographic order
    /// *is* chronological order — which is why this needs no date crate (and
    /// so no `getrandom`-pulling dependency, CLAUDE.md constraint 2). A value
    /// that ever arrived in some other shape would compare as merely
    /// "different", and `pr_comment_deltas` fires on difference, so the
    /// failure direction is one spurious wake, never a missed one.
    pub newest_comment_at: Option<String>,
}

/// The exact `gh pr list` argv `poll_intake` runs, built here rather than
/// inline at the call site **so that the fetch bound is pinnable** — the same
/// reasoning [`issue_list_argv`] states, for the same reason: inline,
/// `--limit` is one deletable word whose removal restores the 30-newest
/// truncation with every behavioural test still green, because every one of
/// them hands [`pr_check_deltas`] a list directly.
/// `the_pr_list_argv_always_carries_the_fetch_bound` fails the moment the
/// flag or its value goes missing.
///
/// The `--json` field list carries `comments,reviews` alongside the check
/// rollup (#864): the discussion-activity signal rides the PR call that was
/// already being made, so it costs zero extra round-trips.
/// `the_pr_list_argv_asks_for_the_comment_fields` pins that — dropping either
/// field would leave `pr_comment_deltas` silently deaf, with every one of its
/// own tests still green, because they all hand it a parsed list directly.
///
/// Returns owned strings because the limit is formatted from
/// [`MAX_INTAKE_PRS`]; the caller borrows them for `gh_capture`.
pub fn pr_list_argv() -> Vec<String> {
    [
        "pr",
        "list",
        "--state",
        "open",
        "--limit",
        &MAX_INTAKE_PRS.to_string(),
        "--json",
        "number,title,statusCheckRollup,comments,reviews",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Parse `gh pr list --json number,title,statusCheckRollup,comments,reviews`
/// output, reducing each PR's nested rollup array to one [`PrCheckState`] with
/// `notify.rs`'s own pending/failing predicates (a condition-gated
/// `SKIPPED`/`NEUTRAL` job must not read as failing here either — see
/// `notify::check_is_failing`'s doc for the #290 regression this avoids) and
/// its comment/review nodes to one newest timestamp. `None` on malformed JSON.
pub fn parse_pr_list(json: &str) -> Option<Vec<RawPr>> {
    let raw: Vec<RawPrJson> = serde_json::from_str(json).ok()?;
    Some(
        raw.into_iter()
            .map(|pr| {
                let states: Vec<&str> = pr.status_check_rollup.iter().map(rollup_entry_state).collect();
                let coarse = if states.is_empty() || states.iter().any(|s| notify::check_is_pending(s)) {
                    PrCheckState::Pending
                } else if states.iter().any(|s| notify::check_is_failing(s)) {
                    PrCheckState::Failure
                } else {
                    PrCheckState::Success
                };
                // Comments and reviews are one signal — "somebody said
                // something on this PR" — so they collapse to a single max
                // rather than two fields nothing downstream would tell apart.
                let newest_comment_at = pr
                    .comments
                    .into_iter()
                    .filter_map(|c| c.created_at)
                    .chain(pr.reviews.into_iter().filter_map(|r| r.submitted_at))
                    .max();
                RawPr { number: pr.number, title: pr.title, state: coarse, newest_comment_at }
            })
            .collect(),
    )
}

/// One PR whose coarse check-state reached a NEW terminal value
/// (Success/Failure) since the last poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCheckSignal {
    pub number: u64,
    pub title: String,
    pub from: PrCheckState,
    pub to: PrCheckState,
}

/// One poll's view of a repo's open PRs, and whether that view is
/// **complete** — every open PR there is, rather than the newest
/// [`MAX_INTAKE_PRS`] of them.
///
/// The same distinction [`OpenIssueList`] carries, and it exists here for the
/// same reason: [`pr_check_deltas`] reads *absent from the response* as
/// *merged or closed*, which makes completeness load-bearing. On a truncated
/// fetch the window is **membership churn**, not merely truncation — opening
/// one PR evicts the oldest in the window, and merging something above it lets
/// that PR back in, where its already-reported terminal state would be
/// announced again as a fresh transition. Pagination masquerading as CI
/// finishing.
#[derive(Debug, Clone, Copy)]
pub struct OpenPrList<'a> {
    pub prs: &'a [RawPr],
    pub complete: bool,
}

impl<'a> OpenPrList<'a> {
    /// Wrap a fetch that asked `gh` for at most [`MAX_INTAKE_PRS`] PRs.
    ///
    /// Fewer came back than were asked for ⇒ that is every open PR there is.
    /// Exactly the bound came back ⇒ **assume there are more**: `gh` reports
    /// no total, so "exactly 200 open PRs" and "the first 200 of many" are
    /// indistinguishable from the response alone. Erring toward incomplete
    /// costs a stated caveat and some retained state; erring the other way
    /// costs the false re-announcement this type exists to prevent — the
    /// identical trade [`OpenIssueList::from_fetch`] makes.
    pub fn from_fetch(prs: &'a [RawPr]) -> Self {
        Self { prs, complete: prs.len() < MAX_INTAKE_PRS }
    }
}

/// Diff `current` against `last_seen` (PR number -> last-observed coarse
/// state) and return one [`PrCheckSignal`] per PR whose state is now terminal
/// (Success/Failure) AND differs from what was last seen — never for Pending
/// (an in-progress PR is not news) and never for a repeat of the same
/// terminal state (a PR sitting at SUCCESS across two polls doesn't refire).
///
/// `last_seen` is updated for every PR present (terminal or not). An entry is
/// **only ever dropped on evidence, never on absence alone**, which is why
/// this takes an [`OpenPrList`] rather than a slice:
/// - absent from a **complete** response → **forget it**. It genuinely merged
///   or closed, and forgetting is what makes a REOPENED PR with the same
///   number start fresh instead of reading its old terminal state as
///   "unchanged".
/// - absent from a **truncated** response → **keep it**. It may simply have
///   fallen past [`MAX_INTAKE_PRS`], and forgetting it would re-announce its
///   long-settled check state the moment the window churned back over it.
///
/// Two costs of that choice, both deliberate. **A PR that merges while beyond
/// the bound keeps its entry** until some later poll returns a complete list,
/// so if that number is ever reopened its old terminal state reads as
/// unchanged and produces no wake — the PR-side twin of the property
/// [`eligible_deltas`] gives up for an issue closed outside the window, and
/// the same trade: a missed wake for a rare reopen beats a spurious one on
/// every churn. **The retained map grows** while listings stay truncated, at
/// one small entry per PR number ever seen open — bytes, and a single
/// complete listing prunes all of it at once, so it wants no mechanism of its
/// own.
pub fn pr_check_deltas(last_seen: &mut HashMap<u64, PrCheckState>, current: OpenPrList) -> Vec<PrCheckSignal> {
    let mut signals = Vec::new();
    let mut still_open: HashSet<u64> = HashSet::new();
    for pr in current.prs {
        still_open.insert(pr.number);
        let prev = last_seen.get(&pr.number).copied();
        if pr.state != PrCheckState::Pending && prev != Some(pr.state) {
            signals.push(PrCheckSignal { number: pr.number, title: pr.title.clone(), from: prev.unwrap_or(PrCheckState::Pending), to: pr.state });
        }
        last_seen.insert(pr.number, pr.state);
    }
    if current.complete {
        last_seen.retain(|n, _| still_open.contains(n));
    }
    signals
}

// ---------------------------------------------------------------------------
// Eligible-unstarted issues — the full-autonomy intake signal (#778)
// ---------------------------------------------------------------------------

/// The exact `gh issue list` argv `poll_intake` runs, built here rather than
/// inline at the call site **so that the fetch bound is pinnable**.
///
/// Inline, `--limit` was one deletable word whose removal restored the
/// 30-newest truncation bug with every test still green — the same
/// silent-restore shape the completeness plumbing exists to prevent, sitting
/// one layer below it. `the_issue_list_argv_always_carries_the_fetch_bound`
/// fails the moment the flag or its value goes missing.
///
/// Returns owned strings because the limit is formatted from
/// [`MAX_INTAKE_ISSUES`]; the caller borrows them for `gh_capture`.
pub fn issue_list_argv() -> Vec<String> {
    ["issue", "list", "--state", "open", "--limit", &MAX_INTAKE_ISSUES.to_string(), "--json", "number,title,labels"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// One open issue that is eligible to start under full autonomy and that no
/// board task is tracking yet — the host-side, zero-token half of the
/// self-select loop. Carries only what the wake summary names; **what the
/// work is worth is never decided here** (the goal string is opaque data to
/// loomux — ranking, goal fit and parking are the orchestrator's documented
/// judgment, so no "what work is valuable" policy lives in product code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibleSignal {
    pub number: u64,
    pub title: String,
}

/// One poll's view of a repo's open issues, and whether that view is
/// **complete** — every open issue there is, rather than the newest
/// [`MAX_INTAKE_ISSUES`] of them.
///
/// The distinction exists for exactly one reader. [`eligible_deltas`] is the
/// first consumer for which *absent from the response* would otherwise mean
/// *no longer eligible*, which makes the response's completeness load-bearing
/// in a way it never was before: on a truncated fetch the 30-newest window is
/// **membership churn**, not merely truncation — filing one new issue evicts
/// the oldest in the window, and closing something above it lets that issue
/// back in, where it would read as *newly* eligible and be announced again.
/// Pagination masquerading as a human un-holding something.
///
/// [`label_deltas`] never had this problem: it only ever touches issues
/// present in the response and never removes an absent issue's entry, so a
/// window that churns is invisible to it. That asymmetry is why this type
/// exists here and not there.
#[derive(Debug, Clone, Copy)]
pub struct OpenIssueList<'a> {
    pub issues: &'a [RawIssue],
    pub complete: bool,
}

impl<'a> OpenIssueList<'a> {
    /// Wrap a fetch that asked `gh` for at most [`MAX_INTAKE_ISSUES`] issues.
    ///
    /// Fewer came back than were asked for ⇒ that is every open issue there
    /// is. Exactly the bound came back ⇒ **assume there are more**: `gh`
    /// reports no total, so the "exactly 300 open issues" and "the first 300
    /// of many" cases are indistinguishable from the response alone. Erring
    /// toward incomplete costs a stated caveat and some retained state; erring
    /// the other way costs false re-announcements, which is the failure this
    /// whole type exists to prevent.
    pub fn from_fetch(issues: &'a [RawIssue]) -> Self {
        Self { issues, complete: issues.len() < MAX_INTAKE_ISSUES }
    }
}

/// Lenient parse of a board task's `Task.issue` string into an issue number.
/// The board is agent-written free text, so this accepts the two spellings
/// agents actually produce (`"#712"`, `"712"`, either side padded) and
/// answers `None` for anything else — a URL, a range, `"#x"`, an empty
/// string, a number too large for `u64`.
///
/// **Refusing to guess is the load-bearing part.** An unparsed ref costs at
/// most one duplicate wake for an issue that is already being worked (noise);
/// a *misparsed* one would suppress the wake for a DIFFERENT issue than the
/// board is tracking, which is silence about real work — the one failure this
/// signal must never have.
fn parse_task_issue_ref(raw: &str) -> Option<u64> {
    let t = raw.trim();
    t.strip_prefix('#').unwrap_or(t).parse::<u64>().ok()
}

/// The set of issue numbers the board is already tracking, from every task's
/// `Task.issue` field. Unparseable refs contribute nothing (see
/// [`parse_task_issue_ref`]).
pub fn board_tracked_issues(refs: &[&str]) -> HashSet<u64> {
    refs.iter().filter_map(|r| parse_task_issue_ref(r)).collect()
}

/// Which of `issues` are eligible to start under full autonomy right now:
/// **open AND not hold-labeled AND not already tracked by a board task**.
/// "Open" is the caller's contract, not a field — the list comes from `gh
/// issue list --state open`, so a closed issue is simply absent.
///
/// The hold label is matched case-insensitively, unlike the exact-match
/// [`INTAKE_LABELS`] check: this one is a **consent boundary**, so the
/// direction of a mismatch matters. GitHub label names are unique
/// case-insensitively, so a case-insensitive compare cannot make a *different*
/// label read as a hold; a case-sensitive one could make a real hold read as
/// eligible (a repo whose `intake.labels.hold:` spelling differs only in case
/// from the label a human actually applied), which is the failure that starts
/// work a human vetoed.
///
/// Board-tracking is a **duplicate-wake suppressor, not a consent gate**. The
/// board is agent-writable, so nothing that authorizes anything may read it
/// (`Task.pr_base`'s "nothing may gate on it" doc is the precedent); what it
/// legitimately buys is not re-announcing work that already has a task. The
/// consent boundary is the hold label and the contract around it.
pub fn eligible_unstarted(issues: &[RawIssue], hold_label: &str, board_tracked: &HashSet<u64>) -> Vec<EligibleSignal> {
    // An empty spelling would make the hold check match nothing at all, i.e.
    // announce every held issue as eligible. No resolution path produces one
    // (`sanitize_intake_label` and `read_intake` both fall back to the
    // built-in default), so this is unreachable today — but it is a consent
    // boundary, and the only safe answer to "I don't know what a hold looks
    // like" is to claim nothing is startable.
    if hold_label.trim().is_empty() {
        return Vec::new();
    }
    issues
        .iter()
        .filter(|i| !board_tracked.contains(&i.number))
        .filter(|i| !i.labels.iter().any(|l| l.eq_ignore_ascii_case(hold_label)))
        .map(|i| EligibleSignal { number: i.number, title: i.title.clone() })
        .collect()
}

/// Diff the currently-eligible set against `last_seen` (the issue numbers that
/// were eligible at the last poll for this group) and return one
/// [`EligibleSignal`] per issue that is eligible now and was not then:
/// - **newly eligible fires once** and never again while it stays eligible;
/// - **eligible → not eligible drops out of `last_seen`**, so an issue that
///   gets held (or picked up onto the board) and is later un-held (or whose
///   task is deleted) fires once more — the state is "eligible at the last
///   poll", not "ever announced";
/// - **an empty `last_seen` fires the whole eligible backlog once** — which is
///   deliberately the enable-time triage trigger, since a group that was not
///   full-autonomy has an empty set by construction (below), and so does a
///   fresh process after a restart (same one-refire-then-settle property
///   [`label_deltas`] has).
///
/// **An entry is only ever dropped on evidence, never on absence alone**, and
/// that is the whole reason this takes an [`OpenIssueList`] rather than a
/// slice. Three cases, and only the first two are evidence:
/// - present in the response and no longer eligible → **forget it** (a real
///   transition: held, or now on the board);
/// - absent from a **complete** response → **forget it** (genuinely closed, so
///   a reopen is news again — the posture `pr_check_deltas` takes for a
///   reopened PR);
/// - absent from a **truncated** response → **keep it**. It may simply have
///   fallen past the fetch bound, and forgetting it would re-announce it as
///   newly eligible the moment the window churned back over it. This is the
///   defect the completeness flag exists to prevent; wholesale replacement of
///   `last_seen` had exactly it.
///
/// The third case gives up one property, stated because it is a real loss and
/// not an oversight (#785 rev-266 NB3): an issue that **closes while beyond
/// the fetch bound** keeps its entry, so if it is later reopened *and* is
/// still eligible, it looks unchanged and produces no wake. Reopen-is-news
/// survives only for issues closed inside the window. That is the deliberate
/// side of the trade — a missed wake for a rare reopen-outside-the-window
/// beats a spurious re-announcement on every window churn, which is the
/// failure that actually recurs — and it is why [`MAX_INTAKE_ISSUES`] is
/// sized to make truncation rare rather than routine.
///
/// Two gates sit inside this function rather than at the call site, so the
/// wiring decisions are testable without `gh`:
/// - `full_autonomy == false`: no signal, and `last_seen` is **cleared** — the
///   set means "eligible at the last poll", and under opt-in intake nothing is.
///   Clearing is also what makes a later re-enable a fresh triage trigger
///   instead of a silent one.
/// - `current == None` (the `gh issue list` half of this poll failed):
///   `last_seen` is left **untouched**. Treating a failed fetch as "nothing is
///   eligible any more" would empty the set and re-announce the entire backlog
///   on the next successful poll — a `gh` blip must not read as a triage
///   trigger (#332's "degrade, don't deny" applied to this signal).
pub fn eligible_deltas(
    last_seen: &mut HashSet<u64>,
    full_autonomy: bool,
    current: Option<OpenIssueList>,
    hold_label: &str,
    board_tracked: &HashSet<u64>,
) -> Vec<EligibleSignal> {
    if !full_autonomy {
        last_seen.clear();
        return Vec::new();
    }
    let Some(list) = current else { return Vec::new() };
    let eligible = eligible_unstarted(list.issues, hold_label, board_tracked);
    let eligible_now: HashSet<u64> = eligible.iter().map(|s| s.number).collect();
    let present: HashSet<u64> = list.issues.iter().map(|i| i.number).collect();

    let signals: Vec<EligibleSignal> =
        eligible.iter().filter(|s| !last_seen.contains(&s.number)).cloned().collect();
    last_seen.retain(|n| {
        if eligible_now.contains(n) {
            true // still eligible
        } else if present.contains(n) {
            false // present and no longer eligible — a real transition
        } else {
            !list.complete // absent: only "gone" if we saw the whole list
        }
    });
    last_seen.extend(eligible_now);
    signals
}

// ---------------------------------------------------------------------------
// PR comment/review activity (#864)
// ---------------------------------------------------------------------------

/// One PR whose newest comment/review timestamp moved since the last poll.
///
/// This is the one delta the orchestrator still polled by hand on every tick
/// (#864): its monitoring cadence re-reads the open PRs' newest comments to
/// find a human's answer, a reviewer's verdict, or a worker's note. Reading it
/// host-side costs zero tokens and, more importantly, is what lets a parked
/// group's tick cadence decay without going blind to the one thing that
/// actually happens while it is parked — a human commenting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCommentSignal {
    pub number: u64,
    pub title: String,
    pub at: String,
}

/// Diff `current` against `last_seen` (PR number -> newest comment/review
/// timestamp observed at the last poll) and return one [`PrCommentSignal`] per
/// PR whose newest timestamp is now DIFFERENT from what was last seen.
///
/// "Different", not "greater": an edited/deleted comment can move the newest
/// timestamp backwards, and the question this answers is "has the discussion
/// changed since loomux last looked", not "is there a strictly newer post".
/// Firing on difference makes the failure direction one extra wake rather than
/// a silently-missed one — the same safe direction `label_deltas` takes.
///
/// **With exactly one deliberate exception, which the else-branch below states
/// again at the line that implements it: a PR going from some discussion to
/// NONE is silent.** Deleting the last comment leaves nothing for the
/// orchestrator to read, so a wake for it would carry no content — and the
/// stale `last_seen` entry it leaves behind cannot hide a later post, because
/// any new comment necessarily carries a newer timestamp than the deleted one
/// and so still reads as different. That is the whole of the exception: one
/// contentless transition, with no effect on the next real one.
///
/// A PR seen for the first time (post-restart, or newly opened) with any
/// discussion on it fires once, exactly like `label_deltas`/`pr_check_deltas`:
/// a comment posted while loomux was down is precisely what must not be
/// missed, and a restart re-arms the fallback anyway, so the re-fire rides a
/// wake that was already going to happen. `last_seen` is pruned of numbers no
/// longer in `current` so a reopened PR starts fresh, mirroring
/// `pr_check_deltas` — and, for the same reason and on the same rule, **only
/// when the listing is complete**, which is why this takes an [`OpenPrList`]
/// rather than a slice. The `gh pr list` window churns (#795): a PR that
/// merely fell past [`MAX_INTAKE_PRS`] has not merged, and forgetting its
/// last-seen timestamp would make its unchanged newest comment read as
/// first-sight discussion the moment the window churned back over it — a wake
/// for a conversation that ended days ago. Absence prunes on evidence
/// (a complete listing), never on absence alone.
///
/// **Every agent shares the human's `gh` identity, so this cannot tell a
/// worker's or the orchestrator's own PR comment from a human's** — an
/// orchestrator that comments on a PR and then falls quiet will see its own
/// comment as a delta on the next poll and be woken once for it. That is a
/// deliberate over-approximation: author-filtering is not available at this
/// layer, the cost is bounded by the quiet window (at most one extra wake),
/// and it is still strictly cheaper than the status quo it replaces, where the
/// orchestrator paid a `gh` round-trip *and* the turn to read it on EVERY
/// tick.
pub fn pr_comment_deltas(last_seen: &mut HashMap<u64, String>, current: OpenPrList) -> Vec<PrCommentSignal> {
    let mut signals = Vec::new();
    let mut still_open: HashSet<u64> = HashSet::new();
    for pr in current.prs {
        still_open.insert(pr.number);
        let Some(at) = pr.newest_comment_at.as_ref() else {
            // No discussion right now: nothing to compare against, so this PR
            // is silent — including the had-one-then-none case, where the
            // author deleted the only comment. That transition is the ONE
            // thing this function does not report (see the doc comment): there
            // is nothing left to read, so the wake would carry no content.
            //
            // Note what does NOT happen here: `still_open.insert` above has
            // already run, so any `last_seen` entry for this PR SURVIVES the
            // retain below rather than being forgotten. That is harmless
            // rather than merely tolerable — a later comment is necessarily
            // newer than the deleted one, so it still compares as different
            // and still fires.
            continue;
        };
        if last_seen.get(&pr.number) != Some(at) {
            signals.push(PrCommentSignal { number: pr.number, title: pr.title.clone(), at: at.clone() });
        }
        last_seen.insert(pr.number, at.clone());
    }
    if current.complete {
        last_seen.retain(|n, _| still_open.contains(n));
    }
    signals
}

// ---------------------------------------------------------------------------
// The wake summary — what changed, so the orchestrator doesn't re-poll it
// ---------------------------------------------------------------------------

/// Which halves of one intake poll came back at their fetch bound, and so
/// describe a partial view of the repo.
///
/// A struct rather than two `bool` parameters on [`intake_wake_summary`]
/// deliberately: the two flags are same-typed, adjacent, and mean opposite
/// things, which is precisely the shape a call site transposes silently.
/// Named fields make that transposition unwritable rather than merely
/// test-caught.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntakeTruncation {
    /// The `gh issue list` half hit [`MAX_INTAKE_ISSUES`].
    pub issues: bool,
    /// The `gh pr list` half hit [`MAX_INTAKE_PRS`].
    pub prs: bool,
}

/// Compose the wake-prompt addendum naming what the host-side poll found.
/// Issue titles are third-party text (#189's threat model applies to notice
/// composition exactly as it does to a `gh`-derived check name) — sanitized
/// and field-capped with the same `notify::sanitize_gh_text` every other
/// GitHub-derived field reaching a `[loomux]` notice already goes through.
/// Bounded at [`MAX_SIGNALS_IN_SUMMARY`]: a large batch states what it
/// dropped rather than growing the notice unboundedly (no silent caps) — and
/// the cap is shared across all four signal kinds, so the enable-time
/// eligible-backlog burst (#778) can't blow the notice open either.
///
/// `truncated` says which half of this poll hit its fetch bound, so what the
/// summary reports about that half is drawn from a partial view. Either
/// caveat rides a notice this poll was already sending rather than generating
/// one of its own: a big repo would otherwise wake its orchestrator every
/// single poll forever to say nothing but "still big".
pub fn intake_wake_summary(
    labels: &[LabelSignal],
    prs: &[PrCheckSignal],
    comments: &[PrCommentSignal],
    eligible: &[EligibleSignal],
    truncated: IntakeTruncation,
) -> String {
    let total = labels.len() + prs.len() + comments.len() + eligible.len();
    let mut lines: Vec<String> = Vec::new();
    for s in labels.iter().take(MAX_SIGNALS_IN_SUMMARY) {
        let title = notify::sanitize_gh_text(&s.title, notify::NOTICE_FIELD_CAP);
        lines.push(format!("issue #{} labeled {} (\"{title}\")", s.number, s.label));
    }
    for s in prs.iter().take(MAX_SIGNALS_IN_SUMMARY.saturating_sub(lines.len())) {
        let title = notify::sanitize_gh_text(&s.title, notify::NOTICE_FIELD_CAP);
        lines.push(format!("PR #{} checks {} → {} (\"{title}\")", s.number, s.from.label(), s.to.label()));
    }
    for s in comments.iter().take(MAX_SIGNALS_IN_SUMMARY.saturating_sub(lines.len())) {
        let title = notify::sanitize_gh_text(&s.title, notify::NOTICE_FIELD_CAP);
        // The timestamp is `gh`-derived text like every other field here, so
        // it goes through the same #189 sanitizer rather than being trusted
        // because it "should" be a machine-generated date.
        let at = notify::sanitize_gh_text(&s.at, notify::NOTICE_FIELD_CAP);
        lines.push(format!("PR #{} new comment/review activity at {at} (\"{title}\")", s.number));
    }
    for s in eligible.iter().take(MAX_SIGNALS_IN_SUMMARY.saturating_sub(lines.len())) {
        let title = notify::sanitize_gh_text(&s.title, notify::NOTICE_FIELD_CAP);
        lines.push(format!("issue #{} eligible under full-autonomy (\"{title}\")", s.number));
    }
    let mut summary = lines.join("; ");
    if total > lines.len() {
        summary.push_str(&format!("; (+{} more — see label/PR/issue sweep)", total - lines.len()));
    }
    if truncated.issues && !summary.is_empty() {
        summary.push_str(&format!(
            "; (PARTIAL: the open-issue fetch stopped at its {MAX_INTAKE_ISSUES}-issue bound, so this \
             poll saw only the {MAX_INTAKE_ISSUES} newest open issues — list the rest yourself before \
             treating the backlog as complete)"
        ));
    }
    if truncated.prs && !summary.is_empty() {
        summary.push_str(&format!(
            "; (PARTIAL: the open-PR fetch stopped at its {MAX_INTAKE_PRS}-PR bound, so this poll's \
             check sweep saw only the {MAX_INTAKE_PRS} newest open PRs — a PR outside that window \
             finishing CI produces no wake, so check such a PR yourself rather than reading silence \
             as still-running)"
        ));
    }
    summary
}

/// Cap on how many poll "blocks" [`PendingIntake`] keeps before it starts
/// dropping the oldest — see that type's doc for the growth this bounds.
pub const MAX_PENDING_INTAKE_BLOCKS: usize = 5;

/// A group's not-yet-delivered intake summary, bounded (rev-33 finding B2,
/// #429). `intake_wake_summary` already bounds any ONE poll's findings
/// (`MAX_SIGNALS_IN_SUMMARY`, with a stated "+N more"), but the poller and
/// the idle tick run on independent clocks: a group whose orchestrator stays
/// output-active for hours (the idle tick's quiet window never clears, so
/// nothing ever consumes/clears this) keeps accumulating a fresh bounded
/// block on every scan regardless — a live 8h output-active run measured
/// ~12KB accumulated into one pending string, contradicting the whole
/// "bounded notice" rationale #332 was built on. `push` keeps at most
/// [`MAX_PENDING_INTAKE_BLOCKS`] blocks (newest first out the front when
/// full — the newest findings are the most actionable), and `render` states
/// how many older ones it dropped rather than growing, or shrinking,
/// silently — the same "no silent caps" discipline `intake_wake_summary`
/// itself already applies within a single poll.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingIntake {
    blocks: std::collections::VecDeque<String>,
    dropped: usize,
}

impl PendingIntake {
    /// Fold one poll's already-bounded summary in. A no-op for an empty
    /// summary (nothing new that scan) — never pushes an empty block.
    pub fn push(&mut self, summary: String) {
        if summary.is_empty() {
            return;
        }
        self.blocks.push_back(summary);
        while self.blocks.len() > MAX_PENDING_INTAKE_BLOCKS {
            self.blocks.pop_front();
            self.dropped += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Whether `push` has ever dropped a block to stay within the cap. Read
    /// by `idle_tick_tick` (rev-33 finding N7) to route delivery to the
    /// sweep-bearing wake text instead of `render()`'s own "act on this
    /// directly, don't re-poll" framing: `render()`'s dropped-block clause
    /// points a human reader at "the intake-signal audit trail", but no MCP
    /// tool lets an AGENT read the audit log — that pointer is a dead end in
    /// a delivered prompt. A real sweep re-discovers everything regardless
    /// of what got dropped, so it's strictly better than trusting a summary
    /// that's already admitted to being incomplete.
    pub fn dropped_any(&self) -> bool {
        self.dropped > 0
    }

    /// The text `idle_tick_notice` embeds and the audit records — empty
    /// string if nothing is pending. States what got dropped, if anything,
    /// as its own leading clause rather than silently thinning the history.
    pub fn render(&self) -> String {
        if self.blocks.is_empty() {
            return String::new();
        }
        let joined: Vec<&str> = self.blocks.iter().map(String::as_str).collect();
        if self.dropped > 0 {
            format!(
                "(+{} earlier finding(s) dropped for space — see the intake-signal audit trail); {}",
                self.dropped,
                joined.join("; ")
            )
        } else {
            joined.join("; ")
        }
    }
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// Smart-default resolution for the gate's poll cadence (#429, user-directed
/// fix for the benchtest finding that the gate shipped default-OFF with no
/// setter anywhere to turn it on — meaning it could never actually engage for
/// any real autonomous group). Resolved fresh wherever `intake_poll_minutes`
/// is consulted, never baked into `Guardrails::clamped()`, so a group that
/// flips autonomous mode ON gets the gate immediately — same "resolve at
/// gate-evaluation time" lesson `compact_nudge_context_floor_met` already
/// established for the min-context floor's own smart default.
/// - `config: None` (unset — absent from group.json, or never explicitly
///   set) resolves to `DEFAULT_INTAKE_POLL_MINUTES` while `autonomous`, so the
///   dead-default trap (autonomous ON, gate silently off) is structurally
///   impossible without an explicit opt-out. Resolves to `0` (inert) while
///   NOT autonomous — a supervised group never idle-ticks, so there is
///   nothing for the poller to feed either way.
/// - `config: Some(0)` is an explicit, deliberate opt-out: stays `0`
///   regardless of `autonomous` — an operator who wants the polling load off
///   even while autonomous gets exactly that, with no smart default
///   overriding their choice.
/// - `config: Some(n)`, `n > 0`, is an explicit cadence: returned as-is
///   (already clamped to range in `Guardrails::clamped()`), regardless of
///   `autonomous` — a value a human bothered to set is never second-guessed.
pub fn effective_intake_poll_minutes(config: Option<u32>, autonomous: bool) -> u32 {
    match config {
        None if autonomous => super::DEFAULT_INTAKE_POLL_MINUTES,
        None => 0,
        Some(explicit) => explicit,
    }
}

/// Whether an idle tick that has already cleared its quiet-window threshold
/// (`idle_tick_should_fire`) should actually wake the orchestrator, or skip
/// quietly and wait. `has_intake_signal` is the host-side poll's own finding;
/// `has_pending_notification` mirrors the "a lost notification degrades to
/// poll-on-sweep" invariant (`orchestrator.md`'s Monitoring open PRs section)
/// — an outstanding CI watch means the tick's fallback-sweep duty still has a
/// job even with no label/PR news; `has_watchdog_stall` covers a worker the
/// watchdog has already flagged that nobody has resolved; `fallback_due` is
/// the bounded backstop (`idle_tick_fallback_due`) that fires regardless, so
/// a poller bug — or a group that is genuinely, permanently quiet — can never
/// silence the orchestrator past it.
pub fn idle_tick_gate(has_intake_signal: bool, has_pending_notification: bool, has_watchdog_stall: bool, fallback_due: bool) -> bool {
    has_intake_signal || has_pending_notification || has_watchdog_stall || fallback_due
}

/// Whether the bounded unconditional fallback has come due. `last_fired_ms` is
/// the wall-clock time of the last tick THIS group actually delivered (a
/// gated fire or a fallback fire alike — see `OrchRegistry::idle_tick_tick`),
/// so the fallback measures real elapsed time since the orchestrator was last
/// woken, not since the gate was last merely re-evaluated (which can happen
/// every `IDLE_TICK_INTERVAL` scan while a group sits quiet).
///
/// `fallback_minutes` is the group's EFFECTIVE interval — the configured base
/// widened by [`fallback_interval_minutes`] for however long the group has
/// been delta-free (#864) — not the raw guardrail.
pub fn idle_tick_fallback_due(last_fired_ms: u64, now_ms: u64, fallback_minutes: u32) -> bool {
    now_ms.saturating_sub(last_fired_ms) >= (fallback_minutes as u64) * 60_000
}

/// Hard bound on how many times [`fallback_interval_minutes`] will double the
/// base interval, independent of the cap. Purely an overflow guard: at 20
/// doublings even a 30-minute base is over 50 years, so the `cap_minutes`
/// clamp is what actually decides the interval in every real configuration —
/// this only stops a shift from running off the end of the type if a streak
/// ever grew absurdly large.
pub const MAX_FALLBACK_BACKOFF_DOUBLINGS: u32 = 20;

/// The EFFECTIVE unconditional-fallback interval for a group that has been
/// delta-free for `empty_streak` consecutive fallback wakes (#864).
///
/// The idle tick's unconditional fallback exists so a poller bug, or a
/// genuinely silent group, can never mute the orchestrator forever. But the
/// interval was fixed, so a group parked entirely on the human — every open
/// item human-gated, no live delegates, nothing changing anywhere the host can
/// see — paid that fallback at full price indefinitely: ~30 consecutive wakes
/// over one parked weekend, each one an API turn over the orchestrator's whole
/// prefix, each one finding nothing (#864).
///
/// So the interval doubles per consecutive delta-free wake and stops at
/// `cap_minutes`: 3h → 6h → 12h → 24h with the shipped defaults. The **shape**
/// is what matters, not the constants — a parked group's cost per unit time
/// decays geometrically toward `base/cap` of what it was, while the guarantee
/// the fallback exists for survives intact, merely coarser (the orchestrator
/// is still woken unconditionally, at worst every `cap_minutes`). The caller
/// resets the streak on any delta or human input, which is what makes the
/// decay a response to *observed quiet* rather than to elapsed time.
///
/// Doubling is fixed rather than configurable: `base` and `cap` already span
/// the whole useful range (equal values switch backoff off entirely — the
/// explicit opt-out), and a third knob would only let an operator pick a
/// curve between two endpoints they have already chosen.
pub fn fallback_interval_minutes(base_minutes: u32, empty_streak: u32, cap_minutes: u32) -> u32 {
    // A cap below the base is a misconfiguration, not an instruction to fire
    // faster than configured: the backoff may only ever make the fallback
    // slower than its base, never quicker.
    let cap = cap_minutes.max(base_minutes);
    let doublings = empty_streak.min(MAX_FALLBACK_BACKOFF_DOUBLINGS);
    let scaled = (base_minutes as u64).saturating_mul(1u64 << doublings);
    scaled.min(cap as u64) as u32
}

/// One group's whole last-seen state — the label sets `label_deltas` diffs
/// against, the coarse PR states `pr_check_deltas` diffs against, and the
/// newest PR comment/review timestamps `pr_comment_deltas` diffs against,
/// bundled so `OrchRegistry` has one entry per group instead of three parallel
/// maps that could fall out of sync.
#[derive(Debug, Clone, Default)]
pub struct IntakeSeenState {
    pub labels: HashMap<u64, HashSet<String>>,
    pub pr_checks: HashMap<u64, PrCheckState>,
    pub pr_comments: HashMap<u64, String>,
    /// Issue numbers that were eligible-unstarted at the last poll (#778) —
    /// what [`eligible_deltas`] diffs against. Empty for every group that is
    /// not in full autonomy, which is what makes the first poll after an
    /// enable fire the whole eligible backlog once.
    pub eligible: HashSet<u64>,
}

// ---------------------------------------------------------------------------
// Poll scheduling — which groups are due this scan (mirrors notify::due_watches)
// ---------------------------------------------------------------------------

/// Most groups one intake scan will actually poll, however many are due
/// (#656). Each due group costs TWO sequential `gh` calls (`issue list`, `pr
/// list`), so this is the intake half's equivalent of
/// `notify::MAX_POLLS_PER_TICK` (8 `gh` calls) and is deliberately set to the
/// same `gh`-call budget rather than the same group count: 4 groups × 2 calls.
///
/// Without it, N autonomous groups falling due on the same scan wake is 2N
/// sequential round-trips inside one tick of the single poll loop — at N =
/// 10–20 that is 20–40 round-trips (~1s each) before the loop can sleep
/// again, and every `notify_when` notice in the process waits behind them.
/// The overflow is not dropped: it is simply still due on the next scan, and
/// the oldest-polled-first ordering below is what makes that a deferral
/// rather than starvation.
pub const MAX_INTAKE_POLLS_PER_TICK: usize = 4;

/// Pick which autonomous groups are due for an intake poll this scan: every
/// group whose `intake_poll_minutes` guardrail is nonzero (0 = the feature is
/// off for that group — no poll, no gate, today's behavior) and whose last
/// poll is at least that many minutes old, oldest-polled first and capped at
/// `MAX_INTAKE_POLLS_PER_TICK`. Pure so the due-selection policy (the GitHub
/// API budget backstop — no more than one `gh` round-trip pair per group per
/// configured interval, and no more than a bounded number of pairs per scan)
/// is testable with no `gh`, no lock, and no registry, exactly like
/// `notify::due_watches`.
///
/// The ordering is load-bearing, not cosmetic (#656): this reads from a
/// `HashMap`, so an uncapped-then-truncated list would cut at an arbitrary
/// point that a fixed hash seed could keep cutting the same way, starving
/// whichever groups landed on the wrong side of it forever. Sorting by
/// `last_poll_ms` makes the group deferred by this scan the oldest — and so
/// the first taken — on the next one. The group name breaks ties so the same
/// due set always yields the same selection: never-polled groups all share
/// `last = 0`, and the round-robin can only be fair if the tiebreak is
/// deterministic rather than the map's iteration order.
pub fn due_intake_polls(now_ms: u64, groups: &HashMap<super::GroupId, u32>, last_poll_ms: &HashMap<super::GroupId, u64>) -> Vec<super::GroupId> {
    let mut due: Vec<(u64, &super::GroupId)> = groups
        .iter()
        .filter(|(_, &minutes)| minutes > 0)
        .filter_map(|(group, &minutes)| {
            let last = last_poll_ms.get(group).copied().unwrap_or(0);
            (now_ms.saturating_sub(last) >= (minutes as u64) * 60_000).then_some((last, group))
        })
        .collect();
    due.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    due.truncate(MAX_INTAKE_POLLS_PER_TICK);
    due.into_iter().map(|(_, group)| group.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::GroupId;
    /// #904: the one constructor, in tests as in production.
    fn gid(s: &str) -> GroupId {
        GroupId::parse(s).unwrap()
    }

    /// Only the `gh issue list` half hit its bound. Named rather than spelled
    /// as a literal at each call site so the two flags can never be read the
    /// wrong way round in a test either.
    const ISSUES_TRUNCATED: IntakeTruncation = IntakeTruncation { issues: true, prs: false };
    /// Only the `gh pr list` half hit its bound.
    const PRS_TRUNCATED: IntakeTruncation = IntakeTruncation { issues: false, prs: true };

    fn issue(number: u64, title: &str, labels: &[&str]) -> RawIssue {
        RawIssue { number, title: title.to_string(), labels: labels.iter().map(|s| s.to_string()).collect() }
    }

    // ---------- parse_issue_list ----------

    #[test]
    fn parse_issue_list_reads_real_gh_shape() {
        let json = r#"[{"labels":[{"id":"L1","name":"agent-ready","description":"","color":"d475bc"}],"number":398,"title":"Terse reports"}]"#;
        let issues = parse_issue_list(json).unwrap();
        assert_eq!(issues, vec![issue(398, "Terse reports", &["agent-ready"])]);
    }

    #[test]
    fn parse_issue_list_rejects_malformed_json() {
        assert!(parse_issue_list("not json").is_none());
    }

    // ---------- label_deltas ----------

    #[test]
    fn label_deltas_fires_on_a_brand_new_labeled_issue() {
        let mut seen = HashMap::new();
        let signals = label_deltas(&mut seen, &[issue(1, "Fix X", &["agent-ready"])]);
        assert_eq!(signals, vec![LabelSignal { number: 1, title: "Fix X".into(), label: "agent-ready".into() }]);
    }

    #[test]
    fn label_deltas_does_not_refire_on_an_unchanged_poll() {
        let mut seen = HashMap::new();
        let issues = vec![issue(1, "Fix X", &["agent-ready"])];
        assert_eq!(label_deltas(&mut seen, &issues).len(), 1);
        assert!(label_deltas(&mut seen, &issues).is_empty(), "the second poll of the same state must not refire");
    }

    #[test]
    fn label_deltas_fires_when_a_label_is_added_to_a_known_issue() {
        let mut seen = HashMap::new();
        // First poll: no watched label yet (issue exists with an unrelated label).
        label_deltas(&mut seen, &[issue(1, "Fix X", &["bug"])]);
        // Second poll: agent-ready landed.
        let signals = label_deltas(&mut seen, &[issue(1, "Fix X", &["bug", "agent-ready"])]);
        assert_eq!(signals.len(), 1, "a label added to an already-known issue must fire");
        assert_eq!(signals[0].label, "agent-ready");
    }

    #[test]
    fn label_deltas_refires_when_a_label_is_removed_then_reapplied() {
        let mut seen = HashMap::new();
        label_deltas(&mut seen, &[issue(1, "Fix X", &["agent-ready"])]);
        label_deltas(&mut seen, &[issue(1, "Fix X", &[])]); // label removed
        let signals = label_deltas(&mut seen, &[issue(1, "Fix X", &["agent-ready"])]); // re-added
        assert_eq!(signals.len(), 1, "a re-applied label is new intake, not a repeat");
    }

    #[test]
    fn label_deltas_ignores_unwatched_labels() {
        let mut seen = HashMap::new();
        let signals = label_deltas(&mut seen, &[issue(1, "Fix X", &["bug", "agent-managed"])]);
        assert!(signals.is_empty(), "only agent-ready/agent-investigation are watched, got: {signals:?}");
    }

    #[test]
    fn label_deltas_a_restart_refires_once_then_settles() {
        // The acceptance criterion, verified directly: an EMPTY last_seen (what
        // a fresh process has after a restart) reads every currently-labeled
        // issue as new exactly once, and the very next poll of the same state
        // is silent — no special-casing needed, it falls out of the diff.
        let issues = vec![issue(7, "Already labeled before restart", &["agent-ready"])];
        let mut seen = HashMap::new(); // simulates post-restart state
        assert_eq!(label_deltas(&mut seen, &issues).len(), 1, "must re-fire once after a restart");
        assert!(label_deltas(&mut seen, &issues).is_empty(), "must not re-fire on every subsequent poll");
    }

    // ---------- parse_pr_list ----------

    #[test]
    fn parse_pr_list_reads_real_gh_shape_all_success() {
        let json = r#"[{"number":380,"statusCheckRollup":[
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS","name":"build"},
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED","name":"deploy"}
        ],"title":"feat: pane plugins"}]"#;
        let prs = parse_pr_list(json).unwrap();
        assert_eq!(
            prs,
            vec![RawPr {
                number: 380,
                title: "feat: pane plugins".into(),
                state: PrCheckState::Success,
                newest_comment_at: None,
            }]
        );
    }

    #[test]
    fn parse_pr_list_in_progress_check_run_is_pending() {
        let json = r#"[{"number":1,"statusCheckRollup":[{"__typename":"CheckRun","status":"IN_PROGRESS","name":"build"}],"title":"t"}]"#;
        assert_eq!(parse_pr_list(json).unwrap()[0].state, PrCheckState::Pending);
    }

    #[test]
    fn parse_pr_list_a_failing_check_run_is_failure() {
        let json = r#"[{"number":1,"statusCheckRollup":[
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS","name":"a"},
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE","name":"b"}
        ],"title":"t"}]"#;
        assert_eq!(parse_pr_list(json).unwrap()[0].state, PrCheckState::Failure);
    }

    #[test]
    fn parse_pr_list_a_status_context_reports_via_state_not_conclusion() {
        let json = r#"[{"number":1,"statusCheckRollup":[{"__typename":"StatusContext","state":"SUCCESS","context":"ci/legacy"}],"title":"t"}]"#;
        assert_eq!(parse_pr_list(json).unwrap()[0].state, PrCheckState::Success);
    }

    #[test]
    fn parse_pr_list_no_checks_yet_is_pending_not_success() {
        let json = r#"[{"number":1,"statusCheckRollup":[],"title":"t"}]"#;
        assert_eq!(parse_pr_list(json).unwrap()[0].state, PrCheckState::Pending);
    }

    // ---------- pr_check_deltas ----------

    fn pr(number: u64, title: &str, state: PrCheckState) -> RawPr {
        RawPr { number, title: title.to_string(), state, newest_comment_at: None }
    }

    /// A PR with discussion on it — the `pr_comment_deltas` fixture (#864).
    fn pr_with_comment(number: u64, title: &str, at: &str) -> RawPr {
        RawPr {
            number,
            title: title.to_string(),
            state: PrCheckState::Success,
            newest_comment_at: Some(at.to_string()),
        }
    }

    /// A `gh pr list` response that returned fewer PRs than the fetch bound —
    /// so it is every open PR there is, and an absence really is a merge.
    fn whole_prs(prs: &[RawPr]) -> OpenPrList<'_> {
        OpenPrList { prs, complete: true }
    }

    /// A fetch that stopped at `MAX_INTAKE_PRS` — there may be more open PRs
    /// than these, so an absence proves nothing.
    fn partial_prs(prs: &[RawPr]) -> OpenPrList<'_> {
        OpenPrList { prs, complete: false }
    }

    #[test]
    fn pr_check_deltas_fires_on_a_new_terminal_state() {
        let mut seen = HashMap::new();
        pr_check_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Pending)]));
        let signals = pr_check_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Success)]));
        assert_eq!(signals, vec![PrCheckSignal { number: 1, title: "t".into(), from: PrCheckState::Pending, to: PrCheckState::Success }]);
    }

    #[test]
    fn pr_check_deltas_never_fires_on_pending() {
        let mut seen = HashMap::new();
        assert!(pr_check_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Pending)])).is_empty());
        assert!(
            pr_check_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Pending)])).is_empty(),
            "still pending, still no news"
        );
    }

    #[test]
    fn pr_check_deltas_does_not_refire_on_a_repeated_terminal_state() {
        let mut seen = HashMap::new();
        let done = vec![pr(1, "t", PrCheckState::Success)];
        assert_eq!(pr_check_deltas(&mut seen, whole_prs(&done)).len(), 1);
        assert!(
            pr_check_deltas(&mut seen, whole_prs(&done)).is_empty(),
            "SUCCESS on two consecutive polls is not news twice"
        );
    }

    #[test]
    fn pr_check_deltas_fires_when_flipping_between_terminal_states() {
        let mut seen = HashMap::new();
        pr_check_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Failure)]));
        let signals = pr_check_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Success)]));
        assert_eq!(signals.len(), 1, "a push that turns FAILURE into SUCCESS is real news");
        assert_eq!(signals[0].from, PrCheckState::Failure);
        assert_eq!(signals[0].to, PrCheckState::Success);
    }

    #[test]
    fn pr_check_deltas_forgets_a_pr_that_closed_so_a_reopen_starts_fresh() {
        let mut seen = HashMap::new();
        pr_check_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Success)]));
        // PR #1 merged/closed: drops out of a COMPLETE `gh pr list --state
        // open`, which is what makes the absence evidence rather than paging.
        pr_check_deltas(&mut seen, whole_prs(&[]));
        // Same number reopened, immediately SUCCESS again (e.g. reopened with
        // green checks already cached) — must read as news, not "unchanged".
        let signals = pr_check_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Success)]));
        assert_eq!(signals.len(), 1, "a reopened PR must not inherit its pre-close state");
    }

    // ---------- newest comment/review timestamps (#864) ----------

    #[test]
    fn parse_pr_list_takes_the_newest_of_comments_and_reviews() {
        // The real `gh pr list --json comments,reviews` shape: comments carry
        // `createdAt`, reviews carry `submittedAt`, and the newest of BOTH is
        // what "somebody said something on this PR" means.
        let json = r#"[{"number":1,"title":"t","statusCheckRollup":[],
            "comments":[
                {"author":{"login":"a"},"body":"first","createdAt":"2026-08-10T08:00:00Z"},
                {"author":{"login":"b"},"body":"second","createdAt":"2026-08-11T08:00:00Z"}],
            "reviews":[{"author":{"login":"c"},"body":"lgtm","submittedAt":"2026-08-11T09:30:00Z","state":"APPROVED"}]}]"#;
        let prs = parse_pr_list(json).unwrap();
        assert_eq!(prs[0].newest_comment_at.as_deref(), Some("2026-08-11T09:30:00Z"),
            "a review submitted after the last comment is the newest activity");
    }

    #[test]
    fn parse_pr_list_without_comment_fields_reads_as_no_activity() {
        // Migration/degradation shape: a `gh` that returned no comment fields
        // at all (an older gh, a partial response) must read as "no discussion
        // seen", never as a parse failure that would drop the check-state half
        // of the same poll too.
        let json = r#"[{"number":1,"title":"t","statusCheckRollup":[]}]"#;
        assert_eq!(parse_pr_list(json).unwrap()[0].newest_comment_at, None);
    }

    #[test]
    fn parse_pr_list_ignores_an_unsubmitted_review() {
        // A PENDING review has `submittedAt: null` — nobody has said anything
        // yet, and waking the orchestrator for it would be a wake for nothing.
        let json = r#"[{"number":1,"title":"t","statusCheckRollup":[],"comments":[],
            "reviews":[{"state":"PENDING","submittedAt":null}]}]"#;
        assert_eq!(parse_pr_list(json).unwrap()[0].newest_comment_at, None);
    }

    #[test]
    fn pr_comment_deltas_fires_once_on_first_sight_then_settles() {
        let mut seen = HashMap::new();
        let prs = vec![pr_with_comment(1, "t", "2026-08-11T08:00:00Z")];
        let signals = pr_comment_deltas(&mut seen, whole_prs(&prs));
        assert_eq!(signals, vec![PrCommentSignal { number: 1, title: "t".into(), at: "2026-08-11T08:00:00Z".into() }]);
        assert!(pr_comment_deltas(&mut seen, whole_prs(&prs)).is_empty(), "the same discussion is not news twice");
    }

    #[test]
    fn pr_comment_deltas_fires_when_a_new_comment_lands() {
        let mut seen = HashMap::new();
        pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T08:00:00Z")]));
        let signals = pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T09:00:00Z")]));
        assert_eq!(signals.len(), 1, "a newer comment on a known PR is exactly the delta #864 is about");
        assert_eq!(signals[0].at, "2026-08-11T09:00:00Z");
    }

    #[test]
    fn pr_comment_deltas_is_silent_for_a_pr_nobody_has_commented_on() {
        let mut seen = HashMap::new();
        assert!(pr_comment_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Success)])).is_empty(),
            "an open PR with no discussion must never read as discussion activity");
        assert!(seen.is_empty(), "and must not occupy a slot in the seen-state either");
    }

    #[test]
    fn pr_comment_deltas_fires_when_the_newest_comment_moves_backwards() {
        // A deleted or edited newest comment moves the timestamp DOWN. The
        // question is "has the discussion changed since we looked", so this is
        // news — a `>` comparison here would go silent on it.
        let mut seen = HashMap::new();
        pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T09:00:00Z")]));
        let signals = pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T08:00:00Z")]));
        assert_eq!(signals.len(), 1, "a comment removed since the last poll changed the discussion");
    }

    #[test]
    fn pr_comment_deltas_is_silent_when_a_pr_loses_its_only_comment() {
        // rev-368 F1: the had-one-then-NONE transition — the single case this
        // function deliberately does not report, and the only one where it is
        // silent about a change. Pinned rather than left implicit, because the
        // doc comment right above it now claims exactly this shape, and an
        // unpinned claim is how a comment drifts from the code under it.
        let mut seen = HashMap::new();
        pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T08:00:00Z")]));
        let signals = pr_comment_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Success)])); // the comment was deleted
        assert!(signals.is_empty(), "a PR losing its last comment carries nothing to read, so it must not wake anyone");
    }

    #[test]
    fn a_comment_posted_after_a_deletion_still_fires() {
        // The half of F1 that actually matters: the deletion above leaves a
        // stale timestamp in `last_seen` (the PR is still open, so the retain
        // keeps it). That must not swallow the NEXT real comment — if it did,
        // the silent transition would stop being contentless and start being
        // a missed wake, which is the failure direction this whole module is
        // built to avoid.
        let mut seen = HashMap::new();
        pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T08:00:00Z")]));
        pr_comment_deltas(&mut seen, whole_prs(&[pr(1, "t", PrCheckState::Success)])); // deleted
        let signals = pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T09:00:00Z")]));
        assert_eq!(signals.len(), 1, "a new comment after a deletion is real news and must still fire");
        assert_eq!(signals[0].at, "2026-08-11T09:00:00Z");
    }

    #[test]
    fn pr_comment_deltas_forgets_a_pr_that_closed_so_a_reopen_starts_fresh() {
        let mut seen = HashMap::new();
        pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T08:00:00Z")]));
        // Absent from a COMPLETE listing: it genuinely merged or closed, which
        // is what makes the absence evidence rather than paging.
        pr_comment_deltas(&mut seen, whole_prs(&[])); // merged/closed: gone from `gh pr list --state open`
        let signals = pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T08:00:00Z")]));
        assert_eq!(signals.len(), 1, "a reopened PR must not inherit its pre-close discussion state");
    }

    // ---------- fallback_interval_minutes (#864 backoff curve) ----------

    #[test]
    fn fallback_interval_doubles_per_delta_free_wake_then_holds_at_the_cap() {
        let (base, cap) = (180, 1440);
        assert_eq!(fallback_interval_minutes(base, 0, cap), 180, "a group that just saw a delta pays the base cadence");
        assert_eq!(fallback_interval_minutes(base, 1, cap), 360);
        assert_eq!(fallback_interval_minutes(base, 2, cap), 720);
        assert_eq!(fallback_interval_minutes(base, 3, cap), 1440);
        assert_eq!(fallback_interval_minutes(base, 4, cap), 1440, "the cap holds — the backstop coarsens, it never disappears");
        assert_eq!(fallback_interval_minutes(base, 99, cap), 1440);
    }

    #[test]
    fn fallback_interval_with_cap_equal_to_base_is_the_explicit_opt_out() {
        // The documented way to switch backoff off: one value, no extra knob.
        for streak in 0..10 {
            assert_eq!(fallback_interval_minutes(180, streak, 180), 180,
                "cap == base must pin the old fixed-cadence behaviour exactly");
        }
    }

    #[test]
    fn fallback_interval_never_fires_faster_than_the_base() {
        // A cap BELOW the base is a misconfiguration; the backoff may only
        // ever slow the fallback down, never speed it up.
        assert_eq!(fallback_interval_minutes(180, 0, 30), 180);
        assert_eq!(fallback_interval_minutes(180, 5, 30), 180);
    }

    #[test]
    fn fallback_interval_survives_an_absurd_streak_without_overflowing() {
        // The shift is bounded independently of the cap: a streak that somehow
        // ran away must still produce the cap, not a wrapped/zero interval
        // that would turn the backstop into a busy-loop.
        assert_eq!(fallback_interval_minutes(u32::MAX, u32::MAX, u32::MAX), u32::MAX);
        assert_eq!(fallback_interval_minutes(30, u32::MAX, 240), 240);
    }

    // ---------- completeness: the PR fetch bound must not look like a merge ----------

    /// **The bound must actually be requested.** Every other test in this
    /// section hands `pr_check_deltas` a listing directly, so if `--limit`
    /// falls off the command line `gh` quietly returns its own 30 newest and
    /// all of them still pass — the same silent-restore shape
    /// `the_issue_list_argv_always_carries_the_fetch_bound` guards for the
    /// issue half, and the reason this argv is built in the module rather
    /// than spelled inline at the call site.
    #[test]
    fn the_pr_list_argv_always_carries_the_fetch_bound() {
        let argv = pr_list_argv();
        let at = argv.iter().position(|a| a == "--limit").unwrap_or_else(|| {
            panic!("the PR listing must request a bound — without --limit gh returns its own 30: {argv:?}")
        });
        assert_eq!(
            argv.get(at + 1),
            Some(&MAX_INTAKE_PRS.to_string()),
            "--limit must carry the bound the rest of this module reasons about: {argv:?}"
        );
        // Independent of the constant's value, so this is not the pin checking
        // itself: a bound at or under gh's own default would buy nothing.
        assert!(MAX_INTAKE_PRS > 30, "the bound must beat gh's 30-PR default to be worth requesting");
        // The rest of the call shape, so a rewrite of this argv can't quietly
        // change what is fetched either — dropping `statusCheckRollup` would
        // leave every PR parsing as Pending and the sweep silently dead.
        assert_eq!(&argv[..4], &["pr", "list", "--state", "open"], "got: {argv:?}");
        assert!(
            argv.iter().any(|a| a.split(',').any(|f| f == "statusCheckRollup")),
            "the field the check sweep reads: {argv:?}"
        );
    }

    /// **The comment fields must actually be requested (#864).** Every
    /// `pr_comment_deltas` test hands it an already-parsed list, so dropping
    /// `comments`/`reviews` from this argv would leave the discussion signal
    /// permanently silent with the whole suite still green — the same
    /// silent-restore shape the `--limit` pin above guards against, one field
    /// over.
    #[test]
    fn the_pr_list_argv_asks_for_the_comment_fields() {
        let argv = pr_list_argv();
        let fields: Vec<&str> = argv
            .iter()
            .find(|a| a.contains("statusCheckRollup"))
            .map(|a| a.split(',').collect())
            .unwrap_or_else(|| panic!("the PR listing must ask for the check rollup: {argv:?}"));
        for want in ["number", "title", "statusCheckRollup", "comments", "reviews"] {
            assert!(fields.contains(&want), "--json must carry {want}, got: {fields:?}");
        }
    }

    /// The boundary rule, stated directly: `gh` reports no total, so "exactly
    /// the bound came back" is indistinguishable from "the first N of many"
    /// and must be treated as the latter.
    #[test]
    fn pr_from_fetch_calls_a_full_window_incomplete_and_a_short_one_complete() {
        let short: Vec<RawPr> = (0..3).map(|n| pr(n, "t", PrCheckState::Success)).collect();
        assert!(OpenPrList::from_fetch(&short).complete, "fewer than the bound is the whole list");

        let full: Vec<RawPr> = (0..MAX_INTAKE_PRS as u64).map(|n| pr(n, "t", PrCheckState::Success)).collect();
        assert!(
            !OpenPrList::from_fetch(&full).complete,
            "a fetch that filled its window must be assumed to have left PRs behind"
        );
    }

    /// **The churn defect this issue is about (#795), reproduced.** `gh pr
    /// list` returns the N newest, so opening one PR evicts the oldest in the
    /// window and merging something above it lets that PR back in. A PR that
    /// merely fell past the bound has not merged — and if it were forgotten,
    /// its return would re-announce a terminal check state already reported
    /// days ago, waking an orchestrator for CI that finished long before.
    #[test]
    fn a_truncated_listing_never_refires_a_pr_that_fell_past_the_bound() {
        let mut seen = HashMap::new();
        let both = vec![pr(1, "older", PrCheckState::Success), pr(2, "newer", PrCheckState::Success)];
        assert_eq!(pr_check_deltas(&mut seen, partial_prs(&both)).len(), 2, "both are news the first time");

        // A newer PR arrives and pushes #1 out of the 'newest N' window.
        let window = vec![pr(2, "newer", PrCheckState::Success), pr(3, "newest", PrCheckState::Success)];
        let churned = pr_check_deltas(&mut seen, partial_prs(&window));
        assert_eq!(churned.len(), 1, "only the genuinely new PR fires: {churned:?}");
        assert_eq!(churned[0].number, 3);
        assert_eq!(seen.get(&1), Some(&PrCheckState::Success), "a PR that merely fell past the bound must not be forgotten");

        // #3 merges, so #1 is back in the window — still green, still not news.
        let back = pr_check_deltas(&mut seen, partial_prs(&both));
        assert!(back.is_empty(), "a re-entering PR must not re-fire its terminal state: {back:?}");
    }

    /// The other half of the same rule, and the property the churn fix must
    /// not cost: on a COMPLETE listing absence really does mean merged/closed,
    /// so a reopen is news again.
    #[test]
    fn a_complete_pr_listing_still_treats_absence_as_closed() {
        let mut seen = HashMap::new();
        let prs = vec![pr(1, "t", PrCheckState::Success)];
        pr_check_deltas(&mut seen, whole_prs(&prs));
        pr_check_deltas(&mut seen, whole_prs(&[]));
        assert!(seen.is_empty(), "a complete listing that omits a PR means it merged or closed");
    }

    /// A state change is evidence, not absence, so a PR **present** in a
    /// truncated listing diffs exactly as it always did — the completeness
    /// flag must gate the prune only, never the signal. Without this, a fix
    /// for the churn that also suppressed real transitions on any repo big
    /// enough to paginate would still pass every other test here.
    #[test]
    fn a_truncated_listing_still_fires_a_real_transition() {
        let mut seen = HashMap::new();
        pr_check_deltas(&mut seen, partial_prs(&[pr(1, "t", PrCheckState::Pending)]));
        let signals = pr_check_deltas(&mut seen, partial_prs(&[pr(1, "t", PrCheckState::Failure)]));
        assert_eq!(signals.len(), 1, "PENDING → FAILURE is news whether or not the fetch was complete");
        assert_eq!(signals[0].to, PrCheckState::Failure);
    }

    /// **The comment signal's half of the same churn rule (#864 meeting
    /// #795).** `pr_comment_deltas` landed before the PR listing was bounded,
    /// so it pruned on absence unconditionally. Against a truncated listing
    /// that is the identical defect `pr_check_deltas` was fixed for, one field
    /// over: a PR that merely fell past `MAX_INTAKE_PRS` loses its last-seen
    /// timestamp, and when the window churns back over it the *same* newest
    /// comment reads as first-sight discussion — waking an orchestrator for a
    /// conversation that ended days ago, every time the window moves.
    #[test]
    fn a_truncated_listing_never_refires_comment_activity_that_fell_past_the_bound() {
        let mut seen = HashMap::new();
        let both =
            vec![pr_with_comment(1, "older", "2026-08-11T08:00:00Z"), pr_with_comment(2, "newer", "2026-08-11T08:30:00Z")];
        assert_eq!(pr_comment_deltas(&mut seen, partial_prs(&both)).len(), 2, "both are news the first time");

        // A newer PR arrives and pushes #1 out of the 'newest N' window. Its
        // discussion did not change — nobody said anything, it just paged out.
        let window =
            vec![pr_with_comment(2, "newer", "2026-08-11T08:30:00Z"), pr_with_comment(3, "newest", "2026-08-11T09:00:00Z")];
        let churned = pr_comment_deltas(&mut seen, partial_prs(&window));
        assert_eq!(churned.len(), 1, "only the genuinely new PR's discussion fires: {churned:?}");
        assert_eq!(churned[0].number, 3);
        assert_eq!(
            seen.get(&1).map(String::as_str),
            Some("2026-08-11T08:00:00Z"),
            "a PR that merely fell past the bound must keep its last-seen discussion"
        );

        // #3 merges, so #1 is back in the window — same comment, still not news.
        let back = pr_comment_deltas(&mut seen, partial_prs(&both));
        assert!(back.is_empty(), "a re-entering PR must not re-announce discussion already reported: {back:?}");
    }

    /// The property the comment-side churn fix must not cost, stated on its
    /// own: on a COMPLETE listing absence still means merged/closed, so a
    /// reopened PR's discussion is news again. Without this a fix that simply
    /// never pruned would pass the churn test above and silently break the
    /// reopen case.
    #[test]
    fn a_complete_listing_still_forgets_a_closed_prs_discussion() {
        let mut seen = HashMap::new();
        pr_comment_deltas(&mut seen, whole_prs(&[pr_with_comment(1, "t", "2026-08-11T08:00:00Z")]));
        pr_comment_deltas(&mut seen, whole_prs(&[]));
        assert!(seen.is_empty(), "a complete listing that omits a PR means it merged or closed");
    }

    /// New discussion on a PR **present** in a truncated listing is evidence,
    /// not absence — the completeness flag gates the prune only, never the
    /// signal, exactly as it does for the check sweep.
    #[test]
    fn a_truncated_listing_still_fires_new_comment_activity() {
        let mut seen = HashMap::new();
        pr_comment_deltas(&mut seen, partial_prs(&[pr_with_comment(1, "t", "2026-08-11T08:00:00Z")]));
        let signals = pr_comment_deltas(&mut seen, partial_prs(&[pr_with_comment(1, "t", "2026-08-11T09:00:00Z")]));
        assert_eq!(signals.len(), 1, "a newer comment is news whether or not the fetch was complete");
        assert_eq!(signals[0].at, "2026-08-11T09:00:00Z");
    }

    // ---------- intake_wake_summary ----------

    #[test]
    fn intake_wake_summary_names_issue_and_pr_deltas() {
        let labels = vec![LabelSignal { number: 42, title: "Do the thing".into(), label: "agent-ready".into() }];
        let prs = vec![PrCheckSignal { number: 7, title: "Fix Y".into(), from: PrCheckState::Pending, to: PrCheckState::Failure }];
        let comments = vec![PrCommentSignal { number: 9, title: "Fix Z".into(), at: "2026-08-11T09:00:00Z".into() }];
        let s = intake_wake_summary(&labels, &prs, &comments, &[], IntakeTruncation::default());
        assert!(s.contains("issue #42 labeled agent-ready"), "got: {s}");
        assert!(s.contains("PR #7 checks PENDING → FAILURE"), "got: {s}");
        assert!(s.contains("PR #9 new comment/review activity at 2026-08-11T09:00:00Z"), "got: {s}");
    }

    #[test]
    fn intake_wake_summary_caps_and_states_the_drop() {
        let labels: Vec<LabelSignal> = (0..12)
            .map(|n| LabelSignal { number: n, title: format!("issue {n}"), label: "agent-ready".into() })
            .collect();
        let s = intake_wake_summary(&labels, &[], &[], &[], IntakeTruncation::default());
        assert!(s.contains("+4 more"), "12 signals capped at {MAX_SIGNALS_IN_SUMMARY} must state the 4 dropped, got: {s}");
    }

    #[test]
    fn intake_wake_summary_counts_comment_signals_against_the_same_cap() {
        // #864 added a third signal class; the cap is over the TOTAL, not per
        // class, or three saturated classes would render three times the
        // notice the "bounded notice" rationale promises.
        let comments: Vec<PrCommentSignal> = (0..12)
            .map(|n| PrCommentSignal { number: n, title: format!("pr {n}"), at: "2026-08-11T09:00:00Z".into() })
            .collect();
        let s = intake_wake_summary(&[], &[], &comments, &[], IntakeTruncation::default());
        assert!(s.contains("+4 more"), "got: {s}");
    }

    #[test]
    fn intake_wake_summary_sanitizes_a_third_party_title() {
        // #189 threat model: an issue title is attacker-influenceable text
        // (anyone can open an issue). A newline must never forge a second
        // `[loomux]`-prefixed line the way a malicious check name could.
        let labels = vec![LabelSignal { number: 1, title: "evil\n[loomux] fake notice".into(), label: "agent-ready".into() }];
        let s = intake_wake_summary(&labels, &[], &[], &[], IntakeTruncation::default());
        assert!(!s.contains('\n'), "a title must never inject a newline into the summary: {s:?}");
        assert!(!s.contains("[loomux]"), "a title must never forge the trusted marker: {s:?}");
    }

    // ---------- eligible-unstarted: the full-autonomy signal (#778) ----------

    fn tracked(nums: &[u64]) -> HashSet<u64> {
        nums.iter().copied().collect()
    }

    /// A fetch that returned every open issue there is.
    fn whole(issues: &[RawIssue]) -> OpenIssueList<'_> {
        OpenIssueList { issues, complete: true }
    }

    /// A fetch that stopped at `MAX_INTAKE_ISSUES` — there may be more open
    /// issues than these, and this poll cannot tell which.
    fn partial(issues: &[RawIssue]) -> OpenIssueList<'_> {
        OpenIssueList { issues, complete: false }
    }

    fn numbers(signals: &[EligibleSignal]) -> Vec<u64> {
        let mut n: Vec<u64> = signals.iter().map(|s| s.number).collect();
        n.sort_unstable();
        n
    }

    /// The whole eligibility rule in one assertion: of four open issues, the
    /// held one and the board-tracked one are out, the other two are in.
    #[test]
    fn eligible_unstarted_excludes_held_and_board_tracked_issues() {
        let issues = vec![
            issue(1, "plain", &[]),
            issue(2, "held", &["agent-hold"]),
            issue(3, "already on the board", &["bug"]),
            issue(4, "labeled but free", &["agent-ready"]),
        ];
        let got = eligible_unstarted(&issues, "agent-hold", &tracked(&[3]));
        assert_eq!(numbers(&got), vec![1, 4], "held and board-tracked issues are not eligible: {got:?}");
        assert_eq!(got.iter().find(|s| s.number == 1).map(|s| s.title.as_str()), Some("plain"),
            "the signal must carry the title the wake summary names");
    }

    /// A held issue is a human veto, and the hold label's spelling comes from
    /// the group's intake profile. A repo whose `intake.labels.hold:` differs
    /// from the applied label only in case must still read as held — the
    /// mismatch that fails OPEN here starts work a human vetoed.
    #[test]
    fn eligible_unstarted_matches_the_hold_label_case_insensitively() {
        let issues = vec![issue(1, "held with a different case", &["Agent-Hold"])];
        assert!(
            eligible_unstarted(&issues, "agent-hold", &HashSet::new()).is_empty(),
            "a consent boundary must fail closed on a case mismatch, not start the work"
        );
    }

    /// The hold label is the profile's, not a hardcoded const (#382 P2's gap
    /// must not be repeated on a consent boundary): a repo that renamed it
    /// gets ITS spelling honored, and `agent-hold` then means nothing.
    #[test]
    fn eligible_unstarted_honors_a_repo_specific_hold_spelling() {
        let issues = vec![issue(1, "held by the repo's own label", &["do-not-touch"]), issue(2, "not held here", &["agent-hold"])];
        let got = eligible_unstarted(&issues, "do-not-touch", &HashSet::new());
        assert_eq!(numbers(&got), vec![2], "the resolved profile spelling is the boundary, not the built-in default: {got:?}");
    }

    /// No resolution path can hand this function an empty hold spelling, so
    /// this pins the answer for the day one does: "I don't know what a hold
    /// looks like" must mean nothing is startable, not everything is.
    #[test]
    fn eligible_unstarted_fails_closed_on_an_empty_hold_spelling() {
        let issues = vec![issue(1, "unlabeled", &[]), issue(2, "held", &["agent-hold"])];
        assert!(eligible_unstarted(&issues, "", &HashSet::new()).is_empty(), "an unknown boundary starts nothing");
        assert!(eligible_unstarted(&issues, "   ", &HashSet::new()).is_empty());
    }

    #[test]
    fn eligible_deltas_fires_once_for_a_newly_eligible_issue() {
        let mut seen = HashSet::new();
        let issues = vec![issue(1, "new work", &[])];
        let first = eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());
        assert_eq!(numbers(&first), vec![1]);
        let second = eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());
        assert!(second.is_empty(), "an issue that stays eligible is not news twice: {second:?}");
    }

    /// The enable-time triage trigger, stated directly: a group that has just
    /// turned full autonomy on has an empty last-seen set, so its first poll
    /// announces the whole eligible backlog exactly once and then settles.
    #[test]
    fn eligible_deltas_fires_the_whole_backlog_once_from_an_empty_last_seen() {
        let mut seen = HashSet::new();
        let issues = vec![issue(1, "a", &[]), issue(2, "b", &[]), issue(3, "c", &["agent-hold"])];
        let first = eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());
        assert_eq!(numbers(&first), vec![1, 2], "the backlog minus the held issue fires once");
        assert!(
            eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new()).is_empty(),
            "…and the very next poll of the same state is silent"
        );
    }

    /// Un-holding is a human decision to release work, and it must re-reach
    /// the orchestrator — the state is "eligible at the last poll", not "ever
    /// announced", so dropping the number on the way out is what re-fires it.
    #[test]
    fn eligible_deltas_refires_once_when_a_hold_is_removed() {
        let mut seen = HashSet::new();
        let held = vec![issue(1, "held", &["agent-hold"])];
        let free = vec![issue(1, "held", &[])];
        assert!(eligible_deltas(&mut seen, true, Some(whole(&held)), "agent-hold", &HashSet::new()).is_empty());
        let after = eligible_deltas(&mut seen, true, Some(whole(&free)), "agent-hold", &HashSet::new());
        assert_eq!(numbers(&after), vec![1], "a hold the human removed must reach the orchestrator");
        assert!(
            eligible_deltas(&mut seen, true, Some(whole(&free)), "agent-hold", &HashSet::new()).is_empty(),
            "…once, not on every poll after"
        );
    }

    /// Same shape for the other suppressor: a task deleted off the board makes
    /// its issue read as unstarted again, and that fires exactly once.
    #[test]
    fn eligible_deltas_refires_once_when_a_board_task_is_deleted() {
        let mut seen = HashSet::new();
        let issues = vec![issue(1, "being worked", &[])];
        assert!(eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &tracked(&[1])).is_empty());
        let after = eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());
        assert_eq!(numbers(&after), vec![1], "an issue whose task vanished is unstarted work again");
        assert!(eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new()).is_empty());
    }

    /// A closed issue drops out of a **complete** `gh issue list --state open`
    /// entirely, so it drops out of the seen set too — and a REOPENED one is
    /// news again, the same posture `pr_check_deltas` takes for a reopened PR.
    /// "Complete" is load-bearing in that sentence: on a truncated fetch an
    /// absent issue may merely have fallen past the bound, which is what
    /// `a_truncated_listing_never_forgets_an_issue_that_fell_past_the_bound`
    /// pins.
    #[test]
    fn eligible_deltas_forgets_a_closed_issue_so_a_reopen_fires_again() {
        let mut seen = HashSet::new();
        let issues = vec![issue(1, "t", &[])];
        assert_eq!(numbers(&eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new())), vec![1]);
        eligible_deltas(&mut seen, true, Some(whole(&[])), "agent-hold", &HashSet::new()); // closed
        let reopened = eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());
        assert_eq!(numbers(&reopened), vec![1], "a reopened issue must not inherit its pre-close state");
    }

    // ---------- completeness: the fetch bound must not look like a closure ----------

    /// **The bound must actually be requested.** Everything else in this file
    /// reasons about a fetch that asked for [`MAX_INTAKE_ISSUES`] issues; if
    /// the flag is not on the command line, `gh` quietly returns its own 30
    /// and every one of those tests still passes, because they all operate on
    /// a listing handed to them rather than on the one the poller fetched.
    /// This is the only assertion standing between that and a silent
    /// regression.
    #[test]
    fn the_issue_list_argv_always_carries_the_fetch_bound() {
        let argv = issue_list_argv();
        let at = argv.iter().position(|a| a == "--limit").unwrap_or_else(|| {
            panic!("the issue listing must request a bound — without --limit gh returns its own 30: {argv:?}")
        });
        assert_eq!(
            argv.get(at + 1),
            Some(&MAX_INTAKE_ISSUES.to_string()),
            "--limit must carry the bound the rest of this module reasons about: {argv:?}"
        );
        // Independent of the constant's value, so this is not the pin checking
        // itself: a bound at or under gh's own default would buy nothing.
        assert!(MAX_INTAKE_ISSUES > 30, "the bound must beat gh's 30-issue default to be worth requesting");
        // The rest of the call shape, so a rewrite of this argv can't quietly
        // change what is fetched either.
        assert_eq!(&argv[..4], &["issue", "list", "--state", "open"], "got: {argv:?}");
        assert!(argv.contains(&"number,title,labels".to_string()), "the fields both diffs read: {argv:?}");
    }

    /// The boundary rule, stated directly: `gh` reports no total, so "exactly
    /// the bound came back" is indistinguishable from "the first N of many"
    /// and must be treated as the latter.
    #[test]
    fn from_fetch_calls_a_full_window_incomplete_and_a_short_one_complete() {
        let short: Vec<RawIssue> = (0..3).map(|n| issue(n, "t", &[])).collect();
        assert!(OpenIssueList::from_fetch(&short).complete, "fewer than the bound is the whole list");

        let full: Vec<RawIssue> = (0..MAX_INTAKE_ISSUES as u64).map(|n| issue(n, "t", &[])).collect();
        assert!(
            !OpenIssueList::from_fetch(&full).complete,
            "a fetch that filled its window must be assumed to have left issues behind"
        );
    }

    /// **The pagination-churn defect, reproduced.** `gh issue list` returns the
    /// N newest, so an ordinary day (file a new issue, close an old one) churns
    /// which issues are in the window. An issue that falls past the bound has
    /// not stopped being eligible — and if it were forgotten, its return to the
    /// window would be announced as brand-new work, which is a wake nobody
    /// caused and a "fires exactly once" guarantee that does not hold.
    #[test]
    fn a_truncated_listing_never_forgets_an_issue_that_fell_past_the_bound() {
        let mut seen = HashSet::new();
        let both = vec![issue(1, "older", &[]), issue(2, "newer", &[])];
        assert_eq!(numbers(&eligible_deltas(&mut seen, true, Some(partial(&both)), "agent-hold", &HashSet::new())), vec![1, 2]);

        // A newer issue arrives and pushes #1 out of the window.
        let window = vec![issue(2, "newer", &[]), issue(3, "newest", &[])];
        let churned = eligible_deltas(&mut seen, true, Some(partial(&window)), "agent-hold", &HashSet::new());
        assert_eq!(numbers(&churned), vec![3], "only the genuinely new issue fires: {churned:?}");
        assert!(seen.contains(&1), "an issue that merely fell past the fetch bound must not be forgotten");

        // #3 is closed, so #1 is back in the window — and is NOT news.
        let back = eligible_deltas(&mut seen, true, Some(partial(&both)), "agent-hold", &HashSet::new());
        assert!(back.is_empty(), "a re-entering issue must not read as newly eligible: {back:?}");
    }

    /// The other half of the same rule: on a COMPLETE listing, absence really
    /// does mean closed, so the "reopen is news again" behaviour survives.
    #[test]
    fn a_complete_listing_still_treats_absence_as_closed() {
        let mut seen = HashSet::new();
        let issues = vec![issue(1, "t", &[])];
        eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());
        eligible_deltas(&mut seen, true, Some(whole(&[])), "agent-hold", &HashSet::new());
        assert!(seen.is_empty(), "a complete listing that omits an issue means it closed");
    }

    /// Held/board-tracked transitions are evidence, not absence, so they still
    /// forget the issue even on a truncated listing — otherwise un-holding
    /// something would stop re-firing on any repo big enough to paginate.
    #[test]
    fn a_truncated_listing_still_forgets_an_issue_that_became_ineligible() {
        let mut seen = HashSet::new();
        let free = vec![issue(1, "t", &[])];
        let held = vec![issue(1, "t", &["agent-hold"])];
        eligible_deltas(&mut seen, true, Some(partial(&free)), "agent-hold", &HashSet::new());
        eligible_deltas(&mut seen, true, Some(partial(&held)), "agent-hold", &HashSet::new());
        assert!(seen.is_empty(), "present-and-no-longer-eligible is a real transition, bound or no bound");
        let after = eligible_deltas(&mut seen, true, Some(partial(&free)), "agent-hold", &HashSet::new());
        assert_eq!(numbers(&after), vec![1], "so un-holding it still re-fires once");
    }

    /// Opt-in intake (full autonomy off) produces no eligible signal at all —
    /// and clears the set, so a LATER enable is a fresh triage trigger rather
    /// than a silent one whose backlog was already "seen" under a mode that
    /// never announced it.
    #[test]
    fn eligible_deltas_is_inert_while_full_autonomy_is_off_and_a_re_enable_refires() {
        let mut seen = HashSet::new();
        let issues = vec![issue(1, "a", &[]), issue(2, "b", &[])];
        assert_eq!(numbers(&eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new())), vec![1, 2]);

        let off = eligible_deltas(&mut seen, false, Some(whole(&issues)), "agent-hold", &HashSet::new());
        assert!(off.is_empty(), "a group not in full autonomy has no eligible signal: {off:?}");
        assert!(seen.is_empty(), "the last-seen set must not survive the mode being off");

        let re_enabled = eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());
        assert_eq!(numbers(&re_enabled), vec![1, 2], "a re-enable must trigger triage again");
    }

    /// #332's "degrade, don't deny", applied here: a failed `gh issue list`
    /// must not read as "the backlog went empty", or the next successful poll
    /// would re-announce all of it as newly eligible.
    #[test]
    fn a_failed_issue_fetch_leaves_the_last_seen_set_untouched() {
        let mut seen = HashSet::new();
        let issues = vec![issue(1, "a", &[]), issue(2, "b", &[])];
        eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());

        let during_failure = eligible_deltas(&mut seen, true, None, "agent-hold", &HashSet::new());
        assert!(during_failure.is_empty(), "a poll with no data has nothing to report: {during_failure:?}");

        let after = eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &HashSet::new());
        assert!(after.is_empty(), "a gh blip must not re-announce the whole backlog: {after:?}");
    }

    // ---------- board_tracked_issues: the lenient Task.issue parse ----------

    #[test]
    fn board_tracked_issues_reads_both_spellings_agents_write() {
        assert_eq!(board_tracked_issues(&["#712", "43", " #7 "]), tracked(&[712, 43, 7]));
    }

    /// The edge case that matters: board text is agent-written, so a malformed
    /// `Task.issue` must never panic and — the sharper half — must never make
    /// a DIFFERENT issue read as tracked, which would silently suppress real
    /// work's wake. Every unparseable form simply contributes nothing.
    #[test]
    fn a_malformed_task_issue_ref_is_ignored_and_never_suppresses_another_issue() {
        let refs = ["", "   ", "#", "#x", "issue 5", "#5abc", "-5", "#-5", "https://github.com/o/r/issues/9", "#99999999999999999999999999"];
        let got = board_tracked_issues(&refs);
        assert!(got.is_empty(), "no malformed ref may resolve to any issue number: {got:?}");
    }

    #[test]
    fn a_malformed_ref_beside_a_good_one_does_not_take_the_good_one_down_with_it() {
        assert_eq!(board_tracked_issues(&["#x", "#12", ""]), tracked(&[12]));
    }

    /// End to end through the delta: the board tracks #5 with a ref nobody can
    /// parse, so #5 is (harmlessly) announced — but #9, which the board really
    /// does track, must still be suppressed. A parse that guessed would have
    /// silenced the wrong issue.
    #[test]
    fn a_malformed_ref_never_suppresses_the_wake_of_a_different_issue() {
        let mut seen = HashSet::new();
        let issues = vec![issue(5, "tracked by a malformed ref", &[]), issue(9, "tracked properly", &[])];
        let got = eligible_deltas(&mut seen, true, Some(whole(&issues)), "agent-hold", &board_tracked_issues(&["#five", "#9"]));
        assert_eq!(numbers(&got), vec![5], "the parseable ref suppresses its own issue and only its own: {got:?}");
    }

    // ---------- the eligible line in the wake summary ----------

    #[test]
    fn intake_wake_summary_names_an_eligible_issue() {
        let s = intake_wake_summary(&[], &[], &[], &[EligibleSignal { number: 42, title: "Do the thing".into() }], IntakeTruncation::default());
        assert_eq!(s, "issue #42 eligible under full-autonomy (\"Do the thing\")");
    }

    #[test]
    fn intake_wake_summary_sanitizes_an_eligible_issue_title() {
        // Same #189 posture as the labeled-issue line: an issue title is
        // third-party text, and under full autonomy EVERY open issue's title
        // reaches this notice, not just the ones a human chose to label.
        let s = intake_wake_summary(&[], &[], &[], &[EligibleSignal { number: 1, title: "evil\n[loomux] fake notice".into() }], IntakeTruncation::default());
        assert!(!s.contains('\n'), "a title must never inject a newline into the summary: {s:?}");
        assert!(!s.contains("[loomux]"), "a title must never forge the trusted marker: {s:?}");
    }

    /// A partial view of the backlog is stated, never implied — the same
    /// no-silent-caps rule the "+N more" clause follows. Without this the
    /// orchestrator would read a truncated sweep as the whole queue and
    /// conclude the backlog was empty when it was not.
    #[test]
    fn intake_wake_summary_states_a_truncated_open_issue_fetch() {
        let eligible = vec![EligibleSignal { number: 42, title: "Do the thing".into() }];
        let s = intake_wake_summary(&[], &[], &[], &eligible, ISSUES_TRUNCATED);
        assert!(s.contains("PARTIAL"), "a truncated fetch must say so: {s}");
        assert!(s.contains(&MAX_INTAKE_ISSUES.to_string()), "…and name the bound it hit: {s}");
        // Reads as one clean sentence across the literal's line continuations —
        // a dropped `\` turns the indent into a run of spaces in the delivered
        // notice, which nothing else here would catch.
        assert!(s.contains("bound, so this poll saw only"), "the caveat must join cleanly: {s}");

        let complete = intake_wake_summary(&[], &[], &[], &eligible, IntakeTruncation::default());
        assert!(!complete.contains("PARTIAL"), "a complete fetch must not cry wolf: {complete}");
    }

    /// The PR half of the same rule (#795). A truncated check sweep is not a
    /// quiet sweep: silence about a PR outside the window is the absence of
    /// evidence, and an orchestrator that reads it as "still running" waits
    /// forever on CI that finished. The caveat has to name which fetch was
    /// short, because only one of the two may be.
    #[test]
    fn intake_wake_summary_states_a_truncated_open_pr_fetch() {
        let prs = vec![PrCheckSignal { number: 7, title: "Fix Y".into(), from: PrCheckState::Pending, to: PrCheckState::Success }];
        let s = intake_wake_summary(&[], &prs, &[], &[], PRS_TRUNCATED);
        assert!(s.contains("PARTIAL"), "a truncated PR fetch must say so: {s}");
        assert!(s.contains(&format!("{MAX_INTAKE_PRS}-PR bound")), "…and name the bound it hit: {s}");
        assert!(s.contains("open-PR fetch"), "…and say which of the two fetches was short: {s}");
        assert!(
            !s.contains(&format!("{MAX_INTAKE_ISSUES}-issue bound")),
            "a short PR fetch must not accuse the issue fetch: {s}"
        );
        // Reads as one clean sentence across the literal's line continuations —
        // a dropped `\` turns the indent into a run of spaces in the delivered
        // notice, which nothing else here would catch.
        assert!(s.contains("bound, so this poll's check sweep saw only"), "the caveat must join cleanly: {s}");

        let complete = intake_wake_summary(&[], &prs, &[], &[], IntakeTruncation::default());
        assert!(!complete.contains("PARTIAL"), "a complete PR fetch must not cry wolf: {complete}");
    }

    /// Both halves can be short at once, and each is reported on its own
    /// evidence — a single shared caveat would let one bound's truncation
    /// speak for a fetch that was actually whole.
    #[test]
    fn the_two_truncation_caveats_are_independent() {
        let prs = vec![PrCheckSignal { number: 7, title: "Fix Y".into(), from: PrCheckState::Pending, to: PrCheckState::Success }];
        let both = IntakeTruncation { issues: true, prs: true };
        let s = intake_wake_summary(&[], &prs, &[], &[], both);
        assert_eq!(s.matches("PARTIAL:").count(), 2, "each short fetch states itself: {s}");

        let issues_only = intake_wake_summary(&[], &prs, &[], &[], ISSUES_TRUNCATED);
        assert!(issues_only.contains("open-issue fetch"), "got: {issues_only}");
        assert!(!issues_only.contains("open-PR fetch"), "a whole PR fetch must not be reported short: {issues_only}");
    }

    /// The PR caveat rides a notice this poll was already sending, exactly as
    /// the issue one does — a repo permanently over the bound must not wake
    /// its orchestrator every poll to report nothing but its own size.
    #[test]
    fn a_truncated_pr_poll_with_no_findings_still_says_nothing() {
        assert_eq!(intake_wake_summary(&[], &[], &[], &[], PRS_TRUNCATED), "");
    }

    /// The caveat rides a notice this poll was already sending; it never
    /// manufactures one. A big repo would otherwise wake its orchestrator on
    /// every poll forever to report nothing but its own size.
    #[test]
    fn a_truncated_poll_with_no_findings_still_says_nothing() {
        assert_eq!(intake_wake_summary(&[], &[], &[], &[], ISSUES_TRUNCATED), "");
    }

    /// **The union this landing creates, in one notice.** Before it, the three
    /// signal classes and the two caveats had never met: `comments` came from
    /// #864 on a summary that had no eligibility half and no truncation
    /// parameter, `eligible` + `PARTIAL` came from #778/#795 on one that had no
    /// comment half. A poll of a big repo in full autonomy can produce all of
    /// them at once, and this is the only test that says what that reads like —
    /// every class named, in a stable order, with each short fetch stating
    /// itself and nothing crowded out.
    #[test]
    fn intake_wake_summary_carries_every_signal_class_and_both_caveats() {
        let labels = vec![LabelSignal { number: 42, title: "Do the thing".into(), label: "agent-ready".into() }];
        let prs = vec![PrCheckSignal { number: 7, title: "Fix Y".into(), from: PrCheckState::Pending, to: PrCheckState::Success }];
        let comments = vec![PrCommentSignal { number: 9, title: "Fix Z".into(), at: "2026-08-11T09:00:00Z".into() }];
        let eligible = vec![EligibleSignal { number: 12, title: "Old backlog item".into() }];
        let s = intake_wake_summary(&labels, &prs, &comments, &eligible, IntakeTruncation { issues: true, prs: true });

        assert!(s.contains("issue #42 labeled agent-ready"), "the label signal survived the union: {s}");
        assert!(s.contains("PR #7 checks PENDING → SUCCESS"), "the check signal survived the union: {s}");
        assert!(
            s.contains("PR #9 new comment/review activity at 2026-08-11T09:00:00Z"),
            "the #864 comment signal survived the union: {s}"
        );
        assert!(s.contains("issue #12 eligible under full-autonomy"), "the #778 eligibility signal survived the union: {s}");
        assert_eq!(s.matches("PARTIAL:").count(), 2, "each short fetch states itself, in the same notice: {s}");
        assert!(!s.contains("more —"), "four signals is well under the cap — nothing may be dropped here: {s}");
        // The order is the read order the orchestrator's contract describes:
        // labels, then checks, then discussion, then eligibility.
        let at = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("missing {needle} in: {s}"));
        assert!(at("issue #42") < at("PR #7"), "got: {s}");
        assert!(at("PR #7") < at("PR #9"), "got: {s}");
        assert!(at("PR #9") < at("issue #12"), "got: {s}");
        assert!(at("issue #12") < at("PARTIAL:"), "the caveats close the notice: {s}");
    }

    /// The cap is shared across all four kinds, and the count it states is
    /// the true total dropped — the enable-time burst is exactly the case
    /// where an unshared cap would let the notice grow.
    #[test]
    fn intake_wake_summary_caps_eligible_signals_against_the_same_budget() {
        let labels = vec![LabelSignal { number: 1, title: "l".into(), label: "agent-ready".into() }];
        let eligible: Vec<EligibleSignal> =
            (0..20).map(|n| EligibleSignal { number: 100 + n, title: format!("backlog {n}") }).collect();
        let s = intake_wake_summary(&labels, &[], &[], &eligible, IntakeTruncation::default());
        assert_eq!(s.matches("eligible under full-autonomy").count(), MAX_SIGNALS_IN_SUMMARY - 1,
            "the label line spends one of the {MAX_SIGNALS_IN_SUMMARY} slots: {s}");
        assert!(s.contains("+13 more"), "21 signals, 8 named, 13 dropped — stated, never silent: {s}");
    }

    // ---------- idle_tick_gate ----------

    #[test]
    fn gate_fires_on_intake_signal_alone() {
        assert!(idle_tick_gate(true, false, false, false));
    }

    #[test]
    fn gate_fires_on_pending_notification_alone() {
        assert!(idle_tick_gate(false, true, false, false), "the lost-notification sweep fallback must still hold");
    }

    #[test]
    fn gate_fires_on_watchdog_stall_alone() {
        assert!(idle_tick_gate(false, false, true, false));
    }

    #[test]
    fn gate_fires_on_fallback_due_alone() {
        assert!(idle_tick_gate(false, false, false, true));
    }

    #[test]
    fn gate_skips_when_nothing_holds() {
        assert!(!idle_tick_gate(false, false, false, false));
    }

    // ---------- idle_tick_fallback_due ----------

    #[test]
    fn fallback_due_is_a_strict_elapsed_check() {
        let fallback_minutes = 180;
        let window_ms = fallback_minutes as u64 * 60_000;
        assert!(!idle_tick_fallback_due(1_000, 1_000 + window_ms - 1, fallback_minutes));
        assert!(idle_tick_fallback_due(1_000, 1_000 + window_ms, fallback_minutes));
    }

    #[test]
    fn fallback_due_tolerates_clock_skew() {
        assert!(!idle_tick_fallback_due(10_000, 1_000, 180), "now before last_fired must read as not-yet-due, never a giant interval");
    }

    // ---------- due_intake_polls ----------

    #[test]
    fn due_intake_polls_skips_a_group_with_polling_off() {
        let mut groups = HashMap::new();
        groups.insert(gid("g1"), 0u32);
        assert!(due_intake_polls(1_000_000, &groups, &HashMap::new()).is_empty());
    }

    #[test]
    fn due_intake_polls_never_polled_is_immediately_due() {
        let mut groups = HashMap::new();
        groups.insert(gid("g1"), 5u32);
        assert_eq!(due_intake_polls(1_000_000, &groups, &HashMap::new()), vec!["g1".to_string()]);
    }

    #[test]
    fn due_intake_polls_respects_the_per_group_interval() {
        let mut groups = HashMap::new();
        groups.insert(gid("g1"), 5u32);
        let mut last = HashMap::new();
        last.insert(gid("g1"), 1_000u64);
        assert!(due_intake_polls(1_000 + 4 * 60_000, &groups, &last).is_empty(), "under 5 min must not be due yet");
        assert_eq!(due_intake_polls(1_000 + 5 * 60_000, &groups, &last), vec!["g1".to_string()]);
    }

    // ---------- due_intake_polls: the per-scan cap (#656) ----------

    /// Every group due at once (the case this cap exists for: N autonomous
    /// groups falling due on the same scan wake) must still cost at most
    /// `MAX_INTAKE_POLLS_PER_TICK` groups' worth of `gh` — the intake half
    /// runs inside the one loop that also delivers every watch notice.
    #[test]
    fn due_intake_polls_caps_one_scan_however_many_are_due() {
        let mut groups = HashMap::new();
        for i in 0..(MAX_INTAKE_POLLS_PER_TICK + 6) {
            groups.insert(gid(&format!("g{i:02}")), 5u32);
        }
        let due = due_intake_polls(1_000_000, &groups, &HashMap::new());
        assert_eq!(due.len(), MAX_INTAKE_POLLS_PER_TICK, "an uncapped scan is 2 gh calls per due group");
        let distinct: HashSet<&GroupId> = due.iter().collect();
        assert_eq!(distinct.len(), due.len(), "the cap must select distinct groups, not repeat one");
    }

    /// The cap is a deferral, not a drop: whoever the cap left out is the
    /// oldest-polled on the next scan, so it is taken first there. Simulated
    /// by stamping exactly what `poll_intake` stamps — the same `now`, only
    /// for the groups the scan actually returned.
    #[test]
    fn due_intake_polls_defers_the_overflow_to_the_next_scan_instead_of_starving_it() {
        let mut groups = HashMap::new();
        for i in 0..(MAX_INTAKE_POLLS_PER_TICK * 2) {
            groups.insert(gid(&format!("g{i:02}")), 5u32);
        }
        let now = 1_000_000u64;
        let mut last = HashMap::new();

        let first = due_intake_polls(now, &groups, &last);
        for g in &first {
            last.insert(g.clone(), now);
        }
        // Same instant, so nothing new has become due — but the first scan's
        // groups are now inside their per-group floor and the deferred ones
        // are not, which is what makes the next scan pick up exactly them.
        let second = due_intake_polls(now, &groups, &last);
        assert_eq!(second.len(), MAX_INTAKE_POLLS_PER_TICK, "the deferred groups must be due on the very next scan");
        let overlap: Vec<&GroupId> = second.iter().filter(|g| first.contains(g)).collect();
        assert!(overlap.is_empty(), "a group already polled this floor must not be re-polled ahead of a deferred one: {overlap:?}");

        let covered: HashSet<&GroupId> = first.iter().chain(second.iter()).collect();
        assert_eq!(covered.len(), groups.len(), "two scans must cover every due group — the cap defers, it never starves");
    }

    /// Oldest-polled first, so the cap rotates. A plain truncation of
    /// `HashMap` iteration order would cut at an arbitrary but *stable* point
    /// and starve the same groups every scan; this is the ordering that turns
    /// the cap into a round-robin.
    #[test]
    fn due_intake_polls_takes_the_oldest_polled_first() {
        let mut groups = HashMap::new();
        let mut last = HashMap::new();
        // Staggered stamps, all far enough back to be due: g00 is the
        // stalest, g09 the freshest.
        for i in 0..10u64 {
            groups.insert(gid(&format!("g{i:02}")), 5u32);
            last.insert(gid(&format!("g{i:02}")), 1_000 + i * 1_000);
        }
        let due = due_intake_polls(1_000 + 60 * 60_000, &groups, &last);
        let expected: Vec<String> = (0..MAX_INTAKE_POLLS_PER_TICK).map(|i| format!("g{i:02}")).collect();
        assert_eq!(due, expected, "the cap must take the stalest groups, in staleness order");
    }

    /// Never-polled groups all share `last = 0`, so without a tiebreak the
    /// selection among them would be `HashMap` iteration order — a different
    /// four each run, and (with a fixed seed) possibly the same four forever.
    /// The group name breaks the tie so one due set always yields one answer.
    #[test]
    fn due_intake_polls_is_deterministic_among_never_polled_groups() {
        let mut groups = HashMap::new();
        for i in 0..(MAX_INTAKE_POLLS_PER_TICK + 6) {
            groups.insert(gid(&format!("g{i:02}")), 5u32);
        }
        let first = due_intake_polls(1_000_000, &groups, &HashMap::new());
        // Rebuilding the map changes its iteration order in general; the
        // selection must not move with it.
        let rebuilt: HashMap<GroupId, u32> = groups.iter().map(|(k, v)| (k.clone(), *v)).collect();
        assert_eq!(due_intake_polls(1_000_000, &rebuilt, &HashMap::new()), first);
        let expected: Vec<String> = (0..MAX_INTAKE_POLLS_PER_TICK).map(|i| format!("g{i:02}")).collect();
        assert_eq!(first, expected, "ties break on the group id, so the answer is stated, not hash-dependent");
    }

    // ---------- effective_intake_poll_minutes (#429 smart default) ----------

    #[test]
    fn unset_config_smart_defaults_on_while_autonomous() {
        assert_eq!(effective_intake_poll_minutes(None, true), super::super::DEFAULT_INTAKE_POLL_MINUTES,
            "the dead-default trap (autonomous ON, gate silently off) must be structurally \
             impossible without an explicit opt-out");
    }

    #[test]
    fn unset_config_is_inert_while_supervised() {
        assert_eq!(effective_intake_poll_minutes(None, false), 0,
            "a supervised group never idle-ticks, so there is nothing for the poller to feed");
    }

    #[test]
    fn explicit_zero_is_deliberate_opt_out_even_while_autonomous() {
        assert_eq!(effective_intake_poll_minutes(Some(0), true), 0,
            "an explicit opt-out must never be overridden by the smart default");
        assert_eq!(effective_intake_poll_minutes(Some(0), false), 0);
    }

    #[test]
    fn explicit_nonzero_is_honored_over_the_smart_default_in_both_directions() {
        assert_eq!(effective_intake_poll_minutes(Some(45), true), 45,
            "a human-set cadence is never second-guessed while autonomous");
        assert_eq!(effective_intake_poll_minutes(Some(45), false), 45,
            "an explicit value is honored even while supervised — only the smart DEFAULT is \
             autonomous-gated, not a value someone actually set");
    }

    // ---------- PendingIntake (rev-33 finding B2: bounded accumulation) ----------

    #[test]
    fn empty_pending_renders_empty() {
        assert!(PendingIntake::default().is_empty());
        assert_eq!(PendingIntake::default().render(), "");
    }

    #[test]
    fn pushing_an_empty_summary_is_a_no_op() {
        let mut p = PendingIntake::default();
        p.push(String::new());
        assert!(p.is_empty(), "an empty poll finding nothing new must never create a block");
    }

    #[test]
    fn a_single_push_renders_verbatim_with_no_dropped_marker() {
        let mut p = PendingIntake::default();
        p.push("issue #1 labeled agent-ready (\"Do X\")".to_string());
        assert_eq!(p.render(), "issue #1 labeled agent-ready (\"Do X\")");
    }

    #[test]
    fn multiple_pushes_within_the_cap_join_in_order_with_no_dropped_marker() {
        let mut p = PendingIntake::default();
        p.push("a".to_string());
        p.push("b".to_string());
        p.push("c".to_string());
        assert_eq!(p.render(), "a; b; c");
    }

    #[test]
    fn pushes_beyond_the_cap_drop_the_oldest_and_state_the_count() {
        // MAX_PENDING_INTAKE_BLOCKS + 3 pushes: the three oldest must be gone,
        // the newest MAX must survive, and the render must say what it dropped
        // — never a silent shrink, mirroring intake_wake_summary's own "+N more".
        let mut p = PendingIntake::default();
        for i in 0..MAX_PENDING_INTAKE_BLOCKS + 3 {
            p.push(format!("block{i}"));
        }
        let rendered = p.render();
        assert!(rendered.starts_with("(+3 earlier finding(s) dropped"), "got: {rendered}");
        assert!(!rendered.contains("block0"), "the oldest must actually be gone: {rendered}");
        assert!(!rendered.contains("block2"), "got: {rendered}");
        assert!(rendered.contains("block3"), "the oldest SURVIVING block must still be present: {rendered}");
        assert!(rendered.contains(&format!("block{}", MAX_PENDING_INTAKE_BLOCKS + 2)),
            "the newest block must always survive: {rendered}");
    }

    #[test]
    fn accumulation_is_bounded_regardless_of_how_many_polls_ever_pushed() {
        // The rev-33 finding directly: an output-active group can push hundreds
        // of poll findings before anything ever consumes the pending state. The
        // rendered text's length must be bounded by the block cap, not by how
        // many times `push` was called.
        let mut p = PendingIntake::default();
        for i in 0..500 {
            p.push(format!("issue #{i} labeled agent-ready (\"finding number {i}, a moderately long title to be realistic\")"));
        }
        let rendered = p.render();
        let one_block_upper_bound = 200; // generous per-block ceiling used in this test's own titles
        assert!(
            rendered.len() < one_block_upper_bound * (MAX_PENDING_INTAKE_BLOCKS + 1),
            "500 pushes must not translate into 500 blocks' worth of bytes: {} bytes",
            rendered.len()
        );
    }
}
