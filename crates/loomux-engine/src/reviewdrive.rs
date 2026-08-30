//! The engine-driven review-loop driver: the pure core (#1778 S1).
//!
//! Design note: `doc/design/review-driver.md`. That note is the spec, and this
//! module is the half of it that has no I/O in it at all — the state machine
//! (§2.1), the persisted shape (§5.2), the counters (§2.3) and the decision
//! `rd_driver_tick` makes once its facts are in hand (§2.4). The tick itself,
//! the `gh` reads behind those facts, the spawns, the notices and the audit
//! lines are S3's, in `src-tauri`; the MCP tools (§5.1) are S4's; the `driver:`
//! block (§5.3) is S2's, in `workflow.rs`.
//!
//! **Why the split is drawn here and not somewhere more convenient.** Every
//! decision this feature makes is a function of facts orrerix read a moment
//! earlier — the PR's head, its checks, a verdict file, a clock. Putting the
//! decision in the same function as the reads makes it testable only through a
//! fake `gh`, which is how a state machine ends up pinned by its plumbing. So
//! [`decide`] takes the facts as an argument, reads no clock, spawns no child
//! and touches no file, and the whole of §2.1's table is exercised by building
//! a [`DriveFacts`] and asserting a [`DriveStep`].
//!
//! # What this module is NOT
//!
//! It is not a second reader of the merge gate. §4 of the note is explicit that
//! a third implementation of the gate decision is a defect rather than an
//! optimization, so the gate's answer, the routed lane list and each lane's
//! verdict arrive here as **facts the existing parsers produced** —
//! [`ReviewVerdict`] itself is what a [`LaneFact`] carries, and the staleness
//! questions are asked with that type's own [`ReviewVerdict::reviewed`] and
//! [`ReviewVerdict::body_changed`] rather than with a comparison written here.
//!
//! # Where this module knowingly goes beyond the note
//!
//! Three persisted fields exist that §5.2's example entry does not show, and
//! they are called out here rather than left for a reader to notice as drift.
//! §2.2 bounds two waits — `held(lane-stalled)` is "no verdict inside
//! `lane_timeout_minutes`", `held(fix-stalled)` is "neither pushed nor reported
//! inside `fix_timeout_minutes`" — and the shape in §5.2 carries no timestamp
//! from which either could be measured, while §2.4 resumes a drive from disk
//! across a restart, so an in-memory clock cannot carry them either. A bound
//! with no anchor is not a bound. So:
//!
//! - [`LaneRecord::spawned_ms`] — when this lane's delegate was last spawned or
//!   resumed. The `lane-stalled` anchor.
//! - [`LaneRecord::briefed_head`] and [`LaneRecord::briefed_digest`] — the
//!   revision that lane was last briefed at, as **one key**, the same
//!   `(head, digest)` the gate binds a verdict to. §2.1 re-briefs a lane whose
//!   `pass` went stale, and without this the driver cannot tell a lane already
//!   re-opened at the live revision (wait for it) from one that still needs
//!   re-opening (brief it) — it would re-brief every tick. The head alone is
//!   not the key; [`lane_open_for`] carries why, and the defect it names is one
//!   this slice actually shipped and CI caught.
//! - [`DriveEntry::fix_handback_ms`] — when the drive last entered `fix-wait`.
//!   The `fix-stalled` anchor; the hand-back is the moment that wait began.
//!
//! `drive-stalled` needs none of these: §2.2 is emphatic that it is the drive's
//! **age**, `now - started_ms`, "never an idle clock reset by each state
//! advance", so it keeps §5.2's own `started_ms`.
//!
//! **That last field is named for what it anchors, and the name is the point.**
//! The obvious spelling — a general "when did the state last change" stamp,
//! written on every arc — is *precisely* the idle clock §2.2 forbids, with §8's
//! `also: [base-green]` row as the worked example of what it breaks. Leaving
//! one in the struct would put that clock one field-access away from any future
//! timeout, and the next implementer finds the field long before they find the
//! paragraph. So it is written on entry to `fix-wait` and nowhere else, and it
//! is a hazard closed by construction rather than by comment.
//!
//! # One seam here is not a contract
//!
//! [`decide`] and the fact types it consumes are a **slice author's seam**, not
//! a §-backed public contract: the note specifies the states, the arcs, the
//! counters and the file, and says nothing about how the tick's decision is
//! factored out of its reads. Do not go looking for the section — there isn't
//! one. What that means for a later change is that this shape may be reworked
//! on its own merits, where [`DriveState`], [`transition`] and
//! [`ReviewDrivesState`] may not: those three are the note's, and changing one
//! changes `doc/design/review-driver.md` first.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::workflow::{BlockId, ReviewVerdict, Verdict};

// ── §2.1 the states ─────────────────────────────────────────────────────────

/// Where one driven PR is (§2.1). **Four working states, one parked state, two
/// terminals** — seven, and the enum is closed.
///
/// **There is no `Unknown` variant and no catch-all arm**, exactly as
/// [`crate::mergeq::EntryState`] has none, and §2.1 makes that a prescription
/// rather than a style note. The reason is that §5.2 persists this as a
/// *string* while promising unknown *fields* are tolerated and preserved, and
/// the two promises pull opposite ways unless one governs. The refusal governs:
/// an unknown field is data some newer build added that this one can carry
/// across a read/write cycle without understanding, and preserving it loses
/// nothing; an unknown state is the entry's entire meaning, and every available
/// default is a guess that either resumes a drive somebody stopped or abandons
/// one still running. So a file naming a state this build does not know fails
/// to parse — §2.4's `rd-state-unreadable`, refuse the tick, back off, never
/// repair and never delete.
///
/// **`Held` is parked, not terminal, and the distinction is load-bearing.** The
/// queue's `KickedBack` *is* terminal, and a kicked-back PR comes back through
/// a fresh `queue_merge` as a NEW entry. A drive cannot copy that, because §2.3
/// carries the spent counters across a resume and a fresh entry would reset
/// them — the one thing INVARIANT 9's "yours count too" forbids. So `Held`
/// keeps its counters, has exactly two outgoing arcs (`drive_review` resumes
/// it, `cancel_review_drive` cancels it), and is never pruned (§5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DriveState {
    /// Waiting on the PR's checks and mergeability at the current head.
    CiWait,
    /// A reviewer lane is open; waiting for its verdict at (head, digest).
    ReviewWait,
    /// Handed back to the worker; waiting for a push or a report.
    FixWait,
    /// Re-reading the gate at the live head, the last thing before `satisfied`.
    GateCheck,
    /// **Parked**, carrying a [`HeldReason`]. The tick does not advance it; the
    /// two tools do.
    Held,
    /// Terminal: the gate is satisfied at the live head.
    Satisfied,
    /// Terminal: cancelled, or reconcile positively established the PR is
    /// closed or merged.
    Cancelled,
}

impl DriveState {
    /// Every state, in §2.1's table order. Exists so a caller — or the test
    /// that walks all forty-nine `(from, to)` pairs — can enumerate the machine
    /// without matching on the enum, which is what lets that test fail when an
    /// eighth state is added rather than silently keep checking seven.
    pub const ALL: [DriveState; 7] = [
        DriveState::CiWait,
        DriveState::ReviewWait,
        DriveState::FixWait,
        DriveState::GateCheck,
        DriveState::Held,
        DriveState::Satisfied,
        DriveState::Cancelled,
    ];

    /// The wire/audit spelling — the same string serde writes.
    pub fn as_str(self) -> &'static str {
        match self {
            DriveState::CiWait => "ci-wait",
            DriveState::ReviewWait => "review-wait",
            DriveState::FixWait => "fix-wait",
            DriveState::GateCheck => "gate-check",
            DriveState::Held => "held",
            DriveState::Satisfied => "satisfied",
            DriveState::Cancelled => "cancelled",
        }
    }

    /// Parse a state word. `None` for anything unrecognized — never coerced,
    /// the same "reject, never guess" posture [`Verdict::parse`] and
    /// [`crate::mergeq::EntryState::parse`] take; the reason is on the enum.
    pub fn parse(s: &str) -> Option<DriveState> {
        match s.trim() {
            "ci-wait" => Some(DriveState::CiWait),
            "review-wait" => Some(DriveState::ReviewWait),
            "fix-wait" => Some(DriveState::FixWait),
            "gate-check" => Some(DriveState::GateCheck),
            "held" => Some(DriveState::Held),
            "satisfied" => Some(DriveState::Satisfied),
            "cancelled" => Some(DriveState::Cancelled),
            _ => None,
        }
    }

    /// `satisfied` / `cancelled`, and **only** those two (§2.1). A terminal
    /// state has no outgoing transition at all, and terminal entries are what
    /// §5.2's retention prunes.
    pub fn is_terminal(self) -> bool {
        matches!(self, DriveState::Satisfied | DriveState::Cancelled)
    }

    /// Parked (§2.1). Not terminal, not live: the tick leaves it alone, its
    /// counters are preserved, and it is **never pruned** — pruning one would
    /// silently grant three fresh review rounds (§5.2).
    pub fn is_parked(self) -> bool {
        matches!(self, DriveState::Held)
    }

    /// The working and gate states — the scope of §5.1's `already-driven`
    /// decline, spelled once here because that is the whole of its definition:
    /// "`already-driven` covers the working and `gate-check` states only". A
    /// `held` entry is parked and §2.3 calls resuming it the default, so a flat
    /// `already-driven` would make that path unreachable and `reset_counters` a
    /// parameter nothing can pass.
    pub fn is_live(self) -> bool {
        !self.is_terminal() && !self.is_parked()
    }
}

/// Why a drive is parked (§2.2). **One state carrying a closed reason enum, not
/// twelve states**, so a reader asking "is this drive parked" asks one
/// question, and the reason travels in the notice and the audit line rather
/// than being inferred from which counter happens to sit at its bound.
///
/// Twelve reasons. With `satisfied` and `cancelled` that is §2.2's fourteen
/// exits back to the LLM orchestrator, and [`HeldReason::ALL`] is what makes
/// that count checkable rather than asserted.
///
/// Closed for [`DriveState`]'s reason: a hold whose reason this build cannot
/// read is a hold it cannot explain, and the notice is the entire product of a
/// hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HeldReason {
    /// A lane recorded `escalate`.
    Escalate,
    /// `review_rounds` reached its bound.
    ReviewLimit,
    /// `ci_attempts` reached its bound.
    CiLimit,
    /// A second conflict after the one rebase hand-back.
    RebaseLimit,
    /// A spawned or resumed **reviewer** lane recorded no verdict inside
    /// `lane_timeout_minutes`.
    LaneStalled,
    /// A resumed **worker** neither pushed nor reported inside
    /// `fix_timeout_minutes`.
    FixStalled,
    /// The drive's **age** passed `drive_timeout_minutes`.
    DriveStalled,
    /// `route_reviewers` returned `None` — the changed-file list could not be
    /// shown complete, so *which reviewers are required* is unknown. Never a
    /// guess: guessing "no rule fired" is guessing in favour of merging.
    RoutingUnaccountable,
    /// The gate file is present and could not be read (an I/O error). **Not**
    /// `gate-not-configured`, which means the file is genuinely absent.
    GateUnreadable,
    /// The worker reported `blocked`.
    WorkerBlocked,
    /// The recorded worker session no longer resolves.
    WorkerUnresumable,
    /// A driven delegate called `message_orchestrator` (§7 — that call is never
    /// intercepted; the delegate's own line arrives by its own path and this
    /// hold is the routing fact beside it).
    Messaged,
}

impl HeldReason {
    /// Every reason, so a caller — or a test counting §2.2's exits — can
    /// enumerate them without matching on the enum. Order is §2.2's table.
    pub const ALL: [HeldReason; 12] = [
        HeldReason::Escalate,
        HeldReason::ReviewLimit,
        HeldReason::CiLimit,
        HeldReason::RebaseLimit,
        HeldReason::LaneStalled,
        HeldReason::FixStalled,
        HeldReason::DriveStalled,
        HeldReason::RoutingUnaccountable,
        HeldReason::GateUnreadable,
        HeldReason::WorkerBlocked,
        HeldReason::WorkerUnresumable,
        HeldReason::Messaged,
    ];

    /// The wire/audit spelling — the same string serde writes, and the detail
    /// `rd-held` carries (§5.4).
    pub fn as_str(self) -> &'static str {
        match self {
            HeldReason::Escalate => "escalate",
            HeldReason::ReviewLimit => "review-limit",
            HeldReason::CiLimit => "ci-limit",
            HeldReason::RebaseLimit => "rebase-limit",
            HeldReason::LaneStalled => "lane-stalled",
            HeldReason::FixStalled => "fix-stalled",
            HeldReason::DriveStalled => "drive-stalled",
            HeldReason::RoutingUnaccountable => "routing-unaccountable",
            HeldReason::GateUnreadable => "gate-unreadable",
            HeldReason::WorkerBlocked => "worker-blocked",
            HeldReason::WorkerUnresumable => "worker-unresumable",
            HeldReason::Messaged => "messaged",
        }
    }

    /// Parse a hold reason. `None` for anything unrecognized.
    pub fn parse(s: &str) -> Option<HeldReason> {
        match s.trim() {
            "escalate" => Some(HeldReason::Escalate),
            "review-limit" => Some(HeldReason::ReviewLimit),
            "ci-limit" => Some(HeldReason::CiLimit),
            "rebase-limit" => Some(HeldReason::RebaseLimit),
            "lane-stalled" => Some(HeldReason::LaneStalled),
            "fix-stalled" => Some(HeldReason::FixStalled),
            "drive-stalled" => Some(HeldReason::DriveStalled),
            "routing-unaccountable" => Some(HeldReason::RoutingUnaccountable),
            "gate-unreadable" => Some(HeldReason::GateUnreadable),
            "worker-blocked" => Some(HeldReason::WorkerBlocked),
            "worker-unresumable" => Some(HeldReason::WorkerUnresumable),
            "messaged" => Some(HeldReason::Messaged),
            _ => None,
        }
    }
}

