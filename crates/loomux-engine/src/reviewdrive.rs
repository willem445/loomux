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
//! - [`DriveEntry::fix_kickback_ms`] — when the drive last answered a worker's
//!   `report(progress)` in that worker's own pane (#1959). Not a timeout
//!   anchor: it is compared against `fix_handback_ms`, which makes the budget
//!   one answer per hand-back and renews it with no reset to remember.
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
/// thirteen states**, so a reader asking "is this drive parked" asks one
/// question, and the reason travels in the notice and the audit line rather
/// than being inferred from which counter happens to sit at its bound.
///
/// Thirteen reasons. With `satisfied` and `cancelled` that is §2.2's fifteen
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
    /// The gate file is present and orrerix could not turn it into a routing
    /// answer — an I/O error, or a file `parse_gate_file` refuses. **Not**
    /// `gate-not-configured`, which means the file is genuinely absent.
    ///
    /// Both are the same fact to a drive: the gate exists and cannot be read,
    /// so what it requires is unknown — and §8 never treats unknown as safe.
    GateUnreadable,
    /// The worker reported `blocked`.
    WorkerBlocked,
    /// **The fix could not be handed back to its worker** — the class, not one
    /// cause. A session that no longer resolves is the original one; a block
    /// that has left the roster and a pane that opened and exited before saying
    /// anything are the two #1961 added, and each names itself in
    /// [`crate::rddrive::HeldFacts::refusal`] rather than being reported as the
    /// first. Narrowing this doc to "the session no longer resolves" is how the
    /// notice came to send an orchestrator after a replacement session for a
    /// session that was fine.
    WorkerUnresumable,
    /// **The group's live-delegate cap refused the pane the drive needed**
    /// (#1960) — its own reason, and the reason it is not
    /// [`WorkerUnresumable`](HeldReason::WorkerUnresumable).
    ///
    /// It was reported as that one, which named a remedy that does not work: it
    /// tells the orchestrator the recorded session no longer resolves and to
    /// re-point the drive at another one. The session resolves fine. What is
    /// exhausted is a slot, the remedy is to free one (or wait), and those are
    /// different actions — measured on the dogfood, an orchestrator went
    /// looking for a replacement session for a session whose `.jsonl` was on
    /// disk and which re-pointed successfully the moment panes were killed.
    ///
    /// A **lane** spawn refused by the cap does not reach this: `review-wait`
    /// backs off and retries, counted only against `drive_timeout_minutes`
    /// (§8's live-delegate-cap row). The asymmetry is the states': a lane can
    /// be opened on any later tick, while `fix-wait` has already taken its arc
    /// and spent its round.
    CapRefused,
    /// A driven delegate called `message_orchestrator` (§7 — that call is never
    /// intercepted; the delegate's own line arrives by its own path and this
    /// hold is the routing fact beside it).
    Messaged,
}