/// A transition the state machine refuses. Carries both ends so the audit event
/// and the notice can say what was actually attempted (§5.4 — an audit action
/// must name what happened).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: DriveState,
    pub to: DriveState,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} -> {} is not a legal review-drive transition",
            self.from.as_str(),
            self.to.as_str()
        )
    }
}

/// **The state machine.** §2.1's thirteen arcs, each with the section that asks
/// for it. Anything else is refused — including a self-transition, which is not
/// a transition: advancing to lane *k+1* leaves the entry in `review-wait` and
/// writes the lane index, exactly as refreshing a `blocked_reason` leaves a
/// queue entry `queued`.
///
/// **Enumerated, and a pair this does not name is a refusal**, copying
/// [`crate::mergeq::transition`], which matches explicit pairs and falls
/// through to `Err`. §2.1 gives the reason: a state machine whose §8 needs an
/// arc §2 never named does not fail as a documentation gap, it fails at
/// runtime, on the degradation path, where nothing is watching.
///
/// Thirteen arc *rows* are nineteen legal `(from, to)` pairs, because two rows
/// are written over a set of froms — arc 12 (`-> held`) over the four working
/// and gate states, arc 13 (`-> cancelled`) over all five non-terminals. Both
/// are spelled here as explicit variant lists rather than as a predicate like
/// `!from.is_terminal()`: the two spellings coincide today, and the enumerated
/// one is what makes an eighth state a compile-time decision instead of
/// something a predicate quietly absorbs.
///
/// **Arc 1 is not here.** `(none) -> ci-wait` is a *creation* — `drive_review`
/// on a PR with no live entry (§5.1) — and it has no `from`. It is
/// [`DriveEntry::new`], the way the queue's own arc-from-nothing is
/// `QueueEntry::new`.
///
/// **Nor is a per-reason restriction on arc 12,** and that is a decision rather
/// than an omission. §2.1's per-state "Leaves for" column names which
/// [`HeldReason`]s each state reaches, but it omits `messaged` from every cell
/// while §2.2 and arc 12 require it from any working state (§7: a driven
/// delegate can call `message_orchestrator` at any point in the loop). Read as
/// a second, finer table it would therefore refuse a hold the note mandates. So
/// the arc list governs — §2.1 says it is "a table of its own", listed in full
/// precisely so nothing is inferred from the prose — and the "Leaves for"
/// column is read as the summary it is. [`decide`] is where each reason is
/// actually produced, and it produces them per state.
pub fn transition(from: DriveState, to: DriveState) -> Result<DriveState, InvalidTransition> {
    use DriveState::*;
    let ok = match (from, to) {
        // 2. Checks green (§2.1).
        (CiWait, ReviewWait) => true,
        // 3. Checks red, or CONFLICTING (§2.1, §8).
        (CiWait, FixWait) => true,
        // 4. The last required lane passed at (head, digest).
        (ReviewWait, GateCheck) => true,
        // 5. A lane recorded `fail` (§2.1).
        (ReviewWait, FixWait) => true,
        // 6. The head moved under a lane mid-review (§8 row 4). The verdict
        //    that lands binds to the old head; a `fail` there still routes, a
        //    `pass` there is stale and the lane is re-briefed after CI.
        (ReviewWait, CiWait) => true,
        // 7. The worker pushed (§2.1).
        (FixWait, CiWait) => true,
        // 8. `report(done)` with the head unchanged — a body-only fix; it
        //    re-enters at the first stale lane (§8 row 5, [`first_stale_lane`]).
        (FixWait, ReviewWait) => true,
        // 9. `evaluate_merge_gate` satisfied at the live head.
        (GateCheck, Satisfied) => true,
        // 10. NOT satisfied, for ANY reason — a stale pass, an unsatisfied
        //     `also:` condition, a push that landed under the check (§8, the
        //     body-changed and `also: [base-green]` rows). Deliberately wider
        //     than "stale": a drive parked on a red default branch is not
        //     staleness, and an arc named only for staleness would refuse it.
        (GateCheck, CiWait) => true,
        // 11. `drive_review` resumes a parked drive (§2.3).
        (Held, CiWait) => true,
        // 12. A counter bound, a lane/fix/drive timeout, an unaccountable
        //     route, an unreadable gate, a blocked or unresumable worker, an
        //     escalate, or a delegate's `message_orchestrator` (§2.2). From the
        //     four working and gate states — NOT from `held` itself, whose two
        //     outgoing arcs §2.1 enumerates, and not from a terminal.
        (CiWait | ReviewWait | FixWait | GateCheck, Held) => true,
        // 13. `cancel_review_drive`, or reconcile positively established the PR
        //     is closed or merged (§8). From any non-terminal, `held` included:
        //     cancelling is a parked drive's second way out.
        (CiWait | ReviewWait | FixWait | GateCheck | Held, Cancelled) => true,
        _ => false,
    };
    if ok {
        Ok(to)
    } else {
        Err(InvalidTransition { from, to })
    }
}

// ── §2.3 the counters, and the bounds they run against ──────────────────────
//
// INVARIANT 9 in `templates/orchestrator.md` reads: *three CI attempts, three
// rounds of review findings (yours count too), one rebase attempt, one
// architectural bounce.* The driver takes three of those four and deliberately
// leaves the fourth alone — an architectural bounce is INVARIANT 4 judgment,
// and §3 says the driver never makes one.

/// The ceiling INVARIANT 9 sets on `max_review_rounds` and `max_ci_attempts`,
/// and the top of §5.3's `1..=3` clamp.
pub const MAX_ROUNDS_CEILING: u32 = 3;
/// The ceiling on `max_rebase_attempts`, and the top of §5.3's `0..=1` clamp.
pub const MAX_REBASE_CEILING: u32 = 1;

/// The bounds one drive runs against — the value type [`decide`] consumes.
///
/// **This is not a second parser of the `driver:` block.** §5.3's block is
/// S2's, in `workflow.rs`, and that is where a malformed block goes loudly down
/// the `workflow-invalid` path. What lives here is the *value* the pure core is
/// handed, with §5.3's own ranges enforced by [`DriveLimits::clamped`] so the
/// decision function cannot be given a bound outside INVARIANT 9 whatever a
/// caller did on the way in.
///
/// **The clamp only ever tightens.** §2.3: a repo may run a *tighter* loop than
/// the orchestrator template promises; it may not run a looser one, because the
/// driver acts on the orchestrator's authority and a repo file that raised the
/// bound would be loosening the orchestrator's own invariant from a
/// configuration file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveLimits {
    pub max_review_rounds: u32,
    pub max_ci_attempts: u32,
    pub max_rebase_attempts: u32,
    pub lane_timeout_minutes: u64,
    pub fix_timeout_minutes: u64,
    pub drive_timeout_minutes: u64,
}

impl Default for DriveLimits {
    /// §5.3's defaults.
    fn default() -> Self {
        DriveLimits {
            max_review_rounds: 3,
            max_ci_attempts: 3,
            max_rebase_attempts: 1,
            lane_timeout_minutes: 60,
            fix_timeout_minutes: 60,
            drive_timeout_minutes: 240,
        }
    }
}

impl DriveLimits {
    /// Every counter bound brought inside §5.3's range. `1..=3` for the two
    /// round counters, `0..=1` for rebases — note the asymmetric floor: zero
    /// review rounds would be a drive that parks on the first `fail` having
    /// handed nothing back, while zero rebase attempts is a coherent policy
    /// (park on the first conflict, §2.2's `rebase-limit`).
    pub fn clamped(self) -> DriveLimits {
        DriveLimits {
            max_review_rounds: self.max_review_rounds.clamp(1, MAX_ROUNDS_CEILING),
            max_ci_attempts: self.max_ci_attempts.clamp(1, MAX_ROUNDS_CEILING),
            max_rebase_attempts: self.max_rebase_attempts.min(MAX_REBASE_CEILING),
            ..self
        }
    }
}

/// What a drive has spent (§5.2's `counters`).
///
/// **The comparison against a bound is check-before-bump**, and that is a
/// decision with evidence rather than a coin flip, because the two orderings
/// differ by a whole round. [`counter_exhausted`] is where it is spelled;
/// §2.2's `rebase-limit` row is what decides it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    #[serde(default)]
    pub review_rounds: u32,
    #[serde(default)]
    pub ci_attempts: u32,
    #[serde(default)]
    pub rebase_attempts: u32,
    /// Preserved unknown fields — see [`ReviewDrivesState`].
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Counters {
    /// Counters seeded by `drive_review`'s `rounds_already_spent` (§2.3).
    ///
    /// "Yours count too" is a property of the **budget**, not of who spends it.
    /// An orchestrator that reviews by hand once, gets a `fail`, and *then*
    /// calls `drive_review` would otherwise start every counter at zero and
    /// spend three more, for five against an invariant of three. Clamped
    /// `0..=3` exactly as §2.3 specifies, so the seed cannot exceed the budget
    /// it is spending from.
    pub fn seeded(rounds_already_spent: u32) -> Counters {
        Counters {
            review_rounds: rounds_already_spent.min(MAX_ROUNDS_CEILING),
            ..Counters::default()
        }
    }
}

/// Which counter a step spends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    /// Spent on a lane's `fail` (§2.1's `review-wait` row).
    ReviewRounds,
    /// Spent on a red CI observation (§2.1's `ci-wait` row).
    CiAttempts,
    /// Spent on a CONFLICTING mergeability (§2.1's `ci-wait` row).
    RebaseAttempts,
}

/// Whether `spent` has reached `bound` — **checked before the bump, never
/// after**, which is the whole of the ordering decision and is worth its own
/// function so both callers and both tests name the same thing.
///
/// The two orderings are not equivalent, they differ by one hand-back, and
/// §2.2's `rebase-limit` row settles it: *"a second conflict after the one
/// rebase hand-back"*. At `max_rebase_attempts = 1`, check-before-bump gives
/// exactly that — the first conflict finds `0 < 1`, bumps to 1 and hands back
/// (the one rebase hand-back); the second finds `1 >= 1` and parks. Bump-then-
/// check would park on the *first* conflict, having handed nothing back, which
/// is not what that row describes.
///
/// It is also the only ordering under which §2.3's `rounds_already_spent`
/// honours "yours count too" rather than overshooting the other way: seeded at
/// 2 of 3 it leaves exactly one driven round, where bump-then-check leaves
/// none at all and makes the parameter a way to disable the drive.
pub fn counter_exhausted(spent: u32, bound: u32) -> bool {
    spent >= bound
}

// ── §5.2 `<group-dir>/review_drives.json` ───────────────────────────────────
//
// Forward compatibility here is the merge queue's, and the OPPOSITE of
// `.orrerix/workflow.yml`'s — §5.2 flags the asymmetry itself, because the two
// persisted surfaces this design adds take opposite postures and a reader will
// otherwise infer one from the other: **policy fails loud, state degrades
// gracefully.** `workflow.yml` is human-authored policy, so a key this build
// does not understand means a human believes a policy is in force that is not.
// `review_drives.json` is machine-authored state, and an older build must be
// able to read it and rewrite it without destroying what a newer one wrote.
//
// "Tolerated" is not enough on its own: serde ignores unknown fields by
// default, and an ignored field is *lost* on the next write. So every type here
// carries a flattened `extra` map, which makes the round trip **preserving**
// rather than merely non-fatal — the property
// `review_drives_round_trip_preserves_unknown_fields` pins.
//
// The one thing that is NOT tolerated is an unknown **state** string, and
// [`DriveState`] carries that argument.

/// Schema version of `review_drives.json`.
pub const REVIEW_DRIVES_VERSION: u32 = 1;
/// The file's name inside the group dir. It sits beside `state.json`,
/// `tasks.json` and `merge_queue.json`; the group dir itself is built by
/// `group_dir_at`, the only place a group id becomes a path.
pub const REVIEW_DRIVES_FILE: &str = "review_drives.json";

/// The whole of `review_drives.json` (§5.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewDrivesState {
    /// Schema version. **Required** — a state file with no version is
    /// malformed, not a v1 file, and §2.4's reconcile refuses such a file
    /// loudly rather than guessing at it.
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<DriveEntry>,
    /// Fields written by a newer build, preserved verbatim across a read/write
    /// cycle. See the section comment above this type.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for ReviewDrivesState {
    fn default() -> Self {
        ReviewDrivesState {
            version: REVIEW_DRIVES_VERSION,
            entries: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl ReviewDrivesState {
    /// Whether this build understands the file's schema.
    ///
    /// The conservative half of forward compatibility: unknown *fields* are
    /// preserved, but a file whose whole schema moved is one this build must
    /// **not act on** — the fields it recognizes may no longer mean what it
    /// thinks. Refuse to operate and leave the file alone, which is the only
    /// way "an older build does not destroy what a newer one wrote" survives a
    /// version bump that changes meanings rather than adding keys.
    pub fn version_supported(&self) -> bool {
        self.version == REVIEW_DRIVES_VERSION
    }

    /// The entry for a PR, if this file has one at all.
    pub fn entry(&self, pr: u64) -> Option<&DriveEntry> {
        self.entries.iter().find(|e| e.pr == pr)
    }

    /// The entry for a PR, mutably.
    pub fn entry_mut(&mut self, pr: u64) -> Option<&mut DriveEntry> {
        self.entries.iter_mut().find(|e| e.pr == pr)
    }

    /// Whether this PR is **live** in §5.1's `already-driven` sense — a working
    /// or `gate-check` entry. A parked entry is deliberately not live: §2.3
    /// calls resuming one the default.
    pub fn is_driven(&self, pr: u64) -> bool {
        self.entry(pr).is_some_and(|e| e.state().is_live())
    }
}

/// One reviewer lane of one drive (§5.2's `lanes`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaneRecord {
    /// The reviewer **block** id the gate names (`rev-std`).
    pub block: BlockId,
    /// The session the driver spawned or resumed for this lane. The **full**
    /// resolved id, never a prefix, for §3.2's reason: a prefix that resolves
    /// uniquely today can become ambiguous tomorrow as the roster grows.
    #[serde(default)]
    pub session: String,
    /// The last verdict seen for this lane — a **record of what was read**,
    /// never a gate input. The live verdict file is re-read every tick, so
    /// nothing decides from this field; it is what `review_drive_status()`
    /// shows.
    ///
    /// Routed through [`Verdict::parse`] on the way in rather than kept as a
    /// raw string, the way `GroupId`'s `Deserialize` re-validates a persisted
    /// id. The alternative stores something that *looks* like a verdict and was
    /// never parsed, which is an invitation to a later `== "pass"` — and §4 is
    /// explicit that the driver is a reader of the gate, never a third
    /// implementation of it.
    #[serde(default, deserialize_with = "de_opt_verdict")]
    pub last_verdict: Option<Verdict>,
    /// The head that **verdict** bound to. Empty until this lane has answered
    /// at least once.
    #[serde(default)]
    pub at_head: String,
    /// The head this lane was last **briefed** at. Beyond §5.2's example — see
    /// the module header for why it has to exist.
    ///
    /// **Not the same question as [`at_head`](LaneRecord::at_head), and the two
    /// must never be conflated into one field.** `at_head` is the head the last
    /// verdict binds to; this is the head the lane was last asked about. A
    /// freshly spawned lane has this and not `at_head`, and that gap is exactly
    /// the call `review-wait` has to make on every tick: a lane already open at
    /// the live head is one to *wait* for, a lane whose brief predates the live
    /// head is one to *re-open*. One field answering both would make "has it
    /// been asked" and "has it answered" indistinguishable, so the driver would
    /// either re-brief a lane on every tick or wait forever on one it never
    /// briefed.
    #[serde(default)]
    pub briefed_head: String,
    /// The body digest this lane was last briefed at; empty when the body could
    /// not be read at brief time. Beyond §5.2's example, with `briefed_head` —
    /// and it travels with it, because **the two are one key**.
    ///
    /// That key is the same `(head, digest)` the gate binds a verdict to — arc
    /// 4 is "the last required lane passed at (head, digest)" — and a lane is
    /// open for exactly the revision it was asked about. The head **alone** is
    /// not that key, and the difference is not cosmetic: a lane that already
    /// answered `pass` at this head, whose body then moved, is indistinguishable
    /// under a head-only comparison from one still thinking about this head.
    /// §8's body-changed row wants the first re-briefed with a body-only delta
    /// and the second waited for, so a head-only key waits on a reviewer that
    /// has already spoken — forever, or until `lane-stalled` reports a stall
    /// that never happened. See [`lane_open_for`].
    #[serde(default)]
    pub briefed_digest: String,
    /// When this lane's delegate was last spawned or resumed — the
    /// `lane-stalled` anchor. Beyond §5.2's example; see the module header.
    #[serde(default)]
    pub spawned_ms: u64,
    /// Preserved unknown fields — see [`ReviewDrivesState`].
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A persisted verdict word, re-validated on the way in.
///
/// An absent or `null` field is `None`; a word [`Verdict::parse`] does not know
/// is an **error**, not a `None`. Silently reading it as "no verdict yet" would
/// make the driver re-open a lane that had already answered.
fn de_opt_verdict<'de, D>(d: D) -> Result<Option<Verdict>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;
    match Option::<String>::deserialize(d)? {
        None => Ok(None),
        Some(s) => Verdict::parse(&s)
            .map(Some)
            .ok_or_else(|| D::Error::custom(format!("unknown verdict {s:?}"))),
    }
}

/// One driven PR (§5.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriveEntry {
    pub pr: u64,
    /// Private on purpose: [`advance`](DriveEntry::advance) is the only
    /// sanctioned way an entry's state changes, so the enumeration in
    /// [`transition`] cannot be bypassed by a caller outside this module
    /// assigning a state directly. Deserialization is the one other writer, and
    /// that is resuming a persisted state rather than transitioning to one.
    state: DriveState,
    /// Set exactly when `state` is [`DriveState::Held`] — maintained by
    /// [`advance`](DriveEntry::advance), which clears it on every other arc so
    /// a resumed drive cannot carry a stale reason into `review_drive_status()`.
    #[serde(default)]
    pub held_reason: Option<HeldReason>,
    /// The PR head this entry was last resolved against. A record of what was
    /// seen; every decision re-reads the live head (§2.4 resumes "against the
    /// **live** head, never against the head the file remembers").
    #[serde(default)]
    pub head: String,
    /// The PR body digest last seen, the same #565 digest the gate reads.
    #[serde(default)]
    pub body_digest: String,
    /// The worker session, **as resolved** by `resolve_session_ref` at
    /// `drive_review` time and never the caller's raw string (§3.2).
    #[serde(default)]
    pub worker_session: String,
    /// The orchestrator this drive acts for. Every action taken under it is
    /// audited with this as the `on_behalf_of` detail key — the actor stays
    /// `brand::AUDIT_ACTOR`, so it is this key, not the actor, that
    /// distinguishes a driver action from any other host action (§3).
    #[serde(default)]
    pub on_behalf_of: String,
    #[serde(default)]
    pub lanes: Vec<LaneRecord>,
    /// Which lane of the gate's required list is open. Advancing it is **not**
    /// a transition (§2.1) — the entry stays in `review-wait`.
    #[serde(default)]
    pub lane_index: usize,
    /// What this drive has spent of INVARIANT 9's budget (§2.3).
    ///
    /// **Required, with no `serde(default)`, for [`DriveState`]'s reason.** An
    /// absent counter block is not a v1 file with nothing spent: defaulting it
    /// to zeros silently grants three fresh review rounds, which is precisely
    /// the outcome §5.2 forbids when it refuses to prune a parked entry. The
    /// conservative direction here is to refuse the file, not to guess low.
    pub counters: Counters,
    /// When the drive began, absolute. The `drive-stalled` anchor: §2.2 derives
    /// an **age** from it (`now - started_ms`), never an idle clock reset by
    /// each state advance. A stored *age* would be stale the instant it was
    /// written and meaningless across a restart.
    #[serde(default)]
    pub started_ms: u64,
    /// When this drive last entered `fix-wait` — the `fix-stalled` anchor, and
    /// the only clock here that is not the drive's age. Beyond §5.2's example;
    /// see the module header, including why it is not the general
    /// "state last changed" stamp it looks like it wants to be.
    ///
    /// Written by [`advance`](DriveEntry::advance) on arcs 3 and 5 — the two
    /// that reach `fix-wait` — and by nothing else. Zero on a drive that has
    /// never been handed back, which is why `fix-stalled` is only ever asked in
    /// `fix-wait`.
    #[serde(default)]
    pub fix_handback_ms: u64,
    /// Preserved unknown fields — see [`ReviewDrivesState`].
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl DriveEntry {
    /// **Arc 1**: `(none) -> ci-wait`, `drive_review` on a PR with no live
    /// entry (§5.1). A creation rather than a transition, which is why
    /// [`transition`] has no arm for it.
    pub fn new(
        pr: u64,
        worker_session: &str,
        on_behalf_of: &str,
        counters: Counters,
        now_ms: u64,
    ) -> DriveEntry {
        DriveEntry {
            pr,
            state: DriveState::CiWait,
            held_reason: None,
            head: String::new(),
            body_digest: String::new(),
            worker_session: worker_session.to_string(),
            on_behalf_of: on_behalf_of.to_string(),
            lanes: Vec::new(),
            lane_index: 0,
            counters,
            started_ms: now_ms,
            fix_handback_ms: 0,
            extra: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> DriveState {
        self.state
    }

    /// Move this entry to `to`, or refuse. The **only** mutation path for
    /// [`DriveEntry::state`]; see that field's comment.
    ///
    /// `reason` must be `Some` exactly when `to` is [`DriveState::Held`] — a
    /// hold with no reason has nothing to put in its notice or its `rd-held`
    /// line, and a reason on any other arc would survive into
    /// `review_drive_status()` as a claim about a drive that is not parked.
    /// Both are refused as [`InvalidTransition`] rather than silently fixed.
    ///
    /// [`fix_handback_ms`](DriveEntry::fix_handback_ms) is stamped **only** on
    /// an arc into `fix-wait`, never on every arc: a stamp written on each
    /// advance is the idle clock §2.2's `drive-stalled` row forbids, and the
    /// module header argues why that must be impossible to reach rather than
    /// merely discouraged.
    pub fn advance(
        &mut self,
        to: DriveState,
        reason: Option<HeldReason>,
        now_ms: u64,
    ) -> Result<(), InvalidTransition> {
        self.state = transition(self.state, to)?;
        self.held_reason = reason;
        if to == DriveState::FixWait {
            self.fix_handback_ms = now_ms;
        }
        Ok(())
    }

    /// The lane record for a block, if this drive has opened one.
    pub fn lane(&self, block: &str) -> Option<&LaneRecord> {
        self.lanes.iter().find(|l| l.block == block)
    }

    /// Record that lane `block` was briefed at `(head, body_digest)` — a spawn
    /// or a resume, which are the same event to both bounds that read this.
    /// Replaces any prior record for that block, so a re-brief re-arms
    /// `lane-stalled` instead of measuring from the first spawn.
    ///
    /// **The digest is taken here and not derived**, so that what the lane was
    /// asked about and what [`lane_open_for`] later compares are the same fact
    /// recorded once. An unreadable body records empty, which that comparison
    /// reads as "cannot tell" rather than as drift.
    pub fn open_lane(
        &mut self,
        block: &str,
        session: &str,
        head: &str,
        body_digest: Option<&str>,
        now_ms: u64,
    ) {
        let extra = self
            .lane(block)
            .map(|l| l.extra.clone())
            .unwrap_or_default();
        self.lanes.retain(|l| l.block != block);
        self.lanes.push(LaneRecord {
            block: block.to_string(),
            session: session.to_string(),
            last_verdict: None,
            at_head: String::new(),
            briefed_head: head.to_string(),
            briefed_digest: body_digest.unwrap_or_default().to_string(),
            spawned_ms: now_ms,
            extra,
        });
    }

    /// The drive's age (§2.2's `drive-stalled` measure).
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.started_ms)
    }
}

/// Why `review_drives.json` could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateError {
    /// The file is there and does not parse — torn, hand-edited, or naming a
    /// state this build does not know. §2.4: the tick refuses, audits
    /// `rd-state-unreadable` and backs off; it never repairs and never deletes.
    Malformed(String),
    /// A schema this build does not understand. Do not operate; do not write.
    Unsupported(u32),
    /// The file is there and could not be read at all.
    Io(String),
}

/// The group's review-drive file.
pub fn state_path(group_dir: &Path) -> PathBuf {
    group_dir.join(REVIEW_DRIVES_FILE)
}

/// Read the group's drive state.
///
/// **An absent file is no drives, not an error** — that is the product default
/// (§5.3: no `driver:` block, nothing ever driven). Every other failure is a
/// [`StateError`], because the difference between "nothing is driven" and
/// "orrerix cannot tell what is driven" is exactly what §5.1 gives the tools
/// their own `rd-state-unreadable` for: answering `not-driven` over a torn file
/// asserts something orrerix cannot know, while a drive may well be live.
pub fn load_state(group_dir: &Path) -> Result<ReviewDrivesState, StateError> {
    let text = match std::fs::read_to_string(state_path(group_dir)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReviewDrivesState::default())
        }
        Err(e) => return Err(StateError::Io(e.to_string())),
    };
    parse_state(&text)
}