impl HeldReason {
    /// Every reason, so a caller — or a test counting §2.2's exits — can
    /// enumerate them without matching on the enum. Order is §2.2's table.
    pub const ALL: [HeldReason; 13] = [
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
        HeldReason::CapRefused,
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
            HeldReason::CapRefused => "cap-refused",
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
            "cap-refused" => Some(HeldReason::CapRefused),
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
/// and the top of §5.3's `1..=3` range.
pub const MAX_ROUNDS_CEILING: u32 = 3;
/// The ceiling on `max_rebase_attempts`, and the top of §5.3's `0..=1` range.
pub const MAX_REBASE_CEILING: u32 = 1;

/// The bounds one drive runs against — the value type [`decide`] consumes.
///
/// **This is not a second parser of the `driver:` block.** §5.3's block is
/// S2's, in `workflow.rs`, and that is where a malformed block goes loudly down
/// the `workflow-invalid` path. What lives here is the *value* the pure core is
/// handed.
///
/// **The clamp only ever tightens, and it is a capability boundary rather than
/// input hygiene.** §2.3: a repo may run a *tighter* loop than the orchestrator
/// template promises; it may not run a looser one, because the driver acts on
/// the orchestrator's authority and a repo file that raised the bound would be
/// loosening the orchestrator's own invariant from a configuration file. That
/// is `doc/design/workflows.md`'s closure exactly — **a workflow file may
/// select from what loomux permits and may never widen it** — so a
/// `driver.max_review_rounds: 9` that reached a decision would be a repo file
/// granting a capability, not a validation slip.
///
/// **Two independent layers hold it, and neither is allowed to rely on the
/// other.** S2 refuses or clamps out-of-range values as it parses `driver:`;
/// [`decide`] clamps again on the values it actually reads. The second is not
/// redundant — [`decide`] is a `pub fn` over a plain value type, so any caller
/// in any crate can reach it without passing through S2's parser at all, and a
/// boundary that holds only when the expected caller is upstream is not a
/// boundary. Round 21 in the PR is the counterfactual for this arm.
///
/// The type carries a private field so the struct cannot be built by *literal*
/// outside this module (E0451): every construction path a caller can reach
/// ([`DriveLimits::new`], [`Default`], [`DriveLimits::clamped`]) clamps.
///
/// **That seal does not make an out-of-range value unspellable, and claiming it
/// did was weaker as well as false.** The bounds fields are `pub` — they are
/// meant to be read — so a caller outside this module can take a clamped value
/// from any of those constructors and then assign to one:
/// `let mut l = DriveLimits::default(); l.max_review_rounds = 9;` compiles
/// anywhere. What the seal actually buys is that such a value can only arise
/// by a *deliberate* post-construction write, never by someone filling in a
/// struct literal without noticing there was a range.
///
/// The load-bearing statement is the stronger one, and it is about reach rather
/// than spelling: **an out-of-range bound cannot reach a decision.** [`decide`]
/// clamps unconditionally — no `if`, no caller opt-out — and *shadows* its own
/// binding with the clamped value, so every read below that line is of a
/// clamped bound and there is no path through the function that consults the
/// argument as passed. That is a property of one function anyone can re-read,
/// which is why it is the claim worth making; "cannot be spelled" was a claim
/// about the whole type system and was not true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveLimits {
    pub max_review_rounds: u32,
    pub max_ci_attempts: u32,
    pub max_rebase_attempts: u32,
    pub lane_timeout_minutes: u64,
    pub fix_timeout_minutes: u64,
    pub drive_timeout_minutes: u64,
    /// Private, and load-bearing: it makes `DriveLimits { … }` a compile error
    /// outside this module (E0451), so the clamping constructors are the only
    /// way in. The fields stay `pub` so a caller can still *read* the bounds —
    /// what is closed is authoring one, not inspecting it.
    _seal: (),
}

impl Default for DriveLimits {
    /// §5.3's defaults, which are already inside every range.
    fn default() -> Self {
        DriveLimits {
            max_review_rounds: 3,
            max_ci_attempts: 3,
            max_rebase_attempts: 1,
            lane_timeout_minutes: 60,
            fix_timeout_minutes: 60,
            drive_timeout_minutes: 240,
            _seal: (),
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

    /// The only way to build a `DriveLimits` from outside this module, and it
    /// clamps. S3 maps S2's parsed `driver:` block through here; the timeouts
    /// pass through unclamped because §5.3 does not bound them against
    /// INVARIANT 9 — they are pacing, not budget.
    pub fn new(
        max_review_rounds: u32,
        max_ci_attempts: u32,
        max_rebase_attempts: u32,
        lane_timeout_minutes: u64,
        fix_timeout_minutes: u64,
        drive_timeout_minutes: u64,
    ) -> DriveLimits {
        DriveLimits {
            max_review_rounds,
            max_ci_attempts,
            max_rebase_attempts,
            lane_timeout_minutes,
            fix_timeout_minutes,
            drive_timeout_minutes,
            _seal: (),
        }
        .clamped()
    }
}

/// What a drive has spent (§5.2's `counters`).
///
/// **The comparison against a bound is check-before-bump**, and that is a
/// decision with evidence rather than a coin flip, because the two orderings
/// differ by a whole round. [`counter_exhausted`] is where it is spelled;
/// §2.2's `rebase-limit` row is what decides it.
/// **Every field is required, not just the block.** `DriveEntry::counters`
/// carries no `serde(default)` for the reason on that field — zeros silently
/// grant a full fresh budget — and a per-field default would have reopened the
/// same hole one level down, where `"counters": {}` parses to three zeros and
/// `"counters": {"review_rounds": 2}` quietly forgives the CI attempts. The
/// block being mandatory is worth nothing if its contents are optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub review_rounds: u32,
    pub ci_attempts: u32,
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
    /// The **agent id** — the pane — this lane's delegate is running in.
    /// Beyond §5.2's example, and it answers two questions the session id
    /// cannot.
    ///
    /// §2.2's `lane-stalled` row says the notice **names the pane**, and a pane
    /// is an agent id (`rev-4`), never a session UUID. And §7's interception is
    /// "keyed on the agent, never on text": an MCP caller arrives as a
    /// `caller.agent_id`, so without this field the driver cannot tell whether
    /// the delegate now calling `report` is one it spawned — and the only
    /// alternative key is something the delegate typed, which is precisely what
    /// §7 forbids.
    ///
    /// Empty when this build recorded the lane before the field existed, or
    /// when the spawn returned no id. Empty never matches a caller, so an
    /// unrecorded pane fails **closed**: its traffic is delivered to the
    /// orchestrator as it always was, rather than being consumed by a drive
    /// that cannot prove it owns the speaker.
    #[serde(default)]
    pub agent: String,
    /// Every EARLIER pane this drive opened for this lane, oldest first.
    ///
    /// **A re-brief supersedes a pane; it does not un-own it** (#1871 B2's
    /// sibling arc). [`open_lane`](DriveEntry::open_lane) replaces this lane's
    /// record wholesale, so before this field the previous pane's id was simply
    /// gone — and that id is §7's interception key. A reviewer still finishing
    /// its previous round then reported as if undriven, into the orchestrator's
    /// pane, which is the one thing the drive exists to absorb.
    ///
    /// Superseded is not dead: `rd_open_lane` resumes the lane's SESSION, and
    /// where it cannot type the brief into that session's own live idle pane
    /// (#1960) orrerix mints a new pane id for the resume while the old pane
    /// keeps running until something closes it. Both panes are this drive's, for
    /// as long as the drive is live. Reuse changed how OFTEN a lane pane is
    /// superseded; it changed nothing about what this field owes one that is.
    #[serde(default)]
    pub prior_agents: Vec<String>,
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

/// Which side of a drive an agent is — the answer §7's interception asks of
/// every incoming `report` and `review_verdict`.
///
/// `Lane` carries the block id because the two consumers need it: the audit
/// line says which lane spoke, and `review-wait` reads that lane's verdict file
/// next tick. `Worker` needs no payload — a drive has exactly one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrivenRole {
    /// A reviewer lane this drive spawned or resumed, by block id.
    Lane(BlockId),
    /// The worker this drive resumed for a hand-back.
    Worker,
}

/// One pane this drive owns, and **whether it is the pane the drive would speak
/// to now** — the two questions §7 has to answer separately (#1871 B2).
///
/// **Owning a pane and taking its word are different decisions, and collapsing
/// them is a live defect in either direction.** A pane the drive superseded is
/// still the drive's: its `report` is exactly the routing traffic §7 exists to
/// absorb, and leaving it to reach the orchestrator defeats the quiet-pane
/// property that is the whole measured benefit. But it is no longer the pane the
/// hand-back went to, and its report describes a revision the drive has moved
/// past — a `done` from a worker pane that was evicted two heads ago would
/// satisfy, through arc 8, work the CURRENT worker is still in the middle of.
///
/// So: consume it, audit it under its own kind so a reader can tell the two
/// apart, and give the state machine nothing. Only [`current`](DrivenPane::current)
/// traffic advances a drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrivenPane {
    pub role: DrivenRole,
    /// `false` for a pane a later spawn or hand-back superseded.
    pub current: bool,
}

/// The superseded-pane list, normalized: no empties, no duplicates, never the
/// pane that is superseding them, oldest first.
///
/// **Deduplicated by agent id because a pane resumed twice is one pane.** The
/// list is read by [`DriveEntry::driven_role`] — where a duplicate changes
/// nothing — and printed in the exit notices, where naming `w-1715` twice reads
/// as two panes a human then goes looking for.
fn retain_panes(prior: Vec<String>, current: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in prior {
        if a.is_empty() || a == current || out.iter().any(|s| *s == a) {
            continue;
        }
        out.push(a);
    }
    out
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

/// How long a terminal entry is kept alive for the sake of a notice that has
/// not reached a pane — §5.2's retention **ceiling** (#1857).
///
/// **A retry with no ceiling is a leak, not a guarantee.** The orchestrator's
/// pane can be gone for good — the group closed, the agent killed and never
/// respawned — and an entry retained on "until it is delivered" alone would sit
/// in `review_drives.json` forever, re-attempted on every wake of the poll loop
/// that also delivers every `notify_when` watch in the fleet.
///
/// **One hour, which is this subsystem's own unit for exactly this judgment.**
/// `lane_timeout_minutes` and `fix_timeout_minutes` both default to 60 and both
/// answer the same question — long enough that a transient (a pane restarting,
/// a full queue, a paused agent) has cleared, short enough that a dead one is
/// not held indefinitely — and `notify_when`'s own TTL defaults to the same 60
/// minutes. It is deliberately **not** a `driver:` policy knob: §5.3's block
/// paces a drive, and how long orrerix keeps its own undelivered record is not
/// a repo's call.
///
/// **Reaching it drops the entry and audits the notice text**, which is what
/// makes the bound honest rather than merely bounded: the defect #1857 names is
/// "no line in the pane AND no record that could produce one", and the second
/// half stays closed by `rd-notice-dropped` carrying the text even in the case
/// where the first cannot be.
pub const NOTICE_RETENTION_MS: u64 = 60 * 60_000;

/// A notice a terminal entry owes the orchestrator's pane, persisted **on the
/// entry** so it outlives the tick that produced it (#1857).
///
/// **Why the rendered text and not a flag plus a re-render.** A terminal entry
/// is the one thing the tick will not step: `rd_step_entry` declines anything
/// parked or terminal before it reads anything, and that early return is a cost
/// bound §2.4 wants rather than an oversight. So the facts a re-render would
/// need — the lane verdicts, the live head, the gate's answer — are not in hand
/// on any later tick, and fetching them again would spend `gh` round trips on a
/// drive that is over, to produce a notice that could legitimately differ from
/// the one the drive actually ended on. The notice is the product of the arc
/// that ended the drive: it is written down at that moment and re-sent
/// unchanged.
///
/// **Only a TERMINAL entry ever owes one, and that is a scope rather than an
/// oversight.** A `held` drive that loses its notice loses a *line*; the entry
/// itself survives — §5.2 never prunes a parked one — and `review_drive_status()`
/// lists it, so the drive has not vanished and the orchestrator has a record it
/// can still act on. A terminal entry has neither: the notice is the entire
/// product of the exit, and retention drops the record that could reproduce it.
/// A future change wanting the same guarantee for a hold has this mechanism to
/// use; it does not get it by accident here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwedNotice {
    /// The rendered notice, exactly as the arc that ended the drive produced it.
    pub text: String,
    /// When it was first owed — the ceiling's anchor, and **absolute** for
    /// [`DriveEntry::started_ms`]'s reason: a stored elapsed time is stale the
    /// instant it is written and meaningless across a restart.
    pub owed_ms: u64,
    /// How many delivery attempts have failed. An audit detail, never the
    /// bound: the tick's cadence is the shared poll loop's, so an attempt count
    /// would be a different real duration on every fleet.
    #[serde(default)]
    pub failures: u32,
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
    ///
    /// **S3 must persist `entry.head = facts.head` whenever a tick resolves the
    /// head, and this obligation is written here rather than left to be
    /// inferred, because nothing in this module can enforce it and every
    /// individual call stays correct while it is violated.** [`decide`] reads
    /// this field only to compare it against the live head — arc 6 in
    /// `review-wait`, arc 7 in `fix-wait` — so a tick that records the head
    /// once at `drive_review` time and never again makes that comparison
    /// permanently true: the drive takes arc 6 to `ci-wait`, goes green, comes
    /// back to `review-wait`, and takes arc 6 again, forever. The failure is
    /// *emergent at the seam*, which is why no test in this crate catches it.
    ///
    /// **The empty-*live*-head case is handled in [`decide`] itself, and the
    /// earlier version of this paragraph got it wrong in the dangerous
    /// direction — it claimed arc 6's `!facts.head.is_empty()` guard meant a
    /// failed head read "does not thrash the state machine".** That guard only
    /// stops arc 6 from *firing*; it does not stop the tick continuing past it
    /// into `first_stale_lane` (where `ReviewVerdict::reviewed("")` is false for
    /// every real verdict head) and [`lane_open_for`] (which refuses every
    /// record briefed at a real head), which together return `OpenLane{k}` on
    /// every tick — a reviewer spawned per tick, with each brief re-arming that
    /// lane's `spawned_ms` so `lane-stalled` can never fire. [`decide`] now
    /// returns `Wait` when the live head is empty, before any state is
    /// dispatched, and the drive stays bounded by `drive-stalled`.
    ///
    /// A *stored* head that is empty while the live head reads fine is the
    /// ordinary first-tick state of a fresh entry and is not a defect: arc 6
    /// then moves the drive to `ci-wait`, which is where the head is recorded.
    #[serde(default)]
    pub head: String,
    /// The PR body digest last seen, the same #565 digest the gate reads.
    #[serde(default)]
    pub body_digest: String,
    /// The worker session, **as resolved** by `resolve_session_ref` at
    /// `drive_review` time and never the caller's raw string (§3.2).
    #[serde(default)]
    pub worker_session: String,
    /// The **agent id** the worker's resumed session is running in, as of the
    /// last hand-back. [`LaneRecord::agent`]'s twin, for §7's reason:
    /// interception is keyed on the agent, and a `report` arrives carrying one.
    ///
    /// Empty until the drive has handed back at least once, which is correct
    /// rather than incidental: before the first hand-back there is no resumed
    /// worker pane for this drive to own, and a drive must never consume the
    /// traffic of a worker it did not resume.
    #[serde(default)]
    pub worker_agent: String,
    /// Every EARLIER pane this drive resumed the worker into, oldest first —
    /// [`LaneRecord::prior_agents`]'s twin, and **the field #1871 B2 was**.
    ///
    /// `worker_agent` was one slot, so the second hand-back evicted the first
    /// pane's id from the only key §7 has. Measured on the dogfood run: the
    /// drive resumed the worker into `w-1715`, whose `report(progress)` was
    /// consumed correctly; it then handed back again into `w-1716`, and
    /// `w-1715` — still running, still on the same session and the same PR —
    /// had both of its `report(done)` calls delivered to the orchestrator as if
    /// nobody owned it. Every pane in that list is the SAME worker session; a
    /// drive that owns the worker owns each pane it resumed that worker into.
    ///
    /// Cleared with `worker_agent` when a resume re-points the drive at a
    /// DIFFERENT session, for that field's reason: those panes are a worker
    /// this drive no longer owns.
    #[serde(default)]
    pub prior_worker_agents: Vec<String>,
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
    /// that reach `fix-wait` — and by nothing else.
    ///
    /// **Zero means the drive has never been handed back.** It does not also
    /// mean "written before this field existed", and the distinction is worth
    /// a sentence because the second reading looks plausible and would license
    /// a guard that guards nothing.
    ///
    /// `now - 0` clears any timeout trivially, so a zero read while the entry
    /// is in `fix-wait` *would* park the drive on a false `held(fix-stalled)`.
    /// No such entry can exist. Nothing writes `review_drives.json` before
    /// S3's tick, and S3 ships alongside this module — so the field is present
    /// in the first file ever written, and there is no build whose output
    /// lacks it. §5.2's read tolerance makes the older *shape* parse; it does
    /// not conjure a writer that produced one. A `fix-wait` entry therefore
    /// always carries an anchor stamped by
    /// [`advance`](DriveEntry::advance) on arc 3 or arc 5, and the comparison
    /// in [`decide`] is left plain deliberately: a defensive `!= 0` here would
    /// have no reachable subject, and the next reader would take it as
    /// evidence that one exists.
    ///
    /// If a future change ever lets something else author an entry — a
    /// migration, an import, a hand-repaired file — that is the change that
    /// owes this field a decision, and this paragraph is the one it invalidates.
    #[serde(default)]
    pub fix_handback_ms: u64,
    /// When this drive last answered a worker's `report(progress)` in its own
    /// pane (#1959) — the bound on [`kickback_owed`](DriveEntry::kickback_owed).
    ///
    /// **Compared against `fix_handback_ms` rather than counted**, so the budget
    /// is one per HAND-BACK and renews on the next one without anything having
    /// to reset it: a worker that reports progress five times in one fix round
    /// is answered once, and a worker that does it again after the next
    /// hand-back is answered again. A counter would have needed a reset on
    /// arcs 3 and 5, which is a second thing to remember beside the anchor those
    /// arcs already stamp.
    ///
    /// Zero means "never answered", and reads correctly against a zero
    /// `fix_handback_ms` too: `0 < 0` is false, so a drive that has never handed
    /// back owes nothing.
    #[serde(default)]
    pub fix_kickback_ms: u64,
    /// The notice this entry owes the orchestrator's pane, and the reason
    /// retention may not drop it yet (#1857). See [`OwedNotice`].
    ///
    /// **Absent is the resting state, so it is not serialized when absent** —
    /// every entry that is not a terminal one mid-delivery carries nothing, and
    /// writing `"owed_notice": null` onto each of them would be noise in a file
    /// §5.2 publishes the shape of. A build that predates the field reads it as
    /// an unknown key into `extra` and rewrites it verbatim, which is the
    /// forward-compatibility promise §5.2 already makes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owed_notice: Option<OwedNotice>,
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
            worker_agent: String::new(),
            prior_worker_agents: Vec::new(),
            on_behalf_of: on_behalf_of.to_string(),
            lanes: Vec::new(),
            lane_index: 0,
            counters,
            started_ms: now_ms,
            fix_handback_ms: 0,
            fix_kickback_ms: 0,
            owed_notice: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> DriveState {
        self.state
    }

    /// The notice this entry owes a pane, if any (#1857).
    pub fn owed_notice(&self) -> Option<&OwedNotice> {
        self.owed_notice.as_ref()
    }

    /// Record `text` as owed by this entry, anchored at `now_ms` (#1857).
    ///
    /// Called at the moment the entry goes terminal, **inside the same
    /// load-decide-store the arc itself takes**, so the obligation is on disk
    /// before anything tries to deliver it. That ordering is the whole
    /// mechanism: a notice built and handed straight to a delivery is lost the
    /// moment the delivery answers `Err`, which is #1857.
    ///
    /// **The first write wins.** Re-owing an entry that is already owing would
    /// re-arm the ceiling clock, and a clock re-armed by the retry it bounds is
    /// the unbounded retry this exists to not be.
    pub fn owe_notice(&mut self, text: &str, now_ms: u64) {
        if self.owed_notice.is_none() {
            self.owed_notice =
                Some(OwedNotice { text: text.to_string(), owed_ms: now_ms, failures: 0 });
        }
    }

    /// The notice reached a pane. Clearing it is what lets retention prune —
    /// see [`prune_terminal`].
    pub fn notice_delivered(&mut self) {
        self.owed_notice = None;
    }

    /// One delivery attempt failed. Counts the attempt and **does not touch the
    /// ceiling anchor**, for [`owe_notice`](DriveEntry::owe_notice)'s reason.
    pub fn notice_delivery_failed(&mut self) {
        if let Some(n) = self.owed_notice.as_mut() {
            n.failures = n.failures.saturating_add(1);
        }
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
    /// `bump` is a **parameter rather than a separate call** for the same
    /// reason `reason` is checked here: a caller that could take arc 5 and
    /// forget the `review_rounds` increment would spend an unbounded number of
    /// review rounds against a bound of three, which is INVARIANT 9 defeated by
    /// an omission rather than by a decision. Pairing them in one signature
    /// means the transition and its cost cannot come apart.
    ///
    /// The counter moves **after** the transition is accepted, so a refused arc
    /// spends nothing.
    pub fn advance(
        &mut self,
        to: DriveState,
        reason: Option<HeldReason>,
        bump: Option<Counter>,
        now_ms: u64,
    ) -> Result<(), InvalidTransition> {
        if reason.is_some() != (to == DriveState::Held) {
            return Err(InvalidTransition { from: self.state, to });
        }
        self.state = transition(self.state, to)?;
        self.held_reason = reason;
        match bump {
            Some(Counter::ReviewRounds) => self.counters.review_rounds += 1,
            Some(Counter::CiAttempts) => self.counters.ci_attempts += 1,
            Some(Counter::RebaseAttempts) => self.counters.rebase_attempts += 1,
            None => {}
        }
        if to == DriveState::FixWait {
            self.fix_handback_ms = now_ms;
        }
        Ok(())
    }

    /// Apply a whole [`DriveStep`] — the form S3's tick uses, so the decision
    /// and its bookkeeping travel together and neither half can be applied
    /// alone.
    ///
    /// [`DriveStep::Wait`] is a no-op. [`DriveStep::OpenLane`] is **not**
    /// applied here and returns `Ok(())` unchanged: briefing a lane needs a
    /// spawned session id, which is exactly the I/O this module does not do —
    /// S3 calls [`open_lane`](DriveEntry::open_lane) with what the spawn
    /// returned.
    pub fn take(&mut self, step: &DriveStep, now_ms: u64) -> Result<(), InvalidTransition> {
        match step {
            DriveStep::Wait | DriveStep::OpenLane { .. } => Ok(()),
            DriveStep::Advance { to, held_reason, bump } => {
                self.advance(*to, *held_reason, *bump, now_ms)
            }
        }
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
    /// **The superseded pane is carried, not dropped** — see
    /// [`LaneRecord::prior_agents`].
    pub fn open_lane(
        &mut self,
        block: &str,
        session: &str,
        agent: &str,
        head: &str,
        body_digest: Option<&str>,
        now_ms: u64,
    ) {
        let prior = self.lane(block).map(|l| {
            let mut p = l.prior_agents.clone();
            p.push(l.agent.clone());
            p
        });
        let extra = self
            .lane(block)
            .map(|l| l.extra.clone())
            .unwrap_or_default();
        self.lanes.retain(|l| l.block != block);
        self.lanes.push(LaneRecord {
            block: block.to_string(),
            session: session.to_string(),
            agent: agent.to_string(),
            prior_agents: retain_panes(prior.unwrap_or_default(), agent),
            last_verdict: None,
            at_head: String::new(),
            briefed_head: head.to_string(),
            briefed_digest: body_digest.unwrap_or_default().to_string(),
            spawned_ms: now_ms,
            extra,
        });
    }

    /// **Does this drive still owe its worker a kick-back for the CURRENT
    /// hand-back?** (#1959)
    ///
    /// A `report(progress)` in `fix-wait` is a delivery the drive consumed and
    /// cannot act on: it moves nothing (a drive turns on the head, the checks
    /// and the verdict files) and it is exactly what a worker sends when it
    /// believes it has finished and has reached for the wrong word — a
    /// body-only fix has nothing to push and no new checks, so "report when the
    /// checks are green" has no trigger. Swallowing it silently is #1857's
    /// shape one arm over: the drive sat until the watchdog woke the
    /// orchestrator.
    ///
    /// The answer is one line typed into the worker's OWN pane, which costs the
    /// orchestrator nothing. It is bounded to one per hand-back rather than one
    /// per report, so a chatty worker cannot turn its own progress reports into
    /// a stream of prompts — an unbounded emission driven by a signal the drive
    /// does not control is the mirror image of the unbounded SUPPRESSION rule,
    /// and wants the same answer.
    ///
    /// **Only in `fix-wait`.** In any other state there is no hand-back to be
    /// waiting on and nothing the worker was asked for.
    pub fn kickback_owed(&self) -> bool {
        self.state == DriveState::FixWait && self.fix_kickback_ms < self.fix_handback_ms
    }

    /// Record that this drive answered its worker at `now_ms` — see
    /// [`kickback_owed`](DriveEntry::kickback_owed).
    ///
    /// **Stamped at no earlier than the hand-back it answers**, which is what
    /// makes the budget hold across a backwards clock step (rev-std round 2,
    /// premortem 1). `now_ms` is wall-clock: a host NTP correction between the
    /// hand-back and the worker's `report(progress)` writes a stamp that never
    /// overtakes `fix_handback_ms`, `kickback_owed` stays true, and the tick
    /// re-emits the kick-back on EVERY tick for as long as the progress signal
    /// stands — the unbounded emission the bound exists to prevent, arriving
    /// through the bound itself. The clamp costs a `max` and needs no reset on
    /// the arcs into `fix-wait`, so it keeps the property that made the
    /// comparison preferable to a counter in the first place.
    pub fn record_kickback(&mut self, now_ms: u64) {
        self.fix_kickback_ms = now_ms.max(self.fix_handback_ms);
    }

    /// Record that this drive has resumed its worker into `agent` — the
    /// hand-back's twin of [`open_lane`](DriveEntry::open_lane), and the reason
    /// the assignment is a method rather than a field write at the call site.
    ///
    /// A hand-back that assigned `worker_agent` directly is exactly how #1871 B2
    /// happened: the write looked total and was, and the pane it overwrote went
    /// on running with the drive no longer able to recognise it. Everything a
    /// new pane must do to the old one is here, once.
    pub fn record_worker_pane(&mut self, agent: &str) {
        let mut prior = std::mem::take(&mut self.prior_worker_agents);
        prior.push(std::mem::take(&mut self.worker_agent));
        self.prior_worker_agents = retain_panes(prior, agent);
        self.worker_agent = agent.to_string();
    }

    /// Forget every worker pane this drive resumed — the resume-onto-a-different
    /// session case, where those panes belong to a worker it no longer owns.
    pub fn forget_worker_panes(&mut self) {
        self.worker_agent = String::new();
        self.prior_worker_agents.clear();
    }

    /// Drop superseded panes that are no longer alive, and answer whether
    /// anything was dropped.
    ///
    /// **This is what BOUNDS the superseded lists, and a size cap is what it
    /// replaces.** A cap has to choose a victim, and the only orderings
    /// available to it are by age — which is precisely #1871 B2 again: the
    /// OLDEST superseded pane is a pane that is still running, still on this
    /// session, still able to `report`, and evicting it un-owns it exactly as
    /// the single slot did. A drive resumed enough times would reproduce the
    /// defect this record exists to fix, and it would do so under the usage that
    /// produced B2 in the first place. So nothing is evicted for being old.
    ///
    /// **A dead pane is safe to forget, and provably so rather than plausibly.**
    /// `resolve_token` refuses a caller whose agent is `Dead` and has no entry
    /// for an agent that is gone, so such a pane cannot reach the MCP seam at
    /// all — there is no traffic left for this drive to fail to own. That makes
    /// liveness the one eviction rule that cannot re-open B2, and it is a real
    /// bound rather than an arbitrary number: what is retained is at most the
    /// panes this group can have alive at once, which the live-delegate cap
    /// already limits.
    ///
    /// `is_live` is injected because liveness is the registry's fact and this
    /// crate is Tauri-free. A predicate that cannot answer must answer **true** —
    /// "we could not check" is not "it is dead", and the fail-closed direction
    /// here is to KEEP a pane, since keeping one costs a string and dropping a
    /// live one costs the leak.
    pub fn forget_dead_panes(&mut self, is_live: &dyn Fn(&str) -> bool) -> bool {
        let before = self.prior_worker_agents.len()
            + self.lanes.iter().map(|l| l.prior_agents.len()).sum::<usize>();
        self.prior_worker_agents.retain(|a| is_live(a));
        for l in self.lanes.iter_mut() {
            l.prior_agents.retain(|a| is_live(a));
        }
        let after = self.prior_worker_agents.len()
            + self.lanes.iter().map(|l| l.prior_agents.len()).sum::<usize>();
        before != after
    }

    /// Every pane this drive still owns, superseded ones included
    /// — what an exit owes the orchestrator (#1871 B3), and the same population
    /// [`driven_role`](DriveEntry::driven_role) recognises.
    ///
    /// Kept as a second walk rather than as `driven_role`'s implementation: that
    /// one has to answer **which** pane and whether it is current, and folding
    /// the two would either lose that distinction or make the common
    /// single-lookup path build a vector per incoming `report`.
    ///
    /// Ordered worker-first then lane by lane, each oldest-first, so the list
    /// reads as the history it is. Deduplicated on the way in — see
    /// [`retain_panes`].
    pub fn owned_panes(&self) -> Vec<(String, DrivenRole)> {
        let mut out: Vec<(String, DrivenRole)> = Vec::new();
        for a in self.prior_worker_agents.iter().chain(std::iter::once(&self.worker_agent)) {
            if !a.is_empty() {
                out.push((a.clone(), DrivenRole::Worker));
            }
        }
        for l in &self.lanes {
            for a in l.prior_agents.iter().chain(std::iter::once(&l.agent)) {
                if !a.is_empty() {
                    out.push((a.clone(), DrivenRole::Lane(l.block.clone())));
                }
            }
        }
        out
    }

    /// The drive's age (§2.2's `drive-stalled` measure).
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.started_ms)
    }

    /// Record what a lane's verdict file said this tick, onto that lane.
    ///
    /// **A record of what was READ, never a gate input.** [`LaneRecord::last_verdict`]
    /// says so, and nothing decides from it — the live verdict file is re-read
    /// every tick through the same parser the gate reads. Two things need it
    /// written all the same, and neither is a decision:
    ///
    /// - `review_drive_status()` shows it, and a status view that never showed a
    ///   verdict would be reporting on a drive it could not describe;
    /// - [`at_head`](LaneRecord::at_head) is what distinguishes a lane that has
    ///   **answered** from one that has only been **asked**, which is the whole
    ///   reason §5.2 keeps it apart from `briefed_head`. Without it a re-briefed
    ///   lane looks like a first-time lane forever, and the delta brief §5.5
    ///   exists for — the line an orchestrator typed by hand nine times on one
    ///   PR — is unreachable.
    ///
    /// Returns whether anything changed, so a tick that only observed a
    /// still-unanswered lane does not rewrite the file for it.
    pub fn record_verdict_seen(
        &mut self,
        block: &str,
        verdict: Verdict,
        at_head: &str,
    ) -> bool {
        let Some(rec) = self.lanes.iter_mut().find(|l| l.block == block) else { return false };
        if rec.last_verdict == Some(verdict) && rec.at_head == at_head {
            return false;
        }
        rec.last_verdict = Some(verdict);
        rec.at_head = at_head.to_string();
        true
    }

    /// Which side of this drive `agent_id` is, if any — §7's interception key.
    ///
    /// **The key is the agent, never text a delegate typed**, and this method is
    /// where that is true rather than merely intended: it compares against ids
    /// orrerix minted at spawn and recorded here, so a delegate cannot name a PR
    /// number and route its own report to the driver, nor name someone else's
    /// and route theirs.
    ///
    /// **An empty id never matches**, which is what makes an unrecorded pane
    /// fail closed: `""` is what an unresumed worker and a pre-field lane record
    /// both carry, and an empty caller id is not a thing the MCP seam produces
    /// anyway. Without this guard a drive with no hand-back yet would own every
    /// caller whose id failed to resolve.
    pub fn driven_role(&self, agent_id: &str) -> Option<DrivenPane> {
        if agent_id.is_empty() {
            return None;
        }
        if self.worker_agent == agent_id {
            return Some(DrivenPane { role: DrivenRole::Worker, current: true });
        }
        if self.prior_worker_agents.iter().any(|a| a == agent_id) {
            return Some(DrivenPane { role: DrivenRole::Worker, current: false });
        }
        self.lanes.iter().find_map(|l| {
            if l.agent == agent_id {
                Some(DrivenPane { role: DrivenRole::Lane(l.block.clone()), current: true })
            } else if l.prior_agents.iter().any(|a| a == agent_id) {
                Some(DrivenPane { role: DrivenRole::Lane(l.block.clone()), current: false })
            } else {
                None
            }
        })
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

/// §5.2's retention: **drop terminal entries whose notice has been delivered,
/// keep parked ones.** Returns what was dropped, so the caller can audit
/// `rd-pruned` per entry — and `rd-notice-dropped` for the one case that leaves
/// with a notice still owing.
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
/// **§5.2's ordering rule is enforced HERE rather than asked of the caller**
/// (#1857). This function used to drop every terminal entry and say in its own
/// doc that the caller owned "prune once the notice has been delivered" — which
/// no caller implemented, so a drive whose final notice failed to deliver was
/// pruned anyway and ended with no line in the pane and no record that could
/// produce one.
///
/// It is enforced rather than delegated because a rule stated on one side of a
/// call and satisfied on neither is what #1857 actually was: this function reads
/// the entry, so it is the thing that can read [`DriveEntry::owed_notice`], and
/// there is no reason for a second party to hold half the condition. What the
/// caller still owns is delivery itself — it must clear the notice
/// ([`DriveEntry::notice_delivered`]) on an attempt that succeeded, and that
/// obligation *is* enforceable from here, because an entry it forgets simply
/// stays.
///
/// **The ceiling is the third condition, and it is why this takes a clock.** An
/// entry whose notice can never be delivered would otherwise be retained
/// forever; at `retention_ms` past [`OwedNotice::owed_ms`] it is dropped anyway,
/// with its text handed back in [`Pruned::undelivered`] so the caller can put it
/// on the audit log. See [`NOTICE_RETENTION_MS`] for the value and the argument.
///
/// A clock that went backwards yields a saturated zero — "not yet" — so the
/// failure direction is keeping the record, never dropping it early.
///
/// Two reasons that are not merely hygiene. Unpruned terminal entries would
/// flow through `review_drive_status()` into the orchestrator's resident
/// context, which is the cost this whole feature exists to remove; and they
/// would make §5.1's `already-driven` refuse every re-drive of a PR forever.
///
/// **Neither reason is weakened by holding one back for a notice.** Both those
/// surfaces already filter on `is_terminal()` — `review_drive_status` lists only
/// live drives and `is_driven` answers `is_live()` — so a retained terminal
/// entry reaches no orchestrator context and refuses no re-drive. What it does
/// do is sit in the file, which is what the ceiling bounds.
///
/// **`held` entries are never pruned**, and that is the whole reason §2.1 makes
/// `held` parked rather than terminal: §2.3's resume needs their counters, and
/// pruning one would silently grant three fresh review rounds. A parked drive
/// leaves this file by being resumed to completion or cancelled, never by
/// retention.
pub fn prune_terminal(
    state: &mut ReviewDrivesState,
    now_ms: u64,
    retention_ms: u64,
) -> Vec<Pruned> {
    // One predicate, read twice — by the collect and by the retain — so the two
    // can never answer differently. A `retain` whose condition is the hand-written
    // negation of the collect's is where a third condition gets added to one and
    // not the other.
    let droppable = |e: &DriveEntry| -> Option<Option<String>> {
        if !e.state().is_terminal() {
            return None;
        }
        match e.owed_notice() {
            // Delivered (or never owed one at all — an entry that went terminal
            // before this field existed). §5.2's ordering rule, satisfied.
            None => Some(None),
            // Past the ceiling. Dropped, and its text goes to the audit log.
            Some(n) if now_ms.saturating_sub(n.owed_ms) >= retention_ms => {
                Some(Some(n.text.clone()))
            }
            // Owing, inside the ceiling: kept, so a later tick re-attempts it.
            Some(_) => None,
        }
    };
    let pruned: Vec<Pruned> = state
        .entries
        .iter()
        .filter_map(|e| droppable(e).map(|undelivered| Pruned { pr: e.pr, undelivered }))
        .collect();
    state.entries.retain(|e| droppable(e).is_none());
    pruned
}

/// One entry §5.2's retention dropped, and whether its notice ever reached a
/// pane (#1857).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pruned {
    pub pr: u64,
    /// `Some(text)` when the entry left at the retention **ceiling** with its
    /// notice still owing. The caller audits that text, so the record that could
    /// still produce the line outlives the entry that owed it — which is the
    /// half of #1857 a bound would otherwise reintroduce.
    ///
    /// `None` is the ordinary exit: the notice was delivered (or the entry never
    /// owed one), and nothing is lost.
    pub undelivered: Option<String>,
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
    // **Only the WORKER produces one of these**, and the name is the contract
    // rather than a label: arc 8 is "`report(done)` with the head unchanged" and
    // `held(worker-blocked)` names a worker's session, so feeding a reviewer
    // lane's report in here moves the drive on a hand-back that never happened.
    // Both sides reach the `report` MCP arm and a reviewer's `approved` resolves
    // to the same `done` word, so the arm decides on `DrivenRole` — the role is
    // what makes this type's name true (§7).
    /// Nothing yet.
    Silent,
    /// `report(done)`.
    Done,
    /// `report(blocked)`.
    Blocked,
    /// The fix could not be handed back to this drive's worker — see
    /// [`HeldReason::WorkerUnresumable`] for the causes this covers.
    ///
    /// **This is not a drive-time check, and the note is honest about why.**
    /// §5.1: a full, well-shaped session id this group never recorded takes
    /// `resolve_session_ref`'s `is_full_session_id` passthrough arm and is
    /// *accepted* by `drive_review`, so its unresumability surfaces here, at
    /// the first hand-back, possibly hours on. Resolving is not the same as
    /// proving resumable, and v1 does not prove it.
    ///
    /// **It is also produced AFTER a hand-back that succeeded** (#1961): the
    /// registry raises it when the pane the drive resumed exits in `fix-wait`
    /// with nothing reported, which is a resume that "worked" and then died on
    /// `Invalid session ID`. Before that the drive waited a full fix timeout on
    /// a process that was already gone.
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

/// Whether a lane's recorded verdict was recorded **about the revision now on
/// the PR** — the `(head, digest)` key, asked of the verdict WORD-BLIND.
///
/// §2.1's first carried-over property is written for a `pass` — *"a `pass` bound
/// to an old head, or to an old body digest, is not a pass"* — and the binding
/// rule underneath it is not about `pass` at all: **a verdict is bound to the
/// revision it reviewed**, so once the head moves the lane owes a fresh one
/// whatever word it last said. Reading the currency test only on the `pass` side
/// is what let a `fail` recorded three commits ago stay authoritative, and
/// [`decide_review_wait`] re-route it as though a reviewer had just spoken
/// (#1871 B1).
///
/// Asked with [`ReviewVerdict`]'s own methods rather than with comparisons
/// written here, because §4 makes the driver a reader of the gate and never a
/// third implementation of it. Both halves of the #565 asymmetry come along for
/// free that way: [`ReviewVerdict::reviewed`] is false for an empty head, so an
/// unbound verdict reads as stale and fails closed; and
/// [`ReviewVerdict::body_changed`] answers `None` when the drift cannot be
/// *known* — a verdict with no digest, or a body that could not be read — which
/// is not `Some(true)` and so does not stale the verdict. "We could not check"
/// and "it changed" are different answers, and only one of them may re-open a
/// lane.
pub fn lane_verdict_is_current(
    verdict: Option<&ReviewVerdict>,
    head: &str,
    body_digest: Option<&str>,
) -> bool {
    let Some(v) = verdict else { return false };
    v.reviewed(head) && v.body_changed(body_digest) != Some(true)
}

/// Whether a lane's recorded verdict is a `pass` that still counts at this
/// `(head, digest)` — the currency rule above, plus the word.
///
/// Kept as its own function because [`first_stale_lane`] asks a different
/// question from [`decide_review_wait`]'s match: "has this lane's pass settled
/// the revision in front of us" versus "does this lane have anything to say
/// about it". A stale `fail` answers **no** to the first and **no** to the
/// second, and before #1871 only the first was asked.
pub fn lane_pass_is_current(
    verdict: Option<&ReviewVerdict>,
    head: &str,
    body_digest: Option<&str>,
) -> bool {
    verdict.is_some_and(|v| v.verdict == Verdict::Pass)
        && lane_verdict_is_current(verdict, head, body_digest)
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
    // **The bounds are clamped HERE, on the values actually read.** §2.3's
    // ranges are a capability boundary, not input hygiene: a repo's `driver:`
    // block may run a tighter loop than INVARIANT 9 and may never run a looser
    // one, because `doc/design/workflows.md`'s closure is that a workflow file
    // selects from what loomux permits and never widens it. S2 clamps as it
    // parses; this clamps again, and the second is not redundant, because this
    // is a `pub fn` over a plain value type that any caller in any crate can
    // reach without going through S2's parser. A boundary that holds only when
    // the expected caller is upstream is not a boundary.
    let limits = &limits.clamped();
    if entry.age_ms(facts.now_ms) >= minutes_ms(limits.drive_timeout_minutes) {
        return DriveStep::held(HeldReason::DriveStalled);
    }
    // **An unresolved head is not a head, and acting on one is an unbounded
    // spawn loop.** Two of the four states below read `facts.head`, and the
    // guard is here rather than in each of them because it is the *drive* that
    // has no revision to act on, not those two states in particular:
    // `review-wait` compares it against the recorded head and keys a lane brief
    // on it, `fix-wait` compares it for arc 7. `ci-wait` and `gate-check` never
    // read it at all — they would be harmless to dispatch, and returning `Wait`
    // for them too is the conservative reading of "orrerix could not resolve
    // this PR's head this tick", not a claim that they would misbehave.
    // `review-wait` is the dangerous one: the arc-6 guard skips itself when
    // `facts.head` is empty, so the tick falls through to `first_stale_lane`,
    // where `ReviewVerdict::reviewed("")` is false for every real verdict head,
    // and then to `lane_open_for`, which refuses every record briefed at a real
    // head — yielding `OpenLane{0}` on EVERY tick. Worse, each brief re-arms
    // that lane's `spawned_ms`, so `lane-stalled` can never fire and the loop
    // defeats the very bound meant to catch it. §8's posture settles it: an
    // unknown is never a fact, and the drive stays bounded by `drive-stalled`
    // above, which needs no head at all.
    if facts.head.is_empty() {
        return DriveStep::Wait;
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
    // **A verdict decides only if it was recorded about THIS revision**, and the
    // currency test is asked here rather than inside the arms so no future word
    // can be added below without it (#1871 B1).
    //
    // The defect this closes: a lane's `fail` recorded at the head the worker has
    // since fixed stayed authoritative for ever. The drive took arc 7 out of
    // `fix-wait` on the new head, went green, came back here — and read the SAME
    // stale `fail`, spent a review round on it, and handed the worker back its
    // own already-addressed findings as "attempt 2". Three passes reached the
    // bound with no re-review having happened at all. Nothing re-opened the lane
    // because nothing below ever reached [`lane_open_for`]: the `Fail` arm
    // answered first, and it answered from a commit that no longer described the
    // PR.
    //
    // **Word-blind on purpose.** `escalate` is the same shape — an escalation of
    // a revision that no longer exists is not a judgment anyone is being asked
    // for — and so is a `pass`, which [`first_stale_lane`] has already filtered
    // for currency before this line is reached. Treating a stale verdict as
    // ABSENT is what puts the lane back on the `Some(Verdict::Pass) | None` arm,
    // where [`lane_open_for`] decides between re-briefing it and waiting for a
    // brief already out at this revision — which is the re-open the head change
    // owed and never got.
    let current = lane
        .verdict
        .as_ref()
        .filter(|v| lane_verdict_is_current(Some(v), &facts.head, digest));
    match current.map(|v| v.verdict) {
        // A lane recorded `escalate` at this revision: an LLM judgment call, and
        // §3 says the driver never makes one.
        Some(Verdict::Escalate) => DriveStep::held(HeldReason::Escalate),
        // Arc 5, spending a review round — or parking, when the budget is gone.
        Some(Verdict::Fail) => {
            if counter_exhausted(entry.counters.review_rounds, limits.max_review_rounds) {
                DriveStep::held(HeldReason::ReviewLimit)
            } else {
                DriveStep::spend(DriveState::FixWait, Counter::ReviewRounds)
            }
        }
        // Either no verdict bound to this revision, or a `pass` that no longer
        // stands here — which are the same thing to this state: the lane is
        // outstanding and must be briefed for this revision. Whether it
        // *already* was is what [`lane_open_for`] answers, on the full
        // (head, digest) key.
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
    fn the_held_reasons_are_the_notes_thirteen() {
        assert_eq!(HeldReason::ALL.len(), 13);
        // §2.2: "There are **fifteen**" exits back to the LLM orchestrator —
        // the thirteen holds plus `satisfied` and `cancelled`.
        let exits =
            HeldReason::ALL.len() + DriveState::ALL.iter().filter(|s| s.is_terminal()).count();
        assert_eq!(exits, 15);
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

    #[test]
    fn a_hold_without_a_reason_and_a_reason_without_a_hold_are_both_refused() {
        let mut e = entry_at(DriveState::CiWait);
        // A hold with nothing to put in its notice or its `rd-held` line.
        assert!(e.advance(DriveState::Held, None, None, 2_000).is_err());
        // A reason riding an arc that is not a hold would survive into
        // `review_drive_status()` as a claim about a drive that is not parked.
        assert!(e
            .advance(DriveState::ReviewWait, Some(HeldReason::Escalate), None, 2_000)
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
        e.advance(DriveState::Held, Some(HeldReason::CiLimit), None, 2_000)
            .unwrap();
        assert_eq!(e.held_reason, Some(HeldReason::CiLimit));
        // Arc 11 — `drive_review` resumes it. §2.3: the same counters, because
        // a fresh entry would reset them and "yours count too" forbids that.
        e.advance(DriveState::CiWait, None, None, 3_000).unwrap();
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
        e.advance(DriveState::ReviewWait, None, None, 5_000).unwrap();
        assert_eq!(e.fix_handback_ms, 0, "a non-fix-wait arc must not stamp it");
        e.advance(DriveState::FixWait, None, None, 7_000).unwrap();
        assert_eq!(e.fix_handback_ms, 7_000);
        e.advance(DriveState::CiWait, None, None, 9_000).unwrap();
        assert_eq!(e.fix_handback_ms, 7_000, "leaving fix-wait must not re-stamp");
        // ...and the age anchor is untouched by every one of those advances.
        assert_eq!(e.started_ms, 1_000);
        assert_eq!(e.age_ms(9_000), 8_000);
    }

    #[test]
    fn re_briefing_a_lane_re_arms_its_stall_clock() {
        let mut e = entry_at(DriveState::ReviewWait);
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000);
        assert_eq!(e.lanes.len(), 1);
        e.open_lane("rev-std", "s1", "rev-1", "head-b", Some("d1"), 9_000);
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
    fn a_repo_cannot_raise_invariant_9_by_handing_decide_a_wider_bound() {
        // THE capability test, and it is deliberately built from RAW limits.
        // Every other `decide` fixture uses `DriveLimits::default()`, which is
        // already inside every range — so the axis §2.3 makes load-bearing was
        // constant across the whole suite, and the property read green under an
        // implementation that did not have it (`clamped()` was called only from
        // tests, and `decide` used the raw values). A fixture that cannot vary
        // the axis cannot witness it.
        //
        // `doc/design/workflows.md`: a workflow file selects from what loomux
        // permits and never widens it. A `driver:` block asking for nine review
        // rounds must get three at the decision, not nine.
        let wide = DriveLimits {
            max_review_rounds: 9,
            max_ci_attempts: 9,
            max_rebase_attempts: 9,
            ..DriveLimits::default()
        };
        assert_eq!(wide.max_review_rounds, 9, "the fixture really is over-bound");

        // Review rounds: at 3 spent, a further `fail` must PARK, not hand back.
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.counters.review_rounds = MAX_ROUNDS_CEILING;
        let failing = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Fail), "head-a", "d1")]),
            ..facts_at("head-a")
        };
        assert_eq!(
            decide(&e, &failing, &wide),
            DriveStep::held(HeldReason::ReviewLimit),
            "a repo file must not buy a fourth review round"
        );

        // CI attempts, same shape.
        let mut c = entry_at(DriveState::CiWait);
        c.counters.ci_attempts = MAX_ROUNDS_CEILING;
        let red = DriveFacts { ci: CiObservation::Red, ..facts_at("head-a") };
        assert_eq!(
            decide(&c, &red, &wide),
            DriveStep::held(HeldReason::CiLimit)
        );

        // Rebases: the ceiling is 1, so a second conflict parks however wide
        // the file asked to be.
        let mut r = entry_at(DriveState::CiWait);
        r.counters.rebase_attempts = MAX_REBASE_CEILING;
        let conflict = DriveFacts { ci: CiObservation::Conflicting, ..facts_at("head-a") };
        assert_eq!(
            decide(&r, &conflict, &wide),
            DriveStep::held(HeldReason::RebaseLimit)
        );

        // The negative control: the same raw fixture one under each ceiling
        // still hands back, so the assertions above are the clamp biting and
        // not `decide` refusing everything.
        let mut ok = entry_at(DriveState::ReviewWait);
        ok.head = "head-a".into();
        ok.counters.review_rounds = MAX_ROUNDS_CEILING - 1;
        assert_eq!(
            decide(&ok, &failing, &wide),
            DriveStep::spend(DriveState::FixWait, Counter::ReviewRounds)
        );
    }

    #[test]
    fn an_unresolved_head_stops_the_tick_instead_of_spawning_a_reviewer() {
        // The failed head read. Without the guard in `decide`, arc 6's own
        // `!facts.head.is_empty()` check skips itself, `first_stale_lane` finds
        // every pass stale against "" (`reviewed("")` is false for any real
        // verdict head), and `lane_open_for` refuses every record briefed at a
        // real head — so the tick returns OpenLane{0} and S3 spawns a reviewer.
        // EVERY tick. And because each brief re-arms `spawned_ms`,
        // `lane-stalled` never fires: the loop disables the bound meant to
        // catch it.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000);
        let blind = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Pass), "head-a", "d1")]),
            head: String::new(),
            ..facts_at("head-a")
        };
        assert_eq!(
            decide(&e, &blind, &limits),
            DriveStep::Wait,
            "an unresolved head must not brief a lane"
        );

        // Not a hold either — a `gh` blip is not a stall, and §8 backs off.
        // The same entry with the head readable still makes progress, which is
        // the control that stops this passing by refusing everything.
        assert_eq!(
            decide(&e, &facts_at("head-a"), &limits),
            DriveStep::to(DriveState::GateCheck)
        );

        // ...and declining costs no boundedness: the age bound needs no head.
        let aged = DriveFacts {
            now_ms: 1_000 + minutes_ms(limits.drive_timeout_minutes),
            head: String::new(),
            ..facts_at("head-a")
        };
        assert_eq!(
            decide(&e, &aged, &limits),
            DriveStep::held(HeldReason::DriveStalled)
        );
    }

    #[test]
    fn a_transition_and_its_cost_cannot_come_apart() {
        // `advance` took a reason but not a bump, so a caller could take arc 5
        // and forget the increment — INVARIANT 9 defeated by an omission.
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.advance(DriveState::FixWait, None, Some(Counter::ReviewRounds), 2_000)
            .unwrap();
        assert_eq!(e.counters.review_rounds, 1);
        assert_eq!(e.counters.ci_attempts, 0, "only the named counter moves");

        // A REFUSED arc spends nothing — the bump lands after the transition is
        // accepted, never before.
        let mut t = entry_at(DriveState::Satisfied);
        let before = t.counters.clone();
        assert!(t
            .advance(DriveState::CiWait, None, Some(Counter::CiAttempts), 3_000)
            .is_err());
        assert_eq!(t.counters, before, "a refused transition costs nothing");

        // `take` applies a whole step, so the tick cannot apply one half.
        let mut s = entry_at(DriveState::CiWait);
        let step = DriveStep::spend(DriveState::FixWait, Counter::CiAttempts);
        s.take(&step, 4_000).unwrap();
        assert_eq!(s.state(), DriveState::FixWait);
        assert_eq!(s.counters.ci_attempts, 1);
        // OpenLane needs a spawned session id, so `take` leaves it to S3.
        let mut o = entry_at(DriveState::ReviewWait);
        o.take(&DriveStep::OpenLane { index: 0 }, 5_000).unwrap();
        assert_eq!(o.state(), DriveState::ReviewWait);
        assert!(o.lanes.is_empty());
    }

    #[test]
    fn a_partial_counter_block_refuses_the_file() {
        // The block being mandatory is worth nothing if its contents are
        // optional: `"counters": {}` would parse to three zeros, which is the
        // full fresh budget the required-block rule exists to deny.
        for partial in [
            r#""counters": {},"#,
            r#""counters": { "review_rounds": 2 },"#,
            r#""counters": { "ci_attempts": 1, "rebase_attempts": 0 },"#,
        ] {
            let bad = NOTE_EXAMPLE.replace(
                r#""counters": { "review_rounds": 1, "ci_attempts": 0, "rebase_attempts": 0 },"#,
                partial,
            );
            assert!(bad.contains(partial), "the mutation must actually land");
            assert!(
                matches!(parse_state(&bad), Err(StateError::Malformed(_))),
                "{partial} must refuse"
            );
        }
        // The complete block still parses, so the loop above is not refusing
        // everything.
        assert!(parse_state(NOTE_EXAMPLE).is_ok());
    }

    #[test]
    fn interception_is_keyed_on_the_agent_and_an_unrecorded_pane_fails_closed() {
        // §7's first bounding property: "It is keyed on the agent, never on a
        // `ref` string a delegate typed, because a delegate that could choose
        // whether its report reaches the orchestrator by naming a PR number is
        // a delegate that can route around the orchestrator."
        let mut e = entry_at(DriveState::ReviewWait);
        e.open_lane("rev-std", "s1", "rev-4", "head-a", Some("d1"), 1_000);
        e.worker_agent = "w-7".into();

        assert_eq!(e.driven_role("rev-4"), Some(lane_pane("rev-std", true)));
        assert_eq!(e.driven_role("w-7"), Some(worker_pane(true)));
        // A delegate this drive did not spawn is not this drive's, however
        // plausible its id: nothing is inferred from a prefix or a role.
        assert_eq!(e.driven_role("rev-5"), None);
        assert_eq!(e.driven_role("w-70"), None);
        assert_eq!(e.driven_role("orch-1"), None);

        // The fail-closed half. A drive that has never handed back carries an
        // empty `worker_agent`, and a lane recorded before the field existed
        // carries an empty `agent`. Neither may own a caller — without the
        // guard, an empty id would match and the drive would consume the
        // traffic of a delegate it cannot prove it spawned.
        let mut fresh = entry_at(DriveState::CiWait);
        assert_eq!(fresh.worker_agent, "", "a fresh drive has resumed nobody");
        assert_eq!(fresh.driven_role(""), None);
        fresh.open_lane("rev-std", "s1", "", "head-a", Some("d1"), 1_000);
        assert_eq!(fresh.driven_role(""), None, "an unrecorded pane owns no caller");
        // ...and the positive control, so the four `None`s above are the guard
        // and not a method that answers `None` to everything.
        fresh.worker_agent = "w-1".into();
        assert_eq!(fresh.driven_role("w-1"), Some(worker_pane(true)));
    }

    fn worker_pane(current: bool) -> DrivenPane {
        DrivenPane { role: DrivenRole::Worker, current }
    }

    fn lane_pane(block: &str, current: bool) -> DrivenPane {
        DrivenPane { role: DrivenRole::Lane(block.into()), current }
    }

    /// **#1871 B2, at the layer that owns the key.** A drive that hands back or
    /// re-briefs twice must still recognise the pane it opened first: that pane
    /// is live, on the same session, working the same PR, and its `report` is
    /// exactly the traffic §7 exists to absorb. Before this the assignment was a
    /// single slot, so the second spawn evicted the first pane's id outright and
    /// its reports reached the orchestrator as if nobody owned it.
    ///
    /// The second half is the one the fix could get wrong in the other
    /// direction: owning a superseded pane must not mean TAKING ITS WORD. Each
    /// assertion therefore pins `current` as well as the role — a version that
    /// answered `current: true` for every owned pane would satisfy a role-only
    /// test and let a two-heads-stale `done` advance the drive.
    #[test]
    fn a_superseded_pane_is_still_owned_and_is_never_current() {
        let mut e = entry_at(DriveState::ReviewWait);
        e.record_worker_pane("w-1");
        e.record_worker_pane("w-2");
        assert_eq!(e.driven_role("w-2"), Some(worker_pane(true)), "the latest pane is current");
        assert_eq!(
            e.driven_role("w-1"),
            Some(worker_pane(false)),
            "the pane the second hand-back superseded is still this drive's — #1871 B2"
        );

        // The lane's sibling arc: `open_lane` replaces the record wholesale.
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000);
        e.open_lane("rev-std", "s1", "rev-2", "head-b", Some("d1"), 2_000);
        assert_eq!(e.driven_role("rev-2"), Some(lane_pane("rev-std", true)));
        assert_eq!(
            e.driven_role("rev-1"),
            Some(lane_pane("rev-std", false)),
            "a re-brief supersedes a reviewer pane; it does not un-own it"
        );

        // The exit list (#1871 B3) is that same population, and it must not name
        // a pane twice — a resumed-twice pane is one pane, and the notice sends a
        // human looking for each id it prints.
        e.record_worker_pane("w-1");
        assert_eq!(e.driven_role("w-2"), Some(worker_pane(false)), "w-2 is superseded in turn");
        let panes: Vec<String> = e.owned_panes().into_iter().map(|(a, _)| a).collect();
        assert_eq!(
            panes,
            vec!["w-2", "w-1", "rev-1", "rev-2"],
            "every pane once, worker first, oldest first within each"
        );

        // Re-pointing the drive at a different worker session forgets ALL of
        // them, not just the current one — they belong to a worker this drive no
        // longer owns.
        e.forget_worker_panes();
        assert_eq!(e.driven_role("w-1"), None);
        assert_eq!(e.driven_role("w-2"), None);
        assert_eq!(
            e.driven_role("rev-2"),
            Some(lane_pane("rev-std", true)),
            "...and the lanes are untouched, so this is not a method that forgets everything"
        );
    }

    /// **The superseded lists are bounded by LIVENESS, never by size** — and a
    /// size cap is what this replaces, because a cap reproduces #1871 B2.
    ///
    /// A cap must choose a victim, and age is the only ordering available to it.
    /// The oldest superseded pane is still running, still on this session and
    /// still able to `report`, so evicting it un-owns it exactly as the single
    /// slot did — B2 again, reachable by the very usage that produced B2. This
    /// test is the pin on that: pane 1 stays owned across an eviction pressure
    /// that a size rule would have acted on.
    ///
    /// Liveness is the one rule that cannot re-open it, and provably rather than
    /// plausibly: `resolve_token` refuses a `Dead` agent and has no entry for a
    /// gone one, so a dead pane cannot reach the MCP seam and there is no
    /// traffic left to fail to own.
    #[test]
    fn a_dead_superseded_pane_is_forgotten_and_a_live_one_is_never_evicted() {
        let mut e = entry_at(DriveState::FixWait);
        for i in 0..40 {
            e.record_worker_pane(&format!("w-{i}"));
        }
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 0);
        e.open_lane("rev-std", "s1", "rev-2", "head-a", Some("d1"), 1);

        // Forty hand-backs and nothing is dropped while every pane is alive:
        // the property a size cap cannot have.
        assert_eq!(e.prior_worker_agents.len(), 39, "no pane is evicted for being old");
        assert_eq!(
            e.driven_role("w-0"),
            Some(worker_pane(false)),
            "the OLDEST superseded pane is exactly the one a size cap drops, and it is still \
             live, still on this session, and still able to report — dropping it is #1871 B2"
        );

        // Now kill two of them. Only those two are forgotten.
        let dead = ["w-0", "rev-1"];
        let is_live = |a: &str| !dead.contains(&a);
        assert!(e.forget_dead_panes(&is_live), "it reports having dropped something");
        assert_eq!(e.driven_role("w-0"), None, "a dead pane cannot call, so forgetting is safe");
        assert_eq!(e.driven_role("rev-1"), None, "…on the lane's list too");
        assert_eq!(
            e.driven_role("w-1"),
            Some(worker_pane(false)),
            "…and every LIVE superseded pane survives the prune"
        );
        assert_eq!(e.prior_worker_agents.len(), 38);

        // Idempotent, and it says so: a second pass drops nothing, so a tick that
        // pruned nothing does not rewrite the file for it.
        assert!(!e.forget_dead_panes(&is_live), "nothing left to drop");

        // The current panes are never candidates — they are the drive's live
        // reference, and `rd_handback`/`open_lane` are what replace them.
        let all_dead = |_: &str| false;
        e.forget_dead_panes(&all_dead);
        assert_eq!(e.driven_role(&e.worker_agent.clone()), Some(worker_pane(true)));
        assert_eq!(e.driven_role("rev-2"), Some(lane_pane("rev-std", true)));

        // "We could not check" is not "it is dead": a predicate that answers
        // true keeps everything, which is the fail-closed direction here —
        // keeping a pane costs a string, dropping a live one costs the leak.
        let mut f = entry_at(DriveState::FixWait);
        f.record_worker_pane("w-1");
        f.record_worker_pane("w-2");
        assert!(!f.forget_dead_panes(&|_| true));
        assert_eq!(f.driven_role("w-1"), Some(worker_pane(false)));
    }

    #[test]
    fn the_two_layers_agree_on_invariant_9() {
        // **The cross-pin, and the reason it exists is the one thing it does NOT
        // change.** §2.3 puts INVARIANT 9's numbers behind two independent
        // enforcers: `workflow.rs` refuses an out-of-range `driver:` value as it
        // parses, and `decide` clamps again on the values it actually reads.
        // Both must keep enforcing — that is the consent boundary, and
        // `a_repo_cannot_raise_invariant_9_by_handing_decide_a_wider_bound` is
        // why the second is not redundant: `decide` is a `pub fn` over a plain
        // value type any caller in any crate can reach without passing through
        // the parser at all.
        //
        // Independence of ENFORCEMENT is not duplication of the VALUE. Two
        // layers encoding "three" separately can drift to three and four with
        // nothing red to say so, and the direction that drift takes is a WIDENED
        // ceiling on an invariant the orchestrator template promises a human.
        // Nothing else in either crate compares them.
        //
        // Sited here because this is the one place that can see both: the
        // ceilings are this module's and the range constants are
        // `crate::workflow`'s, and they are one `use` apart in the same crate.
        use crate::workflow;
        assert_eq!(
            MAX_ROUNDS_CEILING, workflow::DRIVER_MAX_REVIEW_ROUNDS_MAX,
            "the review-round ceiling `decide` clamps to and the one the `driver:` block \
             refuses past have drifted apart"
        );
        assert_eq!(
            MAX_ROUNDS_CEILING, workflow::DRIVER_MAX_CI_ATTEMPTS_MAX,
            "the CI-attempt ceiling has drifted from the review-round one; INVARIANT 9 gives \
             both the same number"
        );
        assert_eq!(
            MAX_REBASE_CEILING, workflow::DRIVER_MAX_REBASE_ATTEMPTS_MAX,
            "the rebase ceiling `decide` clamps to and the one the `driver:` block refuses \
             past have drifted apart"
        );

        // The FLOORS are the other half of the same agreement, and they are not
        // symmetric — `clamped()` floors the two round counters at 1 and lets
        // rebases reach 0, because zero review rounds is a drive that parks on
        // the first `fail` having handed nothing back, while zero rebases is a
        // coherent policy a repo may choose. The parser has to permit exactly
        // what the clamp would produce, or one layer accepts a value the other
        // silently rewrites.
        assert_eq!(workflow::DRIVER_MAX_REVIEW_ROUNDS_MIN, 1);
        assert_eq!(workflow::DRIVER_MAX_CI_ATTEMPTS_MIN, 1);
        assert_eq!(workflow::DRIVER_MAX_REBASE_ATTEMPTS_MIN, 0);
        let floored = DriveLimits::new(0, 0, 0, 60, 60, 240);
        assert_eq!(floored.max_review_rounds, workflow::DRIVER_MAX_REVIEW_ROUNDS_MIN);
        assert_eq!(floored.max_ci_attempts, workflow::DRIVER_MAX_CI_ATTEMPTS_MIN);
        assert_eq!(floored.max_rebase_attempts, workflow::DRIVER_MAX_REBASE_ATTEMPTS_MIN);

        // And the non-vacuity control: the constants are not all the same
        // number, so the three equalities above are three facts rather than one
        // tautology over a single value.
        assert_ne!(MAX_ROUNDS_CEILING, MAX_REBASE_CEILING);
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
    /// **The kick-back budget survives a backwards wall-clock step** (rev-std
    /// round 2, premortem 1).
    ///
    /// `kickback_owed` compares two wall-clock stamps, so an unclamped
    /// `record_kickback` fed a `now` from before the hand-back writes a stamp
    /// that never overtakes `fix_handback_ms` — the budget never spends, and the
    /// tick re-emits on every wake for as long as the progress signal stands.
    /// The forward case is the control: without it this would pass against a
    /// `record_kickback` that ignored its argument entirely.
    #[test]
    fn a_kick_back_stamped_before_its_own_hand_back_still_spends_the_budget() {
        let mut e = entry_at(DriveState::FixWait);
        e.fix_handback_ms = 10_000;
        assert!(e.kickback_owed(), "the pre-state: a fresh hand-back owes one");

        // A clock that stepped backwards between the hand-back and the report.
        e.record_kickback(9_000);
        assert!(
            !e.kickback_owed(),
            "a backwards clock step must not re-arm the budget — that is one prompt per \
             tick into the worker's pane for as long as it keeps reporting progress"
        );

        // The control: an ordinary forward stamp spends it too, and the next
        // hand-back renews it with nothing having to reset anything.
        let mut f = entry_at(DriveState::FixWait);
        f.fix_handback_ms = 10_000;
        f.record_kickback(11_000);
        assert!(!f.kickback_owed(), "the ordinary case still spends");
        f.fix_handback_ms = 20_000;
        assert!(f.kickback_owed(), "…and the next hand-back renews it");
    }

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
        // #1959's, and its default is load-bearing rather than incidental: an
        // entry that has never handed back must owe no kick-back, which is what
        // `0 < 0` being false says.
        assert_eq!(e.fix_kickback_ms, 0);
        assert!(!e.kickback_owed(), "a `review-wait` entry owes nobody a kick-back");
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
              "counters": {"review_rounds": 1, "ci_attempts": 0, "rebase_attempts": 0,
                           "future_counter": 42},
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
        let mut pruned: Vec<u64> =
            prune_terminal(&mut s, 1_000, NOTICE_RETENTION_MS).into_iter().map(|p| p.pr).collect();
        pruned.sort_unstable();
        assert_eq!(pruned, vec![3, 4]);
        let left: Vec<u64> = s.entries.iter().map(|e| e.pr).collect();
        assert_eq!(left, vec![1, 2, 5]);
        // The parked entry kept its counters, which is what the resume spends.
        assert!(s.entry(2).unwrap().state().is_parked());
    }

    /// **§5.2's ordering rule, which no caller implemented before #1857**: a
    /// terminal entry leaves the file once its notice has reached a pane, and
    /// not before.
    ///
    /// The three arms are asserted together because each alone passes under an
    /// implementation that is wrong in one of the other two directions —
    /// "prune everything" passes arm 1, "prune nothing terminal" passes arm 2,
    /// and "retain forever" passes arms 1 and 2 while leaking.
    #[test]
    fn a_terminal_entry_is_kept_while_its_notice_is_owed_and_dropped_at_the_ceiling() {
        let owing = |pr: u64, owed_ms: u64| {
            let mut e = entry_at(DriveState::Satisfied);
            e.pr = pr;
            e.owe_notice(&format!("[orrerix] review drive PR #{pr}: GATE SATISFIED"), owed_ms);
            e
        };
        let mut s = ReviewDrivesState::default();
        // 1: delivered — the ordinary exit.
        let mut delivered = owing(1, 0);
        delivered.notice_delivered();
        s.entries.push(delivered);
        // 2: owing, inside the ceiling — kept, so a later tick re-attempts it.
        s.entries.push(owing(2, 1_000));
        // 3: owing, past the ceiling — dropped, with its text handed back.
        s.entries.push(owing(3, 0));
        // 4: parked and owing nothing — never pruned, on either rule.
        let mut held = entry_at(DriveState::Held);
        held.pr = 4;
        s.entries.push(held);

        let now = NOTICE_RETENTION_MS + 500;
        let pruned = prune_terminal(&mut s, now, NOTICE_RETENTION_MS);
        assert_eq!(
            pruned.iter().map(|p| p.pr).collect::<Vec<_>>(),
            vec![1, 3],
            "a delivered notice prunes and an expired one prunes; an owed one inside the \
             ceiling must not: {pruned:?}"
        );
        assert_eq!(pruned[0].undelivered, None, "#1's notice reached a pane; nothing was lost");
        assert_eq!(
            pruned[1].undelivered.as_deref(),
            Some("[orrerix] review drive PR #3: GATE SATISFIED"),
            "an entry dropped AT the ceiling hands its text back, or the bound reintroduces \
             the very silence #1857 is about"
        );
        assert_eq!(s.entries.iter().map(|e| e.pr).collect::<Vec<_>>(), vec![2, 4]);
        // The retained one still owes exactly what it owed: retention did not
        // quietly re-arm its clock, which is what makes the ceiling reachable.
        assert_eq!(s.entry(2).unwrap().owed_notice().map(|n| n.owed_ms), Some(1_000));
    }

    /// The ceiling clock is anchored at the FIRST owing and a re-owe cannot move
    /// it — a clock re-armed by the retry it bounds is an unbounded retry.
    #[test]
    fn re_owing_a_notice_neither_replaces_the_text_nor_re_arms_the_ceiling() {
        let mut e = entry_at(DriveState::Cancelled);
        e.owe_notice("first", 1_000);
        e.notice_delivery_failed();
        e.owe_notice("second", 50_000);
        let owed = e.owed_notice().expect("still owing");
        assert_eq!(owed.text, "first", "the notice is the one the ending arc produced");
        assert_eq!(owed.owed_ms, 1_000, "the ceiling anchor is the FIRST owing");
        assert_eq!(owed.failures, 1, "and a failed attempt is counted, not the bound");
        // Delivery is the only thing that clears it, and then a fresh drive on
        // the same PR can owe again.
        e.notice_delivered();
        assert!(e.owed_notice().is_none());
        e.owe_notice("second", 50_000);
        assert_eq!(e.owed_notice().map(|n| n.owed_ms), Some(50_000));
    }

    /// A file written before the field existed parses, and its entries owe
    /// nothing — §5.2's read tolerance, checked on the one field #1857 adds.
    /// The round trip is the other half: an owed notice must survive a
    /// load/store cycle, or the whole mechanism is a per-process local again.
    #[test]
    fn an_owed_notice_round_trips_and_a_file_without_one_owes_nothing() {
        let mut e = entry_at(DriveState::Satisfied);
        let text = "[orrerix] review drive PR #1758: GATE SATISFIED at df6a73d0";
        e.owe_notice(text, 7_000);
        let s = ReviewDrivesState { entries: vec![e], ..ReviewDrivesState::default() };
        let json = serde_json::to_string(&s).unwrap();
        let back = parse_state(&json).expect("an owed notice must survive the file");
        let owed = back.entry(1758).unwrap().owed_notice().expect("owed after a round trip");
        assert_eq!((owed.text.as_str(), owed.owed_ms), (text, 7_000));

        // The absent case, spelled as a file this build did not write.
        let older = r#"{"version":1,"entries":[{"pr":9,"state":"satisfied",
            "counters":{"review_rounds":0,"ci_attempts":0,"rebase_attempts":0}}]}"#;
        let old = parse_state(older).expect("§5.2's read tolerance");
        assert!(
            old.entry(9).unwrap().owed_notice().is_none(),
            "an entry from before the field owes nothing — it must not be retained forever"
        );
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

    /// **#1871 B1 at the decision layer.** A verdict decides only for the
    /// revision it reviewed, and the rule is word-blind.
    ///
    /// The same recorded `fail`, read at two heads: at its own it takes arc 5 and
    /// spends a round (the control — without it, an implementation that ignored
    /// every `fail` would pass); at the head the worker moved to it is absent, so
    /// the lane is re-opened. `escalate` is asserted beside it because it takes a
    /// different arc, and a fix that special-cased `Fail` alone would pass on one
    /// and fail on the other.
    ///
    /// `entry.head` is set to the LIVE head deliberately: arc 6 would otherwise
    /// answer first and this would be a test of arc 6, not of the binding rule.
    /// That is exactly the shape the drive is in after a real fix — the tick
    /// persists the head it resolved, so by the time `review-wait` is reached
    /// again the entry and the live head agree and only the VERDICT is stale.
    #[test]
    fn a_verdict_bound_to_an_older_head_decides_nothing_whatever_word_it_is() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-b".into();
        let at = |word, verdict_head| DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(word), verdict_head, "d1")]),
            ..facts_at("head-b")
        };
        // The control: bound to the head in front of the drive, each word decides.
        assert_eq!(
            decide(&e, &at(Verdict::Fail, "head-b"), &limits),
            DriveStep::spend(DriveState::FixWait, Counter::ReviewRounds)
        );
        assert_eq!(
            decide(&e, &at(Verdict::Escalate, "head-b"), &limits),
            DriveStep::held(HeldReason::Escalate)
        );
        // Bound to the head the worker fixed: absent, so the lane is re-opened.
        // Not `fix-wait`, and not a spent round — that loop reached INVARIANT 9's
        // bound in three passes with no re-review having happened.
        assert_eq!(
            decide(&e, &at(Verdict::Fail, "head-a"), &limits),
            DriveStep::OpenLane { index: 0 },
            "a fail recorded against a commit that no longer describes the PR must not route"
        );
        assert_eq!(
            decide(&e, &at(Verdict::Escalate, "head-a"), &limits),
            DriveStep::OpenLane { index: 0 },
            "…nor may a stale escalate park the drive on a judgment nobody is being asked for"
        );
        // The digest half of the same key: same head, body moved under it.
        let moved = DriveFacts {
            body_digest: Some("d2".into()),
            ..at(Verdict::Fail, "head-b")
        };
        assert_eq!(decide(&e, &moved, &limits), DriveStep::OpenLane { index: 0 });
        // …and "we could not check" is not "it changed", in this direction too.
        let unknown = DriveFacts { body_digest: None, ..at(Verdict::Fail, "head-b") };
        assert_eq!(
            decide(&e, &unknown, &limits),
            DriveStep::spend(DriveState::FixWait, Counter::ReviewRounds),
            "an unreadable body must not stale a verdict — one transient gh failure would \
             otherwise re-brief every open lane in the group"
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000);
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000);
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("OLD"), 1_000);
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("OLD"), 1_000);
        assert_eq!(decide(&e, &facts, &limits), DriveStep::OpenLane { index: 0 });
        // S3 performs that brief; the drive now waits on the reviewer.
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("NEW"), 1_500);
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
            agent: "rev-1".into(),
            prior_agents: Vec::new(),
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