/// The parse half of [`load_state`], without the file — so the refusals can be
/// pinned on a string.
pub fn parse_state(text: &str) -> Result<ReviewDrivesState, StateError> {
    let state: ReviewDrivesState =
        serde_json::from_str(text).map_err(|e| StateError::Malformed(e.to_string()))?;
    if !state.version_supported() {
        return Err(StateError::Unsupported(state.version));
    }
    Ok(state)
}

/// Write the drive state atomically, reusing [`crate::fsatomic::atomic_write`]
/// — the #133-hardened writer (same-directory temp, `sync_all` before the
/// rename, a fallback that keeps the temp on failure).
///
/// Deliberately not a fresh `fs::write`: a disk-full `fs::write` is what
/// truncated `tasks.json` and destroyed a live board in #133, and this file has
/// the same "losing it loses in-flight work" property — worse, since §2.3's
/// counters are the only record of how much of INVARIANT 9's budget a drive has
/// spent. One hardened writer, not two.
pub fn store_state(group_dir: &Path, state: &ReviewDrivesState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    crate::fsatomic::atomic_write(&state_path(group_dir), &bytes).map_err(|e| e.to_string())
}

/// §5.2's retention: **drop terminal entries, keep parked ones.** Returns the
/// PRs dropped, so the caller can audit `rd-pruned` per entry.
///
/// Two reasons that are not merely hygiene. Unpruned terminal entries would
/// flow through `review_drive_status()` into the orchestrator's resident
/// context, which is the cost this whole feature exists to remove; and they
/// would make §5.1's `already-driven` refuse every re-drive of a PR forever.
///
/// **`held` entries are never pruned**, and that is the whole reason §2.1 makes
/// `held` parked rather than terminal: §2.3's resume needs their counters, and
/// pruning one would silently grant three fresh review rounds. A parked drive
/// leaves this file by being resumed to completion or cancelled, never by
/// retention.
///
/// The caller owes one ordering obligation this function cannot enforce: §5.2
/// prunes a terminal entry *once its notice has been delivered*, so the tick
/// delivers first and prunes after.
pub fn prune_terminal(state: &mut ReviewDrivesState) -> Vec<u64> {
    let pruned: Vec<u64> = state
        .entries
        .iter()
        .filter(|e| e.state().is_terminal())
        .map(|e| e.pr)
        .collect();
    state.entries.retain(|e| !e.state().is_terminal());
    pruned
}

// ── §2.4 the decision, over injected facts ──────────────────────────────────

/// What one observation of a driven PR's checks and mergeability concluded.
///
/// A closed vocabulary rather than `mqdriver`'s own result types, and that is
/// the seam: S3 maps `mqdriver::resolve_pr_detailed` /
/// `notify::pr_mergeability_result` and `mqdriver::pr_ci_green_detailed` /
/// `notify::pr_checks_result` onto these five, and this module never learns
/// what a `gh` invocation looks like.
///
/// Note what is **not** here: no "assume green", and no arm that folds an
/// unanswered lookup in with an answered one. §8 is emphatic on that — a
/// rate-limited `gh` returns *promptly* with a non-zero exit, so it is not a
/// runner failure, but `BaseUnverifiable` is still an **unknown** rather than a
/// fact about the PR, and "unknown is never treated as safe".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CiObservation {
    /// Checks are still running, or none has reported yet — which
    /// `notify::pr_checks_result` already reads as pending rather than green.
    Pending,
    /// Every required check terminal, none failing.
    Green,
    /// Terminal with failures.
    Red,
    /// GitHub reports the PR `CONFLICTING`: there is no clean merge ref, so no
    /// check suite will ever exist for it. Never discoverable by waiting.
    Conflicting,
    /// orrerix could not tell. Back off; no transition, no notice, bounded by
    /// `drive_timeout_minutes`.
    Unknown,
}

/// What the driven worker did since the hand-back (§2.1's `fix-wait` row).
///
/// Sourced from the **intercepted** `report` (§7), which is keyed on the
/// calling agent and never on a `ref` string a delegate typed — a delegate that
/// could choose whether its report reaches the orchestrator by naming a PR
/// number is a delegate that can route around the orchestrator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerSignal {
    /// Nothing yet.
    Silent,
    /// `report(done)`.
    Done,
    /// `report(blocked)`.
    Blocked,
    /// The recorded worker session no longer resolves.
    ///
    /// **This is not a drive-time check, and the note is honest about why.**
    /// §5.1: a full, well-shaped session id this group never recorded takes
    /// `resolve_session_ref`'s `is_full_session_id` passthrough arm and is
    /// *accepted* by `drive_review`, so its unresumability surfaces here, at
    /// the first hand-back, possibly hours on. Resolving is not the same as
    /// proving resumable, and v1 does not prove it.
    Unresumable,
}

/// What re-reading the gate concluded (§2.1's `gate-check` row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateOutcome {
    /// The tick did not evaluate the gate. The gate is read in `gate-check` and
    /// nowhere else, so every other state passes this.
    NotEvaluated,
    /// `evaluate_merge_gate` satisfied at the live head, including every
    /// declared `also:` condition.
    Satisfied,
    /// Not satisfied, for any reason.
    Unsatisfied,
    /// The gate file is present and could not be read (an I/O error). **Not**
    /// `gate-not-configured`, which means the file is genuinely absent and is a
    /// drive-time decline (§5.1) rather than a hold.
    Unreadable,
}

/// One lane's live reading: the block the gate named, and the verdict file as
/// `workflow::parse_verdict_file` returned it (or `None` for no verdict yet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneFact {
    pub block: BlockId,
    pub verdict: Option<ReviewVerdict>,
}

/// Everything [`decide`] is allowed to know. Read by the tick, immediately
/// before the call; nothing in here is read from the entry, and nothing is
/// fetched inside the decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveFacts {
    /// The clock, injected. [`decide`] never reads one.
    pub now_ms: u64,
    /// Whether the PR is open. `Some(false)` **only on a positive answer**:
    /// `mqloop::draft_pr_open` returns `None` for a lookup it could not
    /// complete, and its doc says reconcile treats that as "the world does not
    /// match", never as "probably fine". Cancelling a live drive on a rate
    /// limit that clears in minutes is the failure this distinction stops.
    pub pr_open: Option<bool>,
    /// The PR's live head.
    pub head: String,
    /// The PR body's digest as it stands, or `None` when the body could not be
    /// read — the same fact `body_drift` takes in rather than fetching (#791).
    pub body_digest: Option<String>,
    /// The lanes the gate requires **at this head**, in
    /// `RoutingDecision::required`'s order. `None` is `route_reviewers`
    /// returning `None`: the changed-file list could not be shown complete, so
    /// which reviewers are required is unknown.
    pub required_lanes: Option<Vec<LaneFact>>,
    pub ci: CiObservation,
    pub worker: WorkerSignal,
    pub gate: GateOutcome,
    /// A driven delegate called `message_orchestrator` since the last tick
    /// (§7). Its own line was delivered unchanged, by its own arm; this is the
    /// routing fact beside it.
    pub messaged: bool,
}

/// What the tick should do with one entry, this tick.
///
/// **At most one state advance per entry per tick** (§2.4) is a property of
/// this type, not a discipline the caller has to remember: there is exactly one
/// advancing variant and it names one arc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveStep {
    /// Nothing to do. The tick backs off per §2.4's principle — back off after
    /// any tick whose next attempt would make the same external calls and reach
    /// the same answer.
    Wait,
    /// Stay in `review-wait` and brief lane `index` — a spawn, or a resume of
    /// the lane's recorded session. **Not a transition** (§2.1): advancing to
    /// lane *k+1* leaves the entry where it is and writes the lane index, which
    /// is why the table has no `review-wait -> review-wait` arm.
    OpenLane { index: usize },
    /// Take one arc, spending `bump` first when there is one.
    Advance {
        to: DriveState,
        held_reason: Option<HeldReason>,
        bump: Option<Counter>,
    },
}

impl DriveStep {
    fn to(to: DriveState) -> DriveStep {
        DriveStep::Advance { to, held_reason: None, bump: None }
    }

    fn held(reason: HeldReason) -> DriveStep {
        DriveStep::Advance {
            to: DriveState::Held,
            held_reason: Some(reason),
            bump: None,
        }
    }

    fn spend(to: DriveState, bump: Counter) -> DriveStep {
        DriveStep::Advance { to, held_reason: None, bump: Some(bump) }
    }
}

/// Whether a lane's recorded verdict is a `pass` that still counts **at this
/// (head, digest)** — §2.1's first carried-over property: *"a `pass` bound to
/// an old head, or to an old body digest, is not a pass."*
///
/// Asked with [`ReviewVerdict`]'s own methods rather than with comparisons
/// written here, because §4 makes the driver a reader of the gate and never a
/// third implementation of it. Both halves of the #565 asymmetry come along for
/// free that way: [`ReviewVerdict::reviewed`] is false for an empty head, so an
/// unbound verdict reads as stale and fails closed; and
/// [`ReviewVerdict::body_changed`] answers `None` when the drift cannot be
/// *known* — a verdict with no digest, or a body that could not be read — which
/// is not `Some(true)` and so does not stale the pass. "We could not check" and
/// "it changed" are different answers, and only one of them may re-open a lane.
pub fn lane_pass_is_current(
    verdict: Option<&ReviewVerdict>,
    head: &str,
    body_digest: Option<&str>,
) -> bool {
    let Some(v) = verdict else { return false };
    v.verdict == Verdict::Pass
        && v.reviewed(head)
        && v.body_changed(body_digest) != Some(true)
}

/// Whether this lane is open for exactly the revision now on the PR: it was
/// briefed at this head **and** at this body digest.
///
/// **Both signals, read by one rule, in one place.** The failure this exists to
/// prevent is the repo's "a guard reads every one of its inputs by one rule"
/// class, and it is not hypothetical — the head-only version of this comparison
/// shipped in the first push of this slice and was caught by
/// `a_pass_whose_body_digest_moved_re_opens_that_lane`. A lane that already
/// answered `pass` at this head and whose body then moved reads, under a
/// head-only key, exactly like a lane still thinking: the driver waits on a
/// reviewer that has already spoken, and `lane-stalled` eventually reports a
/// stall that never happened. §8's body-changed row wants that lane re-briefed
/// with a body-only delta.
///
/// **An unknown live digest does not mismatch**, and neither does an unrecorded
/// briefed one. "We could not check" is not "it changed" — the same asymmetry
/// [`ReviewVerdict::body_changed`] encodes by answering `None` rather than
/// `Some(false)` — and the alternative is that one transient `gh` failure to
/// read a PR body re-briefs every open lane in the group.
pub fn lane_open_for(rec: &LaneRecord, head: &str, body_digest: Option<&str>) -> bool {
    if rec.briefed_head != head {
        return false;
    }
    match body_digest {
        Some(now) if !now.is_empty() && !rec.briefed_digest.is_empty() => {
            rec.briefed_digest == now
        }
        _ => true,
    }
}

/// The first lane whose `pass` does not stand at this (head, digest) — where
/// arc 8 re-enters after a body-only fix (§8 row 5), and equally where
/// `review-wait` resumes when a digest moves under a recorded pass.
///
/// Returns `required.len()` when every lane's pass stands, which is the
/// "nothing left to review" answer arc 4 acts on.
pub fn first_stale_lane(required: &[LaneFact], head: &str, body_digest: Option<&str>) -> usize {
    required
        .iter()
        .position(|l| !lane_pass_is_current(l.verdict.as_ref(), head, body_digest))
        .unwrap_or(required.len())
}

/// **The decision.** One entry, one tick, one step — pure, and that purity is
/// what makes S3's integration testable: no I/O, no `gh`, no clock read, no
/// file. Everything it knows is in `facts`.
///
/// # The order the conditions are asked in
///
/// Precedence is a decision the note does not spell out arc by arc, so it is
/// argued here rather than left to the reading order of a `match`:
///
/// 1. **A terminal or parked entry yields [`DriveStep::Wait`] immediately.**
///    §2.1's `held` row reads "nothing; the tick does not advance it" — a
///    parked drive is left for `drive_review` or `cancel_review_drive`, and if
///    the tick could move one, `held` would not be a park.
/// 2. **A positively-closed PR cancels**, before anything else can hold it.
///    §8: `cancelled`, and only on a positive answer. A PR that is gone has no
///    hold worth reporting, and `cancelled` says more than `drive-stalled`.
/// 3. **`messaged` holds next.** The delegate's own words are already in the
///    orchestrator's pane (§7 — `message_orchestrator` is never intercepted),
///    so this hold is the routing fact that explains them. Holding for anything
///    else here would leave that message beside a hold that does not account
///    for it.
/// 4. **The drive's age holds next, and it must outrank the per-state logic**
///    — this is the one ordering §8 forces rather than merely permits. Its
///    `also: [base-green]` row parks a drive on a red default branch by cycling
///    `gate-check` -> `ci-wait` on every wake; that drive *always* has an
///    advance available, so an age check that ran after the per-state logic
///    would never be reached and the drive would cycle forever. The bound is
///    what keeps stopping the line from being silent.
/// 5. Then the state's own logic.
pub fn decide(entry: &DriveEntry, facts: &DriveFacts, limits: &DriveLimits) -> DriveStep {
    let state = entry.state();
    if state.is_terminal() || state.is_parked() {
        return DriveStep::Wait;
    }
    if facts.pr_open == Some(false) {
        return DriveStep::to(DriveState::Cancelled);
    }
    if facts.messaged {
        return DriveStep::held(HeldReason::Messaged);
    }
    if entry.age_ms(facts.now_ms) >= minutes_ms(limits.drive_timeout_minutes) {
        return DriveStep::held(HeldReason::DriveStalled);
    }
    match state {
        DriveState::CiWait => decide_ci_wait(entry, facts, limits),
        DriveState::ReviewWait => decide_review_wait(entry, facts, limits),
        DriveState::FixWait => decide_fix_wait(entry, facts, limits),
        DriveState::GateCheck => decide_gate_check(facts),
        // Both returned above; repeated here because the enum is closed and a
        // catch-all arm is exactly what §2.1 forbids.
        DriveState::Held | DriveState::Satisfied | DriveState::Cancelled => DriveStep::Wait,
    }
}

fn decide_ci_wait(entry: &DriveEntry, facts: &DriveFacts, limits: &DriveLimits) -> DriveStep {
    match facts.ci {
        // Arc 2.
        CiObservation::Green => DriveStep::to(DriveState::ReviewWait),
        // Arc 3, spending a CI attempt — or parking, when the budget is gone.
        CiObservation::Red => {
            if counter_exhausted(entry.counters.ci_attempts, limits.max_ci_attempts) {
                DriveStep::held(HeldReason::CiLimit)
            } else {
                DriveStep::spend(DriveState::FixWait, Counter::CiAttempts)
            }
        }
        // Arc 3 again: a conflict is a hand-back for a rebase, on its own
        // counter. §2.2's `rebase-limit` is "a second conflict after the one
        // rebase hand-back", which is [`counter_exhausted`]'s ordering.
        CiObservation::Conflicting => {
            if counter_exhausted(entry.counters.rebase_attempts, limits.max_rebase_attempts) {
                DriveStep::held(HeldReason::RebaseLimit)
            } else {
                DriveStep::spend(DriveState::FixWait, Counter::RebaseAttempts)
            }
        }
        // Neither an answer nor a reason to move. §8: unknown is never treated
        // as safe, and it is never treated as a fact about the PR either.
        CiObservation::Pending | CiObservation::Unknown => DriveStep::Wait,
    }
}

fn decide_review_wait(entry: &DriveEntry, facts: &DriveFacts, limits: &DriveLimits) -> DriveStep {
    let Some(required) = facts.required_lanes.as_deref() else {
        return DriveStep::held(HeldReason::RoutingUnaccountable);
    };
    // Arc 6: the head moved under a lane. Checked before the verdicts are read,
    // because a verdict that lands at the old head is answered by the binding
    // rules, not by this state — a `fail` there still routes when the drive
    // comes back through `review-wait`, and a `pass` there is already stale.
    if !facts.head.is_empty() && entry.head != facts.head {
        return DriveStep::to(DriveState::CiWait);
    }
    let digest = facts.body_digest.as_deref();
    // Arc 4's precondition, and equally the re-entry point when a digest moved
    // under a recorded pass (§8's body-changed row): the first lane whose pass
    // does not stand here.
    let k = first_stale_lane(required, &facts.head, digest);
    if k >= required.len() {
        return DriveStep::to(DriveState::GateCheck);
    }
    let lane = &required[k];
    match lane.verdict.as_ref().map(|v| v.verdict) {
        // A lane recorded `escalate`: an LLM judgment call, and §3 says the
        // driver never makes one.
        Some(Verdict::Escalate) => DriveStep::held(HeldReason::Escalate),
        // Arc 5, spending a review round — or parking, when the budget is gone.
        Some(Verdict::Fail) => {
            if counter_exhausted(entry.counters.review_rounds, limits.max_review_rounds) {
                DriveStep::held(HeldReason::ReviewLimit)
            } else {
                DriveStep::spend(DriveState::FixWait, Counter::ReviewRounds)
            }
        }
        // Either no verdict yet, or a `pass` that no longer stands here — which
        // are the same thing to this state: the lane is outstanding and must be
        // briefed for this revision. Whether it *already* was is what
        // [`lane_open_for`] answers, on the full (head, digest) key.
        Some(Verdict::Pass) | None => {
            match entry.lane(&lane.block) {
                Some(rec) if lane_open_for(rec, &facts.head, digest) => {
                    if facts.now_ms.saturating_sub(rec.spawned_ms)
                        >= minutes_ms(limits.lane_timeout_minutes)
                    {
                        DriveStep::held(HeldReason::LaneStalled)
                    } else {
                        DriveStep::Wait
                    }
                }
                _ => DriveStep::OpenLane { index: k },
            }
        }
    }
}

fn decide_fix_wait(entry: &DriveEntry, facts: &DriveFacts, limits: &DriveLimits) -> DriveStep {
    match facts.worker {
        // Nothing to hand back to. Checked first: every other arm here presumes
        // a worker that can be reached.
        WorkerSignal::Unresumable => return DriveStep::held(HeldReason::WorkerUnresumable),
        // INVARIANT 3 territory — a blocked worker is the orchestrator's call.
        WorkerSignal::Blocked => return DriveStep::held(HeldReason::WorkerBlocked),
        WorkerSignal::Done | WorkerSignal::Silent => {}
    }
    // Arc 7: the worker pushed. Outranks `report(done)` deliberately — if both
    // happened, the code moved and CI is what has to answer next.
    if !facts.head.is_empty() && entry.head != facts.head {
        return DriveStep::to(DriveState::CiWait);
    }
    // Arc 8: `report(done)` with the head unchanged — a body-only fix.
    if facts.worker == WorkerSignal::Done {
        return DriveStep::to(DriveState::ReviewWait);
    }
    if facts.now_ms.saturating_sub(entry.fix_handback_ms) >= minutes_ms(limits.fix_timeout_minutes)
    {
        return DriveStep::held(HeldReason::FixStalled);
    }
    DriveStep::Wait
}

fn decide_gate_check(facts: &DriveFacts) -> DriveStep {
    // §4: `route_reviewers` returning `None` is `held(routing-unaccountable)`
    // from every state that reads it, **`gate-check` included**. This is the
    // one degradation whose absence would be a security defect rather than an
    // inconvenience: the unknown thing is *which reviewers are required*, so
    // guessing "no rule fired" is guessing in favour of merging, and the gate
    // would answer allowed on a reviewer list nobody could compute — §3.1's
    // "a bypass with better telemetry".
    if facts.required_lanes.is_none() {
        return DriveStep::held(HeldReason::RoutingUnaccountable);
    }
    match facts.gate {
        GateOutcome::Unreadable => DriveStep::held(HeldReason::GateUnreadable),
        // Arc 9.
        GateOutcome::Satisfied => DriveStep::to(DriveState::Satisfied),
        // Arc 10, which is deliberately wider than "stale".
        GateOutcome::Unsatisfied => DriveStep::to(DriveState::CiWait),
        // The tick reached `gate-check` without evaluating the gate. Nothing is
        // known, so nothing moves — and in particular this is not `satisfied`.
        GateOutcome::NotEvaluated => DriveStep::Wait,
    }
}

fn minutes_ms(minutes: u64) -> u64 {
    minutes.saturating_mul(60_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── §2.1 the closed state enum ──────────────────────────────────────────

    #[test]
    fn the_state_enum_is_the_notes_seven_states() {
        // §2.1: "Four working states, one parked state, and two terminals."
        assert_eq!(DriveState::ALL.len(), 7);
        let live: Vec<&str> = DriveState::ALL
            .iter()
            .filter(|s| s.is_live())
            .map(|s| s.as_str())
            .collect();
        assert_eq!(live, vec!["ci-wait", "review-wait", "fix-wait", "gate-check"]);
        let parked: Vec<&str> = DriveState::ALL
            .iter()
            .filter(|s| s.is_parked())
            .map(|s| s.as_str())
            .collect();
        assert_eq!(parked, vec!["held"]);
        let terminal: Vec<&str> = DriveState::ALL
            .iter()
            .filter(|s| s.is_terminal())
            .map(|s| s.as_str())
            .collect();
        // The load-bearing half: `held` is NOT in this list. Pruning a parked
        // entry would silently grant three fresh review rounds (§5.2).
        assert_eq!(terminal, vec!["satisfied", "cancelled"]);
    }

    #[test]
    fn every_state_round_trips_through_as_str_and_parse() {
        for s in DriveState::ALL {
            assert_eq!(DriveState::parse(s.as_str()), Some(s), "{}", s.as_str());
        }
    }

    #[test]
    fn as_str_is_the_same_spelling_serde_writes() {
        // Two separate code paths: §5.2 persists the serde one while §5.4
        // audits the `as_str` one. Drift between them would write a file this
        // build's own `parse` refuses.
        for s in DriveState::ALL {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
            let back: DriveState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
        for r in HeldReason::ALL {
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(json, format!("\"{}\"", r.as_str()));
            let back: HeldReason = serde_json::from_str(&json).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn an_unknown_state_word_is_refused_never_coerced() {
        // There is no fallback variant to coerce it to, which is the point:
        // every available default either resumes a drive somebody stopped or
        // abandons one still running (§2.1).
        for bad in ["", "  ", "HELD", "landed", "queued", "unknown", "ci_wait"] {
            assert_eq!(DriveState::parse(bad), None, "{bad:?} must not parse");
        }
        // ...while a real one still does, so the loop above is not vacuous.
        assert_eq!(DriveState::parse(" held "), Some(DriveState::Held));
    }

    #[test]
    fn the_held_reasons_are_the_notes_twelve() {
        assert_eq!(HeldReason::ALL.len(), 12);
        // §2.2: "There are **fourteen**" exits back to the LLM orchestrator —
        // the twelve holds plus `satisfied` and `cancelled`.
        let exits =
            HeldReason::ALL.len() + DriveState::ALL.iter().filter(|s| s.is_terminal()).count();
        assert_eq!(exits, 14);
        for r in HeldReason::ALL {
            assert_eq!(HeldReason::parse(r.as_str()), Some(r), "{}", r.as_str());
        }
        for bad in ["", "held", "stalled", "REVIEW-LIMIT", "review_limit", "messages"] {
            assert_eq!(HeldReason::parse(bad), None, "{bad:?} must not parse");
        }
    }

    // ── §2.1 the enumerated transition table ────────────────────────────────

    /// The nineteen legal `(from, to)` pairs §2.1's thirteen arc rows name.
    /// Written out as data so the test below can assert the machine's legal set
    /// is *exactly* this: "get all 13, and add none the note does not name" is
    /// two claims, and a table of only-the-legal-ones would check one.
    fn expected_legal_pairs() -> Vec<(DriveState, DriveState)> {
        use DriveState::*;
        vec![
            (CiWait, ReviewWait),     // 2
            (CiWait, FixWait),        // 3
            (ReviewWait, GateCheck),  // 4
            (ReviewWait, FixWait),    // 5
            (ReviewWait, CiWait),     // 6
            (FixWait, CiWait),        // 7
            (FixWait, ReviewWait),    // 8
            (GateCheck, Satisfied),   // 9
            (GateCheck, CiWait),      // 10
            (Held, CiWait),           // 11
            (CiWait, Held),           // 12, over the four working/gate states
            (ReviewWait, Held),       // 12
            (FixWait, Held),          // 12
            (GateCheck, Held),        // 12
            (CiWait, Cancelled),      // 13, over all five non-terminals
            (ReviewWait, Cancelled),  // 13
            (FixWait, Cancelled),     // 13
            (GateCheck, Cancelled),   // 13
            (Held, Cancelled),        // 13
        ]
    }

    #[test]
    fn the_transition_table_is_exactly_the_notes_arcs() {
        let expected = expected_legal_pairs();
        assert_eq!(expected.len(), 19, "13 arc rows are 19 (from, to) pairs");
        let mut actual = Vec::new();
        for from in DriveState::ALL {
            for to in DriveState::ALL {
                if transition(from, to).is_ok() {
                    actual.push((from, to));
                }
            }
        }
        // Both directions, so neither a missing arc nor an invented one passes.
        for pair in &expected {
            assert!(
                actual.contains(pair),
                "{} -> {} is a note arc and was refused",
                pair.0.as_str(),
                pair.1.as_str()
            );
        }
        for pair in &actual {
            assert!(
                expected.contains(pair),
                "{} -> {} is legal and the note names no such arc",
                pair.0.as_str(),
                pair.1.as_str()
            );
        }
        assert_eq!(actual.len(), 19);
    }

    #[test]
    fn a_pair_the_table_does_not_name_is_refused_not_defaulted() {
        use DriveState::*;
        // A sample of the thirty refusals, each chosen because a fallthrough
        // implementation would silently accept it.
        for (from, to) in [
            (CiWait, GateCheck),     // skipping review entirely
            (CiWait, Satisfied),     // green CI is not a satisfied gate
            (ReviewWait, Satisfied), // a lane pass is not the gate's answer
            (FixWait, GateCheck),
            (FixWait, Satisfied),
            (Held, Held), // §2.1: `held` has exactly two outgoing arcs
            (Held, ReviewWait),
            (Held, FixWait),
            (Held, GateCheck),
            (Held, Satisfied),
            (Satisfied, CiWait), // terminal
            (Cancelled, CiWait),
            (Satisfied, Cancelled),
            (Cancelled, Satisfied),
        ] {
            let err = transition(from, to).unwrap_err();
            assert_eq!(err, InvalidTransition { from, to });
        }
    }

    #[test]
    fn a_self_transition_is_not_a_transition() {
        // Advancing to lane k+1 leaves the entry in `review-wait` and writes
        // the lane index (§2.1), which is why there is no review-wait arm.
        for s in DriveState::ALL {
            assert!(transition(s, s).is_err(), "{} -> itself", s.as_str());
        }
    }

    #[test]
    fn held_is_parked_with_exactly_two_ways_out() {
        let out: Vec<&str> = DriveState::ALL
            .iter()
            .filter(|to| transition(DriveState::Held, **to).is_ok())
            .map(|to| to.as_str())
            .collect();
        assert_eq!(out, vec!["ci-wait", "cancelled"]);
    }

    #[test]
    fn a_terminal_state_has_no_outgoing_arc_at_all() {
        for from in DriveState::ALL.iter().filter(|s| s.is_terminal()) {
            for to in DriveState::ALL {
                assert!(
                    transition(*from, to).is_err(),
                    "{} -> {}",
                    from.as_str(),
                    to.as_str()
                );
            }
        }
    }

    #[test]
    fn an_invalid_transition_names_both_ends() {
        let err = transition(DriveState::Held, DriveState::Satisfied).unwrap_err();
        assert_eq!(
            err.to_string(),
            "held -> satisfied is not a legal review-drive transition"
        );
    }

    // ── the entry's own guards ──────────────────────────────────────────────

    fn entry_at(state: DriveState) -> DriveEntry {
        let mut e = DriveEntry::new(1758, "sess-full", "orch-1", Counters::default(), 1_000);
        // Walk there through legal arcs, so a fixture cannot encode a state the
        // machine refuses to reach.
        match state {
            DriveState::CiWait => {}
            DriveState::ReviewWait => {
                e.advance(DriveState::ReviewWait, None, 1_000).unwrap();
            }
            DriveState::FixWait => {
                e.advance(DriveState::FixWait, None, 1_000).unwrap();
            }
            DriveState::GateCheck => {
                e.advance(DriveState::ReviewWait, None, 1_000).unwrap();
                e.advance(DriveState::GateCheck, None, 1_000).unwrap();
            }
            DriveState::Held => {
                e.advance(DriveState::Held, Some(HeldReason::CiLimit), 1_000)
                    .unwrap();
            }
            DriveState::Satisfied => {
                e.advance(DriveState::ReviewWait, None, 1_000).unwrap();
                e.advance(DriveState::GateCheck, None, 1_000).unwrap();
                e.advance(DriveState::Satisfied, None, 1_000).unwrap();
            }
            DriveState::Cancelled => {
                e.advance(DriveState::Cancelled, None, 1_000).unwrap();
            }
        }
        assert_eq!(e.state(), state);
        e
    }

    #[test]
    fn a_hold_without_a_reason_and_a_reason_without_a_hold_are_both_refused() {
        let mut e = entry_at(DriveState::CiWait);
        // A hold with nothing to put in its notice or its `rd-held` line.
        assert!(e.advance(DriveState::Held, None, 2_000).is_err());
        // A reason riding an arc that is not a hold would survive into
        // `review_drive_status()` as a claim about a drive that is not parked.
        assert!(e
            .advance(DriveState::ReviewWait, Some(HeldReason::Escalate), 2_000)
            .is_err());
        // Neither attempt moved anything.
        assert_eq!(e.state(), DriveState::CiWait);
        assert_eq!(e.held_reason, None);
    }

    #[test]
    fn resuming_a_parked_drive_clears_the_reason_and_keeps_the_counters() {
        let mut e = entry_at(DriveState::CiWait);
        e.counters.review_rounds = 2;
        e.counters.ci_attempts = 3;
        e.advance(DriveState::Held, Some(HeldReason::CiLimit), 2_000)
            .unwrap();
        assert_eq!(e.held_reason, Some(HeldReason::CiLimit));
        // Arc 11 — `drive_review` resumes it. §2.3: the same counters, because
        // a fresh entry would reset them and "yours count too" forbids that.
        e.advance(DriveState::CiWait, None, 3_000).unwrap();
        assert_eq!(e.held_reason, None);
        assert_eq!(e.counters.review_rounds, 2);
        assert_eq!(e.counters.ci_attempts, 3);
    }

    #[test]
    fn the_handback_clock_is_stamped_on_fix_wait_arcs_and_nowhere_else() {
        // The whole point of the field's name: it must not become the idle
        // clock §2.2's `drive-stalled` row forbids.
        let mut e = entry_at(DriveState::CiWait);
        assert_eq!(e.fix_handback_ms, 0);
        e.advance(DriveState::ReviewWait, None, 5_000).unwrap();
        assert_eq!(e.fix_handback_ms, 0, "a non-fix-wait arc must not stamp it");
        e.advance(DriveState::FixWait, None, 7_000).unwrap();
        assert_eq!(e.fix_handback_ms, 7_000);
        e.advance(DriveState::CiWait, None, 9_000).unwrap();
        assert_eq!(e.fix_handback_ms, 7_000, "leaving fix-wait must not re-stamp");
        // ...and the age anchor is untouched by every one of those advances.
        assert_eq!(e.started_ms, 1_000);
        assert_eq!(e.age_ms(9_000), 8_000);
    }

    #[test]
    fn re_briefing_a_lane_re_arms_its_stall_clock() {
        let mut e = entry_at(DriveState::ReviewWait);
        e.open_lane("rev-std", "s1", "head-a", Some("d1"), 1_000);
        assert_eq!(e.lanes.len(), 1);
        e.open_lane("rev-std", "s1", "head-b", Some("d1"), 9_000);
        // Replaced, not appended: a second record would leave `lane()` reading
        // the first and measuring `lane-stalled` from the original spawn.
        assert_eq!(e.lanes.len(), 1);
        let rec = e.lane("rev-std").unwrap();
        assert_eq!(rec.spawned_ms, 9_000);
        assert_eq!(rec.briefed_head, "head-b");
        assert_eq!(rec.briefed_digest, "d1");
    }

    // ── §2.3 the counters ───────────────────────────────────────────────────

    #[test]
    fn the_bound_is_checked_before_the_bump_which_is_the_rebase_rows_arithmetic() {
        // §2.2's `rebase-limit` row is the decisive wording: "a second conflict
        // after the one rebase hand-back". At max_rebase_attempts = 1 that is
        // hand back once, park on the second — which only check-before-bump
        // produces. Bump-then-check parks on the FIRST conflict, having handed
        // nothing back, and no reading of that row describes it.
        let limits = DriveLimits::default();
        assert_eq!(limits.max_rebase_attempts, 1);
        assert!(!counter_exhausted(0, limits.max_rebase_attempts));
        assert!(counter_exhausted(1, limits.max_rebase_attempts));

        let mut e = entry_at(DriveState::CiWait);
        let facts = DriveFacts {
            ci: CiObservation::Conflicting,
            ..facts_at("head-a")
        };
        // First conflict: the one rebase hand-back.
        assert_eq!(
            decide(&e, &facts, &limits),
            DriveStep::Advance {
                to: DriveState::FixWait,
                held_reason: None,
                bump: Some(Counter::RebaseAttempts),
            }
        );
        e.counters.rebase_attempts += 1;
        // Second conflict: parked.
        assert_eq!(
            decide(&e, &facts, &limits),
            DriveStep::Advance {
                to: DriveState::Held,
                held_reason: Some(HeldReason::RebaseLimit),
                bump: None,
            }
        );
    }

    #[test]
    fn rounds_already_spent_leaves_the_rest_of_the_budget_not_none_of_it() {
        // §2.3: "yours count too" is a property of the budget, not of who
        // spends it. Seeded at 2 of 3, exactly one driven round remains — the
        // reading under which the parameter bounds the loop rather than
        // disabling it.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.counters = Counters::seeded(2);
        assert_eq!(e.counters.review_rounds, 2);
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Fail), "head-a", "d1")]),
            ..facts_at("head-a")
        };
        assert_eq!(
            decide(&e, &facts, &limits),
            DriveStep::Advance {
                to: DriveState::FixWait,
                held_reason: None,
                bump: Some(Counter::ReviewRounds),
            },
            "one round of the three is still unspent"
        );
        e.counters.review_rounds += 1;
        assert_eq!(
            decide(&e, &facts, &limits),
            DriveStep::Advance {
                to: DriveState::Held,
                held_reason: Some(HeldReason::ReviewLimit),
                bump: None,
            },
            "and the next fail parks at 3/3"
        );
    }

    #[test]
    fn the_seed_and_the_clamps_only_ever_tighten() {
        // §2.3: a repo may run a tighter loop than the orchestrator template
        // promises; it may not run a looser one.
        assert_eq!(Counters::seeded(9).review_rounds, 3);
        assert_eq!(Counters::seeded(0).review_rounds, 0);
        let loose = DriveLimits {
            max_review_rounds: 5,
            max_ci_attempts: 99,
            max_rebase_attempts: 4,
            ..DriveLimits::default()
        }
        .clamped();
        assert_eq!(loose.max_review_rounds, 3);
        assert_eq!(loose.max_ci_attempts, 3);
        assert_eq!(loose.max_rebase_attempts, 1);
        // A tighter repo policy survives untouched — the clamp is a ceiling,
        // not a substitution.
        let tight = DriveLimits {
            max_review_rounds: 1,
            max_ci_attempts: 2,
            max_rebase_attempts: 0,
            ..DriveLimits::default()
        }
        .clamped();
        assert_eq!(tight.max_review_rounds, 1);
        assert_eq!(tight.max_ci_attempts, 2);
        assert_eq!(tight.max_rebase_attempts, 0);
        // Zero review rounds would be a drive that parks having handed nothing
        // back, so that floor is 1 — the asymmetry with rebases is deliberate.
        let floored = DriveLimits {
            max_review_rounds: 0,
            ..DriveLimits::default()
        }
        .clamped();
        assert_eq!(floored.max_review_rounds, 1);
    }

    // ── §5.2 the state file ─────────────────────────────────────────────────

    /// §5.2's own example entry, verbatim in shape — the fields it shows and no
    /// others. It parses, which is what makes the note's documented shape a
    /// contract rather than an illustration, and the three fields this module
    /// adds are absent from it on purpose: each is `serde(default)`, so a file
    /// written against the published shape still reads.
    const NOTE_EXAMPLE: &str = r#"{
      "version": 1,
      "entries": [
        { "pr": 1758,
          "state": "review-wait",
          "held_reason": null,
          "head": "abc123",
          "body_digest": "3f1a",
          "worker_session": "cafb930d-0000-0000-0000-000000000000",
          "on_behalf_of": "orch-1",
          "lanes": [ { "block": "rev-std", "session": "1111",
                       "last_verdict": "pass", "at_head": "abc123" } ],
          "lane_index": 0,
          "counters": { "review_rounds": 1, "ci_attempts": 0, "rebase_attempts": 0 },
          "started_ms": 0 }
      ]
    }"#;

    #[test]
    fn the_notes_own_example_entry_parses() {
        let s = parse_state(NOTE_EXAMPLE).unwrap();
        assert_eq!(s.version, REVIEW_DRIVES_VERSION);
        let e = s.entry(1758).unwrap();
        assert_eq!(e.state(), DriveState::ReviewWait);
        assert_eq!(e.held_reason, None);
        assert_eq!(e.counters.review_rounds, 1);
        assert_eq!(e.lanes[0].block, "rev-std");
        assert_eq!(e.lanes[0].last_verdict, Some(Verdict::Pass));
        // The added fields default rather than refusing the published shape.
        assert_eq!(e.fix_handback_ms, 0);
        assert_eq!(e.lanes[0].spawned_ms, 0);
        assert_eq!(e.lanes[0].briefed_head, "");
        assert_eq!(e.lanes[0].briefed_digest, "");
    }

    #[test]
    fn review_drives_round_trip_preserves_unknown_fields() {
        // §5.2: unknown fields are tolerated AND preserved. "Tolerated" alone
        // is what serde does by default, and a field ignored on read is lost on
        // the next write — which breaks the actual promise, that an older build
        // can read this file and rewrite it without destroying what a newer one
        // wrote. Every level carries an `extra`, and this walks all four.
        let text = r#"{
          "version": 1,
          "future_top": {"k": 1},
          "entries": [
            { "pr": 7,
              "state": "ci-wait",
              "head": "h",
              "counters": {"review_rounds": 1, "future_counter": 42},
              "lanes": [{"block": "rev-std", "future_lane": ["x"]}],
              "future_entry": "keep me" }
          ]
        }"#;
        let s = parse_state(text).expect("a newer file must still read");
        let back: Value = serde_json::to_value(&s).unwrap();
        assert_eq!(back["future_top"], serde_json::json!({"k": 1}));
        assert_eq!(back["entries"][0]["future_entry"], "keep me");
        assert_eq!(back["entries"][0]["counters"]["future_counter"], 42);
        assert_eq!(back["entries"][0]["lanes"][0]["future_lane"], serde_json::json!(["x"]));
        // ...and the known fields survived the trip too, so the assertions
        // above are not passing over a document that lost everything else.
        assert_eq!(back["entries"][0]["pr"], 7);
        assert_eq!(back["entries"][0]["state"], "ci-wait");
        assert_eq!(back["entries"][0]["counters"]["review_rounds"], 1);
    }

    #[test]
    fn an_unknown_state_string_refuses_the_whole_file() {
        // The asymmetry §5.2 argues for, and the half that is easy to get
        // backwards: an unknown FIELD is carried, an unknown STATE refuses.
        let bad = NOTE_EXAMPLE.replace(r#""state": "review-wait""#, r#""state": "reviewing""#);
        assert_ne!(bad, NOTE_EXAMPLE, "the mutation must actually land");
        match parse_state(&bad) {
            Err(StateError::Malformed(_)) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_held_reason_and_an_unknown_verdict_refuse_the_file_too() {
        // Both are closed vocabularies for the state's reason: a hold whose
        // reason this build cannot read is a hold it cannot explain, and a
        // verdict word that never went through `Verdict::parse` is one a later
        // `== "pass"` could act on.
        let bad_reason = NOTE_EXAMPLE
            .replace(r#""state": "review-wait""#, r#""state": "held""#)
            .replace(r#""held_reason": null"#, r#""held_reason": "vibes""#);
        assert!(bad_reason.contains("vibes"), "the mutation must actually land");
        assert!(matches!(
            parse_state(&bad_reason),
            Err(StateError::Malformed(_))
        ));

        let bad_verdict = NOTE_EXAMPLE.replace(r#""last_verdict": "pass""#, r#""last_verdict": "PASS""#);
        assert!(bad_verdict.contains("PASS"), "the mutation must actually land");
        assert!(matches!(
            parse_state(&bad_verdict),
            Err(StateError::Malformed(_))
        ));

        // A `held` entry with a KNOWN reason still parses, so the two refusals
        // above are about the vocabulary and not about the shape.
        let good = NOTE_EXAMPLE
            .replace(r#""state": "review-wait""#, r#""state": "held""#)
            .replace(r#""held_reason": null"#, r#""held_reason": "review-limit""#);
        let s = parse_state(&good).unwrap();
        assert_eq!(s.entry(1758).unwrap().held_reason, Some(HeldReason::ReviewLimit));
    }

    #[test]
    fn a_versionless_or_counterless_file_is_malformed_and_a_future_one_is_unsupported() {
        // No version is malformed, not "a v1 file" — §5.2's `version` is
        // required for the same reason the queue's is.
        assert!(matches!(
            parse_state(r#"{"entries":[]}"#),
            Err(StateError::Malformed(_))
        ));
        // Missing counters is refused rather than defaulted to zeros: zeros
        // would silently grant a full fresh budget.
        let no_counters = NOTE_EXAMPLE.replace(
            r#""counters": { "review_rounds": 1, "ci_attempts": 0, "rebase_attempts": 0 },"#,
            "",
        );
        assert!(!no_counters.contains("counters"), "the mutation must actually land");
        assert!(matches!(
            parse_state(&no_counters),
            Err(StateError::Malformed(_))
        ));
        // A schema this build does not understand: do not operate, do not write.
        assert_eq!(
            parse_state(r#"{"version":2,"entries":[]}"#),
            Err(StateError::Unsupported(2))
        );
    }

    #[test]
    fn retention_prunes_the_terminals_and_never_the_parked_one() {
        // §5.2's asymmetry, and the whole reason §2.1 makes `held` parked:
        // pruning a parked entry would silently grant three fresh rounds.
        let mut s = ReviewDrivesState::default();
        for (pr, st) in [
            (1u64, DriveState::CiWait),
            (2, DriveState::Held),
            (3, DriveState::Satisfied),
            (4, DriveState::Cancelled),
            (5, DriveState::GateCheck),
        ] {
            let mut e = entry_at(st);
            e.pr = pr;
            s.entries.push(e);
        }
        let mut pruned = prune_terminal(&mut s);
        pruned.sort_unstable();
        assert_eq!(pruned, vec![3, 4]);
        let left: Vec<u64> = s.entries.iter().map(|e| e.pr).collect();
        assert_eq!(left, vec![1, 2, 5]);
        // The parked entry kept its counters, which is what the resume spends.
        assert!(s.entry(2).unwrap().state().is_parked());
    }

    #[test]
    fn already_driven_covers_the_working_and_gate_states_only() {
        // §5.1: a flat `already-driven` would make §2.3's resume unreachable
        // and `reset_counters` a parameter nothing can pass.
        for (st, driven) in [
            (DriveState::CiWait, true),
            (DriveState::ReviewWait, true),
            (DriveState::FixWait, true),
            (DriveState::GateCheck, true),
            (DriveState::Held, false),
            (DriveState::Satisfied, false),
            (DriveState::Cancelled, false),
        ] {
            let mut s = ReviewDrivesState::default();
            s.entries.push(entry_at(st));
            assert_eq!(s.is_driven(1758), driven, "{}", st.as_str());
        }
        // A PR with no entry at all is not driven either.
        assert!(!ReviewDrivesState::default().is_driven(1758));
    }

    // ── §2.4 the decision ───────────────────────────────────────────────────

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

    #[test]
    fn the_tick_never_advances_a_parked_or_terminal_drive() {
        // §2.1's `held` row: "nothing; the tick does not advance it". If the
        // tick could move one, `held` would not be a park — and the two tools
        // would not be its only ways out.
        let limits = DriveLimits::default();
        for st in [DriveState::Held, DriveState::Satisfied, DriveState::Cancelled] {
            let e = entry_at(st);
            // Facts that would move any working state, so this is not passing
            // because there was nothing to do.
            let facts = DriveFacts {
                ci: CiObservation::Green,
                gate: GateOutcome::Satisfied,
                worker: WorkerSignal::Blocked,
                messaged: true,
                now_ms: 10_000_000_000,
                ..facts_at("head-a")
            };
            assert_eq!(decide(&e, &facts, &limits), DriveStep::Wait, "{}", st.as_str());
        }
    }

    #[test]
    fn only_a_positive_answer_cancels_a_drive() {
        // §8: cancelling a live drive on a rate limit that clears in minutes is
        // the failure this distinction exists to stop. `None` is "the world
        // does not match", never "probably fine".
        let limits = DriveLimits::default();
        let e = entry_at(DriveState::CiWait);
        let closed = DriveFacts { pr_open: Some(false), ..facts_at("head-a") };
        assert_eq!(decide(&e, &closed, &limits), DriveStep::to(DriveState::Cancelled));
        let unknown = DriveFacts { pr_open: None, ..facts_at("head-a") };
        assert_eq!(decide(&e, &unknown, &limits), DriveStep::Wait);
    }

    #[test]
    fn a_delegates_message_parks_the_drive() {
        // §7: `message_orchestrator` is never intercepted, so the delegate's
        // own line is already in the pane; this hold is the routing fact.
        let limits = DriveLimits::default();
        let e = entry_at(DriveState::ReviewWait);
        let facts = DriveFacts { messaged: true, ..facts_at("head-a") };
        assert_eq!(decide(&e, &facts, &limits), DriveStep::held(HeldReason::Messaged));
    }

    #[test]
    fn the_age_bound_outranks_an_available_advance() {
        // §8's `also: [base-green]` row is the worked example: that drive
        // cycles gate-check -> ci-wait on EVERY wake, so it always has an
        // advance available. An age check that ran after the per-state logic
        // would never be reached and the drive would cycle forever — which is
        // exactly the silent park the bound exists to prevent.
        let limits = DriveLimits::default();
        let e = entry_at(DriveState::GateCheck);
        let cycling = DriveFacts {
            gate: GateOutcome::Unsatisfied,
            ..facts_at("head-a")
        };
        // Young: it takes arc 10, as that row describes.
        assert_eq!(
            decide(&e, &cycling, &limits),
            DriveStep::to(DriveState::CiWait)
        );
        // Aged past the bound: parked, despite that arc still being available.
        let aged = DriveFacts {
            now_ms: e.started_ms + minutes_ms(limits.drive_timeout_minutes),
            ..cycling
        };
        assert_eq!(
            decide(&e, &aged, &limits),
            DriveStep::held(HeldReason::DriveStalled)
        );
    }

    #[test]
    fn ci_wait_reads_its_four_answers_and_waits_on_the_two_unknowns() {
        let limits = DriveLimits::default();
        let e = entry_at(DriveState::CiWait);
        let with = |ci| decide(&e, &DriveFacts { ci, ..facts_at("head-a") }, &limits);
        // Arc 2.
        assert_eq!(with(CiObservation::Green), DriveStep::to(DriveState::ReviewWait));
        // Arc 3, on each of its two counters.
        assert_eq!(
            with(CiObservation::Red),
            DriveStep::spend(DriveState::FixWait, Counter::CiAttempts)
        );
        assert_eq!(
            with(CiObservation::Conflicting),
            DriveStep::spend(DriveState::FixWait, Counter::RebaseAttempts)
        );
        // Neither an answer nor a reason to move: "unknown is never treated as
        // safe", and it is never treated as a fact about the PR either.
        assert_eq!(with(CiObservation::Pending), DriveStep::Wait);
        assert_eq!(with(CiObservation::Unknown), DriveStep::Wait);
    }

    #[test]
    fn ci_attempts_park_at_their_bound() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::CiWait);
        e.counters.ci_attempts = limits.max_ci_attempts;
        let facts = DriveFacts { ci: CiObservation::Red, ..facts_at("head-a") };
        assert_eq!(decide(&e, &facts, &limits), DriveStep::held(HeldReason::CiLimit));
    }

    #[test]
    fn an_unaccountable_route_parks_from_every_state_that_reads_one() {
        // §4: never a guess. The unknown thing is *which reviewers are
        // required*, so guessing "no rule fired" is guessing in favour of
        // merging — and at `gate-check` that would be a false GATE SATISFIED
        // notice, which §3.1 names "a bypass with better telemetry".
        let limits = DriveLimits::default();
        for st in [DriveState::ReviewWait, DriveState::GateCheck] {
            let mut e = entry_at(st);
            e.head = "head-a".into();
            let facts = DriveFacts {
                required_lanes: None,
                // The gate would say SATISFIED, which is precisely what must
                // not win here.
                gate: GateOutcome::Satisfied,
                ..facts_at("head-a")
            };
            assert_eq!(
                decide(&e, &facts, &limits),
                DriveStep::held(HeldReason::RoutingUnaccountable),
                "{}",
                st.as_str()
            );
        }
    }

    #[test]
    fn review_wait_walks_its_lanes_in_the_gates_order() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        let two_lanes = |first: Option<Verdict>, second: Option<Verdict>| DriveFacts {
            required_lanes: Some(vec![
                lane_fact("rev-std", first, "head-a", "d1"),
                lane_fact("rev-final", second, "head-a", "d1"),
            ]),
            ..facts_at("head-a")
        };
        // Lane 1 not yet briefed: open it.
        assert_eq!(
            decide(&e, &two_lanes(None, None), &limits),
            DriveStep::OpenLane { index: 0 }
        );
        // Lane 1 passed at this (head, digest): move to lane 2 — and note this
        // is NOT a transition, which is why the table has no review-wait arm.
        assert_eq!(
            decide(&e, &two_lanes(Some(Verdict::Pass), None), &limits),
            DriveStep::OpenLane { index: 1 }
        );
        // Both passed: arc 4.
        assert_eq!(
            decide(
                &e,
                &two_lanes(Some(Verdict::Pass), Some(Verdict::Pass)),
                &limits
            ),
            DriveStep::to(DriveState::GateCheck)
        );
        // Arc 5, and an escalate that §3 refuses to decide.
        assert_eq!(
            decide(&e, &two_lanes(Some(Verdict::Fail), None), &limits),
            DriveStep::spend(DriveState::FixWait, Counter::ReviewRounds)
        );
        assert_eq!(
            decide(&e, &two_lanes(Some(Verdict::Escalate), None), &limits),
            DriveStep::held(HeldReason::Escalate)
        );
    }

    #[test]
    fn a_head_that_moved_under_a_lane_re_enters_ci_wait() {
        // Arc 6 / §8 row 4. The race is not designed away — it is the race the
        // verdict binding already exists to handle.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Pass), "head-a", "d1")]),
            ..facts_at("head-b")
        };
        assert_eq!(decide(&e, &facts, &limits), DriveStep::to(DriveState::CiWait));
    }

    #[test]
    fn a_lane_briefed_at_this_head_is_waited_for_then_bounded() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.open_lane("rev-std", "s1", "head-a", Some("d1"), 1_000);
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", None, "", "")]),
            now_ms: 1_000 + minutes_ms(limits.lane_timeout_minutes) - 1,
            ..facts_at("head-a")
        };
        // Open and inside its wait: nothing to do, and in particular NOT a
        // re-brief on every tick.
        assert_eq!(decide(&e, &facts, &limits), DriveStep::Wait);
        // Past `lane_timeout_minutes` with no verdict: parked, naming the pane.
        let stalled = DriveFacts {
            now_ms: 1_000 + minutes_ms(limits.lane_timeout_minutes),
            ..facts
        };
        assert_eq!(
            decide(&e, &stalled, &limits),
            DriveStep::held(HeldReason::LaneStalled)
        );
    }

    #[test]
    fn a_lane_briefed_at_an_older_head_is_re_briefed_not_waited_for() {
        // This is what `briefed_head` exists to answer, and it is why that
        // field is not `at_head`: a lane whose brief predates the live head has
        // been asked nothing about this revision.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-b".into();
        e.open_lane("rev-std", "s1", "head-a", Some("d1"), 1_000);
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", None, "", "")]),
            ..facts_at("head-b")
        };
        assert_eq!(decide(&e, &facts, &limits), DriveStep::OpenLane { index: 0 });
    }

    #[test]
    fn a_pass_whose_body_digest_moved_re_opens_that_lane() {
        // §8's body-changed row: the (head, digest) key is re-read every tick,
        // so a moved digest with an unchanged head re-enters at the first stale
        // lane with a body-only delta brief.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.open_lane("rev-std", "s1", "head-a", Some("OLD"), 1_000);
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Pass), "head-a", "OLD")]),
            body_digest: Some("NEW".into()),
            ..facts_at("head-a")
        };
        assert_eq!(decide(&e, &facts, &limits), DriveStep::OpenLane { index: 0 });
    }

    #[test]
    fn a_re_brief_ends_the_loop_rather_than_repeating_it_every_tick() {
        // The other half of the fix above, and the failure a head-only key
        // would swap this one for: once the lane HAS been re-briefed at the new
        // digest, the very same stale `pass` must read as "asked, thinking" and
        // not re-brief again. The verdict file still holds the old pass — a
        // reviewer has not re-recorded yet — so nothing but the brief key can
        // tell these two ticks apart.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Pass), "head-a", "OLD")]),
            body_digest: Some("NEW".into()),
            ..facts_at("head-a")
        };
        e.open_lane("rev-std", "s1", "head-a", Some("OLD"), 1_000);
        assert_eq!(decide(&e, &facts, &limits), DriveStep::OpenLane { index: 0 });
        // S3 performs that brief; the drive now waits on the reviewer.
        e.open_lane("rev-std", "s1", "head-a", Some("NEW"), 1_500);
        assert_eq!(decide(&e, &facts, &limits), DriveStep::Wait);
    }

    #[test]
    fn a_lane_is_open_only_for_the_revision_it_was_asked_about() {
        // All four crossings of {briefed head matches} x {briefed digest
        // matches}, because this guard reads two signals and the defect it
        // shipped with was reading one. Three of the four must re-brief; a
        // guard that answered "open" on the head alone passes two of them.
        let rec = |head: &str, digest: &str| LaneRecord {
            block: "rev-std".into(),
            session: "s1".into(),
            last_verdict: None,
            at_head: String::new(),
            briefed_head: head.into(),
            briefed_digest: digest.into(),
            spawned_ms: 0,
            extra: BTreeMap::new(),
        };
        let now = Some("d1");
        assert!(lane_open_for(&rec("head-a", "d1"), "head-a", now));
        assert!(!lane_open_for(&rec("head-a", "OLD"), "head-a", now));
        assert!(!lane_open_for(&rec("head-OLD", "d1"), "head-a", now));
        assert!(!lane_open_for(&rec("head-OLD", "OLD"), "head-a", now));
        // "We could not check" is not "it changed", in either direction — one
        // transient failure to read a PR body must not re-brief every open lane
        // in the group.
        assert!(lane_open_for(&rec("head-a", "d1"), "head-a", None));
        assert!(lane_open_for(&rec("head-a", "d1"), "head-a", Some("")));
        assert!(lane_open_for(&rec("head-a", ""), "head-a", now));
        // ...but an unknown digest never rescues a head that really did move.
        assert!(!lane_open_for(&rec("head-OLD", ""), "head-a", None));
    }

    #[test]
    fn fix_wait_takes_its_two_arcs_and_its_three_holds() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::FixWait);
        e.head = "head-a".into();
        assert_eq!(e.fix_handback_ms, 1_000);

        // Arc 7: the worker pushed.
        assert_eq!(
            decide(&e, &facts_at("head-b"), &limits),
            DriveStep::to(DriveState::CiWait)
        );
        // Arc 8: report(done) with the head unchanged — a body-only fix.
        let done_same_head = DriveFacts {
            worker: WorkerSignal::Done,
            ..facts_at("head-a")
        };
        assert_eq!(
            decide(&e, &done_same_head, &limits),
            DriveStep::to(DriveState::ReviewWait)
        );
        // A push outranks a `done` that arrived with it: the code moved, so CI
        // is what has to answer next.
        let done_and_pushed = DriveFacts {
            worker: WorkerSignal::Done,
            ..facts_at("head-b")
        };
        assert_eq!(
            decide(&e, &done_and_pushed, &limits),
            DriveStep::to(DriveState::CiWait)
        );
        // The three holds.
        for (signal, reason) in [
            (WorkerSignal::Blocked, HeldReason::WorkerBlocked),
            (WorkerSignal::Unresumable, HeldReason::WorkerUnresumable),
        ] {
            let facts = DriveFacts { worker: signal, ..facts_at("head-a") };
            assert_eq!(decide(&e, &facts, &limits), DriveStep::held(reason));
        }
        // Silent inside the wait, then bounded by it.
        let quiet = DriveFacts {
            now_ms: e.fix_handback_ms + minutes_ms(limits.fix_timeout_minutes) - 1,
            ..facts_at("head-a")
        };
        assert_eq!(decide(&e, &quiet, &limits), DriveStep::Wait);
        let stalled = DriveFacts {
            now_ms: e.fix_handback_ms + minutes_ms(limits.fix_timeout_minutes),
            ..facts_at("head-a")
        };
        assert_eq!(
            decide(&e, &stalled, &limits),
            DriveStep::held(HeldReason::FixStalled)
        );
    }

    #[test]
    fn an_unresumable_worker_is_a_hold_and_never_a_drive_time_refusal() {
        // §5.1 is deliberately honest about this: a full, well-shaped session
        // id this group never recorded takes `resolve_session_ref`'s
        // passthrough arm and is ACCEPTED by `drive_review`, so its
        // unresumability surfaces here, at the first hand-back, possibly hours
        // on. Nothing in this module claims to catch it earlier — resolving is
        // not the same as proving resumable.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::FixWait);
        e.head = "head-a".into();
        let facts = DriveFacts {
            worker: WorkerSignal::Unresumable,
            ..facts_at("head-a")
        };
        assert_eq!(
            decide(&e, &facts, &limits),
            DriveStep::held(HeldReason::WorkerUnresumable)
        );
    }

    #[test]
    fn gate_check_answers_satisfied_only_when_the_gate_did() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::GateCheck);
        e.head = "head-a".into();
        let with = |gate| decide(&e, &DriveFacts { gate, ..facts_at("head-a") }, &limits);
        // Arc 9.
        assert_eq!(with(GateOutcome::Satisfied), DriveStep::to(DriveState::Satisfied));
        // Arc 10 — wider than "stale" on purpose.
        assert_eq!(with(GateOutcome::Unsatisfied), DriveStep::to(DriveState::CiWait));
        // Present and unreadable is a hold, NOT `gate-not-configured`.
        assert_eq!(
            with(GateOutcome::Unreadable),
            DriveStep::held(HeldReason::GateUnreadable)
        );
        // Nothing was evaluated, so nothing is known — and in particular this
        // is not `satisfied`.
        assert_eq!(with(GateOutcome::NotEvaluated), DriveStep::Wait);
    }

    // ── the two carried-over gate properties (§2.1) ─────────────────────────

    #[test]
    fn a_pass_bound_to_an_old_head_or_an_old_body_is_not_a_pass() {
        let v = |verdict, head: &str, digest: &str| ReviewVerdict {
            pr: 1,
            block: "rev-std".into(),
            agent_id: "rev-1".into(),
            verdict,
            head: head.into(),
            body_digest: digest.into(),
            summary: String::new(),
            ts_ms: 0,
        };
        let now = Some("d1");
        // The positive control first, so every refusal below is a difference.
        assert!(lane_pass_is_current(
            Some(&v(Verdict::Pass, "head-a", "d1")),
            "head-a",
            now
        ));
        // Bound to an old head.
        assert!(!lane_pass_is_current(
            Some(&v(Verdict::Pass, "head-OLD", "d1")),
            "head-a",
            now
        ));
        // Bound to an old body.
        assert!(!lane_pass_is_current(
            Some(&v(Verdict::Pass, "head-a", "OLD")),
            "head-a",
            now
        ));
        // Unbound: an empty head can never equal a real one, so it fails closed.
        assert!(!lane_pass_is_current(
            Some(&v(Verdict::Pass, "", "d1")),
            "head-a",
            now
        ));
        // Not a pass at all, and no verdict at all.
        assert!(!lane_pass_is_current(
            Some(&v(Verdict::Fail, "head-a", "d1")),
            "head-a",
            now
        ));
        assert!(!lane_pass_is_current(None, "head-a", now));
        // "We could not check" is not "it changed": a verdict with no digest,
        // or a body that could not be read, does not stale a pass. Only
        // `Some(true)` re-opens a lane.
        assert!(lane_pass_is_current(
            Some(&v(Verdict::Pass, "head-a", "")),
            "head-a",
            now
        ));
        assert!(lane_pass_is_current(
            Some(&v(Verdict::Pass, "head-a", "d1")),
            "head-a",
            None
        ));
    }

    #[test]
    fn the_re_entry_point_is_the_first_lane_whose_pass_does_not_stand() {
        // Arc 8's "re-enters at the first stale lane" (§8 row 5).
        let lanes = vec![
            lane_fact("rev-std", Some(Verdict::Pass), "head-a", "d1"),
            lane_fact("rev-final", Some(Verdict::Pass), "head-OLD", "d1"),
            lane_fact("rev-extra", None, "", ""),
        ];
        assert_eq!(first_stale_lane(&lanes, "head-a", Some("d1")), 1);
        // Every lane standing is the "nothing left to review" answer arc 4 acts
        // on, and it is the length rather than an index.
        let all_good = vec![lane_fact("rev-std", Some(Verdict::Pass), "head-a", "d1")];
        assert_eq!(first_stale_lane(&all_good, "head-a", Some("d1")), 1);
        assert_eq!(first_stale_lane(&[], "head-a", Some("d1")), 0);
    }

    #[test]
    fn a_held_step_always_carries_a_reason_and_no_other_step_ever_does() {
        // The same invariant `advance` enforces, asserted over everything
        // `decide` can actually emit — so a new arm cannot ship a reasonless
        // hold, or a reason riding an arc that is not one.
        let limits = DriveLimits::default();
        let mut seen_held = 0;
        let mut seen_other = 0;
        for st in DriveState::ALL {
            let mut e = entry_at(st);
            e.head = "head-a".into();
            for ci in [
                CiObservation::Green,
                CiObservation::Red,
                CiObservation::Conflicting,
                CiObservation::Pending,
                CiObservation::Unknown,
            ] {
                for worker in [
                    WorkerSignal::Silent,
                    WorkerSignal::Done,
                    WorkerSignal::Blocked,
                    WorkerSignal::Unresumable,
                ] {
                    for gate in [
                        GateOutcome::NotEvaluated,
                        GateOutcome::Satisfied,
                        GateOutcome::Unsatisfied,
                        GateOutcome::Unreadable,
                    ] {
                        for required in [
                            None,
                            Some(vec![lane_fact("rev-std", Some(Verdict::Fail), "head-a", "d1")]),
                        ] {
                            let facts = DriveFacts {
                                ci,
                                worker,
                                gate,
                                required_lanes: required.clone(),
                                ..facts_at("head-a")
                            };
                            match decide(&e, &facts, &limits) {
                                DriveStep::Advance {
                                    to: DriveState::Held,
                                    held_reason,
                                    ..
                                } => {
                                    assert!(held_reason.is_some());
                                    seen_held += 1;
                                }
                                DriveStep::Advance { held_reason, .. } => {
                                    assert!(held_reason.is_none());
                                    seen_other += 1;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        // Positive controls: the sweep really did produce both shapes, so the
        // assertions above are not passing over an empty walk.
        assert!(seen_held > 0, "the sweep produced no hold at all");
        assert!(seen_other > 0, "the sweep produced no ordinary advance");
    }
}
