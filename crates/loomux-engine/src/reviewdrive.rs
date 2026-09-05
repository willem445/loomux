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
//! `lane_timeout_minutes`", `held(fix-stalled)` was "neither pushed nor reported
//! inside `fix_timeout_minutes`" until #2168 E1 gave it a second site with its own
//! anchor — and the shape in §5.2 carries no timestamp
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
//!   The `fix-stalled` anchor **in `fix-wait`**; the hand-back is the moment
//!   that wait began. #2168 E1 gives that hold a second site, in `ci-wait`,
//!   whose wait began at the PUSH rather than at the hand-back and so is
//!   measured from `state_since_ms` — see [`decide_fix_receipts`].
//! - [`DriveEntry::fix_kickback_ms`] — when the drive last answered a worker's
//!   `report(progress)` in that worker's own pane (#1959). Not a timeout
//!   anchor: it is compared against `fix_handback_ms`, which makes the budget
//!   one answer per hand-back and renews it with no reset to remember.
//! - [`DriveEntry::fix_pushed_ms`] — when the worker last pushed onto a head
//!   this drive handed back for; `None` unless `ci-wait` was entered by arc 7
//!   (#2168 E1). The `fix-stalled` anchor in THAT state, and the one clock here
//!   that a non-arc also writes: `note_fix_push` re-stamps it when a further
//!   push lands mid-wait, which is exactly what `state_since_ms` cannot do,
//!   since `transition` refuses a `ci-wait` -> `ci-wait` self-arc and so leaves
//!   the state clock on the FIRST push.
//!
//! `drive-stalled` needs none of these: it is the drive's **age**,
//! `now - started_ms`, so it keeps §5.2's own `started_ms`.
//!
//! **§2.2 used to forbid a general "when did the state last change" stamp, and
//! #2110 adds exactly that field on purpose** —
//! [`DriveEntry::state_since_ms`], written on every arc. The ban was never
//! about the stamp; it was about a drive whose ONLY bound is one, and its
//! worked example is §8's `also: [base-green]` row: that drive advances
//! `gate-check` → `ci-wait` on every wake, resets any per-state clock forever,
//! and would sit on a red default branch in silence. So the age is kept, as the
//! backstop that cycler falls through to, and the per-state clocks are added
//! above it. Both, not either.
//!
//! What forced the addition is the other half of the same question. Two drives
//! were parked `drive-stalled` at four hours, and neither was stalled: one was
//! mid-round with CI green at a new head, the other had spent three of those
//! hours unable to spawn a lane at all because another drive held every slot.
//! An age cannot tell those from paralysis, because every drive's age grows at
//! the same rate whatever it is doing — and the reason the *first* clock in
//! this struct was an age is that a bound with no anchor is not a bound, not
//! that an age was the right measure. See [`state_bound_ms`], and
//! [`DriveEntry::starved_total_ms`] for the time both clocks now exclude.
//!
//! - [`DriveEntry::state_since_ms`] — the `state-stalled` anchor.
//! - [`DriveEntry::starved_total_ms`], [`DriveEntry::starved_state_ms`] — what
//!   the drive spent unable to spawn, which neither clock charges it for.
//! - [`DriveEntry::held_from`], [`DriveEntry::held_after_ms`] — what the drive
//!   was doing when a bound fired, so a resume is a decision rather than a
//!   reflex.
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

use crate::workflow::{BlockId, ReviewVerdict, Verdict, DRIVER_DRIVE_TIMEOUT_DEFAULT_MIN};

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
/// fifteen states**, so a reader asking "is this drive parked" asks one
/// question, and the reason travels in the notice and the audit line rather
/// than being inferred from which counter happens to sit at its bound.
///
/// Fifteen reasons. With `satisfied` and `cancelled` that is §2.2's seventeen
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
    /// A resumed **worker** went quiet on a hand-back for
    /// `fix_timeout_minutes`. **Two shapes since #2168 E1, and the notice says
    /// which** (`rddrive::held_notice` branches on `HeldFacts::held_state`):
    /// from `fix-wait`, the worker neither pushed nor reported; from `ci-wait`,
    /// it pushed and then did not report on the pushed head, which is the wait
    /// [`decide_fix_receipts`] adds. One reason rather than two because it is
    /// one wait for one worker on one hand-back — and one remedy, the pane the
    /// notice already names.
    FixStalled,
    /// The drive's **age** passed `drive_timeout_minutes` — the backstop, and
    /// since #2110 only the backstop. Its notice names the state the drive was
    /// in and how long it had been there, because at twelve hours the age alone
    /// says nothing about what to do next.
    DriveStalled,
    /// **The drive sat in ONE working state past that state's own bound**
    /// (#2110) — [`state_bound_ms`], reset by every transition.
    ///
    /// Its own reason rather than [`DriveStalled`](HeldReason::DriveStalled),
    /// and the argument is what the orchestrator LEARNS. The two measured
    /// incidents that produced #2110 both reported `drive-stalled`, and in
    /// neither was anything stalled: one drive was mid-round with CI green at a
    /// new head, the other had spent three of its four hours unable to spawn a
    /// lane because another drive held every slot. An age is the one quantity
    /// that cannot distinguish those from a drive that is genuinely stuck,
    /// because every drive's age grows at the same rate whatever it is doing.
    /// This one grows only while nothing moves, so a drive that reaches it
    /// really is sitting still — and the notice says where.
    ///
    /// **Not a replacement for the wait-specific holds, and never their
    /// preemption.** `lane-stalled`, `fix-stalled` and `cap-full` each name a
    /// remedy this cannot ("read that pane", "free a slot"), and each fires
    /// well inside its state's bound; [`state_bound_ms`] is floored against the
    /// repo's own knobs so that stays true however they are configured — and
    /// #2168 E1 is why that clause is load-bearing rather than decorative: it
    /// put a second, `fix_timeout_minutes`-long wait into `ci-wait` behind the
    /// check wait, so that arm became its constant PLUS the knob. What is left
    /// for this to catch is a
    /// `ci-wait` on a check run that never resolves, the state that should
    /// never be a wait at all (`gate-check`), and any future path that sits in
    /// a state none of the others can see.
    StateStalled,
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
    /// backs off and retries (§8's live-delegate-cap row). The asymmetry is the
    /// states': a lane can be opened on any later tick, while `fix-wait` has
    /// already taken its arc and spent its round. A lane refusal that does not
    /// clear becomes [`CapFull`](HeldReason::CapFull) after [`CAP_HOLD_MS`]
    /// (#2109) — a different reason, on that variant's argument, and never this
    /// one.
    CapRefused,
    /// **The cap has refused this drive's LANE, continuously, for
    /// [`CAP_HOLD_MS`]** (#2109) — the starvation made visible.
    ///
    /// "Continuously" is a promise the TICK keeps, not this enum: a refusal
    /// that is not the cap's clears the stamp rather than letting it age, so
    /// this reason cannot be reached behind a run of refusals that were
    /// something else. Guarding only the write made the stamp a latch, which is
    /// review 4's W1 on #2112.
    ///
    /// Its own reason rather than [`CapRefused`](HeldReason::CapRefused), and
    /// the argument is a DURATION rather than a remedy. The two share a remedy
    /// (free a slot), so a reader deciding what to *do* could be served by one
    /// spelling. What one spelling cannot say is how long: `cap-refused` is a
    /// single hand-back refusal, held on the spot, with a round already spent;
    /// this is "every lane spawn for this drive has been refused since `t`".
    /// The measured incident is exactly that difference — a drive sat in
    /// `review-wait` with `lanes: []` for three hours emitting 37 identical
    /// `rd-refused` rows and no notice at all, and an orchestrator reading
    /// `cap-refused` there would have learned that the cap refused *a* spawn,
    /// which had been true and harmless thirty-seven ticks earlier.
    ///
    /// **The driver still never kills a pane to make room** (§3.1 item 5, as
    /// #2501 narrowed it: it releases a lane whose verdict is recorded at this
    /// head and a worker whose report it consumed, on facts about those panes
    /// and never on how full the group is), and this hold is what makes the
    /// remaining starvation survivable rather than silent: the notice names who
    /// can free a slot. What actually releases the cap is a human or the
    /// orchestrator killing an idle delegate, the idle reaper where one is
    /// configured, or another drive ending — and #2109's other two fixes are
    /// what make waiting terminate at all, because a drive now costs ONE pane
    /// per lane block for its whole life instead of one or two per round.
    CapFull,
    /// A driven delegate called `message_orchestrator` (§7 — that call is never
    /// intercepted; the delegate's own line arrives by its own path and this
    /// hold is the routing fact beside it).
    Messaged,
}

impl HeldReason {
    /// Every reason, so a caller — or a test counting §2.2's exits — can
    /// enumerate them without matching on the enum. Order is §2.2's table.
    pub const ALL: [HeldReason; 15] = [
        HeldReason::Escalate,
        HeldReason::ReviewLimit,
        HeldReason::CiLimit,
        HeldReason::RebaseLimit,
        HeldReason::LaneStalled,
        HeldReason::FixStalled,
        HeldReason::DriveStalled,
        HeldReason::StateStalled,
        HeldReason::RoutingUnaccountable,
        HeldReason::GateUnreadable,
        HeldReason::WorkerBlocked,
        HeldReason::WorkerUnresumable,
        HeldReason::CapRefused,
        HeldReason::CapFull,
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
            HeldReason::StateStalled => "state-stalled",
            HeldReason::RoutingUnaccountable => "routing-unaccountable",
            HeldReason::GateUnreadable => "gate-unreadable",
            HeldReason::WorkerBlocked => "worker-blocked",
            HeldReason::WorkerUnresumable => "worker-unresumable",
            HeldReason::CapRefused => "cap-refused",
            HeldReason::CapFull => "cap-full",
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
            "state-stalled" => Some(HeldReason::StateStalled),
            "routing-unaccountable" => Some(HeldReason::RoutingUnaccountable),
            "gate-unreadable" => Some(HeldReason::GateUnreadable),
            "worker-blocked" => Some(HeldReason::WorkerBlocked),
            "worker-unresumable" => Some(HeldReason::WorkerUnresumable),
            "cap-refused" => Some(HeldReason::CapRefused),
            "cap-full" => Some(HeldReason::CapFull),
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
            drive_timeout_minutes: DRIVER_DRIVE_TIMEOUT_DEFAULT_MIN as u64,
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
/// **Every COUNT is required, not just the block.** `DriveEntry::counters`
/// carries no `serde(default)` for the reason on that field — zeros silently
/// grant a full fresh budget — and a per-field default on a count would have
/// reopened the same hole one level down, where `"counters": {}` parses to three
/// zeros and `"counters": {"review_rounds": 2}` quietly forgives the CI attempts.
/// The block being mandatory is worth nothing if its contents are optional.
///
/// **The one `serde(default)` below is `body_only_grace`, and it is an argued
/// exception rather than a hole in that rule** (#2509): it is a bool, not a
/// count, a defaulted `false` grants one round once rather than a whole fresh
/// budget, and it is the TRUE reading of a file written before the field
/// existed. The full argument is on the field. Read the rule above as being
/// about the three counts — a NEW count still takes no default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub review_rounds: u32,
    pub ci_attempts: u32,
    pub rebase_attempts: u32,
    /// **#2509's one-shot grace — the only field here that is a BOOL rather
    /// than a count.** Whether this drive has already spent its single extra
    /// review round for a body-only blocking fail (§2.3).
    ///
    /// It is a SEPARATE budget on purpose, and that is the whole of what
    /// "grace is inside the ceiling" means. `review_rounds` still stops dead at
    /// `max_review_rounds` and is never bumped past it, so [`MAX_ROUNDS_CEILING`]
    /// bounds exactly what it bounded before; the extra round is funded from
    /// here instead, once, which caps a drive at `max_review_rounds + 1` review
    /// rounds and no more. Funding it out of `review_rounds` was the other
    /// reading and is vacuous at stock knobs — the default `max_review_rounds`
    /// IS the ceiling — so the feature would have helped only a repo that had
    /// lowered its own bound, and never the case it was filed for.
    ///
    /// **`serde(default)` here, and NOT on the three counts above.** The
    /// argument on those is that a defaulted zero silently re-grants a whole
    /// fresh budget, so the conservative direction is to refuse the file. A
    /// defaulted `false` grants one round once, and it is also the TRUE reading
    /// of a file written before this field existed: that drive cannot have
    /// spent a grace the build that wrote it had never shipped. Refusing to
    /// parse every in-flight drive at upgrade is the worse failure, and §5.2's
    /// posture for machine-authored state is that it degrades rather than
    /// fails loud.
    #[serde(default)]
    pub body_only_grace: bool,
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
    /// Spent on a body-only `fail` recorded AT the review bound (#2509) — the
    /// one-shot grace, and the only variant here that spends a bool rather
    /// than a count. See [`Counters::body_only_grace`].
    BodyOnlyGrace,
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

/// **Does #2509's one-shot grace apply to the `fail` this lane has just
/// recorded at the review bound?**
///
/// A `held(review-limit)` costs the orchestrator three wakes and a hand-written
/// brief: cancel, resume the worker by hand, re-drive with
/// `rounds_already_spent`. INVARIANT 9 bounds review rounds so that a reviewer
/// surfacing one new nit per round cannot run for ever — and a blocking fail on
/// the PR **body**, at a head that has not moved since a full round was already
/// spent on it, is not that shape. It is a text edit the worker can make in one
/// turn with nothing to build and nothing to re-check. PR #2397 reached the
/// bound on two sentences.
///
/// # What it reads, and what it deliberately does not
///
/// It reads the **brief this drive sent** — [`LaneRecord::briefed_body_only`],
/// stamped by [`DriveEntry::open_lane`] from the step that chose the lane — and
/// never the reviewer's prose, never a word the reviewer could type. #2509
/// considered a `body_only` parameter on `review_verdict` and rejected it:
/// `workflow.rs`'s line-5 marker is placed where it is precisely because "a
/// marker a reviewer could type would be a marker a reviewer could forge", and
/// a reviewer that can mark its own fail body-only can buy itself a round.
///
/// The pair is matched **exactly**, not through [`lane_open_for`]'s
/// unknown-tolerant comparison, and an empty digest never matches: this is a
/// grant, and a brief whose revision cannot be pinned grants nothing. That is
/// [`LaneRecord::briefed_verify`]'s posture one grant over.
///
/// # The residual, disclosed and bounded
///
/// The mark says the driver **asked** about the body alone, not that the
/// findings that came back are about the body. A reviewer answering a body-only
/// re-brief with a code nit it missed a round earlier still earns the grace.
/// That is the honest cost of deriving the bit rather than trusting a
/// reviewer's word for it, and it is contained by construction rather than
/// argued away: at most ONE extra round, once per drive, and only ever on code
/// that a full review round has already been spent on.
pub fn body_only_grace_applies(
    entry: &DriveEntry,
    block: &str,
    head: &str,
    digest: Option<&str>,
) -> bool {
    // Spent once and never again — §2.3's "never stacking". Asked FIRST so the
    // rest reads as the grant's preconditions rather than as its budget.
    if entry.counters.body_only_grace {
        return false;
    }
    let Some(digest) = digest.filter(|d| !d.is_empty()) else { return false };
    entry.lane(block).is_some_and(|r| {
        r.briefed_body_only && r.briefed_head == head && r.briefed_digest == digest
    })
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
    /// **This lane's current brief is a body-verification delta** (#2168 E2) —
    /// the head has not moved, every required lane has already passed it, and
    /// only the PR body has.
    ///
    /// Per-revision, like `briefed_head`/`briefed_digest`, and read only
    /// together with them: it is the *brief that is out*, never a standing
    /// property of the lane. `review_verdict` consults it to decide whether the
    /// verdict it is about to write carries
    /// [`ReviewVerdict::verified_body`](crate::workflow::ReviewVerdict::verified_body),
    /// which is what lets the gate accept the passes this one supersedes — so
    /// it is a capability grant, and it is checked against an exact
    /// `(briefed_head, briefed_digest)` match rather than through
    /// [`lane_open_for`]'s unknown-tolerant comparison. A brief whose revision
    /// cannot be pinned grants nothing.
    ///
    /// Cleared by [`LaneRecord::reseeded`] with the rest of the per-revision
    /// fields, per the rule stated there.
    #[serde(default)]
    pub briefed_verify: bool,
    /// **This lane's current brief is about the BODY of a revision whose CODE
    /// this drive has already reviewed to completion** (#2509) — the head has
    /// not moved, this lane was already briefed at it, and every required lane
    /// has ANSWERED at it.
    ///
    /// Strictly weaker than [`briefed_verify`](LaneRecord::briefed_verify),
    /// which it nests inside: that one needs every required lane to have
    /// *passed*, this one only that each has *spoken*. The gap between them is
    /// exactly the case #2509 is for — a lane that recorded `fail` on the body,
    /// whose worker then moved the body and not the head.
    ///
    /// Per-revision, like `briefed_head`/`briefed_digest`, and read only
    /// together with them: it describes the *brief that is out*, never a
    /// standing property of the lane. It is read at
    /// [`decide_review_wait`]'s `fail` arm against an exact
    /// `(briefed_head, briefed_digest)` match rather than through
    /// [`lane_open_for`]'s unknown-tolerant comparison — same posture as
    /// `briefed_verify`, and for the same reason: a brief whose revision
    /// cannot be pinned grants nothing.
    ///
    /// **It rides no verdict and reaches no gate.** #2509 considered putting
    /// the bit on [`ReviewVerdict`](crate::workflow::ReviewVerdict) and did
    /// not: line 5 of a verdict file is also read by the `gh` shim, and a mark
    /// the shim did not learn reads there as *no digest*, which makes
    /// `body-unchanged` refuse a merge it should allow. That is #2308's
    /// divergence exactly, and this grant needs none of it — the only consumer
    /// is the driver's own bound.
    ///
    /// Cleared by [`LaneRecord::reseeded`] with the rest of the per-revision
    /// fields, per the rule stated there.
    #[serde(default)]
    pub briefed_body_only: bool,
    /// Preserved unknown fields — see [`ReviewDrivesState`].
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl LaneRecord {
    /// This lane's **memory**, carried onto a fresh drive of the same PR and
    /// nothing else (#2153).
    ///
    /// **The gap is the cross-drive boundary, and it is the ordinary path.**
    /// Inside a live drive the resume is complete: `rd_lane_session` falls back
    /// from the record to the roster to the merged records, so even a killed
    /// lane pane resumes its conversation. But lane memory lives only on the
    /// entry, and `drive_review` on a PR whose previous entry is TERMINAL drops
    /// that entry and pushes a `DriveEntry::new` with `lanes: []` — so the
    /// sequence a satisfied gate is designed to produce (satisfied →
    /// orchestrator dispositions the findings → re-drive) spawned every lane
    /// cold. Measured on PR #2141: two lanes with live, resolvable sessions
    /// that had already read the PR once, both re-opened `resumed=false`, on
    /// the round where the warm session is cheapest.
    ///
    /// **What is kept is the conversation and what §7 owns; what is dropped is
    /// every claim about a revision THIS BUILD CAN NAME.** `briefed_head`,
    /// `briefed_digest`, `briefed_verify`, `briefed_body_only`, `last_verdict`,
    /// `at_head` and
    /// `spawned_ms` all describe the drive that ended, so carrying any of them
    /// would have the first tick either wait on a brief nobody sent, read a
    /// stale verdict as this round's answer, or — for `briefed_verify` and
    /// `briefed_body_only` — stamp a fresh round with a grant the previous
    /// drive made.
    ///
    /// **`extra` is the disclosed exception, and the qualifier above is what
    /// makes this doc honest** (#2169 review 2, premortem 2). That field is by
    /// construction the lane fields this build has no name for
    /// ([`ReviewDrivesState`]), and it is carried across rather than dropped
    /// because "a field a newer build wrote is not this build's to erase" is
    /// the rule the whole passthrough exists for — an older binary that
    /// silently deleted one would be the data-loss this type is designed to
    /// prevent. The residual is the mirror of that: if a FUTURE build stores a
    /// revision-scoped lane field in `extra`, an older binary performing the
    /// re-drive carries that build's claim about the round that just ended into
    /// the new one, and cannot know it did. No test can reach it — a fixture
    /// would have to invent a field the code does not know — so it is stated
    /// here and its trigger named: **any new per-revision lane field must be
    /// cleared in this function at the same time it is added.** Every field
    /// this build knows about is named above, so that check is a read of one
    /// function rather than a search.
    /// `lane_open_for` is then false for every head, which is what makes the
    /// first tick BRIEF each lane rather than wait on it.
    ///
    /// **That is a claim about the RECORD, not about what the brief says.** The
    /// verdict FILE outlives the drive that produced it, and the tick re-reads
    /// it through the gate's own parser as it does on every other tick — so
    /// [`DriveEntry::record_verdict_seen`] re-derives `last_verdict`/`at_head`
    /// from the file before the brief is built, and a lane that really did
    /// answer gets the §5.5 delta template naming the head it answered at. That
    /// is #2109's point rather than a leak past this one: a reviewer is asked
    /// again in its own conversation instead of being replaced by a stranger who
    /// is told what "its" previous verdict had been. What clearing the pair buys
    /// is that the new entry asserts nothing this drive has not itself read —
    /// so a first tick whose verdict file is absent or unreadable brief this
    /// lane as new rather than as having answered.
    ///
    /// **The pane moves to `prior_agents` rather than staying current.** It is
    /// still this drive's to intercept — a reviewer finishing its previous
    /// round must not report to the orchestrator as if undriven — but it is no
    /// longer the pane the drive would speak to, and [`DrivenPane::current`] is
    /// exactly that distinction. Leaving it as `agent` would also make
    /// `rd_live_lane_pane` treat it as a duplicate and `pane_dead` treat its
    /// death as this drive's, both of which are claims about a round that is
    /// over.
    ///
    /// `spawned_ms` goes to zero: it is the `lane-stalled` anchor, and a lane
    /// that has not been briefed has not been silent.
    ///
    /// **`session` is passed in rather than copied off this record**, and that
    /// is #2109's lesson applied one boundary over: `LaneRecord::session` is
    /// what the spawn RETURNED, which is a session id only on a CLI that
    /// pre-assigns one, so a copilot or opencode lane carries `""` for its whole
    /// life and its conversation lives on the pane and the roster row instead.
    /// A seed built from this field alone would therefore drop exactly the lanes
    /// #2109 was about. The caller resolves it through `rd_lane_session` — the
    /// one function that knows all three sources — and hands the answer here.
    pub fn reseeded(&self, session: &str) -> LaneRecord {
        let mut prior = self.prior_agents.clone();
        prior.push(self.agent.clone());
        LaneRecord {
            block: self.block.clone(),
            session: session.to_string(),
            agent: String::new(),
            // `""` as the "superseding" pane, which `retain_panes` treats as no
            // pane at all — it drops empties either way, so nothing is excluded
            // by it and the dedup and ordering still apply.
            prior_agents: retain_panes(prior, ""),
            last_verdict: None,
            at_head: String::new(),
            briefed_head: String::new(),
            briefed_digest: String::new(),
            spawned_ms: 0,
            briefed_verify: false,
            briefed_body_only: false,
            // Preserved for `ReviewDrivesState`'s reason: a field a newer build
            // wrote is not this build's to drop. **This is the one thing here
            // that is NOT re-derived**, and the doc above names the residual it
            // leaves and the trigger that would close it.
            extra: self.extra.clone(),
        }
    }
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

/// How long the live-delegate cap may refuse this drive's lane before the drive
/// parks as [`HeldReason::CapFull`] (#2109).
///
/// **A grace period, because a capped lane usually clears itself** — §8's
/// live-delegate-cap row, and the reason the refusal is a back-off rather than a
/// hold in the first place: another drive's lane finishes, the reaper takes an
/// idle pane, a human closes one, and the next tick spawns. Holding on the first
/// refusal would spend an orchestrator turn on a condition that resolves in one
/// `RD_BACKOFF_MS` interval, which is the opposite defect from the one #2109
/// reports.
///
/// **Fifteen minutes**, which is three back-off intervals: long enough that a
/// transient cap never reaches it, and short enough that the measured incident —
/// three hours in `review-wait` with `lanes: []`, invisible until a human read
/// `review_drive_status` by hand — is impossible. The other bound that would
/// eventually have caught it is `drive_timeout_minutes`, at its twelve-hour
/// default, whose notice does now name the state and the clock but still says
/// nothing about the cap.
///
/// **Not a `driver:` policy knob**, on `NOTICE_RETENTION_MS`'s argument: §5.3's
/// block paces a drive against INVARIANT 9's budget, and how long orrerix waits
/// on its own group's slot pressure before saying so is not a repo's call. A
/// repo that wants a longer wait raises its own `max_agents`.
pub const CAP_HOLD_MS: u64 = 15 * 60_000;

// ── §2.2's per-state bounds (#2110) ────────────────────────────────────────
//
// **Why these exist at all, and what they replaced.** Until #2110 the only
// clock over a working drive was its total age, and the two measured failures
// were the two that measure cannot tell apart: a drive making steady progress
// across four hours of transitions was parked as "stalled", and a drive that
// spent three of its four hours unable to spawn a lane at all had that
// starvation counted against its own budget. An age answers "how long has this
// drive existed"; nobody wanted that bounded. What a bound is *for* is "how
// long has this drive sat in one place", and that is what these measure.
//
// **They are constants and not `driver:` policy**, on [`CAP_HOLD_MS`]'s
// argument: how long orrerix waits on its OWN machinery — a check run, a
// reviewer lane, a resumed worker — before telling an orchestrator is not a
// repo's call.
//
// **How each combines with the repo's knobs differs per arm, and there is no
// one sentence for all four.** This block used to carry one — "a per-state
// bound is the LARGER of its constant and whatever the repo configured" — and
// it was the fifth surface of a claim rounds 3 and 4 had already corrected
// elsewhere. It was true of one arm, never true of two, and stopped being true
// of the fourth when #2117's W3 made that arm an add. [`state_bound_ms`] is
// where the per-arm rule is argued; the short form:
//
// - `gate-check` has **no knob at all** — [`DriveLimits`] carries no `gate_`
//   timeout — so there is nothing to combine with and nothing to shadow.
// - `fix-wait` is the LARGER of its constant and `fix_timeout_minutes`. It is
//   the only arm that is a max at all.
// - `review-wait` is its constant PLUS `lane_timeout_minutes` per required
//   lane, because that state holds several waits in sequence and the product
//   alone funds their silences and none of the gaps between them.
// - `ci-wait` is its constant PLUS `fix_timeout_minutes` (#2168 E1), on the
//   same argument: since E1 that state holds the check matrix and then the
//   worker's report on a pushed head, one after the other. It was a bare
//   constant before E1 and belonged on the `gate-check` line.

/// How long a drive may sit in `ci-wait` (§2.1) before [`HeldReason::StateStalled`].
///
/// **Ninety minutes, and `ci-wait` is the state that most needed one**: it is
/// the only working state with no bound of its own at all —
/// `CiObservation::Pending` and `Unknown` both return `Wait`, forever, and
/// before #2110 the first thing to notice was the total age hours later. This
/// project's own matrix is three platforms at twenty to thirty minutes; ninety
/// covers a full re-run behind a queue and is still an hour clear of anything
/// that has ever been legitimate here.
///
/// **The SLACK since #2168 E1, not the bound outright.** [`state_bound_ms`]
/// ADDS `fix_timeout_minutes` to this, because that state now holds a second
/// wait measured on that knob — [`decide_fix_receipts`] — after the check wait
/// this number is sized for. Compared rather than added, the margin at any
/// `fix_timeout_minutes` above ninety minutes is exactly zero and
/// `held(state-stalled)` preempts the `held(fix-stalled)` that names the pane
/// to read; that is `REVIEW_WAIT_BOUND_MS`'s own argument, one state over.
pub const CI_WAIT_BOUND_MS: u64 = 90 * 60_000;

/// How long a drive may sit in `review-wait` (§2.1) before
/// [`HeldReason::StateStalled`].
///
/// **Three hours, because `review-wait` is the one state that legitimately
/// holds several waits in a row.** The gate's lanes are reviewed in SEQUENCE
/// and `lane_index` advancing is deliberately not a transition (§2.1), so a
/// three-lane gate whose reviewers each take their full `lane_timeout_minutes`
/// spends three hours here with nothing wrong.
///
/// **It is the SLACK over that product, not a rival to it** (#2117 review 2,
/// W3): [`state_bound_ms`] ADDS this to `lane_timeout_minutes * lanes` rather
/// than taking the larger of the two. The product covers the lanes' silences;
/// this covers everything inside `review-wait` that is not one of them — the
/// stretch before the first brief, and the tick-detection gap after each
/// verdict. Compared rather than added, the margin is exactly zero at the
/// configurations the floor exists to protect, and a drive whose reviewers all
/// answered in time parks anyway. See [`state_bound_ms`] for the worked case.
pub const REVIEW_WAIT_BOUND_MS: u64 = 180 * 60_000;

/// How long a drive may sit in `fix-wait` (§2.1) before
/// [`HeldReason::StateStalled`].
///
/// **Ninety minutes, a floor over `fix_timeout_minutes` rather than a second
/// opinion about it.** `held(fix-stalled)` already bounds the wait this state
/// is for — a worker that neither pushes nor reports — and it is the better
/// notice whenever it applies, so this is sized *above* that knob's own
/// sixty-minute default and named in [`state_bound_ms`] so a repo that raises
/// the knob raises this with it. What is left for this to catch is the case
/// `fix-stalled` cannot see: a drive in `fix-wait` whose worker is neither
/// silent nor finished.
pub const FIX_WAIT_BOUND_MS: u64 = 90 * 60_000;

/// How long a drive may sit in `gate-check` (§2.1) before
/// [`HeldReason::StateStalled`].
///
/// **Fifteen minutes, and it is the tightest because `gate-check` is a
/// decision rather than a wait.** Every outcome but one leaves the state on the
/// tick that entered it; the exception is `GateOutcome::NotEvaluated`, a tick
/// that reached `gate-check` without evaluating the gate, which is a fault in
/// the read and not a thing to wait out. Three back-off intervals — the same
/// number [`CAP_HOLD_MS`] allows a transient cap — is generous for a condition
/// that ought to clear on the next tick.
pub const GATE_CHECK_BOUND_MS: u64 = 15 * 60_000;

/// The bound on time spent in one working state (#2110), or `None` for a state
/// that is not a wait — `held` is parked and the two terminals are over, so
/// none of the three has a clock at all.
///
/// **What each state's bound is made of, per state** — there is no single rule,
/// and an earlier version of this paragraph claimed one ("`max` of the constant
/// and the state's own configured bound"). That was true of every arm when it
/// was written, stopped being true of `review-wait` the moment #2117's W3 made
/// that arm an ADD and of `ci-wait` when #2168 E1 made that one an ADD too, and
/// was never true of the arm that has no knob to take a max against. Three
/// shapes over four arms:
///
/// - `gate-check` is a **bare constant**. [`DriveLimits`] has no `gate_`
///   timeout, so there is nothing to shadow and nothing to take a maximum
///   with.
/// - `fix-wait` is `max(constant, fix_timeout_minutes)` — a floor over the
///   knob, so a repo that raises `fix_timeout_minutes` to four hours raises
///   this with it rather than being parked at ninety minutes by a number in
///   loomux.
/// - `ci-wait` is the constant **PLUS** `fix_timeout_minutes` since #2168 E1,
///   on `review-wait`'s argument rather than `fix-wait`'s, and the choice of
///   ADD over `max` is the whole of the point. That state now holds two waits
///   in SEQUENCE — the check matrix, and then the worker's read-and-report on
///   the head it pushed ([`decide_fix_receipts`]) — so a bound that is merely
///   the LARGER of the two funds only one of them. Under `max`, a repo with
///   `fix_timeout_minutes` above the constant gets a state bound EQUAL to the
///   knob, and [`decide`] reads the state bound above the state's own logic:
///   the first tick past it answers `state-stalled` and the `fix-stalled` that
///   names the pane to read never fires at all. That is the preemption
///   [`HeldReason::StateStalled`] says cannot happen, and it is the same
///   zero-margin defect #2117 review 2 found in `review-wait`, one state over.
///   Added, the constant is the slack over the second wait exactly as it is
///   there.
///
///   **The add applies to every `ci-wait` drive, including one that never
///   handed anything back**, and that is chosen rather than overlooked. A bound
///   scoped to the waiting drives would have to read [`DriveEntry::fix_pushed_ms`],
///   and `rdtick` computes `held_bound_ms` off the entry AFTER the arc into
///   `held` — the arc that clears that flag — so the notice would quote a
///   different bound from the one that fired. What a first drive pays is that a
///   check run which never resolves is caught at 150 minutes rather than 90 on
///   stock knobs. It fails toward a later park and never toward an earlier
///   false one, every wait-specific hold still fires inside it, and the
///   twelve-hour backstop is unmoved.
/// - `review-wait` is the constant **PLUS** `lane_timeout_minutes * lanes`, for
///   the reason argued below. It is the arm that is not a max, and the sentence
///   above exists because saying "max" of all four read as covering it.
///
/// # `review-wait`'s floor is the sum of the SILENCES, plus slack
///
/// `required_lanes` is how many lanes the gate requires at this head, and the
/// gate's lanes are reviewed in SEQUENCE — `first_stale_lane` picks one at a
/// time and a lane brief is not an arc, so nothing re-stamps
/// [`DriveEntry::state_since_ms`] between them. So a legitimate `review-wait`
/// can hold `required_lanes` full `lane_timeout_minutes` waits end to end, and
/// the floor has to cover their sum or the bound fires on a drive whose every
/// reviewer answered in time.
///
/// **That product alone is not enough, and the gap is what review 2 on #2117
/// found.** `lane_timeout_minutes` bounds a lane's SILENCE, measured from that
/// lane's own `spawned_ms`; this bound measures ELAPSED time in the state, from
/// `state_since_ms`. Every interval inside `review-wait` that is not one
/// lane's silence is unfunded by the product: the stretch from entering the
/// state to the first brief, and the tick-detection gap after each verdict
/// lands. With three lanes at the sixty-minute default, three reviewers each
/// answering at fifty-nine minutes and three such intervals costing ninety
/// seconds apiece crosses a bare 180-minute floor — and the margin is exactly
/// zero wherever `lane_timeout_minutes * required_lanes` reaches
/// [`REVIEW_WAIT_BOUND_MS`], which is precisely the configuration the floor
/// exists to protect. A drive whose reviewers all answered promptly would park
/// `state-stalled` naming no lane, which is the same class of false park
/// #2110 exists to remove.
///
/// So the slack is [`REVIEW_WAIT_BOUND_MS`] itself, ADDED to the product rather
/// than compared against it. It is not a tuned allowance-per-lane: an
/// allowance sized to the detection gap would be a guess about tick timing that
/// goes stale with `RD_BACKOFF_MS`, while a whole extra copy of the constant is
/// a bound whose looseness is stated rather than estimated. The cost of being
/// generous here is small and one-directional — this is the catch-all, and
/// every wait-specific hold (`lane-stalled`, `cap-full`) still fires inside it.
///
/// **The residual is that the sum can exceed the twelve-hour backstop**, and
/// then the backstop fires first because [`decide`] checks the age above the
/// state bound: `lane_timeout_minutes: 240` on a three-lane gate floors this at
/// 900 minutes, so such a drive parks `drive-stalled`. **On STOCK knobs the
/// crossover is nine lanes** — `180 + 60n >= 720` from `n = 9` — which is the
/// number an operator declaring a wide gate wants and which the worked example
/// above does not give; pinned by
/// `the_review_wait_floor_overtakes_the_backstop_at_nine_lanes_on_stock_knobs`.
/// That is degraded but not
/// the pre-#2110 notice — `held_from` is stamped on every hold arc, so the
/// `drive-stalled` notice still names the state and the time in it. Pinned by
/// `a_review_wait_floor_that_outruns_the_backstop_still_names_the_state`.
///
/// Zero required lanes (a routing answer this tick could not produce) falls
/// back to the constant plus its slack; that drive holds
/// `routing-unaccountable` on the same tick anyway.
///
/// # What these clocks charge that nobody wants them to
///
/// Two properties are disclosed rather than closed, both inherited from
/// [`DriveEntry::state_elapsed_ms`] being wall time minus cap starvation and
/// nothing else (#2117 review 3):
///
/// - **orrerix's own downtime is charged to the state.** The clocks are
///   absolute stamps, not tick counts, so a group paused or an app closed for
///   two hours while a drive sat in `ci-wait` parks it `state-stalled` on the
///   first tick after the restart. The age bound had this property before
///   #2110 and nobody hit it at four hours; these bounds are tighter, so it is
///   now reachable in an ordinary lunch break. It is not silent (the notice
///   names the state and the elapsed figure) and it is recoverable by the
///   remedy that notice prints — arc 11 re-stamps every clock. Closing it wants
///   a `last_tick_ms` and a gap-detection rule, which is a second clock with
///   its own failure mode (a drive that never ticks would never bound), and
///   that is a decision this issue did not ask for. Pinned by
///   `orrerix_downtime_is_charged_to_the_state_it_spanned`.
/// - **The `review-wait` bound moves when the GATE does**, because
///   `required_lanes` is read fresh from `facts` on every tick rather than
///   stamped when the state was entered (#2117 review 6, premortem 1). A
///   workflow edit or a path-scoped routing rule that takes a three-lane gate
///   down to one shrinks this bound from six hours to four **retroactively**,
///   so a drive five hours into a legitimate three-lane sequence parks on the
///   next tick for time it spent inside the wider bound it was actually
///   running under. Reading the count fresh is what makes the bound track the
///   gate at all, and stamping it at state entry would freeze a six-hour
///   bound onto a drive whose gate had since narrowed — the opposite error,
///   and the one that fails toward NOT parking. Disclosed rather than
///   chosen between: every test here holds `required_lanes` constant, so
///   nothing pins bound stability against a moving lane list.
/// - **A backward wall-clock step suspends every bound** rather than firing
///   one: the subtraction saturates, so `state_elapsed_ms` reads zero until the
///   clock catches up. That is the fail-safe direction — no false park — and it
///   is the same behaviour `age_ms` has had since #1778. Pinned by
///   `a_clock_that_steps_backward_suspends_the_bound_rather_than_firing_it`.
pub fn state_bound_ms(
    state: DriveState,
    limits: &DriveLimits,
    required_lanes: usize,
) -> Option<u64> {
    let lane = minutes_ms(limits.lane_timeout_minutes);
    match state {
        DriveState::CiWait => {
            Some(CI_WAIT_BOUND_MS.saturating_add(minutes_ms(limits.fix_timeout_minutes)))
        }
        DriveState::ReviewWait => Some(
            REVIEW_WAIT_BOUND_MS
                .saturating_add(lane.saturating_mul(required_lanes.max(1) as u64)),
        ),
        DriveState::FixWait => {
            Some(FIX_WAIT_BOUND_MS.max(minutes_ms(limits.fix_timeout_minutes)))
        }
        DriveState::GateCheck => Some(GATE_CHECK_BOUND_MS),
        DriveState::Held | DriveState::Satisfied | DriveState::Cancelled => None,
    }
}

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
    /// an **age** from it (`now - started_ms`), and since #2110 that age is the
    /// BACKSTOP — the bound §8's `also: [base-green]` cycler cannot reset,
    /// beneath the per-state clocks that do the work
    /// ([`state_since_ms`](DriveEntry::state_since_ms)). A stored *age* would be
    /// stale the instant it was written and meaningless across a restart.
    ///
    /// Time the drive spent unable to spawn is subtracted from it rather than
    /// charged to it — [`bounded_age_ms`](DriveEntry::bounded_age_ms).
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
    /// **This drive reached `ci-wait` by arc 7 — the worker pushed a fix** —
    /// and has not left it since (#2168 E1). What `decide_ci_wait` reads to
    /// tell a fix push from every other way into that state.
    ///
    /// **Why the state alone cannot answer it.** `ci-wait` is entered five
    /// ways — the creation (arc 1) and arcs 6, 7, 10 and 11 — and only arc 7
    /// means "this drive handed the worker a fix and the worker has just
    /// pushed it". The defect #1875 measures needs exactly that distinction:
    /// after a push the worker fills the PR body's CI section, which it can
    /// only do once the checks have settled, so a lane briefed the moment CI
    /// goes green is briefed at a body digest the worker is about to move —
    /// and the pass it records is stale before it is written. Every code PR of
    /// that session paid at least one re-record round for it.
    ///
    /// **Arc 6 is a worker push too and is deliberately NOT covered**, because
    /// the two preconditions for expecting a `report(done)` are the ones arc 7
    /// carries and arc 6 does not. §7's interception is keyed on
    /// [`worker_agent`](DriveEntry::worker_agent), which is empty until the
    /// first hand-back — so before one, a driven worker's `report` goes to the
    /// orchestrator's pane exactly as it always did and no tick can ever see a
    /// `Done`. And nothing has asked that worker for one: arc 6 is a push the
    /// drive did not solicit, mid-review, while arc 7 answers a hand-back whose
    /// brief says *push, and report when the checks are green*. Gating arc 6 on
    /// a signal that cannot arrive and was never requested would park every such
    /// drive on `held(fix-stalled)` at the timeout — a false park, where the
    /// arc-6 status quo costs at most the one re-record round.
    ///
    /// **A STAMP and not a flag, and the first draft of this slice had it the
    /// other way** (rev-final round 2, premortem 2). The obvious shape is a
    /// `bool` bounded from [`state_since_ms`](DriveEntry::state_since_ms) — one
    /// clock instead of two, and true as far as it goes, since
    /// [`transition`] refuses a `ci-wait` -> `ci-wait` self-arc so the state
    /// stamp really is the arc-7 moment. What that argument misses is that a
    /// worker may push AGAIN inside one `ci-wait` stay. No arc fires, nothing
    /// re-stamps, and the wait therefore runs from the FIRST push: a follow-up
    /// commit fifty-five minutes into a sixty-minute `fix_timeout_minutes`
    /// leaves five minutes to run a fresh matrix and report, and the
    /// `held(fix-stalled)` notice then names the current head beside a clock
    /// belonging to the previous one. So this is the anchor, re-stamped by
    /// [`note_fix_push`](DriveEntry::note_fix_push) on every observed head move
    /// while it is set — and it is a THIRD clock only in the sense that it
    /// measures a wait `state_since_ms` cannot.
    ///
    /// **Written by [`advance`](DriveEntry::advance) on every arc** — `Some` on
    /// `fix-wait` -> `ci-wait`, `None` otherwise — and re-stamped by
    /// `note_fix_push` in between. Assigning on every arc is not optional:
    /// an entry that carried it out of `ci-wait` and back in by arc 10 would
    /// claim a push that did not happen. And `note_fix_push` re-stamps only
    /// what is already `Some`, so a head move in any other state cannot
    /// manufacture one.
    ///
    /// **`None` on an entry written before this field existed**, and that is
    /// the safe direction rather than an accident: such a drive advances on
    /// green alone, which is the pre-#2168 behaviour, so an upgrade mid-drive
    /// costs at most the one re-record round it was already going to cost. The
    /// other direction would park a first drive on `held(fix-stalled)` waiting
    /// for a `report(done)` its worker was never asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_pushed_ms: Option<u64>,
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
    /// When the live-delegate cap started refusing this drive's lane spawns,
    /// absolute — the [`HeldReason::CapFull`] anchor (#2109).
    ///
    /// **`None` rather than a zero sentinel**, deliberately, because this is
    /// the one clock here whose "unset" and whose lowest legal value are both
    /// reachable in the same run: `fix_handback_ms` argues at length that a
    /// zero cannot occur in `fix-wait`, and that argument does not transfer —
    /// a cap refusal is observed at whatever `now_ms` the tick was handed, and
    /// a test harness ticks from small numbers. `Option` makes "not currently
    /// starved" a value the type carries rather than one a comment defends.
    ///
    /// Stamped by the tick on the FIRST **cap** refusal of a run and left alone
    /// by the cap refusals after it, so what it measures is the duration of the
    /// starvation and not the age of the most recent tick.
    ///
    /// **Cleared at four sites, and the NON-CAP one is what makes the word
    /// "continuously" on [`HeldReason::CapFull`] true** (#2109 review 4).
    /// Named rather than numbered, because an ordinal here is a claim about a
    /// list's ORDER and goes stale the moment the list grows: #2135 added a
    /// fourth site and bumped "the third" to "the fourth" in step, which
    /// silently moved the credit onto the restart clear — contradicting the
    /// citation on this very sentence, the sentence after it, and two other
    /// surfaces.
    /// [`clear_cap_starvation`](DriveEntry::clear_cap_starvation) runs when a
    /// lane does open, and on any refusal that is **not** the cap's;
    /// [`advance`](DriveEntry::advance) runs on every arc; and
    /// [`discard_cap_starvation_run`](DriveEntry::discard_cap_starvation_run)
    /// runs at §2.4's restart reconcile. The first two are the tick's, and it is
    /// the non-cap one that keeps this a claim about the
    /// run happening NOW: guarded on the write edge alone, the stamp was a
    /// latch, and a single early cap refusal aged into `held(cap-full)` behind
    /// a run of refusals that were nothing of the kind. The arc clear is its own
    /// reason — a drive that MOVED is not the drive that was stuck, and carrying
    /// the stamp across an arc would let a later, unrelated refusal inherit a
    /// duration it did not spend. The restart clear is #2135's, and is the only
    /// one that charges nothing: a run cannot straddle a process boundary,
    /// because no tick observed the cap across it — that function carries the
    /// whole argument.
    ///
    /// **This field IS serialized, so the stamp survives a shutdown**, and the
    /// restart clear above is what stops that from being a defect rather than a
    /// feature. It is written for the same reason every other clock here is:
    /// §5.2 is the drive's whole memory, and a field the tick decides from that
    /// the file omitted would make a resumed drive decide from a different
    /// entry than the one that was stored.
    ///
    /// Absent is the resting state, so it is not serialized when absent —
    /// `owed_notice`'s reason, unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_starved_since_ms: Option<u64>,
    /// When the drive entered the state it is in now — the
    /// [`HeldReason::StateStalled`] anchor (#2110).
    ///
    /// **This IS the general "when did the state last change" stamp the module
    /// header used to say must never exist, and the ban is discharged rather
    /// than ignored.** What §2.2 forbade was an idle clock *as the drive's only
    /// bound*, and its worked example is §8's `also: [base-green]` row: a drive
    /// that advances `gate-check` → `ci-wait` on every wake resets a per-state
    /// stamp forever, so a bound measured from one alone would leave it parked
    /// on a red default branch in silence. That is still true, and it is still
    /// the reason `started_ms` and `drive-stalled` remain — as the BACKSTOP the
    /// base-green cycler falls through to. The two clocks answer different
    /// questions, and the incident that produced #2110 is what happens when only
    /// the second is asked: a drive with progress on every axis was reported as
    /// stalled, because an age cannot tell progress from paralysis.
    ///
    /// Written by [`advance`](DriveEntry::advance) on EVERY arc, which is the
    /// difference from [`fix_handback_ms`](DriveEntry::fix_handback_ms) beside
    /// it — that one is stamped on two arcs and means something narrower.
    ///
    /// **Zero on an entry written before this field existed**, which reads as
    /// "entered at the epoch" and so as a state older than any bound. That is
    /// the safe direction and it is bounded rather than merely tolerable: such
    /// an entry holds `state-stalled` on the first tick at which the DRIVE'S OWN
    /// AGE reaches the bound: [`state_elapsed_ms`](DriveEntry::state_elapsed_ms)
    /// is capped at that age (#2117 review 3), so a young entry is not parked —
    /// which is correct, and is what this sentence overstated as "on its first
    /// tick" for as long as the cap existed. Its notice names the state, and its
    /// remedy — `drive_review` — re-stamps this field through `advance` and does
    /// not re-hold. The alternative
    /// (treating zero as "unknown, exempt") would make the field's own bound
    /// unreachable for exactly the entries that predate it.
    #[serde(default)]
    pub state_since_ms: u64,
    /// Milliseconds this drive has spent unable to spawn, summed over the
    /// STARVATION RUNS THAT HAVE ENDED, for the whole life of the drive —
    /// subtracted from the age `drive-stalled` measures (#2110).
    ///
    /// **A hold is not progress and it is not a stall**, which is the whole of
    /// why this is subtracted rather than counted: the measured drive spent
    /// three of its four hours in `review-wait` with `lanes: []` because
    /// another drive's released lanes held every slot, and that starvation was
    /// charged to the budget of the drive that was starved. Nothing about that
    /// time was the driven PR's doing and nothing about it was recoverable by
    /// the orchestrator the notice went to.
    ///
    /// **The run in flight is NOT in here**, deliberately: it is
    /// [`cap_starved_since_ms`](DriveEntry::cap_starved_since_ms), and
    /// [`starved_ms`](DriveEntry::starved_ms) adds it live. A stored total that
    /// included an open run would have to be re-written on every tick to stay
    /// true, which is the stored-age mistake `started_ms` argues against one
    /// field up.
    ///
    /// Reset with `started_ms` on arc 11, for that field's reason: a resumed
    /// drive's age starts again, so what it had spent starved before the resume
    /// is not owed back twice.
    #[serde(default)]
    pub starved_total_ms: u64,
    /// The same sum, but only over the runs that ended since the drive entered
    /// its current state — subtracted from the elapsed time
    /// [`HeldReason::StateStalled`] measures (#2110).
    ///
    /// A second accumulator rather than a subtraction of two snapshots, because
    /// `advance` clears [`cap_starved_since_ms`](DriveEntry::cap_starved_since_ms)
    /// on every arc, so a starvation run can never straddle a transition and
    /// the two totals genuinely diverge only in what they are reset by. It is
    /// not always zero: `clear_cap_starvation` ends a run when a lane opens,
    /// which is not a transition, so one `review-wait` may contain several.
    #[serde(default)]
    pub starved_state_ms: u64,
    /// The working state this drive parked OUT of, and how long it had been
    /// there — what a hold's notice and `review_drive_status` need to say what
    /// the drive was doing when a bound fired (#2110).
    ///
    /// Stamped by [`advance`](DriveEntry::advance) on every arc into `held` and
    /// cleared on every other, so it describes THIS hold and never a previous
    /// one. `None` on an entry that is not parked.
    ///
    /// **The elapsed figure is stamped rather than derived**, because the arc
    /// that records it is the same arc that re-stamps
    /// [`state_since_ms`](DriveEntry::state_since_ms): by the time anything
    /// reads the entry the clock the hold fired on has already been reset, and
    /// a reader recomputing it would report the age of the hold instead of the
    /// wait that caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_from: Option<DriveState>,
    /// Time in [`held_from`](DriveEntry::held_from) at the moment of the hold,
    /// starvation already excluded — see that field.
    #[serde(default)]
    pub held_after_ms: u64,
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
            fix_pushed_ms: None,
            owed_notice: None,
            cap_starved_since_ms: None,
            state_since_ms: now_ms,
            starved_total_ms: 0,
            starved_state_ms: 0,
            held_from: None,
            held_after_ms: 0,
            extra: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> DriveState {
        self.state
    }

    /// Whether this drive is sitting on a head its own worker pushed in answer
    /// to a hand-back — see [`fix_pushed_ms`](DriveEntry::fix_pushed_ms).
    pub fn fix_pushed(&self) -> bool {
        self.fix_pushed_ms.is_some()
    }

    /// **A further push landed while the drive is still waiting on this one**
    /// (#2168 E1, rev-final round 2 premortem 2). Re-anchors the receipts wait
    /// at `now_ms`, so a worker that pushes a follow-up commit gets a whole
    /// window to run the new matrix and report rather than the remainder of the
    /// window the previous push opened.
    ///
    /// **Re-stamps only what is already `Some`**, which is what keeps this from
    /// being a second way to enter the wait: a head move in `review-wait` is
    /// arc 6 and a head move in `fix-wait` is arc 7, and both go through
    /// [`advance`](DriveEntry::advance). Called by S3 at the one place a moved
    /// head is recorded, and that call site already guards on the head having
    /// actually changed and on the read having succeeded — an empty `facts.head`
    /// is a failed read, not a push, and re-anchoring on one would hand a silent
    /// worker a fresh hour every time `gh` hiccuped.
    ///
    /// It cannot postpone the drive indefinitely: `state-stalled` measures from
    /// `state_since_ms`, which nothing here touches, so a drive that pushes
    /// forever still parks on `ci-wait`'s own bound.
    pub fn note_fix_push(&mut self, now_ms: u64) {
        if self.fix_pushed_ms.is_some() {
            self.fix_pushed_ms = Some(now_ms);
        }
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
    /// an arc into `fix-wait`, and stays that way now that
    /// [`state_since_ms`](DriveEntry::state_since_ms) beside it IS written on
    /// every arc (#2110): they anchor different bounds, and folding the narrow
    /// one into the general one would give `held(fix-stalled)` — a claim about
    /// the WORKER's silence — a clock that a lane opening or a gate re-check
    /// could restart.
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
        // Read before the arc is taken and before any clock below is re-stamped
        // (#2110). A refused arc must spend nothing, so nothing is written from
        // these until `transition` has accepted.
        let from = self.state;
        let state_elapsed = self.state_elapsed_ms(now_ms);
        self.state = transition(self.state, to)?;
        self.held_reason = reason;
        match bump {
            Some(Counter::ReviewRounds) => self.counters.review_rounds += 1,
            Some(Counter::CiAttempts) => self.counters.ci_attempts += 1,
            Some(Counter::RebaseAttempts) => self.counters.rebase_attempts += 1,
            // #2509. An ASSIGNMENT to `true` rather than a bump: the grace is
            // one-shot, and `decide_review_wait` refuses to propose this bump
            // a second time, so a set here can only ever be idempotent. It
            // deliberately does NOT touch `review_rounds`, which is what keeps
            // INVARIANT 9's ceiling meaning what it meant.
            Some(Counter::BodyOnlyGrace) => self.counters.body_only_grace = true,
            None => {}
        }
        if to == DriveState::FixWait {
            self.fix_handback_ms = now_ms;
        }
        // **Arc 7, recorded — an ASSIGNMENT and not a set** (#2168 E1). Every
        // other arc clears it, including the three that reach `ci-wait` from
        // somewhere else: an entry that carried the flag out of `ci-wait` and
        // back in by arc 10 would claim a push nobody made, and hold the drive
        // waiting on a `report(done)` its worker was never asked for.
        //
        // Written here rather than in the tick for `fix_handback_ms`'s reason:
        // the arc and the fact it establishes may not come apart. And it is
        // read against `state_since_ms`, which the lines below re-stamp on this
        // same arc — see the field, and the module header on why that is not a
        // fourth clock.
        self.fix_pushed_ms = (from == DriveState::FixWait && to == DriveState::CiWait).then_some(now_ms);
        // **What the drive was doing, recorded before the clocks that say so
        // are reset** (#2110). `held_from` and `held_after_ms` are read by this
        // hold's notice and by `review_drive_status`; both are computed from
        // `state_since_ms`, which the next three lines overwrite.
        if to == DriveState::Held {
            self.held_from = Some(from);
            self.held_after_ms = state_elapsed;
        } else {
            self.held_from = None;
            self.held_after_ms = 0;
        }
        // #2109: an arc is proof the drive is no longer stuck on the cap, so
        // the starvation clock does not survive one. Including the arc INTO
        // `held(cap-full)` itself — the hold is the report, and a resume must
        // start the clock over rather than re-hold on the next tick from a
        // stamp the previous starvation left behind.
        //
        // #2110: what that run COST is kept, though, and the lifetime total is
        // what `drive-stalled` subtracts. Folded here rather than discarded,
        // because the arc out of `held(cap-full)` is precisely the arc after the
        // longest run there is.
        self.end_starvation_run(now_ms);
        self.cap_starved_since_ms = None;
        // The per-state clock, re-stamped on EVERY arc — see `state_since_ms`
        // for why that is the stamp §2.2 used to forbid and why the backstop is
        // what makes it safe.
        self.state_since_ms = now_ms;
        self.starved_state_ms = 0;
        Ok(())
    }

    /// Fold any starvation run in flight into both accumulators and end it.
    ///
    /// Shared by [`advance`](DriveEntry::advance) and
    /// [`clear_cap_starvation`](DriveEntry::clear_cap_starvation) so that the
    /// two ways a run can end cost the same thing — a run charged by one path
    /// and not the other would make the exclusion depend on whether the cap
    /// cleared before or after the drive moved, which is the arbitrary half of
    /// the original defect.
    fn end_starvation_run(&mut self, now_ms: u64) {
        if let Some(since) = self.cap_starved_since_ms {
            let d = now_ms.saturating_sub(since);
            self.starved_total_ms = self.starved_total_ms.saturating_add(d);
            self.starved_state_ms = self.starved_state_ms.saturating_add(d);
        }
    }

    /// Everything this drive has spent unable to spawn, the run in flight
    /// included (#2110). The quantity `drive-stalled` subtracts.
    pub fn starved_ms(&self, now_ms: u64) -> u64 {
        self.starved_total_ms.saturating_add(self.open_starvation_ms(now_ms))
    }

    /// The same, since the current state was entered — what
    /// [`HeldReason::StateStalled`] subtracts.
    pub fn starved_in_state_ms(&self, now_ms: u64) -> u64 {
        self.starved_state_ms.saturating_add(self.open_starvation_ms(now_ms))
    }

    fn open_starvation_ms(&self, now_ms: u64) -> u64 {
        self.cap_starved_since_ms.map_or(0, |since| now_ms.saturating_sub(since))
    }

    /// How long this drive has been in its current state, starvation excluded
    /// — the [`HeldReason::StateStalled`] measure (#2110).
    ///
    /// **Capped at the drive's own age, because a drive cannot have been in a
    /// state longer than it has existed** (#2117 review 2, premortem 1). That
    /// is a tautology about a healthy entry and load-bearing about one entry
    /// class that is not: an entry written before
    /// [`state_since_ms`](DriveEntry::state_since_ms) existed reads zero there,
    /// so the raw subtraction answers `now` — an epoch-scaled figure. The HOLD
    /// that produces is correct and argued at that field; what was wrong is the
    /// number, because [`advance`](DriveEntry::advance) stamps `held_after_ms`
    /// from here and the notice then told an operator their drive had been in
    /// `ci-wait` for some twenty thousand days. Capping makes that notice read
    /// the drive's real age, which is both true and the figure they want.
    pub fn state_elapsed_ms(&self, now_ms: u64) -> u64 {
        now_ms
            .saturating_sub(self.state_since_ms)
            .saturating_sub(self.starved_in_state_ms(now_ms))
            .min(self.bounded_age_ms(now_ms))
    }

    /// Record that the live-delegate cap refused this drive's lane spawn, and
    /// answer whether that changed anything (#2109).
    ///
    /// **First-refusal-wins**: a run of CAP refusals is one starvation, and the
    /// stamp is when it began. ("Cap" rather than bare "refusals" because that
    /// is the only kind that reaches here at all, and the loose word read as a
    /// claim the field's own doc contradicts — #2135, folded from #2112's final
    /// pass.) Re-stamping on each tick would make
    /// [`CAP_HOLD_MS`] unreachable — the same defeat-your-own-bound shape
    /// `decide`'s empty-head guard describes, where every tick re-armed the
    /// clock meant to catch it.
    pub fn note_cap_starvation(&mut self, now_ms: u64) -> bool {
        if self.cap_starved_since_ms.is_some() {
            return false;
        }
        self.cap_starved_since_ms = Some(now_ms);
        true
    }

    /// Forget any recorded cap starvation, and answer whether there was one.
    ///
    /// **Two callers, both the tick's**: a lane DOES open, or the tick takes a
    /// refusal that is **not** the cap's. Both mean the cap run is over, and the
    /// second is #2109 review 4's — without it this doc described a rule the
    /// tick had stopped following.
    ///
    /// The stamp has two clear sites that are not calls of this function, and
    /// they are said explicitly because "who calls this" and "what clears the
    /// stamp" are different questions — a reader who conflates them will grep
    /// for this name and conclude the other two do not exist.
    /// [`advance`](DriveEntry::advance) zeroes the field directly on every arc,
    /// and [`discard_cap_starvation_run`](DriveEntry::discard_cap_starvation_run)
    /// is the restart reconcile's (#2135).
    ///
    /// Takes `now_ms` since #2110 because ending a run is no longer free: what
    /// it cost is folded into the accumulators the age bound subtracts, and a
    /// run ended without charging it would leave the time in the budget of the
    /// drive that was starved. That is exactly why the restart clear is a
    /// separate function rather than a call of this one — see it.
    pub fn clear_cap_starvation(&mut self, now_ms: u64) -> bool {
        self.end_starvation_run(now_ms);
        self.cap_starved_since_ms.take().is_some()
    }

    /// Drop a starvation run **without charging what it cost**, and answer
    /// whether there was one — §2.4's restart reconcile (#2135).
    ///
    /// **A starvation run cannot straddle a process boundary**, because every
    /// other site that touches this stamp is a tick that OBSERVED the cap, and
    /// across a shutdown no tick ran. What the field means — "the cap has been
    /// refusing this drive's lane spawns continuously since this instant" — is
    /// therefore not a claim the surviving stamp can still make: the interval
    /// between the last tick of the old process and the first of the new one is
    /// time in which the cap refused nothing, and after a restart every pane
    /// this group's cap was counting is gone. Left standing, a stamp older than
    /// [`CAP_HOLD_MS`] parks the resumed drive `held(cap-full)` on its FIRST
    /// tick, before a single spawn is attempted, on a notice telling an
    /// orchestrator to free a slot in a group whose slots are all free.
    ///
    /// **Nothing is charged, and that is the choice rather than an oversight.**
    /// [`clear_cap_starvation`](DriveEntry::clear_cap_starvation) folds
    /// `now - since` into the two accumulators both age bounds subtract, and
    /// here that difference is mostly orrerix's own downtime — so charging it
    /// would FORGIVE the downtime from `drive-stalled` and `state-stalled`,
    /// which is precisely the property #2117 disclosed and pinned as charged
    /// (`orrerix_downtime_is_charged_to_the_state_it_spanned`). The stamp is a
    /// START and not a total, so the genuinely-starved stretch before the
    /// shutdown cannot be recovered from it either; discarding loses that
    /// forgiveness, which fails toward parking rather than toward silence and
    /// is the direction every other unknown here is resolved in.
    ///
    /// **Scoped to the process boundary, and no wider.** An in-process tick gap
    /// longer than [`CAP_HOLD_MS`] still parks on a single observed refusal —
    /// the reconcile is once per group per REGISTRY INSTANCE, so nothing here
    /// reaches it. Registry and not process, precisely: the latch is a field of
    /// the registry, so the two coincide only while a process holds one, which
    /// is true today and is not a thing the type system holds true. A second
    /// registry built over a live state root would forgive a genuinely open run
    /// — this defect in reverse, with `cap_run_forgotten: true` on the row to
    /// make it look intended. No test here can see the difference, because
    /// `relaunch_registry` IS a second registry in one process (#2135 review 2,
    /// premortem 1).
    /// That residual is real and is pinned rather than merely admitted, by
    /// `an_in_process_tick_gap_still_parks_on_a_single_observed_cap_refusal`.
    pub fn discard_cap_starvation_run(&mut self) -> bool {
        self.cap_starved_since_ms.take().is_some()
    }

    /// How long the cap has been refusing this drive's lane, or `None` when it
    /// is not currently refusing one.
    pub fn cap_starved_for(&self, now_ms: u64) -> Option<u64> {
        self.cap_starved_since_ms.map(|t| now_ms.saturating_sub(t))
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
    ///
    /// **`spawned_ms` is the `lane-stalled` ANCHOR the caller chose, not a
    /// clock read** (#2163). For an ordinary brief it is `now`; for the re-open
    /// of a lane whose pane DIED it is the anchor the previous brief set, so
    /// replacing a dead pane cannot hand the lane a fresh hour of silence.
    /// [`lane_stall_anchor`] is that choice, made in one place.
    pub fn open_lane(
        &mut self,
        block: &str,
        session: &str,
        agent: &str,
        head: &str,
        body_digest: Option<&str>,
        spawned_ms: u64,
        verify: bool,
        body_only: bool,
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
            spawned_ms,
            // #2168 E2. Recorded from the STEP rather than re-derived here: the
            // decision is `decide_review_wait`'s, taken on the same facts that
            // chose the lane, and a second derivation at the write would be a
            // second implementation of a capability grant.
            briefed_verify: verify,
            // #2509, and recorded from the STEP for `briefed_verify`'s reason
            // one line up: the decision is `decide_review_wait`'s, taken on the
            // same facts that chose the lane.
            briefed_body_only: body_only,
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
    /// **Wherever a hand-back is outstanding, which since #2168 E1 is two
    /// states and not one** (rev-final round 2, finding 1). This used to read
    /// "only in `fix-wait`; in any other state there is no hand-back to be
    /// waiting on and nothing the worker was asked for", and E1 falsified both
    /// halves for `ci-wait` on a head that arrived by arc 7: there the worker
    /// HAS been handed a round and IS being waited on, for the very
    /// `report(done)` this line tells it to send.
    ///
    /// Left state-scoped, the #1959 defect reappears one state over and worse.
    /// §7's interception is keyed on the calling agent and not on the drive's
    /// state, so a `report(progress)` there is consumed, the worker is answered
    /// `"reported to orchestrator"`, nothing reaches the orchestrator's pane and
    /// nothing is typed back into the worker's — and a fix timeout later the
    /// hold says the driver has heard nothing from a worker that spoke. That is
    /// the false claim `rddrive::held_notice`'s own comment goes out of its way
    /// to avoid, arriving through a guard that reads one worker input by a
    /// different rule from the other four.
    ///
    /// The budget is unchanged and still one per HAND-BACK, not one per state:
    /// `fix_handback_ms` is not re-stamped by arc 7, so a worker answered in
    /// `fix-wait` is not answered again after it pushes. The same round, the
    /// same answer, once.
    pub fn kickback_owed(&self) -> bool {
        let handed_back = self.state == DriveState::FixWait
            || (self.state == DriveState::CiWait && self.fix_pushed());
        handed_back && self.fix_kickback_ms < self.fix_handback_ms
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

    /// The drive's age on the wall — what `review_drive_status` reports as
    /// `since_ms`, and **no longer what `drive-stalled` measures**: see
    /// [`bounded_age_ms`](DriveEntry::bounded_age_ms).
    ///
    /// The two are kept apart rather than folded because they answer different
    /// questions and a reader needs both. A human asking "how long has this
    /// drive been going" wants the wall figure — the queue's own `since_ms` is
    /// this, and an age that silently shrank when a cap cleared would be a
    /// worse answer than the one it replaced. A BOUND asking the same question
    /// must not charge the drive for time it was not allowed to act, which is
    /// #2110. `review_drive_status` publishes the wall age and the excluded
    /// total side by side so the difference is visible rather than inferred.
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.started_ms)
    }

    /// The drive's age with starvation excluded — §2.2's `drive-stalled`
    /// measure since #2110.
    pub fn bounded_age_ms(&self, now_ms: u64) -> u64 {
        self.age_ms(now_ms).saturating_sub(self.starved_ms(now_ms))
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

    /// **Un-record the pane [`releasable`] named, keeping the conversation**
    /// (#2501) — the record half of a release. Answers the pane id it dropped,
    /// or `None` when there was nothing to drop.
    ///
    /// # Why the pane slot is CLEARED rather than marked
    ///
    /// A released pane is dead, and [`forget_dead_panes`](DriveEntry::forget_dead_panes)
    /// already argues at length why a dead pane is safe to forget: `resolve_token`
    /// refuses a caller whose agent is `Dead` and has no entry for one that is
    /// gone, so such a pane cannot reach the MCP seam at all and there is no
    /// traffic left for this drive to fail to own. §7's interception key is
    /// therefore not weakened by dropping it, and three readers get the right
    /// answer for free instead of needing a second field threaded into each:
    /// `owned_panes` stops naming a pane an exit notice would say is "still
    /// running"; `rd_live_lane_pane`'s duplicate refusal stops guarding a pane
    /// that is not there; and `rd_dead_lane_pane` stops reporting a deliberate
    /// release as a lane this drive LOST, which is what `rd-lane-reopened` means
    /// and would have been a false claim on the row a reader chases after
    /// killing an idle delegate.
    ///
    /// It is not pushed onto `prior_agents` either, for the same reason and one
    /// more: that list is bounded by liveness, so the very next
    /// `forget_dead_panes` would drop it — recording it would be a write whose
    /// only effect is to be undone.
    ///
    /// # The conversation, and the one thing that makes this fail closed
    ///
    /// `session` is what [`LaneRecord::session`] should carry afterwards, and it
    /// is passed IN rather than read off the record for #2109's reason (see
    /// [`LaneRecord::reseeded`]): the recorded field is what a SPAWN returned,
    /// which is a session id only on a CLI that pre-assigns one, so a copilot or
    /// opencode lane carries `""` and its conversation lives on the pane and the
    /// roster row instead. The caller resolves all three sources and hands the
    /// answer here, and this **refuses the release outright** when the answer is
    /// empty: dropping the pane of a lane whose session cannot be named would
    /// destroy the conversation this whole mechanism promises to keep. The
    /// worker's session is the entry's own and is refused the same way.
    pub fn release_pane(&mut self, role: &DrivenRole, session: &str) -> Option<String> {
        let session = session.trim();
        match role {
            DrivenRole::Worker => {
                if self.worker_session.trim().is_empty() || self.worker_agent.is_empty() {
                    return None;
                }
                Some(std::mem::take(&mut self.worker_agent))
            }
            DrivenRole::Lane(block) => {
                let rec = self.lanes.iter_mut().find(|l| l.block == *block)?;
                if rec.agent.trim().is_empty() {
                    return None;
                }
                if rec.session.trim().is_empty() {
                    if session.is_empty() {
                        return None;
                    }
                    rec.session = session.to_string();
                }
                Some(std::mem::take(&mut rec.agent))
            }
        }
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

/// One lane's live reading: the block the gate named, the verdict file as
/// `workflow::parse_verdict_file` returned it (or `None` for no verdict yet),
/// and whether the pane this drive recorded for that lane is GONE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneFact {
    pub block: BlockId,
    pub verdict: Option<ReviewVerdict>,
    /// **The recorded pane is positively [`AgentStatus::Dead`]** (#2163) — the
    /// lane-side twin of the `worker_exit` observation `fix-wait` has had since
    /// #1961, and read for the same reason: `review-wait` waits for a verdict
    /// that a dead pane can never produce.
    ///
    /// Before this the exit was observed only for the worker and only in
    /// `fix-wait` — this module's own comment said so, and gave the reason as
    /// "`review-wait` has `lane-stalled` for its own panes". That bound is
    /// `lane_timeout_minutes` (60 at stock knobs) measured from the brief, so a
    /// reviewer pane killed at minute twelve left the drive silent for
    /// forty-eight more with no rd-* row at all — measured on PR #2140, and
    /// reached on the driver's OWN advice, since a `cap-refused` notice tells
    /// an orchestrator to kill an idle delegate and a lane that has finished
    /// its turn is on that list.
    ///
    /// **`false` is "we could not check" as well as "it is alive"**, and the
    /// fail direction is deliberate: the same asymmetry
    /// [`DriveEntry::forget_dead_panes`] states, resolved the same way
    /// `rd_pane_exit` resolves it — only a positive `Dead` counts, so an
    /// emptied agent map (a restart) re-opens nothing.
    ///
    /// A lane with no record, or one whose record carries no pane, is `false`:
    /// there is no pane to be dead, and the lane is opened by the ordinary path.
    pub pane_dead: bool,
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
    ///
    /// `verify` is #2168 E2: this brief is a **body-verification delta** — every
    /// required lane has passed the code at this head and only the PR body has
    /// moved, so what this lane is asked for is the body as it stands. It
    /// travels on the step rather than being re-derived at the write because it
    /// becomes a grant on the recorded verdict, and a grant derived twice is a
    /// grant that can disagree with itself.
    ///
    /// `body_only` is #2509, and travels for the same reason: this brief is
    /// about the BODY of a revision whose CODE this drive has already reviewed
    /// to completion — the head has not moved, this lane was already briefed at
    /// it, and every required lane has ANSWERED at it. It is strictly weaker
    /// than `verify`, which it nests inside (`verify` needs every lane to have
    /// PASSED), and the gap between the two is the case #2509 exists for: a
    /// lane that recorded `fail` on the body. It becomes
    /// [`LaneRecord::briefed_body_only`], which is the ONLY thing the one-shot
    /// grace below is granted on.
    OpenLane { index: usize, verify: bool, body_only: bool },
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

/// Whether a lane's `pass` still **settles** this revision — the currency rule
/// above, or the #2168 E2 delegation: a required lane recorded a
/// body-verification pass covering the body as it stands, and this pass is
/// bound to the same head.
///
/// **`verified` is passed in rather than computed here** because it is a
/// property of the whole required set, not of one lane, and re-deriving it per
/// lane would make the answer depend on iteration order. [`first_stale_lane`]
/// computes it once.
///
/// The delegation is asked through the gate's own
/// [`ReviewVerdict::pass_covers_body`], not re-implemented, for §4's reason: a
/// driver that decided "this lane is settled" on a rule the gate does not share
/// would drive to `gate-check` and be refused there for ever.
///
/// **That claim is scoped to the DELEGATION arm, and the scoping is not
/// pedantry** (#2308 review 1, premortem 2). The first arm is deliberately NOT
/// the gate's rule: [`lane_pass_is_current`] reads an unknown digest — the
/// verdict carries none, or the body could not be read now — as *not drift*,
/// because one transient `gh` failure must not re-brief every open lane in the
/// group, while the gate refuses an empty digest outright (unknown may never
/// discharge a merge condition). So on a gate declaring `body-unchanged`, a pass
/// recorded during a body-read outage settles here and is refused there, and the
/// drive cycles `gate-check -> ci-wait -> review-wait` on unchanged facts.
///
/// **Pre-existing, bounded, and deliberately not closed here.** That divergence
/// predates #2168 E2 — it is the #565 asymmetry meeting the #791 one, and both
/// arms are right about their own question — and it is bounded by
/// `drive_timeout_minutes` into `held(drive-stalled)`, which is the same exit
/// §8's `also: [base-green]` row parks on. Closing it means deciding which
/// asymmetry yields, which is a change to the gate's contract rather than to
/// this function, and it wants its own slice. What this slice DOES pin is that
/// the two sides agree about the verification question itself, over every
/// crossing of it: `the_driver_and_the_gate_answer_the_verification_question_identically`.
fn lane_pass_settles(
    verdict: Option<&ReviewVerdict>,
    head: &str,
    body_digest: Option<&str>,
    verified: bool,
) -> bool {
    if lane_pass_is_current(verdict, head, body_digest) {
        return true;
    }
    verdict.is_some_and(|v| v.pass_covers_body(head, body_digest, verified))
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

/// The `lane-stalled` anchor [`DriveEntry::open_lane`] must be given for the
/// brief about to be sent — `now_ms` for an ordinary one, and the anchor
/// already on the record when this brief is only REPLACING a dead pane (#2163).
///
/// **The re-open must not re-arm the clock, and that is what bounds the
/// re-open.** `decide_review_wait` re-opens a lane whose recorded pane is
/// `Dead`, so a pane that dies on every spawn would be replaced on every tick
/// for as long as the drive lives if each replacement started the silence
/// timer over. Preserving the anchor makes `lane-stalled` reachable through the
/// loop: the lane parks at `lane_timeout_minutes` from the ORIGINAL brief,
/// naming itself, and the notice's own remedy (read that pane) is the right one.
///
/// **It is also the more honest reading of the field.** `spawned_ms` anchors a
/// SILENCE test — `decide_review_wait`'s stall arm exempts a lane that has
/// answered — and a pane dying and being replaced does not make a lane less
/// silent about the head it was asked about. Re-arming would be the
/// defeat-your-own-bound shape `decide`'s empty-head guard describes.
///
/// **Only for a replacement at the SAME REVISION, and the revision is the full
/// `(head, digest)` key** — [`lane_open_for`], the one place that comparison is
/// written. A lane with no record, or one whose recorded pane is alive (an
/// ordinary re-brief, a reuse fall-through), gets `now_ms` too: nothing about
/// those is a replacement.
///
/// **The digest half is not decoration, and keying on the head alone was an
/// asymmetry** (#2169 review 2, N1). This module treats `(head, digest)` as ONE
/// revision key — §5.2 says so in as many words, `lane_open_for` implements it,
/// and a verdict binds to both — so a head-only test here answered "same round"
/// for a case the rest of the module calls a new one: a lane briefed at
/// `(H, d1)` whose pane dies, followed by a BODY-ONLY fix moving the digest to
/// `d2`, re-opens with a reviewer that has read nothing and inherits the
/// original anchor. Fifty minutes in, that fresh reviewer had nine minutes
/// before `held(lane-stalled)` — while the *same* body-only fix under a LIVE
/// idle pane re-arms to `now` and gets the full window, which is the same
/// revision getting two different clocks depending on whether a pane happened
/// to die.
///
/// **And the bound does not need the digest case.** The runaway this exists to
/// stop is a pane that dies on every spawn, which loops on the tick's own clock
/// with nothing else moving; a digest only moves when a human or a worker edits
/// the PR body, so it cannot drive a loop. Widening the exception to a new
/// revision therefore costs the bound nothing.
///
/// An **unknown** live digest does not re-arm: `lane_open_for` reads "we could
/// not check" as still-open rather than as drift, which is the same asymmetry
/// [`ReviewVerdict::body_changed`] encodes and the fail-safe direction here —
/// one transient `gh` failure to read a PR body must not hand every dying lane
/// a fresh hour.
///
/// A recorded anchor of `0` is a pre-field row, which is "unset" rather than
/// "the epoch"; it takes `now_ms` for the same reason `fix_handback_ms == 0`
/// means ancient rather than unset elsewhere in this module.
pub fn lane_stall_anchor(
    rec: Option<&LaneRecord>,
    head: &str,
    body_digest: Option<&str>,
    pane_dead: bool,
    now_ms: u64,
) -> u64 {
    match rec {
        Some(r)
            if pane_dead
                && !head.is_empty()
                && lane_open_for(r, head, body_digest)
                && r.spawned_ms != 0 =>
        {
            r.spawned_ms
        }
        _ => now_ms,
    }
}

/// Whether one of the required lanes has recorded a **body-verification pass**
/// covering the body as it stands (#2168 E2) — the driver's read of
/// [`mergeq::body_verified_by_required`](crate::mergeq::body_verified_by_required),
/// over the same routed reviewer list the gate would use.
///
/// One definition, asked through [`crate::workflow::body_verified`], because a
/// driver that answered this differently from the gate would advance to
/// `gate-check` and be refused there on every tick until `state-stalled`.
pub fn body_is_verified(required: &[LaneFact], head: &str, body_digest: Option<&str>) -> bool {
    crate::workflow::body_verified(
        required.iter().filter_map(|l| l.verdict.as_ref()),
        head,
        body_digest,
    )
}

/// The first lane whose `pass` does not stand at this (head, digest) — where
/// arc 8 re-enters after a body-only fix (§8 row 5), and equally where
/// `review-wait` resumes when a digest moves under a recorded pass.
///
/// Returns `required.len()` when every lane's pass stands, which is the
/// "nothing left to review" answer arc 4 acts on.
///
/// **Since #2168 E2 a body-only move re-briefs ONE lane, not all of them.**
/// Before the verification pass exists, every lane's pass is stale at the new
/// digest and this returns the FIRST of them — which is the lane the gate's own
/// `reviewers:` order puts first, and the order the driver already briefs in.
/// Once that lane answers with a verification pass,
/// [`body_is_verified`] is true and every other lane's pass at the head it was
/// bound to settles again, so this returns `required.len()` and the drive goes
/// to `gate-check` rather than walking the rest of the list. That walk is the
/// re-record cascade #2168 measured; the argument for accepting the delegation
/// is on `mergeq::body_unchanged`.
///
/// **Which lane is "first" is the gate file's decision, not this module's.**
/// `required` arrives in `RoutingDecision::required` order, which is the
/// `reviewers:` list a repo wrote plus the routed additions — so a repo says
/// which lane it wants asked first by writing it first, and orrerix never
/// forms an opinion about what a lane costs (CLAUDE.md constraint 8).
pub fn first_stale_lane(required: &[LaneFact], head: &str, body_digest: Option<&str>) -> usize {
    let verified = body_is_verified(required, head, body_digest);
    required
        .iter()
        .position(|l| !lane_pass_settles(l.verdict.as_ref(), head, body_digest, verified))
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
///    what keeps stopping the line from being silent. Since #2110 it is the
///    BACKSTOP rather than the working bound, and the reason it still outranks
///    everything below is unchanged: it is the one bound the base-green cycler
///    cannot reset.
/// 5. **Then time in the current state** (#2110) — [`state_bound_ms`], reset by
///    every transition, which is what an orchestrator actually wants bounded.
///    Below the age for the reason argued at the line itself: both are past
///    only for a drive resumed out of a very long park, where the twelve-hour
///    figure is the more important of the two.
/// 6. Then the state's own logic.
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
    if entry.bounded_age_ms(facts.now_ms) >= minutes_ms(limits.drive_timeout_minutes) {
        return DriveStep::held(HeldReason::DriveStalled);
    }
    // **5a. Then time in THIS state** (#2110) — the bound that does the work,
    // with the age above it as the backstop it falls through to.
    //
    // Below the age deliberately, and the ordering is not the one it looks like
    // it should be. The state bounds are smaller, so in the ordinary run of
    // events one of them fires first *in time* and this line is never reached
    // with both past. Where both ARE past, the drive has just been resumed out
    // of a long park — arc 11 re-stamps `started_ms` and `state_since_ms`
    // together, so the only way to be past the twelve-hour backstop at all is a
    // drive that has genuinely run that long, and that fact outranks whichever
    // state it happens to be sitting in. Putting this first would also reopen
    // §8's `also: [base-green]` row from the other end: that drive resets this
    // clock on every wake, so it must reach the age check, and an ordering that
    // let a per-state bound answer first for some other drive is one more thing
    // to keep true.
    //
    // `required_lanes` is the gate's list at this head; `None` is a routing
    // answer this tick could not produce, and that drive holds
    // `routing-unaccountable` in `decide_review_wait` below on this same tick.
    let lanes = facts.required_lanes.as_deref().map_or(0, |l| l.len());
    if let Some(bound) = state_bound_ms(state, limits, lanes) {
        if entry.state_elapsed_ms(facts.now_ms) >= bound {
            return DriveStep::held(HeldReason::StateStalled);
        }
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
        // **Arc 2, and since #2168 E1 green is not on its own enough when the
        // head arrived by arc 7.** See [`decide_fix_receipts`] for the whole
        // argument; the guard is here, on the Green arm alone, because it is
        // the only arm that briefs a reviewer. Red and CONFLICTING hand the
        // worker back through arc 3, where `decide_fix_wait`'s own ladder
        // already answers every worker signal, and `Pending`/`Unknown` wait as
        // they always did.
        CiObservation::Green if entry.fix_pushed() => decide_fix_receipts(entry, facts, limits),
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

/// Green at a head the worker pushed under arc 7: **arc 2 waits for that
/// worker's `report(done)` first** (#2168 E1, closing #1875's class).
///
/// # What green alone does not mean
///
/// A green matrix says the checks settled; it does not say the ROUND is over.
/// This repo's worker persona forbids a `report(done)` before the whole matrix
/// has been re-read, and the PR body's CI receipts — run ids, per-platform
/// conclusions, the head they were measured at — cannot be written until that
/// same moment. So the sequence a green observation sits in the middle of is
/// fixed: push, checks settle, worker reads them, worker edits the body, worker
/// reports. A lane briefed on the green brief is briefed at digest d1 and
/// re-briefed at d2 the moment the receipts land, because
/// [`first_stale_lane`] re-reads the `(head, digest)` key every tick and a
/// `pass` recorded at d1 does not stand at d2. #1875's measurement is that
/// every code PR of that session paid at least one such round, and #1870 is the
/// fully instrumented one: `pass` at digest `bbff76b8`, 0 findings, the CI
/// section filled, `BODY CHANGED SINCE PASS`, gate blocked, re-record — with
/// the head never having moved and not one line of code having changed.
///
/// The digest rule itself is not weakened here and must not be: the body
/// becomes the squash commit message, so a `pass` recorded against different
/// text really has approved something else. What changes is only WHEN the lane
/// is briefed — after the revision has stopped moving rather than during.
///
/// # The two shapes this deliberately is not
///
/// #1875 offers three candidate fixes and two are refused. A **digest
/// carve-out** — a fenced evidence region excluded from the digest — needs that
/// region to be genuinely un-claim-bearing, and a CI section that also carries
/// prose is back where it started; it would also make `body-unchanged` a
/// weaker condition than the `gh` shim's, which §4 forbids. **The engine
/// writing the CI section itself** puts a second author on the PR body beside
/// the worker, which is a wire change and a new class of §3.1 action, for
/// receipts the worker already produces.
///
/// # The worker ladder is `decide_fix_wait`'s, not a second one
///
/// The four signals are read in the same order and answered with the same
/// holds, because this is the same wait for the same worker on the same
/// hand-back — only its location moved. Reading them by a different rule here
/// is the asymmetry `CLAUDE.md` names ("a guard reads every one of its inputs
/// by one rule"), and the two that would be dropped are the two that matter
/// most: a `Blocked` worker is INVARIANT 3 territory and a dead resumed pane is
/// `worker-unresumable`, and letting either fall through to the timeout below
/// would report both as `fix-stalled` — a claim that the worker went silent,
/// about a worker that said something.
///
/// # The bound, and why it is `fix_timeout_minutes` measured from the push
///
/// A silent worker must not hold the drive for ever, and the wait is the same
/// wait §2.2 already bounds — so it takes the same knob. The anchor is
/// [`DriveEntry::fix_pushed_ms`], the LATEST push in this `ci-wait` stay. Not
/// `fix_handback_ms`, which predates the push and would charge the worker for
/// the time it spent doing the work it was asked to do; and not
/// `state_since_ms`, which is the FIRST push, so a follow-up commit late in the
/// window would get the remainder of a window rather than one (rev-final round
/// 2, premortem 2).
/// [`state_bound_ms`]'s `ci-wait` arm ADDS the same knob to its constant so
/// that `held(state-stalled)` cannot preempt this hold on a repo that raised
/// it — the property [`HeldReason::StateStalled`] states, and the reason that
/// arm is an add rather than a max is argued there.
///
/// **`fix_timeout_minutes` cannot push that bound past the backstop**, which is
/// why this arm needs no counterpart to [`state_bound_ms`]'s `review-wait`
/// residual: `parse_workflow` runs the knob through `clamp_expires_minutes`, so
/// the largest value a workflow file can produce is 240 minutes and the sum
/// tops out at 330 against a twelve-hour `drive_timeout_minutes`. The engine
/// deliberately does not re-clamp the timeouts ([`DriveLimits::clamped`] takes
/// only the counters), so a caller building limits directly can still exceed
/// it — and there the age answers first, exactly as `review-wait`'s residual
/// describes. Pinned at the ceiling by
/// `the_ci_wait_bound_funds_the_receipts_wait_instead_of_preempting_it`.
///
/// # Two residuals, both bounded and both recoverable by the resume
///
/// **A worker pane that DIES here.** #1961's exit read is asked only in
/// `fix-wait` (`rdtick`'s `worker_exit`), so a pane killed while the drive
/// waits in `ci-wait` is not seen as `Unresumable` on the next tick; that drive
/// waits out `fix_timeout_minutes` and parks `held(fix-stalled)`, which is
/// bounded and named but is the slower notice. Disclosed rather than closed:
/// widening the exit read is an S3 change to what a tick OBSERVES, and this
/// slice changes what `decide` does with what it is already given.
///
/// **A worker that pushes and reports inside ONE tick window.** Both facts
/// reach the same tick, [`decide_fix_wait`]'s arc 7 outranks the report on
/// purpose ("the code moved and CI is what has to answer next"), and `rdtick`
/// clears the signal on every arc — so the report is spent on the arc and this
/// state waits for one that will not come, to `held(fix-stalled)`. It is not
/// closed here because a `WorkerSignal` is a word and not a timestamp: the same
/// `Done` beside a moved head is equally consistent with *reported, then
/// pushed*, where the report is about the pre-push tree and honouring it would
/// brief a lane over exactly the unfinished revision #1875 is about. Failing
/// toward a bounded hold rather than toward that defect is the direction this
/// slice takes everywhere, and the ordering is uncommon here by construction —
/// the worker persona forbids `report(done)` before the matrix is re-read, and
/// the matrix takes twenty to thirty minutes against a thirty-second tick.
///
/// **Both clear on the first tick after `drive_review`**, which is what §2.2
/// requires of a hold whose cause is a wait: arc 11 re-enters `ci-wait` from
/// `held`, so [`DriveEntry::advance`] assigns [`DriveEntry::fix_pushed_ms`] false
/// and the next green takes arc 2 with nothing further asked of the worker.
/// Pinned by `a_resume_out_of_fix_stalled_briefs_on_the_next_green`.
fn decide_fix_receipts(entry: &DriveEntry, facts: &DriveFacts, limits: &DriveLimits) -> DriveStep {
    match facts.worker {
        // Nothing to hand back to. Checked first, exactly as in
        // `decide_fix_wait`: every arm below presumes a worker that can be
        // reached.
        WorkerSignal::Unresumable => DriveStep::held(HeldReason::WorkerUnresumable),
        // INVARIANT 3 territory — a blocked worker is the orchestrator's call.
        WorkerSignal::Blocked => DriveStep::held(HeldReason::WorkerBlocked),
        // Arc 2 at last: the revision has stopped moving, so the lane can be
        // briefed at a `(head, digest)` that will still be current when it
        // records.
        WorkerSignal::Done => DriveStep::to(DriveState::ReviewWait),
        WorkerSignal::Silent => {
            // **The LATEST push in this `ci-wait` stay**, which is what
            // `fix_pushed_ms` exists to carry — see that field, and
            // `note_fix_push` for the re-stamp. `state_since_ms` would be the
            // first one, and a worker that pushed a follow-up commit late in
            // the window would get the remainder rather than a window.
            //
            // The fallback is unreachable through `decide`: this function is
            // called from one place, under a guard that already asked
            // `fix_pushed()`. It is `state_since_ms` rather than a panic or a
            // zero because those are the two ways a fallback goes wrong — a
            // zero clears every timeout, which is the false-park direction the
            // field doc argues against, and an unwind out of the poll thread
            // takes the fleet's watches down with it.
            let since = entry.fix_pushed_ms.unwrap_or(entry.state_since_ms);
            if facts.now_ms.saturating_sub(since) >= minutes_ms(limits.fix_timeout_minutes) {
                DriveStep::held(HeldReason::FixStalled)
            } else {
                DriveStep::Wait
            }
        }
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
    // **Is the brief this state may be about to send a body-verification delta?**
    // (#2168 E2.)
    //
    // The precondition is the whole of what makes the delegation honest, so it
    // is spelled as one `all` over the required set rather than inferred from
    // the fact that `k` happened to be stale: EVERY required lane has recorded a
    // `pass` bound to the head that would merge. Nothing about the CODE is
    // outstanding, so the only thing that can have staled lane `k` above is the
    // body — and what the lane is about to be asked for is the body as it
    // stands.
    //
    // A readable live digest is required too (`digest.is_some()`), because the
    // grant this writes is meaningless without one: the verdict's own digest is
    // what a gate later compares. "We could not read the body" grants nothing,
    // the same direction `lane_open_for` and `ReviewVerdict::body_changed` take
    // one question over.
    //
    // Deliberately NOT conditioned on the digest having moved for lane `k`. It
    // has — a lane whose pass is bound to this head reaches `k` only when
    // `lane_pass_settles` said no, and at this point `body_is_verified` is false
    // (or `first_stale_lane` would have returned `required.len()`), so the digest
    // is the only axis left. Re-deriving that here would be a second
    // implementation of `first_stale_lane`'s answer, and the two could drift.
    let verify = digest.is_some()
        && required.iter().all(|l| {
            l.verdict.as_ref().is_some_and(|v| {
                v.verdict == Verdict::Pass && v.reviewed(&facts.head)
            })
        });
    // **Is this brief about the BODY of a revision whose CODE is already fully
    // reviewed?** (#2509.)
    //
    // One conjunct weaker than `verify` above and one stronger, and both
    // differences are load-bearing.
    //
    // Weaker: every required lane must have ANSWERED at this head, not passed
    // at it. That gap IS the case #2509 was filed for — a lane that recorded
    // `fail` on the PR body, whose worker then moved the body and not the head,
    // which `verify` can never see because that lane's word is not `pass`.
    // Measured on PR #2397, which reached `held(review-limit)` on two body
    // sentences with the code green and unmoved throughout.
    //
    // Stronger: THIS lane must already have been briefed at this head. Without
    // it the very first brief at a head qualifies — every lane can be bound to
    // a head the moment they have all spoken — and the grace would be granted
    // for a fail on code nobody had reviewed twice. `briefed_head` is what says
    // this is a RE-brief, and a re-brief at an unchanged head can only be about
    // the body: `lane_open_for` is what routed us here, and at an equal head the
    // digest is the only axis it has left.
    //
    // A readable live digest is required for `verify`'s reason: what this
    // grants is read back against an exact `(briefed_head, briefed_digest)`
    // pair, and a brief whose revision cannot be pinned grants nothing.
    let body_only = digest.is_some()
        && entry
            .lane(&lane.block)
            .is_some_and(|r| !r.briefed_head.is_empty() && r.briefed_head == facts.head)
        && required
            .iter()
            .all(|l| l.verdict.as_ref().is_some_and(|v| v.reviewed(&facts.head)));
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
        // Arc 5, spending a review round — or parking, when the budget is gone,
        // unless #2509's one-shot grace answers first.
        Some(Verdict::Fail) => {
            if counter_exhausted(entry.counters.review_rounds, limits.max_review_rounds) {
                // **The grace is asked ONLY at the bound**, which is what makes
                // it a grace rather than a discount. Below the bound the
                // ordinary round is spent and `body_only_grace` is untouched, so
                // a drive that never reaches the bound never consumes it — and a
                // drive that reaches the bound on a code fail never gets it.
                if body_only_grace_applies(entry, &lane.block, &facts.head, digest) {
                    DriveStep::spend(DriveState::FixWait, Counter::BodyOnlyGrace)
                } else {
                    DriveStep::held(HeldReason::ReviewLimit)
                }
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
                // **The stall clock is keyed on the HEAD, not on the full
                // (head, digest) key** (#2109).
                //
                // It used to hang off the arm below, so it was asked only of a
                // lane that was still open for this exact revision — and a body
                // edit under a silent reviewer moved the digest, dropped through
                // to a re-brief, and re-armed `spawned_ms`. A reviewer that had
                // said nothing for fifty-nine minutes was given another hour by
                // an edit it never read, which is the defeat-your-own-bound shape
                // `decide`'s empty-head guard describes one screen up.
                //
                // #2109 makes that reachable rather than theoretical: the
                // re-brief it produced is now REFUSED while that lane's pane is
                // live, so without this the drive would retry the refusal until
                // `state-stalled` at the `review-wait` bound, and then
                // `drive-stalled` — four hours and twelve on a one-lane gate at
                // stock knobs, on notices that name no lane (#2110 renumbered
                // those two; neither names one).
                //
                // **`at_head` is what makes this a silence test rather than a
                // stopwatch** (#2109 review 1). The first version keyed on
                // `(briefed_head, spawned_ms)` alone, and that pair cannot see
                // whether the reviewer ANSWERED — which is the very distinction
                // [`LaneRecord::at_head`] exists to carry. A lane that recorded
                // `fail` at this head, whose worker then made a BODY-ONLY fix,
                // returns here through arc 8 at the same head with a moved
                // digest: the stale verdict reads as absent, this arm is
                // reached, and `now - spawned_ms` is review time PLUS fix time.
                // Past sixty minutes of that sum the drive parked
                // `held(lane-stalled)` on a reviewer that had never been
                // silent — and stuck, since only `open_lane` writes
                // `spawned_ms`, so a `drive_review` resume re-decided on
                // unchanged facts and re-held.
                //
                // A lane that has answered about THIS revision is therefore
                // exempt, whatever the body has done since; what it is owed is
                // the delta re-brief below, which resets both `at_head` and the
                // clock. Only a lane asked about this head and still silent past
                // its timeout stalls.
                Some(rec)
                    if rec.briefed_head == facts.head
                        && rec.at_head != facts.head
                        && facts.now_ms.saturating_sub(rec.spawned_ms)
                            >= minutes_ms(limits.lane_timeout_minutes) =>
                {
                    DriveStep::held(HeldReason::LaneStalled)
                }
                // **A lane whose pane is GONE is not a lane to wait for**
                // (#2163). `lane_open_for` answers "was this lane asked about
                // this revision", which stays true forever after the pane that
                // was asked has exited — so a killed reviewer left this state
                // waiting on a verdict nothing could produce, with `lane-stalled`
                // an hour away and not one rd-* row in between.
                //
                // Read AFTER the stall arm, and the ordering is the bound. A
                // pane that dies on every spawn would otherwise be re-opened for
                // ever: this arm answers first on every tick, so the stall arm
                // would never be evaluated. Below it, and with the re-open
                // PRESERVING `spawned_ms` (see [`lane_stall_anchor`]), a lane
                // whose panes keep dying still reaches `lane_timeout_minutes`
                // from the ORIGINAL brief and parks `held(lane-stalled)` naming
                // it — which is the true statement about that lane anyway: it
                // has been silent about this head for an hour.
                //
                // What the re-open costs is one pane on a session the lane
                // already has, which `rd_lane_session` resolves from the record,
                // the roster or the merged records; a session that no longer
                // resolves falls to the existing `rd-lane-resume-failed` →
                // fresh-spawn path.
                Some(rec) if lane_open_for(rec, &facts.head, digest) && !lane.pane_dead => {
                    DriveStep::Wait
                }
                // **The cap's starvation is reported before another spawn is
                // proposed** (#2109). The tick stamps
                // `cap_starved_since_ms` on the first lane spawn the
                // live-delegate cap refuses and leaves it alone on the rest, so
                // this reads the duration of one refusal RUN, not the age of the
                // last tick.
                //
                // Proposing `OpenLane` anyway would be harmless — the spawn
                // would be refused again and the entry re-stamped with nothing —
                // and that is exactly what made the measured incident invisible:
                // 37 identical `rd-refused` rows, `lanes: []`, no notice, three
                // hours. §2.2's exits are the only thing an orchestrator reads,
                // so a condition that needs an orchestrator has to become one.
                _ if entry.cap_starved_for(facts.now_ms).is_some_and(|d| d >= CAP_HOLD_MS) => {
                    DriveStep::held(HeldReason::CapFull)
                }
                _ => DriveStep::OpenLane { index: k, verify, body_only },
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

/// Why a driven pane is no longer needed — the **closed set** of two, and the
/// whole of what #2501 narrowed §3.1 item 5 to.
///
/// The note's item 5 was a closed sentence ("the driver may never kill a pane")
/// and it named its own reopening condition: *"a later measurement shows drives
/// starving on panes nothing frees"*. #2501 is that measurement — 12 driven PRs,
/// 20 `rd-refused` rows on one of them, five `held(cap-refused)` exits, and about
/// 25 orchestrator wakes spent doing by hand exactly what these two variants do.
/// So the item is narrowed rather than deleted, and the narrowing is this enum:
/// anything not spelled here is still a pane the driver may not touch.
///
/// **What makes exactly these two safe is not that they are idle.** The idle
/// reaper's demotion argument ("no task in flight, nothing to lose") is half of
/// it; the other half is that in both states the pane's OUTPUT is already on
/// durable record — a verdict file the gate re-reads, or a `report` the drive has
/// consumed and acted on — and the CONVERSATION survives, because the driver
/// resumes lanes and workers by session and has done since #2109. A release
/// therefore destroys nothing: not work, not a decision, not a reviewer's memory
/// of the PR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseReason {
    /// A reviewer lane whose verdict is recorded **at the revision now on the
    /// PR**. The lane has answered the only question this drive asks it, so
    /// there is nothing left in that pane for the drive to wait for; the next
    /// round resumes its session (§5.2's `session`, resolved through the record,
    /// the roster or the merged records).
    ///
    /// Word-blind, on [`lane_verdict_is_current`]'s own argument: a verdict is
    /// bound to the revision it reviewed, and what makes the pane finished is
    /// that it ANSWERED about this revision, not which word it chose.
    VerdictRecorded,
    /// The worker pane whose `report` this tick consumed. §7 intercepts it, arc
    /// 8 acts on it, and the hand-back that follows resumes
    /// [`DriveEntry::worker_session`] rather than the pane — so the pane's only
    /// remaining function was to hold a slot.
    ReportConsumed,
}

impl ReleaseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseReason::VerdictRecorded => "verdict-recorded",
            ReleaseReason::ReportConsumed => "report-consumed",
        }
    }
}

/// One pane [`releasable`] says this drive no longer needs.
///
/// The pane id is deliberately NOT here. This crate is Tauri-free and cannot ask
/// whether a pane is alive, idle or typeable — those are the registry's facts,
/// injected the way [`DriveEntry::forget_dead_panes`] injects liveness — so what
/// this carries is the DRIVE's half of the decision (which side, and why) and
/// the caller reads the pane off the entry and applies its own half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseCandidate {
    pub role: DrivenRole,
    pub reason: ReleaseReason,
}

/// **The panes this drive no longer needs, at this tick's facts and this tick's
/// step** (#2501) — the drive-side half of §3.1 item 5's narrowing.
///
/// Pure, and separate from [`decide`] rather than folded into it, because a
/// release is **not an arc**: it moves no state, spends no counter and appears
/// in no row of §2.1's table. It is the same shape as [`DriveStep::OpenLane`]
/// being "not a transition" — a side effect the tick performs beside the step,
/// not instead of one.
///
/// # The three conditions, and what each excludes
///
/// **1. The step must leave the drive LIVE.** An `Advance` into `held`,
/// `satisfied` or `cancelled` releases nothing, and that is not caution — it is
/// what keeps §6's exit notices true. A parked drive's notice says its panes are
/// "still running" and that a `drive_review` resume speaks to them again
/// ([`PaneStanding::Owned`](crate::rddrive::PaneStanding::Owned)); a terminal
/// one hands them to the orchestrator to dispose of. A lane released on an
/// EARLIER tick is simply not in [`DriveEntry::owned_panes`] any more, so the
/// notice keeps naming exactly the panes that are still there.
///
/// **This is a condition on the STEP, and the drive can still end the tick
/// parked despite it** (rev-final W1). The step is what `decide` proposed; the
/// tick's ARM can then refuse and park on its own — `Advance{to: FixWait}` whose
/// `rd_handback` cannot resume the worker becomes `held(worker-unresumable)` or
/// `held(cap-refused)` — and by then the release has happened, because the tick
/// performs it BEFORE the arm. Saying "a parking DRIVE releases nothing" would
/// be false there, so it is not said anywhere; what holds is the step.
///
/// **Nothing is lost by the narrower reading, and one thing is gained.** The
/// notices are built after the release, off the live record, so a hold's line
/// still names only panes that are actually there. And releasing before the arm
/// is what stops the commonest way that hold was reached at all: the freed slot
/// is available to `rd_handback`'s own spawn, so a drive whose lane has just
/// answered no longer hits the live-delegate cap handing the fix back.
///
/// **2. A lane's verdict must be CURRENT.** Asked with
/// [`lane_verdict_is_current`] — the same function `review-wait` decides with,
/// never a second implementation of it — over the lane facts this tick READ, so
/// a verdict recorded three commits ago releases nothing. A head that could not
/// be read is empty, `lane_verdict_is_current` is false for it, and the tick
/// releases nothing at all: "we could not check" is not "it has answered".
///
/// **3. The worker must have REPORTED, into a drive that is in `fix-wait`.**
/// `WorkerSignal::Done` and no other word: `Blocked` is INVARIANT 3 territory
/// and parks the drive for the orchestrator, which is a pane a human is about to
/// talk to. `Silent` has reported nothing at all. And the state condition is
/// what makes "the driver has consumed the report" a fact rather than a
/// description — the signal is one-shot, cleared when the arc it fed is durable,
/// so the tick that sees `Done` in `fix-wait` is the tick that consumed it.
///
/// # What this deliberately does NOT release
///
/// A lane that has been briefed and has not answered; a lane whose verdict binds
/// to an older revision; a worker that reported `blocked` or has said nothing; a
/// pane belonging to a drive whose STEP parks or ends it this tick (see
/// condition 1 for why that is not the same as "a drive that parks"); and every
/// pane in the group that is not this drive's. The orchestrator's kill authority is
/// untouched by all of it — this narrows what the DRIVER may do, and adds
/// nothing anywhere else.
///
/// # The residual, stated because a narrowing must state one
///
/// The worker rule fires on ONE tick — the arc out of `fix-wait` — and the
/// caller's own half of the decision can refuse it there: a pane still finishing
/// its turn is not idle, and an idle check that says "no" is not retried,
/// because the next tick is no longer in `fix-wait` and the fact has expired.
/// That case costs exactly what it cost before #2501 (the pane holds its slot
/// until the next hand-back reuses it, per #1960), so the failure direction is
/// the old behaviour rather than anything new.
///
/// **A worker session two drives share is the one case where a release reaches a
/// pane that is not only this drive's**, and it is disclosed rather than guarded
/// against, because the guard would have to be a claim about another drive's
/// intentions. Nothing stops two `drive_review` calls on different PRs naming one
/// worker session, and after both hand back they name one pane; the `report(done)`
/// that arrives is consumed by whichever drive `rd_owner` matches, so exactly one
/// of them releases it. The other learns on its next tick — `rd_pane_exit` reads
/// the death and names the initiator (`ended by driver-release`), so it parks
/// `held(worker-unresumable)` with a truthful line instead of waiting out
/// `fix-stalled`. That is faster and more informative than the pre-#2501
/// behaviour on the same fixture, where the second drive sat ninety minutes for a
/// report it was never going to be handed; what it is NOT is free, and a repo
/// that drives two PRs off one worker session is already the #338/#359 shape the
/// exit notices warn about.
///
/// The lane rule retries instead of expiring — it is a standing property of the
/// tick's facts — but it is **bounded by which states read those facts**, and
/// that bound is worth naming rather than leaving to be discovered. `facts.
/// required_lanes` is fetched only in `review-wait` and `gate-check`; §2.4 pays
/// for the routed-file list in exactly the states that consult it and nowhere
/// else. So a lane whose pane was still writing when its verdict landed is
/// released on the next tick the drive spends in one of those two states —
/// immediately, while another required lane is outstanding, and otherwise on the
/// drive's next review round. A one-lane gate whose only lane was busy on the
/// tick that read its pass therefore keeps that pane to the exit, where §6's
/// notice names it for the orchestrator exactly as it always did.
pub fn releasable(
    entry: &DriveEntry,
    facts: &DriveFacts,
    step: &DriveStep,
) -> Vec<ReleaseCandidate> {
    // Condition 1. A STEP that parks or ends the drive releases nothing — which
    // is not the same as "a drive that parks": the arm can refuse and park after
    // this has answered. See the doc above (rev-final W1).
    if let DriveStep::Advance { to, .. } = step {
        if to.is_parked() || to.is_terminal() {
            return Vec::new();
        }
    }
    let mut out: Vec<ReleaseCandidate> = Vec::new();
    // Condition 3, first, so the list reads worker-first exactly as
    // `owned_panes` does.
    if entry.state() == DriveState::FixWait
        && facts.worker == WorkerSignal::Done
        && !entry.worker_agent.is_empty()
    {
        out.push(ReleaseCandidate {
            role: DrivenRole::Worker,
            reason: ReleaseReason::ReportConsumed,
        });
    }
    // Condition 2. `required_lanes` is `None` in the states that do not read it
    // and when the routing could not be computed at all; both are "we do not
    // know which lanes are required", and neither is a licence to release one.
    let digest = facts.body_digest.as_deref();
    for l in facts.required_lanes.iter().flatten() {
        if !lane_verdict_is_current(l.verdict.as_ref(), &facts.head, digest) {
            continue;
        }
        // A lane with no record, or one whose record carries no pane, has
        // nothing to release — including one released on an earlier tick, whose
        // pane slot is empty precisely so this stays true.
        match entry.lane(&l.block) {
            Some(rec) if !rec.agent.trim().is_empty() => {}
            _ => continue,
        }
        out.push(ReleaseCandidate {
            role: DrivenRole::Lane(l.block.clone()),
            reason: ReleaseReason::VerdictRecorded,
        });
    }
    out
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
    fn the_held_reasons_are_the_notes_fifteen() {
        assert_eq!(HeldReason::ALL.len(), 15);
        // §2.2: "There are **seventeen**" exits back to the LLM orchestrator —
        // the fifteen holds plus `satisfied` and `cancelled`.
        let exits =
            HeldReason::ALL.len() + DriveState::ALL.iter().filter(|s| s.is_terminal()).count();
        assert_eq!(exits, 17);
        // The two cap reasons are DIFFERENT exits, and nothing else here would
        // notice them collapsing into one spelling: `ALL` would still hold
        // fifteen entries and every one of them would still round-trip.
        assert_ne!(
            HeldReason::CapFull.as_str(),
            HeldReason::CapRefused.as_str(),
            "a lane starved by the cap and a hand-back refused by it are separate exits"
        );
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000, false, false);
        assert_eq!(e.lanes.len(), 1);
        e.open_lane("rev-std", "s1", "rev-1", "head-b", Some("d1"), 9_000, false, false);
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000, false, false);
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
        o.take(&DriveStep::OpenLane { index: 0, verify: false, body_only: false }, 5_000).unwrap();
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
        e.open_lane("rev-std", "s1", "rev-4", "head-a", Some("d1"), 1_000, false, false);
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
        fresh.open_lane("rev-std", "s1", "", "head-a", Some("d1"), 1_000, false, false);
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000, false, false);
        e.open_lane("rev-std", "s1", "rev-2", "head-b", Some("d1"), 2_000, false, false);
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 0, false, false);
        e.open_lane("rev-std", "s1", "rev-2", "head-a", Some("d1"), 1, false, false);

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
        // #2168 E1's, and its default is load-bearing in the same way: an entry
        // written before the field existed must read as a head this drive did
        // NOT hand back for.
        assert!(!e.fix_pushed());
    }

    /// **An entry written before `fix_pushed_ms` existed degrades toward the
    /// pre-#2168 behaviour, never toward a false park** (#2168 E1).
    ///
    /// `serde(default)` gives `false`, and which way that falls is the whole
    /// question: `false` means such a drive advances on green alone and pays at
    /// most the one re-record round it was already going to pay, while `true`
    /// would hold it in `ci-wait` waiting for a `report(done)` its worker was
    /// never asked for and park it `fix-stalled` an hour later. The default is
    /// asserted through `decide` rather than off the field, because the field's
    /// value is not the promise — what the drive DOES with it is.
    #[test]
    fn an_entry_from_before_the_field_advances_on_green_as_it_used_to() {
        let limits = DriveLimits::default();
        let old = parse_state(NOTE_EXAMPLE).unwrap();
        let mut e = old.entry(1758).unwrap().clone();
        // Walk it to `ci-wait` the way arc 6 does, so the state is one the
        // machine really reaches and `fix_pushed_ms` is whatever the arc leaves.
        e.advance(DriveState::CiWait, None, None, 1_000).unwrap();
        assert!(!e.fix_pushed(), "the pre-state: nothing in the older file said otherwise");
        assert_eq!(
            decide(
                &e,
                &DriveFacts {
                    ci: CiObservation::Green,
                    worker: WorkerSignal::Silent,
                    ..facts_at("head-a")
                },
                &limits,
            ),
            DriveStep::to(DriveState::ReviewWait),
            "an upgrade mid-drive must not strand the drive on a signal nobody asked \
             its worker for"
        );
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
                verified_body: false,
                summary: String::new(),
                ts_ms: 0,
            }),
            // The overwhelmingly common reading, so it is the helper's default
            // and `pane_dead` is set by the one test that is about it (#2163).
            pane_dead: false,
        }
    }

    /// [`lane_fact`] whose `pass` is the driver's body-VERIFICATION delta
    /// (#2168 E2) — the mark `review_verdict` writes for a lane the driver
    /// briefed because only the body had moved.
    fn verified_lane_fact(block: &str, at_head: &str, digest: &str) -> LaneFact {
        let mut l = lane_fact(block, Some(Verdict::Pass), at_head, digest);
        if let Some(v) = l.verdict.as_mut() {
            v.verified_body = true;
        }
        l
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

    // ── #2168 E1: green at a pushed head is not on its own an arc ───────────

    /// A drive that reached `ci-wait` the way arc 7 does: hand back, then the
    /// worker pushes. Walked through the real arcs rather than assembled, so a
    /// fixture cannot encode a `fix_pushed_ms` the machine would not stamp.
    fn pushed_fix_entry() -> DriveEntry {
        let mut e = entry_at(DriveState::ReviewWait);
        // Arc 5, the hand-back — this is what stamps `fix_handback_ms`.
        e.advance(DriveState::FixWait, None, Some(Counter::ReviewRounds), 10_000).unwrap();
        // Arc 7, the push. `state_since_ms` is now this moment, and it is the
        // anchor the receipts wait is measured from.
        e.advance(DriveState::CiWait, None, None, 100_000).unwrap();
        e
    }

    /// **The defect, and the test that is red without the guard** (#1875,
    /// #2168 E1). A worker fills the PR body's CI section only once the checks
    /// have settled, so a lane briefed on green alone is briefed at a digest
    /// the worker is about to move — and the `pass` it records is stale before
    /// it is written. #1870 is the measured instance: `pass` at digest
    /// `bbff76b8`, 0 findings, the CI section filled, `BODY CHANGED SINCE
    /// PASS`, gate blocked, re-record, with the head never having moved.
    #[test]
    fn green_at_a_pushed_head_waits_for_the_workers_report_before_it_briefs_a_lane() {
        let limits = DriveLimits::default();
        let e = pushed_fix_entry();
        assert!(e.fix_pushed(), "the pre-state: arc 7 is what this test is about");
        let green_and_silent = DriveFacts {
            ci: CiObservation::Green,
            worker: WorkerSignal::Silent,
            ..facts_at("head-b")
        };
        assert_eq!(
            decide(&e, &green_and_silent, &limits),
            DriveStep::Wait,
            "green is the checks settling, not the round ending — the worker has not \
             reported, so the body may still move under whatever lane this briefs"
        );
        // And the arc it is waiting FOR, so this is not "refuse everything".
        let green_and_done = DriveFacts { worker: WorkerSignal::Done, ..green_and_silent };
        assert_eq!(
            decide(&e, &green_and_done, &limits),
            DriveStep::to(DriveState::ReviewWait),
            "arc 2 fires once the revision has stopped moving"
        );
    }

    /// The negative control, and the property the brief calls out by name: a
    /// drive that never handed anything back advances on green as it always
    /// did. `drive_review` is called after the worker has already reported, so
    /// there is no second report to wait for and waiting would park every first
    /// drive on a signal nobody was asked for.
    #[test]
    fn green_on_a_head_this_drive_never_handed_back_still_advances_on_its_own() {
        let limits = DriveLimits::default();
        for e in [entry_at(DriveState::CiWait), gate_check_recycled_entry()] {
            assert!(!e.fix_pushed(), "neither of these reached `ci-wait` by arc 7");
            let facts = DriveFacts {
                ci: CiObservation::Green,
                worker: WorkerSignal::Silent,
                ..facts_at("head-a")
            };
            assert_eq!(
                decide(&e, &facts, &limits),
                DriveStep::to(DriveState::ReviewWait),
                "a drive with no outstanding hand-back has no report to wait for"
            );
        }
    }

    /// A drive that went out to `gate-check` and came back by arc 10 — one of
    /// the three non-arc-7 ways into `ci-wait`, and the one that would still be
    /// carrying the flag if `advance` merely SET it instead of assigning it.
    fn gate_check_recycled_entry() -> DriveEntry {
        let mut e = pushed_fix_entry();
        e.advance(DriveState::ReviewWait, None, None, 200_000).unwrap();
        e.advance(DriveState::GateCheck, None, None, 200_000).unwrap();
        // Arc 10: not satisfied, back to `ci-wait`.
        e.advance(DriveState::CiWait, None, None, 200_000).unwrap();
        e
    }

    /// `fix_pushed_ms` is assigned on every arc, not set on one — pinned across
    /// each way into and out of `ci-wait`, because the failure mode of a set is
    /// invisible: the flag survives, the drive waits for a `report(done)` its
    /// worker was never asked for, and it parks `fix-stalled` an hour later
    /// naming a worker that did nothing wrong.
    #[test]
    fn only_arc_seven_marks_a_head_as_one_the_worker_pushed() {
        // Arc 1: the creation.
        assert!(!DriveEntry::new(1, "s", "o", Counters::default(), 0).fix_pushed());
        // Arc 7 sets it…
        let mut e = pushed_fix_entry();
        assert!(e.fix_pushed(), "arc 7");
        // …arc 2 out of `ci-wait` clears it…
        e.advance(DriveState::ReviewWait, None, None, 110_000).unwrap();
        assert!(!e.fix_pushed(), "arc 2 leaves `ci-wait`, so the push is spent");
        // …and arc 6 back INTO `ci-wait` does not re-set it.
        e.advance(DriveState::CiWait, None, None, 120_000).unwrap();
        assert!(!e.fix_pushed(), "arc 6 is a push this drive did not hand back for");
        // Arc 10, the same question from `gate-check`.
        assert!(!gate_check_recycled_entry().fix_pushed(), "arc 10");
        // Arc 11, a resume out of a park — the drive that was waiting on a
        // report before it was held is not still waiting on one after.
        let mut held = pushed_fix_entry();
        held.advance(DriveState::Held, Some(HeldReason::FixStalled), None, 300_000).unwrap();
        held.advance(DriveState::CiWait, None, None, 400_000).unwrap();
        assert!(!held.fix_pushed(), "arc 11");
    }

    /// The bound, and **which clock it is measured on**. `fix_handback_ms` is
    /// the hand-back, which predates the push: measured from there, a worker
    /// that spent fifty-nine minutes writing the fix would get one minute to
    /// read a green matrix and report. The anchor is `state_since_ms`, which
    /// is the LATEST push in this `ci-wait` stay, which is what `fix_pushed_ms`
    /// carries and `state_since_ms` cannot.
    #[test]
    fn a_silent_worker_on_a_pushed_head_parks_fix_stalled_a_fix_timeout_after_the_push() {
        let limits = DriveLimits::default();
        let e = pushed_fix_entry();
        assert!(
            e.state_since_ms > e.fix_handback_ms,
            "the fixture's whole point: the two anchors are 90s apart, so a test that \
             read the wrong one cannot pass by coincidence"
        );
        let at = |now_ms| {
            decide(
                &e,
                &DriveFacts {
                    now_ms,
                    ci: CiObservation::Green,
                    worker: WorkerSignal::Silent,
                    ..facts_at("head-b")
                },
                &limits,
            )
        };
        let timeout = minutes_ms(limits.fix_timeout_minutes);
        assert_eq!(at(e.state_since_ms + timeout - 1), DriveStep::Wait, "one ms inside");
        assert_eq!(
            at(e.state_since_ms + timeout),
            DriveStep::held(HeldReason::FixStalled),
            "and the bound itself"
        );
        // The discriminator: at a `now` that is past the timeout measured from
        // the HAND-BACK but not from the push, the drive is still waiting.
        assert_eq!(
            at(e.fix_handback_ms + timeout),
            DriveStep::Wait,
            "measured from `fix_handback_ms` this would already have parked"
        );
    }

    /// The other two worker signals, answered by their own names rather than
    /// waited out. Letting either fall through to the timeout would report a
    /// worker that SAID something as one that went silent — and would cost an
    /// hour before saying even that.
    #[test]
    fn the_pushed_head_wait_reads_every_worker_signal_by_the_same_rule_fix_wait_does() {
        let limits = DriveLimits::default();
        let e = pushed_fix_entry();
        for (worker, want) in [
            (WorkerSignal::Blocked, HeldReason::WorkerBlocked),
            (WorkerSignal::Unresumable, HeldReason::WorkerUnresumable),
        ] {
            let facts =
                DriveFacts { ci: CiObservation::Green, worker, ..facts_at("head-b") };
            assert_eq!(
                decide(&e, &facts, &limits),
                DriveStep::held(want),
                "{worker:?} is answered here exactly as `decide_fix_wait` answers it"
            );
            // …and still by its own name once the clock HAS run out, which is
            // the discriminator: read below the timeout instead of above it,
            // both of these would report `fix-stalled` — a claim that the
            // worker went silent, about a worker that said something.
            let expired = DriveFacts {
                now_ms: e.state_since_ms + minutes_ms(limits.fix_timeout_minutes) + 1,
                ..facts
            };
            assert_eq!(
                decide(&e, &expired, &limits),
                DriveStep::held(want),
                "{worker:?} is not reported as `fix-stalled` once the wait expires"
            );
        }
    }

    /// The guard is on the GREEN arm alone. A red or a conflicting matrix at a
    /// pushed head still hands the worker back through arc 3, where
    /// `decide_fix_wait`'s ladder answers, and the two unknowns wait as they
    /// always did. Without this, the receipts wait would swallow the arc that
    /// tells the worker its fix failed.
    #[test]
    fn a_pushed_head_that_is_not_green_takes_the_arcs_it_always_did() {
        let limits = DriveLimits::default();
        let e = pushed_fix_entry();
        let with = |ci| {
            decide(
                &e,
                &DriveFacts { ci, worker: WorkerSignal::Silent, ..facts_at("head-b") },
                &limits,
            )
        };
        assert_eq!(
            with(CiObservation::Red),
            DriveStep::spend(DriveState::FixWait, Counter::CiAttempts)
        );
        assert_eq!(
            with(CiObservation::Conflicting),
            DriveStep::spend(DriveState::FixWait, Counter::RebaseAttempts)
        );
        assert_eq!(with(CiObservation::Pending), DriveStep::Wait);
        assert_eq!(with(CiObservation::Unknown), DriveStep::Wait);
    }

    /// **`state-stalled` may not preempt `fix-stalled`, at any configuration**
    /// — the property `HeldReason::StateStalled` states, and the one a `max`
    /// would have broken. `decide` reads the state bound ABOVE the state's own
    /// logic, so at `fix_timeout_minutes` above `CI_WAIT_BOUND_MS` a bound
    /// equal to the knob would answer first on the very tick the receipts wait
    /// expires, and the hold naming the pane to read would never fire at all.
    ///
    /// The two knobs are the two sides of the constant: 60 (the stock value,
    /// under it) and 240 (over it, which is where a `max` and an add differ —
    /// and the CEILING, since `parse_workflow` runs this knob through
    /// `clamp_expires_minutes`, so no workflow file can ask for more). At that
    /// ceiling the sum is 330 minutes, comfortably inside the twelve-hour
    /// backstop, which is why this arm needs no counterpart to `review-wait`'s
    /// overtake residual — that one is unbounded in the LANE COUNT, and this
    /// one has a single clamped term.
    #[test]
    fn the_ci_wait_bound_funds_the_receipts_wait_instead_of_preempting_it() {
        for minutes in [60u64, 240] {
            let limits = DriveLimits { fix_timeout_minutes: minutes, ..DriveLimits::default() };
            let bound = state_bound_ms(DriveState::CiWait, &limits, 1).unwrap();
            assert_eq!(
                bound,
                CI_WAIT_BOUND_MS + minutes_ms(minutes),
                "the constant is the SLACK over the receipts wait, not a rival to it"
            );
            let e = pushed_fix_entry();
            let facts = DriveFacts {
                now_ms: e.state_since_ms + minutes_ms(minutes),
                ci: CiObservation::Green,
                worker: WorkerSignal::Silent,
                ..facts_at("head-b")
            };
            assert_eq!(
                decide(&e, &facts, &limits),
                DriveStep::held(HeldReason::FixStalled),
                "at fix_timeout_minutes={minutes} the wait-specific hold is what fires; \
                 under `max(constant, knob)` this is `state-stalled` at 240"
            );
            assert!(
                bound < minutes_ms(limits.drive_timeout_minutes),
                "…and the sum stays inside the backstop at the ceiling the workflow \
                 parser clamps to, so this arm owes no overtake residual"
            );
        }
    }

    /// **A follow-up push mid-wait gets a whole window, not the remainder of
    /// one** (rev-final round 2, premortem 2). `transition` refuses a `ci-wait`
    /// -> `ci-wait` self-arc, so nothing re-stamps `state_since_ms` and a
    /// `bool` bounded from it would run the wait from the FIRST push: a commit
    /// landing at minute 55 of a 60-minute knob would leave five minutes to run
    /// a fresh matrix and report.
    ///
    /// The three assertions are the defect, the fix and the bound it must not
    /// remove — a re-stamp that also pushed `state-stalled` out would be an
    /// unbounded suppression driven by a signal the drive does not control.
    #[test]
    fn a_second_push_inside_one_ci_wait_stay_re_anchors_the_receipts_wait() {
        let limits = DriveLimits::default();
        let timeout = minutes_ms(limits.fix_timeout_minutes);
        let mut e = pushed_fix_entry();
        let first = e.fix_pushed_ms.expect("arc 7 stamps the anchor");
        let silent = |e: &DriveEntry, now_ms| {
            decide(
                e,
                &DriveFacts {
                    now_ms,
                    ci: CiObservation::Green,
                    worker: WorkerSignal::Silent,
                    ..facts_at("head-c")
                },
                &limits,
            )
        };
        // Without the re-stamp this is the park — the defect, stated as the
        // pre-state so the fix below cannot pass vacuously.
        assert_eq!(
            silent(&e, first + timeout),
            DriveStep::held(HeldReason::FixStalled),
            "the pre-state: measured from the first push, the window is spent"
        );

        // The worker pushes again at minute 55, and the tick records it.
        let second = first + timeout - minutes_ms(5);
        e.note_fix_push(second);
        assert_eq!(e.state_since_ms, first, "the STATE clock is deliberately untouched");
        assert_eq!(
            silent(&e, first + timeout),
            DriveStep::Wait,
            "the same instant is now five minutes into a fresh window"
        );
        assert_eq!(
            silent(&e, second + timeout),
            DriveStep::held(HeldReason::FixStalled),
            "…and the fresh window is a whole one, not an unbounded reprieve"
        );

        // The bound the re-stamp may not remove: `state-stalled` measures from
        // `state_since_ms`, so a worker that pushed for ever still parks.
        let forever = first + state_bound_ms(DriveState::CiWait, &limits, 0).unwrap();
        e.note_fix_push(forever);
        assert_eq!(
            silent(&e, forever),
            DriveStep::held(HeldReason::StateStalled),
            "a drive that keeps pushing is still bounded by the state it is sitting in"
        );

        // And it re-stamps only what exists: a head move in a state that never
        // took arc 7 must not manufacture a wait.
        let mut fresh = entry_at(DriveState::CiWait);
        fresh.note_fix_push(9_999);
        assert!(!fresh.fix_pushed(), "note_fix_push is a re-stamp, never an entry point");
    }

    /// **A driven worker's `report(progress)` is answered wherever a hand-back
    /// is outstanding, which E1 makes two states** (rev-final round 2, finding
    /// 1). `kickback_owed` was scoped to `fix-wait` on the argument that "in
    /// any other state there is no hand-back to be waiting on and nothing the
    /// worker was asked for" — which E1 falsified for `ci-wait` on an arc-7
    /// head, where the worker has been handed a round and is being waited on
    /// for the very `report(done)` that line asks it to send.
    ///
    /// Left unswept, #1959 reappears one state over and worse: §7 consumes the
    /// report (its interception is keyed on the agent, not the state), the
    /// orchestrator's pane gets nothing, the worker's pane gets nothing, and a
    /// fix timeout later the hold says the driver heard nothing from a worker
    /// that spoke.
    ///
    /// The `review-wait` row is the control that keeps this from becoming
    /// "answer everywhere": that state has no outstanding hand-back.
    #[test]
    fn a_progress_report_is_owed_an_answer_in_both_states_that_are_waiting_on_the_worker() {
        // `fix-wait`, unchanged — and the pre-state for the arc-7 case below.
        let mut e = entry_at(DriveState::ReviewWait);
        e.advance(DriveState::FixWait, None, Some(Counter::ReviewRounds), 10_000).unwrap();
        assert!(e.kickback_owed(), "#1959's own state, unchanged");

        // Arc 7. The hand-back is still outstanding: the worker was asked to
        // push AND report, and it has done half of that.
        e.advance(DriveState::CiWait, None, None, 100_000).unwrap();
        assert!(e.fix_pushed());
        assert!(
            e.kickback_owed(),
            "the worker is still being waited on, for the report this line asks it to send"
        );

        // The budget is one per HAND-BACK and not one per state: `fix_handback_ms`
        // is not re-stamped by arc 7, so a worker answered before it pushed is
        // not answered again after.
        let mut answered = e.clone();
        answered.record_kickback(20_000);
        assert!(!answered.kickback_owed(), "same round, same answer, once");

        // The control: `ci-wait` on a head this drive never handed back for, and
        // `review-wait`, owe nobody anything.
        let first_drive = entry_at(DriveState::CiWait);
        assert!(!first_drive.fix_pushed());
        assert!(!first_drive.kickback_owed(), "no hand-back is outstanding here");
        let mut reviewing = e.clone();
        reviewing.advance(DriveState::ReviewWait, None, None, 200_000).unwrap();
        assert!(!reviewing.kickback_owed(), "and the lane's state owes the worker nothing");
    }

    /// **The two residuals `decide_fix_receipts` discloses are recoverable by
    /// the remedy their own notice prints**, which §2.2 requires of every hold
    /// whose cause is a wait — and which a hold on a signal that will never
    /// arrive would otherwise fail: a resume that only restarted the clocks
    /// would re-hold an hour later, for ever.
    ///
    /// This is the counterfactual for the disclosure, performed rather than
    /// argued (CLAUDE.md: a documented escape hatch is only pinned by a test
    /// that performs the edit). Arc 11 re-enters `ci-wait` from `held`, so
    /// `advance` assigns `fix_pushed_ms` None — it is the ASSIGNMENT, not a
    /// clock, that makes the resume work, and a `set` would leave the drive
    /// waiting on the same absent report.
    ///
    /// The pre-state is the hold itself, so the test cannot pass by resuming a
    /// drive that was never stuck.
    #[test]
    fn a_resume_out_of_fix_stalled_briefs_on_the_next_green() {
        let limits = DriveLimits::default();
        let mut e = pushed_fix_entry();
        let stalled = DriveFacts {
            now_ms: e.state_since_ms + minutes_ms(limits.fix_timeout_minutes),
            ci: CiObservation::Green,
            worker: WorkerSignal::Silent,
            ..facts_at("head-b")
        };
        assert_eq!(
            decide(&e, &stalled, &limits),
            DriveStep::held(HeldReason::FixStalled),
            "the pre-state: a drive really parked on the wait this test is about"
        );
        e.take(&DriveStep::held(HeldReason::FixStalled), stalled.now_ms).unwrap();

        // Arc 11: `drive_review` resumes it, hours later — a human takes their
        // time over a hold, so the resume must not depend on any clock.
        let resumed_at = stalled.now_ms + minutes_ms(300);
        e.advance(DriveState::CiWait, None, None, resumed_at).unwrap();
        assert!(!e.fix_pushed(), "the resume leaves no outstanding report to wait for");
        assert_eq!(
            decide(
                &e,
                &DriveFacts { now_ms: resumed_at + 1_000, ..stalled },
                &limits
            ),
            DriveStep::to(DriveState::ReviewWait),
            "the very next green briefs, with nothing further asked of a worker that \
             may have said its piece already"
        );
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
            DriveStep::OpenLane { index: 0, verify: false, body_only: false }
        );
        // Lane 1 passed at this (head, digest): move to lane 2 — and note this
        // is NOT a transition, which is why the table has no review-wait arm.
        assert_eq!(
            decide(&e, &two_lanes(Some(Verdict::Pass), None), &limits),
            DriveStep::OpenLane { index: 1, verify: false, body_only: false }
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

    /// **The residual the answered-lane exemption leaves — NARROWED by #2110,
    /// not closed** (#2109 review 2, premortem 1).
    ///
    /// One composition escapes every per-lane bound: a reviewer answers at this
    /// head, the body then moves, and that reviewer's pane goes BUSY on
    /// something else — a human re-tasking it, another drive taking it over. The
    /// re-brief the moved digest calls for cannot be delivered (the reuse arm
    /// needs an idle pane) and must not be spawned beside it (#2109's duplicate
    /// refusal, which #2162 narrowed to exactly this busy case), while
    /// `lane-stalled` is exempt because the lane did answer. So the drive
    /// retries and audits two rows a tick.
    ///
    /// **What #2110 changed is the exit, and only the exit.** #2109 disclosed
    /// this gap and said closing it needs a per-lane refusal clock, "which
    /// belongs with #2110's age work". #2110 did not build that clock: there is
    /// still nothing per-lane here, and the hold that ends this still names no
    /// lane. What it built is a per-STATE bound, and `review-wait` is a state,
    /// so the drive now leaves at the `review-wait` state bound as
    /// `held(state-stalled)` instead
    /// of at twelve as `held(drive-stalled)` — a fraction of the wait, and a
    /// notice that at least says which wait. The honest description of the
    /// residual is therefore *bounded per state, still not per lane*, and §8's
    /// row says exactly that.
    ///
    /// A disclosure is a claim like any other: without this test the suite pins
    /// only the arms that work, and that §8 row could go false with nothing red
    /// to say so. The three halves are the gap itself (past the LANE timeout,
    /// still proposing the re-brief), the non-vacuity control one tick short of
    /// the bound that now ends it, and the bound.
    ///
    /// **The age backstop is deliberately not asserted here any more**, and its
    /// absence is the finding rather than an omission: the narrower bound fires
    /// first, so from `review-wait` the twelve-hour clock is now unreachable for
    /// this composition. Asserting it would be asserting a path the code no
    /// longer takes.
    #[test]
    fn an_answered_lane_whose_re_brief_is_refused_is_bounded_by_the_review_wait_state_bound() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.open_lane("rev-std", "sess", "rev-4", "head-a", Some("d1"), 1_000, false, false);
        assert!(e.record_verdict_seen("rev-std", Verdict::Fail, "head-a"));
        let facts = |now: u64| DriveFacts {
            now_ms: now,
            body_digest: Some("d2".to_string()),
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Fail), "head-a", "d1")]),
            ..facts_at("head-a")
        };

        // The gap: twice the LANE timeout and still inside the state bound, so
        // this keeps proposing the re-brief the tick will refuse. `lane-stalled`
        // never fires here, by design, and nothing per-lane replaces it.
        let past_lane = 1_000 + minutes_ms(limits.lane_timeout_minutes) * 2;
        assert_eq!(
            decide(&e, &facts(past_lane), &limits),
            DriveStep::OpenLane { index: 0, verify: false, body_only: true },
            "the exemption really does leave this composition unbounded PER LANE — if this \
             ever becomes a lane-named hold, the §8 row disclosing the gap is what needs \
             rewriting"
        );

        // The bound that now limits it, and its own non-vacuity control one tick
        // short. Asked of `state_bound_ms` rather than spelled here, because
        // this test is about WHICH bound ends the composition; the bound's own
        // value is pinned against literals by
        // `the_review_wait_floor_exceeds_the_sequential_gate_it_covers`, so the
        // number is not built from the code under test without a witness.
        let bound = state_bound_ms(DriveState::ReviewWait, &limits, 1).unwrap();
        assert!(
            bound > minutes_ms(limits.lane_timeout_minutes),
            "the lane timeout must not be what fires, or the assertions below are \
             about the wrong bound"
        );
        assert!(
            bound < minutes_ms(limits.drive_timeout_minutes),
            "…and neither must the backstop, which outranks it in `decide`"
        );
        assert_eq!(
            decide(&e, &facts(1_000 + bound - 1), &limits),
            DriveStep::OpenLane { index: 0, verify: false, body_only: true },
            "…still inside the state bound, so the hold below is that bound and not a \
             coincidence"
        );
        assert_eq!(
            decide(&e, &facts(1_000 + bound), &limits),
            DriveStep::held(HeldReason::StateStalled),
            "…and time in `review-wait` is what ends it now, where before #2110 nothing \
             but the twelve-hour age could"
        );
    }

    /// **#2153: a re-drive carries the CONVERSATION and nothing that describes a
    /// revision.**
    ///
    /// Every field is asserted, and that is deliberate rather than exhaustive
    /// for its own sake: this record is what the next drive's first tick decides
    /// from, and each field it must NOT carry produces a different wrong answer.
    /// A carried `briefed_head`/`briefed_digest` makes `lane_open_for` true, so
    /// the first tick waits on a brief nobody sent; a carried `last_verdict` and
    /// `at_head` make `lane_has_answered` true, so the delta template describes
    /// a round that is over and the duplicate guard reads the previous drive's
    /// pane as holding this one; a carried `spawned_ms` starts `lane-stalled`
    /// counting from a brief in another drive's lifetime; and a carried `agent`
    /// makes a pane the new drive never spoke to its CURRENT one.
    #[test]
    fn a_reseeded_lane_keeps_its_session_and_its_owned_panes_and_no_claim_about_a_revision() {
        let mut e = entry_at(DriveState::ReviewWait);
        e.open_lane("rev-std", "sess-1", "rev-4", "head-a", Some("d1"), 1_000, false, false);
        e.open_lane("rev-std", "sess-1", "rev-9", "head-a", Some("d1"), 2_000, false, false);
        assert!(e.record_verdict_seen("rev-std", Verdict::Fail, "head-a"));
        let before = e.lane("rev-std").expect("the fixture's own premise").clone();
        assert_eq!(before.prior_agents, vec!["rev-4".to_string()], "the fixture: one supersede");

        let after = before.reseeded("sess-resolved");
        assert_eq!(after.block, "rev-std", "the lane is the same lane");
        assert_eq!(
            after.session, "sess-resolved",
            "and it carries the session the CALLER resolved, not the one on the record — a \
             lane on a CLI that mints its session after boot has `\"\"` here for its whole \
             life, and seeding off this field would drop exactly those lanes (#2109)"
        );
        assert_eq!(
            after.agent, "",
            "the previous drive's pane is not this drive's CURRENT one — leaving it here \
             would have the duplicate guard read it as holding this round and `pane_dead` \
             read its death as this drive's"
        );
        assert_eq!(
            after.prior_agents,
            vec!["rev-4".to_string(), "rev-9".to_string()],
            "…but both panes are still OWNED, oldest first, so a reviewer finishing the \
             previous round is intercepted rather than reporting as if undriven (§7)"
        );
        assert_eq!(after.last_verdict, None, "the previous drive's answer is not this one's");
        assert_eq!(after.at_head, "", "…and nothing may claim this lane has answered here");
        assert_eq!(after.briefed_head, "", "…nor that it has been asked");
        assert_eq!(after.briefed_digest, "");
        assert_eq!(after.spawned_ms, 0, "a lane that has not been briefed has not been silent");
    }

    /// **#2163: a lane whose pane is GONE is re-opened, not waited for — and
    /// the stall arm still outranks that.**
    ///
    /// `lane_open_for` answers "was this lane ASKED about this revision", which
    /// stays true for ever after the pane that was asked has exited. So a killed
    /// reviewer left `review-wait` waiting on a verdict nothing could produce
    /// until `lane-stalled` an hour later — measured on PR #2140 as 25+ minutes
    /// with no rd-* row at all, and reached on the driver's own advice.
    ///
    /// **The live arm is the control, and it is not decoration**: without it an
    /// implementation that re-opened on every tick regardless of the pane would
    /// pass the dead arm and destroy the wait this state is made of.
    ///
    /// **The ordering arm is the BOUND.** A pane that dies on every spawn must
    /// not be replaced for ever; the stall arm is read first, so a lane whose
    /// panes keep dying reaches `lane_timeout_minutes` — from the ORIGINAL
    /// brief, which is what [`lane_stall_anchor`] preserves — and parks
    /// `held(lane-stalled)` naming itself. Asserting it here is what makes that
    /// bound a property rather than a hope.
    #[test]
    fn a_lane_whose_pane_died_is_re_opened_and_a_live_one_is_still_waited_for() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.open_lane("rev-std", "sess", "rev-4", "head-a", Some("d1"), 1_000, false, false);
        let facts = |dead: bool, now: u64| DriveFacts {
            now_ms: now,
            required_lanes: Some(vec![LaneFact {
                pane_dead: dead,
                ..lane_fact("rev-std", None, "", "")
            }]),
            ..facts_at("head-a")
        };

        assert_eq!(
            decide(&e, &facts(false, 2_000), &limits),
            DriveStep::Wait,
            "the control: a lane open at this revision with a LIVE pane is one to wait for, \
             and re-opening it would be a second reviewer per tick"
        );
        assert_eq!(
            decide(&e, &facts(true, 2_000), &limits),
            DriveStep::OpenLane { index: 0, verify: false, body_only: false },
            "a lane whose recorded pane is gone is one to re-open — `lane_open_for` says it \
             was ASKED, which a dead pane cannot un-say and cannot answer"
        );

        // The bound on a pane that keeps dying: past the lane timeout the stall
        // arm answers first, and it is reached because the re-open preserves the
        // anchor rather than re-arming it.
        let stalled = 1_000 + minutes_ms(limits.lane_timeout_minutes);
        assert_eq!(
            decide(&e, &facts(true, stalled - 1), &limits),
            DriveStep::OpenLane { index: 0, verify: false, body_only: false },
            "…one tick short, so the hold below is the timeout and not a coincidence"
        );
        assert_eq!(
            decide(&e, &facts(true, stalled), &limits),
            DriveStep::held(HeldReason::LaneStalled),
            "…and the silence bound still outranks the re-open, which is what stops a pane \
             that dies on every spawn from being replaced for ever"
        );
    }

    /// **#2163: replacing a dead pane keeps the silence clock where it was.**
    ///
    /// [`lane_stall_anchor`] is what makes the bound in the test above
    /// REACHABLE: `open_lane` writes whatever anchor it is given, so a re-open
    /// that passed `now` would restart `lane-stalled` on every death and the
    /// loop would run until the drive's own age bound.
    ///
    /// Six rows, each killing an implementation the others let through. Always
    /// returning `now_ms` fails row 1; always returning the record's value fails
    /// rows 2, 3 and 5; ignoring the head fails row 2.
    ///
    /// **Rows 3 and 4 are #2169 review 2's N1, and the first version of this
    /// table did not have them** — that is the finding, not a footnote. Four
    /// rows read as discriminating while leaving the DIGEST half of the
    /// revision key untested, because the function took no digest at all: a
    /// head-only implementation passed every one of them. Row 3 is the case
    /// that exposed (a lane whose pane died, then a body-only fix moving the
    /// digest, inheriting an anchor a reviewer that has read nothing did not
    /// earn); row 4 is its own control, so "re-arm whenever the digest does not
    /// match" — which would re-arm on an unreadable body too — cannot pass row 3
    /// by being wrong in the other direction.
    ///
    /// A test that cannot express an axis is a test that does not pin it, and
    /// the missing axis was in the SIGNATURE rather than in the rows.
    #[test]
    fn a_dead_panes_replacement_inherits_the_stall_anchor_and_a_new_round_does_not() {
        let mut e = entry_at(DriveState::ReviewWait);
        e.open_lane("rev-std", "sess", "rev-4", "head-a", Some("d1"), 1_000, false, false);
        let rec = || e.lane("rev-std");

        let d1 = Some("d1");
        let observed = vec![
            ("dead pane, same revision", lane_stall_anchor(rec(), "head-a", d1, true, 9_000)),
            ("dead pane, new head", lane_stall_anchor(rec(), "head-b", d1, true, 9_000)),
            // The row #2169 review 2 (N1) showed was unpinned: same head, moved
            // DIGEST. A head-only implementation answers 1_000 here and passes
            // every other row in this table unchanged.
            ("dead pane, moved digest", lane_stall_anchor(rec(), "head-a", Some("d2"), true, 9_000)),
            // …and its own control: an UNKNOWN live digest is "we could not
            // check", not drift, so it still inherits. Without this row an
            // implementation that re-armed on any non-matching digest — `None`
            // included — passes the row above.
            ("dead pane, digest unknown", lane_stall_anchor(rec(), "head-a", None, true, 9_000)),
            ("live pane, same revision", lane_stall_anchor(rec(), "head-a", d1, false, 9_000)),
            ("no record at all", lane_stall_anchor(None, "head-a", d1, true, 9_000)),
        ];
        let expected = vec![
            // The replacement inherits the silence this lane has already spent.
            ("dead pane, same revision", 1_000),
            // A new revision is a new round, owed the full window — and the
            // revision is the FULL key, so either half moving is a new one.
            ("dead pane, new head", 9_000),
            ("dead pane, moved digest", 9_000),
            ("dead pane, digest unknown", 1_000),
            // Not a replacement — an ordinary re-brief re-arms, as it always has.
            ("live pane, same revision", 9_000),
            ("no record at all", 9_000),
        ];
        assert_eq!(
            observed, expected,
            "each row is (fixture, the `spawned_ms` the next brief must carry). A `9_000` on \
             row 1 hands a lane whose pane keeps dying a fresh hour on every death, so \
             `lane-stalled` never fires and nothing per-lane bounds the loop. A `1_000` on \
             the `new head`, `moved digest` or `live pane` rows stalls a lane that has not \
             been silent at all — and on `moved digest` it does so to a reviewer that has \
             read nothing, while the same body-only fix under a live pane gets a full window."
        );
    }

    /// **#2109 review 1, finding 1.** The stall clock is a SILENCE test, and a
    /// reviewer that answered is not silent however long the round has run.
    ///
    /// This is the arc-8 return the first version of that arm could not see. The
    /// lane records `fail` at `head-a`; the worker's fix is BODY-ONLY, so
    /// `report(done)` at an unchanged head returns the drive straight to
    /// `review-wait` with the digest moved; the stale verdict then reads as
    /// absent and this arm is reached with `now - spawned_ms` equal to review
    /// time PLUS fix time. Keyed on `(briefed_head, spawned_ms)` alone that sum
    /// crosses `lane_timeout_minutes` and parks the drive `lane-stalled` on a
    /// reviewer that answered promptly — and parks it STUCK, because only
    /// `open_lane` writes `spawned_ms`, so a resume re-decides on unchanged
    /// facts and re-holds. Its only exits were a head change, a cancel, or the
    /// age backstop.
    ///
    /// **The two halves differ in exactly one field**, which is what makes this
    /// a pin on `at_head` rather than on the timeout: same lane, same clock,
    /// same moved digest, same stale-verdict fact handed to `decide` — and only
    /// whether `record_verdict_seen` ever ran. Without the second half an
    /// implementation that simply deleted the stall arm would pass.
    #[test]
    fn a_lane_that_answered_at_this_head_is_not_stalled_by_the_fix_round_that_followed() {
        let limits = DriveLimits::default();
        let past = 1_000 + minutes_ms(limits.lane_timeout_minutes);
        // The gate's own fact for the lane: the `fail` it recorded, bound to the
        // head it reviewed and to the digest that has since moved. `decide`
        // reads this as ABSENT (`lane_verdict_is_current`), which is what puts
        // the lane back on the arm under test.
        let facts = DriveFacts {
            now_ms: past,
            body_digest: Some("d2".to_string()),
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Fail), "head-a", "d1")]),
            ..facts_at("head-a")
        };

        let answered = {
            let mut e = entry_at(DriveState::ReviewWait);
            e.head = "head-a".into();
            e.open_lane("rev-std", "sess", "rev-4", "head-a", Some("d1"), 1_000, false, false);
            assert!(
                e.record_verdict_seen("rev-std", Verdict::Fail, "head-a"),
                "the fixture must actually record the answer, or this pins nothing"
            );
            e
        };
        assert_eq!(
            decide(&answered, &facts, &limits),
            DriveStep::OpenLane { index: 0, verify: false, body_only: true },
            "a reviewer that ANSWERED at this head is not silent — arc 8 owes it the delta \
             re-brief, not a hold naming it as the thing that went quiet"
        );

        // The control: the identical entry that never answered. One field apart,
        // and it must still stall.
        let silent = {
            let mut e = entry_at(DriveState::ReviewWait);
            e.head = "head-a".into();
            e.open_lane("rev-std", "sess", "rev-4", "head-a", Some("d1"), 1_000, false, false);
            e
        };
        assert_eq!(
            decide(&silent, &facts, &limits),
            DriveStep::held(HeldReason::LaneStalled),
            "…while the lane that was asked and said nothing still stalls, which is the \
             whole point of keying the clock on the head"
        );
    }

    /// **#2109.** A lane's stall clock is keyed on the HEAD it was asked about,
    /// so a body edit under a SILENT reviewer cannot hand it another hour.
    ///
    /// The silence half of that key is pinned by the sibling above; every
    /// fixture here builds its record through `open_lane` alone, so `at_head`
    /// is empty and the lane has answered nothing.
    ///
    /// The two halves discriminate. The first is the pre-#2109 behaviour and is
    /// the control: a lane still open for this exact revision, past its timeout,
    /// has always been `lane-stalled`. The second is the one that moved — same
    /// lane, same silence, same clock, and only the DIGEST different — and under
    /// the old keying it read as a re-brief, which re-armed `spawned_ms` and
    /// bought the reviewer a fresh timeout it had done nothing to earn.
    ///
    /// The third is the negative control, and without it an implementation that
    /// simply held `lane-stalled` for every lane record would pass both halves.
    #[test]
    fn a_lanes_stall_clock_survives_a_body_edit_it_never_read() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.open_lane("rev-std", "sess", "rev-4", "head-a", Some("d1"), 1_000, false, false);
        let stale = 1_000 + minutes_ms(limits.lane_timeout_minutes);
        let facts = |now: u64, digest: &str| DriveFacts {
            now_ms: now,
            body_digest: Some(digest.to_string()),
            required_lanes: Some(vec![lane_fact("rev-std", None, "", "")]),
            ..facts_at("head-a")
        };

        assert_eq!(
            decide(&e, &facts(stale, "d1"), &limits),
            DriveStep::held(HeldReason::LaneStalled),
            "the control: a lane open at this revision and past its timeout has always stalled"
        );
        assert_eq!(
            decide(&e, &facts(stale, "d2"), &limits),
            DriveStep::held(HeldReason::LaneStalled),
            "and a body edit the silent reviewer never read must not re-arm its clock"
        );
        assert_eq!(
            decide(&e, &facts(stale - 1, "d2"), &limits),
            DriveStep::OpenLane { index: 0, verify: false, body_only: false },
            "the negative control: inside the timeout a moved digest is still a re-brief"
        );
    }

    /// **#2109's third ask at the decision layer.** A drive whose lane the
    /// live-delegate cap keeps refusing must become one of §2.2's exits, and it
    /// must not become one on the first refusal.
    ///
    /// The three points are asserted together because each alone passes under an
    /// implementation that is wrong at another: an arm that never holds passes
    /// the first two, and an arm that holds on the stamp alone (no duration)
    /// passes the first and third.
    #[test]
    fn a_cap_that_keeps_refusing_a_lane_parks_the_drive_only_once_the_window_is_spent() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        let facts = |now: u64| DriveFacts {
            now_ms: now,
            required_lanes: Some(vec![lane_fact("rev-std", None, "", "")]),
            ..facts_at("head-a")
        };

        // The control: an un-starved drive proposes the spawn, so every
        // assertion below is about the starvation and not about a lane that
        // could never have opened.
        assert_eq!(decide(&e, &facts(10_000), &limits), DriveStep::OpenLane { index: 0, verify: false, body_only: false });

        assert!(e.note_cap_starvation(10_000), "the first refusal of a run stamps the clock");
        // One tick short of the window: still trying, still no orchestrator turn
        // spent on a condition that clears itself.
        assert_eq!(
            decide(&e, &facts(10_000 + CAP_HOLD_MS - 1), &limits),
            DriveStep::OpenLane { index: 0, verify: false, body_only: false },
            "a cap that has not refused for the whole window is a back-off, not a hold"
        );
        assert_eq!(
            decide(&e, &facts(10_000 + CAP_HOLD_MS), &limits),
            DriveStep::held(HeldReason::CapFull),
            "a starvation that outlasts the window is one of §2.2's exits, not silence"
        );
    }

    /// The clock measures a RUN, and every way out of one clears it.
    ///
    /// The re-stamp half is the one with teeth: a tick that re-stamped on each
    /// refusal would make [`CAP_HOLD_MS`] unreachable, and the hold above would
    /// then be dead code that no test in this file could tell apart from a hold
    /// that fires — `decide` reads a stamp it is handed either way.
    #[test]
    fn the_starvation_clock_is_stamped_once_per_run_and_no_arc_carries_one_across() {
        let mut e = entry_at(DriveState::ReviewWait);
        assert_eq!(e.cap_starved_for(10_000), None, "a fresh entry is not starved");

        assert!(e.note_cap_starvation(10_000));
        assert!(!e.note_cap_starvation(20_000), "a second refusal in one run is not a new run");
        assert_eq!(
            e.cap_starved_for(20_000),
            Some(10_000),
            "the duration is measured from the FIRST refusal, not the newest tick"
        );

        assert!(e.clear_cap_starvation(20_000), "a lane that opens ends the run");
        assert_eq!(e.cap_starved_for(20_000), None);
        assert!(
            !e.clear_cap_starvation(20_000),
            "and clearing an unstarved entry changes nothing"
        );
        // #2110: what the run COST survives the clear, because that is the
        // quantity both age bounds subtract. A `clear` that only forgot the
        // anchor would leave the ten seconds charged to the drive that was
        // starved — which is the whole defect, one code path over from where
        // it was reported.
        assert_eq!(
            e.starved_ms(20_000),
            10_000,
            "ending a starvation run must bank what it cost, not discard it"
        );

        // An arc clears it too, including the arc into the hold it produces: a
        // resume must start the window over rather than re-hold on the next tick
        // from a stamp the previous starvation left behind.
        assert!(e.note_cap_starvation(30_000));
        e.advance(DriveState::Held, Some(HeldReason::CapFull), None, 30_000).unwrap();
        assert_eq!(
            e.cap_starved_for(30_000),
            None,
            "a drive that MOVED is not the drive that was stuck"
        );
    }

    /// **#2135's restart clear, and the ONE thing that distinguishes it from
    /// the tick's.** A starvation run cannot straddle a process boundary, and
    /// the interval it would otherwise be charged for is mostly orrerix's own
    /// downtime.
    ///
    /// The two halves are the same entry, the same stamp and the same clock,
    /// differing in exactly one call — so this pins that the restart clear
    /// charges NOTHING, and not merely that it forgets the anchor. An
    /// implementation spelled `clear_cap_starvation(now)` forgets the anchor
    /// perfectly and fails the second assertion, which is the point: charging
    /// the gap would FORGIVE orrerix's own downtime from both age bounds, and
    /// that downtime being CHARGED is the property #2117 disclosed and pinned
    /// (`orrerix_downtime_is_charged_to_the_state_it_spanned`).
    #[test]
    fn the_restart_clear_forgets_a_run_without_charging_the_downtime_it_spans() {
        let mut restarted = entry_at(DriveState::ReviewWait);
        assert!(
            !restarted.discard_cap_starvation_run(),
            "an entry with no run has none to drop, and says so"
        );
        assert!(restarted.note_cap_starvation(10_000));
        assert!(restarted.discard_cap_starvation_run(), "…and a stamped one says it dropped one");
        assert_eq!(restarted.cap_starved_for(2_000_000), None, "the run is over");
        assert_eq!(
            restarted.starved_ms(2_000_000),
            0,
            "and the half hour between the last tick of one process and the first of the next \
             is charged to the drive, never forgiven as a starvation nobody observed"
        );

        // The contrast, which is what makes the assertion above a discriminator
        // rather than a restatement: the TICK's clear, over the same stamp at
        // the same clock, banks every millisecond of it.
        let mut ticked = entry_at(DriveState::ReviewWait);
        assert!(ticked.note_cap_starvation(10_000));
        assert!(ticked.clear_cap_starvation(2_000_000));
        assert_eq!(
            ticked.starved_ms(2_000_000),
            1_990_000,
            "the tick's clear banks what the run cost — that IS the difference, and it is why \
             the restart clear is a separate function rather than a call of this one"
        );
    }

    /// **#2110's exclusion, at the decision itself.** A drive the live-delegate
    /// cap will not let spawn is neither progressing nor stalled, and no clock
    /// may charge it for the difference.
    ///
    /// **The two halves differ in exactly one call** — `note_cap_starvation` —
    /// so this pins the EXCLUSION and not the bound: same entry, same state,
    /// same head, same lane list, same clock, and only whether the cap had been
    /// refusing throughout. Without the control half an implementation that
    /// never holds `state-stalled` at all would pass it; without the starved
    /// half one that ignores starvation would.
    ///
    /// And the starved half parks on the CAP rather than on nothing, which is
    /// the outcome #2110 asks for: the orchestrator is told the one thing it
    /// can act on (free a slot), instead of being told a drive stalled when
    /// what happened is that it was never allowed to move.
    #[test]
    fn time_the_cap_refused_a_lane_advances_neither_age_bound() {
        let limits = DriveLimits::default();
        let facts = |now: u64| DriveFacts {
            now_ms: now,
            required_lanes: Some(vec![lane_fact("rev-std", None, "", "")]),
            ..facts_at("head-a")
        };
        // `review-wait`'s bound is FOUR hours here: the three-hour constant
        // plus one lane at the default sixty-minute timeout (#2117 review 2
        // made it an add rather than a max). Four hours therefore reaches it
        // exactly, and is well short of the twelve-hour backstop, so exactly
        // one time bound is in play and a red is attributable to it.
        let past = 1_000 + 4 * 60 * 60_000;

        let mut moving = entry_at(DriveState::ReviewWait);
        moving.head = "head-a".to_string();
        assert_eq!(
            decide(&moving, &facts(past), &limits),
            DriveStep::held(HeldReason::StateStalled),
            "a drive that really did sit four hours in `review-wait` must park"
        );

        let mut starved = entry_at(DriveState::ReviewWait);
        starved.head = "head-a".to_string();
        // One minute of ordinary life, THEN the cap. Not starved from the
        // instant it was created: both figures below would then read zero,
        // which is also what every arithmetic error produces, and they would
        // stop discriminating between an exclusion that works and one that
        // returns nothing.
        starved.note_cap_starvation(61_000);
        assert_eq!(
            decide(&starved, &facts(past), &limits),
            DriveStep::held(HeldReason::CapFull),
            "the cap held this drive still for four hours; charging that to its own state clock reports a stall, which is the false claim #2110 is about"
        );
        assert_eq!(
            starved.state_elapsed_ms(past),
            60_000,
            "the state clock must hold at the one minute this drive spent able to act"
        );
        assert_eq!(
            starved.bounded_age_ms(past),
            60_000,
            "and so must the age the backstop reads: it is the drive's own, not the cap's"
        );
        assert_eq!(
            starved.age_ms(past),
            past - 1_000,
            "…while the WALL age is untouched, which is what `since_ms` reports"
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
            DriveStep::OpenLane { index: 0, verify: false, body_only: false },
            "a fail recorded against a commit that no longer describes the PR must not route"
        );
        assert_eq!(
            decide(&e, &at(Verdict::Escalate, "head-a"), &limits),
            DriveStep::OpenLane { index: 0, verify: false, body_only: false },
            "…nor may a stale escalate park the drive on a judgment nobody is being asked for"
        );
        // The digest half of the same key: same head, body moved under it.
        let moved = DriveFacts {
            body_digest: Some("d2".into()),
            ..at(Verdict::Fail, "head-b")
        };
        assert_eq!(decide(&e, &moved, &limits), DriveStep::OpenLane { index: 0, verify: false, body_only: false });
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000, false, false);
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d1"), 1_000, false, false);
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", None, "", "")]),
            ..facts_at("head-b")
        };
        assert_eq!(decide(&e, &facts, &limits), DriveStep::OpenLane { index: 0, verify: false, body_only: false });
    }

    #[test]
    fn a_pass_whose_body_digest_moved_re_opens_that_lane() {
        // §8's body-changed row: the (head, digest) key is re-read every tick,
        // so a moved digest with an unchanged head re-enters at the first stale
        // lane with a body-only delta brief.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("OLD"), 1_000, false, false);
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Pass), "head-a", "OLD")]),
            body_digest: Some("NEW".into()),
            ..facts_at("head-a")
        };
        // …and `verify` is TRUE (#2168 E2): the single required lane has a pass
        // bound to this head, so nothing about the code is outstanding and the
        // brief about to go out is a body-verification delta.
        assert_eq!(decide(&e, &facts, &limits), DriveStep::OpenLane { index: 0, verify: true, body_only: true });
    }

    #[test]
    fn a_body_only_move_re_briefs_one_lane_and_the_verification_settles_the_rest() {
        // #2168 E2, the driver half. Two required lanes, both passed at
        // (head-a, d1); the worker edits the PR body and nothing else.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        let with = |lanes: Vec<LaneFact>| DriveFacts {
            required_lanes: Some(lanes),
            body_digest: Some("d2".into()),
            ..facts_at("head-a")
        };
        let stale = vec![
            lane_fact("rev-std", Some(Verdict::Pass), "head-a", "d1"),
            lane_fact("rev-final", Some(Verdict::Pass), "head-a", "d1"),
        ];

        // ONE lane is briefed — the first in the gate's order — and the brief
        // is a verification delta, which is what makes the verdict it produces
        // able to discharge the clause for the other.
        assert_eq!(
            decide(&e, &with(stale.clone()), &limits),
            DriveStep::OpenLane { index: 0, verify: true, body_only: false }
        );

        // S3 sends it; the drive waits rather than moving on to lane 1. That is
        // the whole saving: before #2168 E2 the second lane's turn came next.
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("d2"), 1_500, true, false);
        assert_eq!(decide(&e, &with(stale.clone()), &limits), DriveStep::Wait);

        // rev-std records the verification pass. rev-final's pass at d1 settles
        // again, so the drive goes to the gate instead of briefing lane 1.
        let answered = vec![
            verified_lane_fact("rev-std", "head-a", "d2"),
            lane_fact("rev-final", Some(Verdict::Pass), "head-a", "d1"),
        ];
        assert_eq!(
            decide(&e, &with(answered), &limits),
            DriveStep::to(DriveState::GateCheck)
        );

        // **The control, on the axis that carries the saving: WHICH step comes
        // back.** The same round with rev-std's pass recorded WITHOUT the mark
        // — an ordinary re-review that happens to sit at the current digest —
        // still owes lane 1 a brief, where the marked round above went straight
        // to `gate-check`. An implementation that accepted any newer pass would
        // answer `gate-check` here too, and would have weakened
        // `body-unchanged` for every repo, driver or no driver.
        //
        // The brief it gets is itself a verification round, and that is right
        // rather than a leak: every required lane has a `pass` bound to this
        // head, so nothing about the CODE is outstanding and what rev-final is
        // owed is the body. The grant it carries is simply not needed here —
        // rev-std's pass already sits at the current digest — which is why the
        // discriminator in this test is the index and not the flag.
        let unmarked = vec![
            lane_fact("rev-std", Some(Verdict::Pass), "head-a", "d2"),
            lane_fact("rev-final", Some(Verdict::Pass), "head-a", "d1"),
        ];
        assert_eq!(
            decide(&e, &with(unmarked), &limits),
            DriveStep::OpenLane { index: 1, verify: true, body_only: false }
        );

        // And the delegation is bounded by the HEAD it was granted at: once the
        // code moves, a body verification settles nothing and arc 6 takes the
        // drive back to `ci-wait`.
        let pushed = DriveFacts {
            required_lanes: Some(vec![
                verified_lane_fact("rev-std", "head-a", "d2"),
                lane_fact("rev-final", Some(Verdict::Pass), "head-a", "d1"),
            ]),
            body_digest: Some("d2".into()),
            ..facts_at("head-b")
        };
        assert_eq!(decide(&e, &pushed, &limits), DriveStep::to(DriveState::CiWait));
    }

    #[test]
    fn a_verification_delta_is_only_briefed_when_nothing_about_the_code_is_outstanding() {
        // #2168 E2's precondition, one crossing per way it can fail. The grant
        // the brief carries is what lets the gate stop asking the other lanes,
        // so it is issued only when EVERY required lane has a pass bound to the
        // head that would merge — anything else and this is an ordinary round.
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        let with = |lanes: Vec<LaneFact>, digest: Option<&str>| DriveFacts {
            required_lanes: Some(lanes),
            body_digest: digest.map(|d| d.to_string()),
            ..facts_at("head-a")
        };
        let std_pass = lane_fact("rev-std", Some(Verdict::Pass), "head-a", "d1");

        // A lane that has never answered: the code is still outstanding. Lane 0
        // is still the one briefed — its own pass staled with the digest — but
        // the brief is an ORDINARY one, and the verdict it produces will not
        // discharge anything for lane 1.
        assert_eq!(
            decide(
                &e,
                &with(vec![std_pass.clone(), lane_fact("rev-final", None, "", "")], Some("d2")),
                &limits
            ),
            DriveStep::OpenLane { index: 0, verify: false, body_only: false }
        );

        // A lane whose pass is bound to a head the branch has left reads as
        // absent (#1871 B1) — and "absent" is exactly what must not be granted
        // over, because the code that lane approved is not the code in front of
        // the drive.
        assert_eq!(
            decide(
                &e,
                &with(
                    vec![
                        std_pass.clone(),
                        lane_fact("rev-final", Some(Verdict::Pass), "head-OLD", "d1")
                    ],
                    Some("d2")
                ),
                &limits
            ),
            DriveStep::OpenLane { index: 0, verify: false, body_only: false }
        );

        // A body orrerix could not read grants nothing: the verdict this brief
        // produces would carry no digest, so there would be no body for the
        // mark to be ABOUT. (An unknown digest does not stale a pass either, so
        // lane 1 here is outstanding for its own reason — no verdict at all.)
        assert_eq!(
            decide(
                &e,
                &with(vec![std_pass.clone(), lane_fact("rev-final", None, "", "")], None),
                &limits
            ),
            DriveStep::OpenLane { index: 1, verify: false, body_only: false }
        );

        // The positive control, so the three rows above are the precondition
        // deciding and not a constant.
        assert_eq!(
            decide(
                &e,
                &with(
                    vec![
                        std_pass,
                        lane_fact("rev-final", Some(Verdict::Pass), "head-a", "d1")
                    ],
                    Some("d2")
                ),
                &limits
            ),
            DriveStep::OpenLane { index: 0, verify: true, body_only: false }
        );
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
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("OLD"), 1_000, false, false);
        assert_eq!(decide(&e, &facts, &limits), DriveStep::OpenLane { index: 0, verify: true, body_only: true });
        // S3 performs that brief; the drive now waits on the reviewer.
        e.open_lane("rev-std", "s1", "rev-1", "head-a", Some("NEW"), 1_500, true, true);
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
            briefed_verify: false,
            briefed_body_only: false,
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
            verified_body: false,
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
    fn a_body_verification_never_stands_in_for_a_pass_at_another_head() {
        // **#2168 E2's delegation is bounded by the head, and this is the row
        // that pins it on the DRIVER side.** Its own test rather than a row
        // appended to `the_re_entry_point_is_the_first_lane_whose_pass_does_not_stand`,
        // because that test's first assertion covers the same predicate: a
        // neuter reddens it there and panics before ever reaching this, so the
        // red would evidence the older assertion and say nothing about this one
        // (CLAUDE.md — a red evidences only the assertion it reached and moved).
        //
        // What it is for: `pass_covers_body`'s direct arm shipped comparing
        // digests without asking whether the pass was bound to the head that
        // would merge. The gate's own loop filters that case out one line
        // earlier, so only the driver could reach it — and there a `pass`
        // recorded against code the worker had already fixed read as settling
        // the revision in front of the drive, walking straight past the lane
        // #1871 B1 exists to re-open.
        let across_heads = vec![
            verified_lane_fact("rev-std", "head-a", "d2"),
            lane_fact("rev-final", Some(Verdict::Pass), "head-OLD", "d1"),
        ];
        assert_eq!(
            first_stale_lane(&across_heads, "head-a", Some("d2")),
            1,
            "a verification of the body may stand in for a pass at THIS head, never for one \
             recorded against code the branch has left"
        );
        // The positive control: the same two lanes with rev-final's pass bound
        // to the live head — so the row above is the head deciding, not a
        // delegation that never fires.
        let same_head = vec![
            verified_lane_fact("rev-std", "head-a", "d2"),
            lane_fact("rev-final", Some(Verdict::Pass), "head-a", "d1"),
        ];
        assert_eq!(first_stale_lane(&same_head, "head-a", Some("d2")), 2);
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

    /// **The `review-wait` floor must EXCEED the sum of the silences it covers,
    /// not equal it** (#2117 review 2, W3).
    ///
    /// `lane_timeout_minutes` bounds one lane's SILENCE, from that lane's own
    /// `spawned_ms`; `state-stalled` measures ELAPSED time, from
    /// `state_since_ms`, which nothing re-stamps between sequential lane briefs.
    /// A floor equal to the product funds the silences and nothing else, so the
    /// gaps between them — before the first brief, and after each verdict lands
    /// — come out of a margin of exactly zero, at precisely the configurations
    /// the floor exists to protect.
    ///
    /// The strict inequality is the property and the literals are the pin: the
    /// first would pass under any generous formula, and the second alone would
    /// go stale silently if the slack were ever folded back into a `max`.
    #[test]
    fn the_review_wait_floor_exceeds_the_sequential_gate_it_covers() {
        let limits = DriveLimits::default();
        let lane = minutes_ms(limits.lane_timeout_minutes);
        let bound = |lanes: usize| state_bound_ms(DriveState::ReviewWait, &limits, lanes).unwrap();

        for lanes in 1..=4 {
            assert!(
                bound(lanes) > lane * lanes as u64,
                "a {lanes}-lane gate whose reviewers each answer just inside their own \
                 timeout must not park: the floor has to fund the gaps between them too"
            );
        }

        // The literals, so folding the slack back into a `max` is not silent.
        assert_eq!(bound(1), minutes_ms(240));
        assert_eq!(bound(3), minutes_ms(360));
        // Zero required lanes reads as one — a routing answer this tick could
        // not produce, which holds `routing-unaccountable` on the same tick.
        assert_eq!(bound(0), bound(1));

        // The worked case from the finding: three reviewers each answering at
        // fifty-nine minutes, plus the THREE unfunded intervals a three-lane
        // gate has — the stretch before the first brief, and one detection gap
        // after each of the first two verdicts. Under the bare product this sum
        // parks a drive nothing was wrong with.
        //
        // The first draft of this fixture used two gaps and came to 178.5
        // minutes, which is UNDER the 180-minute product — so it asserted the
        // finding was not reachable, and CI said so. Three intervals is what
        // the mechanism actually has.
        let real = minutes_ms(59) * 3 + 90_000 * 3;
        assert!(real > lane * 3, "the fixture must exceed the bare product, or it pins nothing");
        assert!(real < bound(3), "…and must sit inside the floor as shipped");
    }

    /// **A state clock may not outrun the drive's own age** (#2117 review 2,
    /// premortem 1).
    ///
    /// `state_since_ms` is `serde(default)`, so an entry written before it
    /// existed reads zero and the raw subtraction answers `now` — epoch-scaled.
    /// The HOLD that produces is correct and argued at the field; the NUMBER was
    /// not, and `advance` stamps `held_after_ms` from it, so the notice told an
    /// operator their drive had been in `ci-wait` for some twenty thousand days.
    ///
    /// The two halves are the cap and its non-vacuity control: without the
    /// second, an implementation returning a constant zero would pass.
    #[test]
    fn a_state_clock_never_outruns_the_drives_own_age() {
        // The pre-#2110 entry, reconstructed the way serde would: a real
        // `started_ms`, and a `state_since_ms` the older build never wrote.
        let limits = DriveLimits::default();
        let mut old = entry_at(DriveState::CiWait);
        old.started_ms = 1_000;
        old.state_since_ms = 0;
        let now = 1_000 + minutes_ms(200);

        assert_eq!(
            old.state_elapsed_ms(now),
            minutes_ms(200),
            "a drive that has existed for 200 minutes cannot have been in one state longer"
        );
        // Asked of `state_bound_ms` rather than of the constant: since #2168 E1
        // `CI_WAIT_BOUND_MS` is the slack over `fix_timeout_minutes`, so a
        // comparison against it would understate the bound this half is about.
        assert!(
            old.state_elapsed_ms(now) >= state_bound_ms(DriveState::CiWait, &limits, 0).unwrap(),
            "…and it still reaches the bound, so the argued hold-then-resume is unchanged"
        );

        // The control: an ordinary entry, where the cap must not be what decides.
        let mut fresh = entry_at(DriveState::CiWait);
        fresh.started_ms = 1_000;
        fresh.state_since_ms = 1_000 + minutes_ms(150);
        assert_eq!(
            fresh.state_elapsed_ms(now),
            minutes_ms(50),
            "an entry whose state clock is younger than the drive reports the state clock"
        );
    }

    /// **A `review-wait` floor that outruns the backstop still names the
    /// state** (#2117 review 2, premortem 2).
    ///
    /// `lane_timeout_minutes: 240` on a three-lane gate floors `review-wait`
    /// above the twelve-hour age, and [`decide`] checks the age first — so that
    /// drive parks `drive-stalled` and the state bound is unreachable for it.
    /// That is a real residual and it is disclosed at `state_bound_ms`.
    ///
    /// What it is NOT is the pre-#2110 notice, and this pins the difference:
    /// `held_from` is stamped on every arc into `held`, so the hold still
    /// records which state the drive was in and for how long. The reviewer's
    /// premise — "parks `drive-stalled` naming no state" — is the half that does
    /// not hold, and an assertion is worth more here than a correction in prose.
    #[test]
    fn a_review_wait_floor_that_outruns_the_backstop_still_names_the_state() {
        let limits = DriveLimits::new(3, 3, 1, 240, 60, 720);
        assert!(
            state_bound_ms(DriveState::ReviewWait, &limits, 3).unwrap()
                > minutes_ms(limits.drive_timeout_minutes),
            "the fixture must actually be the configuration the premortem names"
        );

        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        let past = 1_000 + minutes_ms(limits.drive_timeout_minutes);
        let facts = DriveFacts {
            now_ms: past,
            required_lanes: Some(vec![
                lane_fact("rev-std", None, "", ""),
                lane_fact("rev-two", None, "", ""),
                lane_fact("rev-final", None, "", ""),
            ]),
            ..facts_at("head-a")
        };
        assert_eq!(
            decide(&e, &facts, &limits),
            DriveStep::held(HeldReason::DriveStalled),
            "the age outranks a state bound this config put out of reach"
        );

        // …and the hold still carries what the drive was doing, which is the
        // whole of #2110's third ask and the half the premortem got wrong.
        e.advance(DriveState::Held, Some(HeldReason::DriveStalled), None, past).unwrap();
        assert_eq!(e.held_from, Some(DriveState::ReviewWait));
        assert_eq!(
            e.held_after_ms,
            minutes_ms(limits.drive_timeout_minutes),
            "…and for how long, which is what the notice interpolates"
        );
    }

    /// **orrerix's own downtime is charged to the state it spanned** (#2117
    /// review 3, premortem 1) — disclosed at [`state_bound_ms`], and pinned here
    /// so the disclosure cannot go false with nothing red to say so.
    ///
    /// The clocks are absolute stamps rather than tick counts, so a group paused
    /// or an app closed across a gap is indistinguishable from a drive that sat
    /// in `ci-wait` for that long. The age bound had this property before #2110
    /// and nobody reached it at four hours; at the per-state bounds it is an
    /// unattended afternoon, and on the three arms shorter than `ci-wait`'s it
    /// is less than that.
    ///
    /// **The gap this test uses is derived from the bound rather than written**,
    /// because the figure moved once already: it was a literal two hours against
    /// a bare ninety-minute `ci-wait` bound, and #2168 E1 made that arm the
    /// constant plus `fix_timeout_minutes`.
    ///
    /// **This pins the residual itself, not a fix**, and the second half is what
    /// makes that honest: the drive is recoverable by the remedy its own notice
    /// prints, because arc 11 re-stamps every clock. A pin on the first half
    /// alone would read as a defect nobody had thought about.
    #[test]
    fn orrerix_downtime_is_charged_to_the_state_it_spanned() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::CiWait);
        e.head = "head-a".into();
        // One tick at 1_000, then nothing at all — orrerix was not running. No
        // starvation was recorded, because no tick was there to record one.
        //
        // **The gap is derived from the bound and not written as a number.** It
        // was a literal 120 minutes, chosen when `ci-wait`'s bound was the bare
        // `CI_WAIT_BOUND_MS`; #2168 E1 made that arm the constant PLUS
        // `fix_timeout_minutes`, and a literal that no longer crosses the bound
        // stops witnessing the residual while still reading like a lunch break.
        // Asked of `state_bound_ms` instead, so the next change to that arm
        // moves the specimen with it (CLAUDE.md: a test's specimen must stay a
        // member of the class it witnesses).
        let bound = state_bound_ms(DriveState::CiWait, &limits, 0).unwrap();
        let after_gap = 1_000 + bound;
        assert_eq!(
            decide(&e, &DriveFacts { now_ms: after_gap, ..facts_at("head-a") }, &limits),
            DriveStep::held(HeldReason::StateStalled),
            "the gap is charged to `ci-wait`, which is the disclosed residual — if this ever \
             stops being true, `state_bound_ms`'s downtime paragraph is what needs rewriting"
        );

        // …and the remedy the notice prints really does clear it: arc 11
        // re-stamps `state_since_ms`, so the drive goes back to work rather than
        // re-holding on its first tick.
        e.advance(DriveState::Held, Some(HeldReason::StateStalled), None, after_gap).unwrap();
        e.advance(DriveState::CiWait, None, None, after_gap).unwrap();
        assert_eq!(
            e.state_elapsed_ms(after_gap),
            0,
            "a resume must start the state clock over, or the hold is unrecoverable"
        );
        assert_eq!(
            decide(&e, &DriveFacts { now_ms: after_gap + 1_000, ..facts_at("head-a") }, &limits),
            DriveStep::Wait,
            "…and the very next tick must not re-hold on the gap it already reported"
        );
    }

    /// **A backward wall-clock step suspends the bound rather than firing it**
    /// (#2117 review 3, premortem 2) — the fail-safe direction, disclosed at
    /// [`state_bound_ms`] and pinned here.
    ///
    /// Every clock here is a `saturating_sub` against an absolute stamp, so a
    /// `now` behind `state_since_ms` reads zero rather than wrapping to
    /// `u64::MAX` and parking the drive instantly. The three halves are the
    /// suspension, its non-vacuity control (the same entry at a forward clock
    /// DOES hold, so the first assertion is not passing because nothing ever
    /// holds), and the recovery once the clock is ahead again.
    #[test]
    fn a_clock_that_steps_backward_suspends_the_bound_rather_than_firing_it() {
        let limits = DriveLimits::default();
        let mut e = entry_at(DriveState::CiWait);
        e.head = "head-a".into();
        e.state_since_ms = minutes_ms(500);
        e.started_ms = minutes_ms(500);

        // The clock steps back behind the stamp.
        let behind = minutes_ms(400);
        assert_eq!(e.state_elapsed_ms(behind), 0, "saturating, not wrapping");
        assert_eq!(e.bounded_age_ms(behind), 0, "…and the age with it");
        assert_eq!(
            decide(&e, &DriveFacts { now_ms: behind, ..facts_at("head-a") }, &limits),
            DriveStep::Wait,
            "a clock that went backwards must not park a drive; failing toward WAIT is the \
             only safe direction here"
        );

        // The control: the same entry at a forward clock past the bound holds,
        // so the assertion above is about the step and not about a drive that
        // could never hold at all. **The bound is asked of `state_bound_ms`**,
        // not restated as `CI_WAIT_BOUND_MS` — since #2168 E1 that constant is
        // the SLACK over `fix_timeout_minutes` rather than the whole bound, and
        // a control that stops crossing it is a control that stops controlling.
        let ahead = minutes_ms(500) + state_bound_ms(DriveState::CiWait, &limits, 0).unwrap();
        assert_eq!(
            decide(&e, &DriveFacts { now_ms: ahead, ..facts_at("head-a") }, &limits),
            DriveStep::held(HeldReason::StateStalled),
            "the same entry past its bound on a forward clock must still park"
        );
    }

    /// **Where the `review-wait` floor overtakes the backstop, on STOCK
    /// knobs** (#2117 review 4's premortem) — the crossover asserted rather
    /// than left to arithmetic in a doc comment.
    ///
    /// The residual was already disclosed at [`state_bound_ms`], but only with
    /// a worked example on a NON-default `lane_timeout_minutes: 240`, and the
    /// floor test beside it covers one to four lanes. Neither says where the
    /// crossover actually is at defaults, which is the number an operator
    /// declaring a wide gate would want. It is **nine**: `180 + 60n >= 720`
    /// from `n = 9`, so a nine-lane gate on stock knobs already has an
    /// unreachable state bound, one lane earlier than the review's estimate of
    /// ten.
    ///
    /// The two halves are the last reachable width and the first unreachable
    /// one, so this fails if the crossover moves in either direction — which it
    /// does if any of the three numbers involved is retuned.
    #[test]
    fn the_review_wait_floor_overtakes_the_backstop_at_nine_lanes_on_stock_knobs() {
        let limits = DriveLimits::default();
        let backstop = minutes_ms(limits.drive_timeout_minutes);
        let bound = |lanes: usize| state_bound_ms(DriveState::ReviewWait, &limits, lanes).unwrap();

        assert_eq!(bound(8), minutes_ms(660));
        assert!(
            bound(8) < backstop,
            "an eight-lane gate on stock knobs must still be able to reach its state bound"
        );
        assert_eq!(bound(9), minutes_ms(720));
        assert!(
            bound(9) >= backstop,
            "…and at nine the floor has overtaken the twelve-hour backstop, so `decide`'s \
             age check — which runs first — is what such a drive parks on. Disclosed at \
             `state_bound_ms`; this is where it starts"
        );
    }

    // ── #2501: what the driver may release ──────────────────────────────────

    /// A `review-wait` entry with one lane, briefed at `head`, running in pane
    /// `rev-1` on session `sess-lane`.
    fn lane_open_at(head: &str) -> DriveEntry {
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = head.to_string();
        e.open_lane("rev-std", "sess-lane", "rev-1", head, Some("d1"), 1_000, false, false);
        e
    }

    /// The step a live `review-wait` tick takes when it is simply waiting — the
    /// neutral one, so a release these tests observe is the LANE rule firing and
    /// not the step type.
    const LIVE: DriveStep = DriveStep::Wait;

    /// **The lane rule, both directions, on one fixture whose only moving part
    /// is the verdict's binding.**
    ///
    /// The two rows differ in the head the verdict is bound to and in nothing
    /// else — same entry, same pane, same live head — so a `releasable` that
    /// ignored currency and a `releasable` that released nothing are both red
    /// here. That is the discrimination CLAUDE.md's non-discriminating-fixture
    /// rule asks for: the candidate outputs must DIVERGE.
    #[test]
    fn a_lane_is_released_when_its_verdict_binds_to_the_live_revision_and_not_before() {
        for (label, at_head, digest, owed) in [
            ("answered about this revision", "h1", "d1", true),
            ("answered about an older head", "h0", "d1", false),
            ("answered before the body moved", "h1", "d0", false),
        ] {
            let e = lane_open_at("h1");
            let mut f = facts_at("h1");
            f.required_lanes =
                Some(vec![lane_fact("rev-std", Some(Verdict::Pass), at_head, digest)]);
            let got = releasable(&e, &f, &LIVE);
            assert_eq!(
                got.len(),
                usize::from(owed),
                "{label}: releasable said {got:?}"
            );
            if owed {
                assert_eq!(got[0].role, DrivenRole::Lane("rev-std".into()), "{label}");
                assert_eq!(got[0].reason, ReleaseReason::VerdictRecorded, "{label}");
            }
        }
    }

    /// **Word-blind, exactly as [`lane_verdict_is_current`] is.** A lane that
    /// said `fail` or `escalate` about this revision has answered, and what makes
    /// its pane finished is that it answered — not which word it chose.
    ///
    /// The negative control is in the row above and is the one that makes this
    /// mean something: currency, not the word, is what decides.
    #[test]
    fn the_lane_rule_reads_the_binding_and_never_the_word() {
        for word in [Verdict::Pass, Verdict::Fail, Verdict::Escalate] {
            let e = lane_open_at("h1");
            let mut f = facts_at("h1");
            f.required_lanes = Some(vec![lane_fact("rev-std", Some(word), "h1", "d1")]);
            assert_eq!(
                releasable(&e, &f, &LIVE).len(),
                1,
                "a lane that answered {:?} at this revision is finished with this round",
                word
            );
        }
    }

    /// **A lane with no pane on record releases nothing** — which is what makes
    /// the release idempotent rather than a loop that kills once and then keeps
    /// naming a candidate nothing can act on. Same facts as the positive row
    /// above; the only difference is that the pane slot is already empty.
    #[test]
    fn a_lane_whose_pane_is_already_released_is_not_a_candidate_again() {
        let mut e = lane_open_at("h1");
        let mut f = facts_at("h1");
        f.required_lanes = Some(vec![lane_fact("rev-std", Some(Verdict::Pass), "h1", "d1")]);
        assert_eq!(releasable(&e, &f, &LIVE).len(), 1, "the control: it is a candidate first");

        let freed = e.release_pane(&DrivenRole::Lane("rev-std".into()), "sess-lane");
        assert_eq!(freed.as_deref(), Some("rev-1"), "the pane it un-recorded");
        assert_eq!(
            e.lane("rev-std").map(|l| l.session.as_str()),
            Some("sess-lane"),
            "…and the conversation, which is the whole promise"
        );
        assert!(releasable(&e, &f, &LIVE).is_empty(), "not a candidate a second time");
    }

    /// **A step that parks or ends the drive releases nothing**, which is what
    /// keeps §6's exit notices true: they say the panes they name are still
    /// running and are the orchestrator's to resume or dispose of.
    ///
    /// Driven over every terminal and parked target, and over a live one as the
    /// positive control — otherwise "released nothing" is indistinguishable from
    /// a fixture that was never releasable at all.
    #[test]
    fn a_parking_or_terminal_step_releases_nothing() {
        let mut f = facts_at("h1");
        f.required_lanes = Some(vec![lane_fact("rev-std", Some(Verdict::Pass), "h1", "d1")]);
        assert_eq!(
            releasable(&lane_open_at("h1"), &f, &LIVE).len(),
            1,
            "the positive control: these facts DO release under a live step"
        );
        for (to, reason) in [
            (DriveState::Held, Some(HeldReason::Escalate)),
            (DriveState::Satisfied, None),
            (DriveState::Cancelled, None),
        ] {
            let step = DriveStep::Advance { to, held_reason: reason, bump: None };
            assert!(
                releasable(&lane_open_at("h1"), &f, &step).is_empty(),
                "a step into {} must release nothing",
                to.as_str()
            );
        }
        let live = DriveStep::Advance {
            to: DriveState::GateCheck,
            held_reason: None,
            bump: None,
        };
        assert_eq!(
            releasable(&lane_open_at("h1"), &f, &live).len(),
            1,
            "…while an arc that leaves the drive live still does"
        );
    }

    /// **The worker rule: `fix-wait` plus `Done`, and nothing else.**
    ///
    /// Every other worker signal is a row here, because each is a different
    /// reason to keep the pane: `Blocked` parks the drive for the orchestrator,
    /// which is about to talk to that pane; `Silent` has said nothing;
    /// `Unresumable` has already gone. And the state condition is a row too —
    /// the same `Done` in `ci-wait` is a report about a hand-back this drive has
    /// already consumed, so it may not release a second time.
    #[test]
    fn the_worker_is_released_only_on_the_tick_that_consumes_its_done_report() {
        for (state, signal, owed) in [
            (DriveState::FixWait, WorkerSignal::Done, true),
            (DriveState::FixWait, WorkerSignal::Blocked, false),
            (DriveState::FixWait, WorkerSignal::Silent, false),
            (DriveState::FixWait, WorkerSignal::Unresumable, false),
            (DriveState::CiWait, WorkerSignal::Done, false),
            (DriveState::ReviewWait, WorkerSignal::Done, false),
        ] {
            let mut e = entry_at(state);
            e.head = "h1".to_string();
            e.record_worker_pane("w-1");
            let mut f = facts_at("h1");
            f.worker = signal;
            let got = releasable(&e, &f, &LIVE);
            assert_eq!(
                got.len(),
                usize::from(owed),
                "{}/{signal:?} released {got:?}",
                state.as_str()
            );
            if owed {
                assert_eq!(got[0].role, DrivenRole::Worker);
                assert_eq!(got[0].reason, ReleaseReason::ReportConsumed);
            }
        }
    }

    /// **A release refuses when the conversation cannot be named.** Killing the
    /// pane of a lane whose session nothing can resolve would end the reviewer's
    /// memory of the PR, which is the one thing this mechanism promises to keep.
    ///
    /// The positive control is the second half: the same lane, with a session,
    /// releases — so the refusal is the empty session and not the fixture.
    #[test]
    fn a_release_refuses_a_lane_whose_session_cannot_be_named() {
        let mut e = entry_at(DriveState::ReviewWait);
        e.open_lane("rev-std", "", "rev-1", "h1", Some("d1"), 1_000, false, false);
        assert_eq!(
            e.release_pane(&DrivenRole::Lane("rev-std".into()), ""),
            None,
            "no session on the record and none resolved: the pane stays"
        );
        assert_eq!(
            e.lane("rev-std").map(|l| l.agent.as_str()),
            Some("rev-1"),
            "…and the record still names it, so the next tick can try again"
        );
        assert_eq!(
            e.release_pane(&DrivenRole::Lane("rev-std".into()), "sess-resolved"),
            Some("rev-1".to_string()),
            "the control: a resolved session releases"
        );
        assert_eq!(
            e.lane("rev-std").map(|l| l.session.as_str()),
            Some("sess-resolved"),
            "…and the resolved id is written onto the record, so the resume no longer \
             depends on the dead pane's own row"
        );
    }

    /// The worker's half of the same refusal, and the pane list that follows
    /// from it: a released pane is not in [`DriveEntry::owned_panes`], which is
    /// what keeps every exit notice's "still running" true.
    #[test]
    fn a_released_worker_leaves_the_owned_pane_list() {
        let mut e = entry_at(DriveState::FixWait);
        e.record_worker_pane("w-1");
        assert!(
            e.owned_panes().iter().any(|(a, r)| a == "w-1" && *r == DrivenRole::Worker),
            "the control: it is owned first"
        );
        assert_eq!(e.release_pane(&DrivenRole::Worker, ""), Some("w-1".to_string()));
        assert!(
            !e.owned_panes().iter().any(|(a, _)| a == "w-1"),
            "a released pane must not be named by a notice that says its panes are still running"
        );
        assert_eq!(
            e.release_pane(&DrivenRole::Worker, ""),
            None,
            "…and there is nothing left to release twice"
        );
    }

    /// `DriveEntry::new` records the worker SESSION, so the worker arm's
    /// fail-closed branch needs an entry that has none — the shape a drive
    /// started without one would have. Pinned because the branch is otherwise
    /// unreachable from the fixtures above and would read as dead code.
    #[test]
    fn a_worker_with_no_recorded_session_is_never_released() {
        let mut e = entry_at(DriveState::FixWait);
        e.worker_session = String::new();
        e.record_worker_pane("w-1");
        assert_eq!(
            e.release_pane(&DrivenRole::Worker, "sess-ignored"),
            None,
            "the worker's session is the entry's own; a caller may not supply one for it"
        );
    }

    // ── §2.3 #2509's one-shot grace for a body-only fail ─────────────────────

    /// A drive sitting AT the review bound, with `rev-std`'s brief carrying (or
    /// not carrying) #2509's mark.
    ///
    /// `body_only` is the axis every test below varies, and it is the only one:
    /// the counters, the live revision, the lane's pane and the reviewer's word
    /// are identical on both sides, so a rule that decided on the bound alone —
    /// or on nothing — is red rather than green-by-luck.
    fn at_the_bound(body_only: bool) -> DriveEntry {
        let mut e = entry_at(DriveState::ReviewWait);
        e.head = "head-a".into();
        e.counters.review_rounds = DriveLimits::default().max_review_rounds;
        e.open_lane(
            "rev-std", "sess-lane", "rev-1", "head-a", Some("d1"), 1_000, false, body_only,
        );
        e
    }

    /// The facts a lane's blocking `fail`, recorded about the live revision,
    /// puts in front of `review-wait`.
    fn failing_at_the_bound() -> DriveFacts {
        DriveFacts {
            required_lanes: Some(vec![lane_fact("rev-std", Some(Verdict::Fail), "head-a", "d1")]),
            ..facts_at("head-a")
        }
    }

    /// **THE test.** Two rows differing in the BRIEF the driver sent and in
    /// nothing else.
    #[test]
    fn a_body_only_fail_at_the_bound_buys_one_more_round_and_a_code_fail_does_not() {
        let limits = DriveLimits::default();
        let facts = failing_at_the_bound();
        assert_eq!(
            decide(&at_the_bound(true), &facts, &limits),
            DriveStep::spend(DriveState::FixWait, Counter::BodyOnlyGrace),
            "a fail on a lane re-briefed about the body alone buys the one grace round"
        );
        assert_eq!(
            decide(&at_the_bound(false), &facts, &limits),
            DriveStep::held(HeldReason::ReviewLimit),
            "a fail on a lane briefed about the code parks the drive exactly as it always did"
        );
    }

    /// §2.3's "never stacking", performed rather than asserted: the arc is
    /// taken, the drive goes round again on the same body-only brief, and the
    /// second fail parks.
    #[test]
    fn the_grace_is_spent_once_and_the_next_body_only_fail_parks() {
        let limits = DriveLimits::default();
        let facts = failing_at_the_bound();
        let mut e = at_the_bound(true);
        let step = decide(&e, &facts, &limits);
        assert_eq!(step, DriveStep::spend(DriveState::FixWait, Counter::BodyOnlyGrace));
        e.take(&step, 3_000).unwrap();
        assert!(e.counters.body_only_grace, "the arc is what spends it");

        // Arc 7 then arc 2: the worker pushes, CI settles, and the drive is back
        // in `review-wait` with the same body-only brief out and the same fail.
        e.advance(DriveState::CiWait, None, None, 4_000).unwrap();
        e.advance(DriveState::ReviewWait, None, None, 5_000).unwrap();
        e.open_lane(
            "rev-std", "sess-lane", "rev-1", "head-a", Some("d1"), 6_000, false, true,
        );
        assert_eq!(
            decide(&e, &facts, &limits),
            DriveStep::held(HeldReason::ReviewLimit),
            "the grace is one per drive, not one per body-only fail"
        );
    }

    /// **"`MAX_ROUNDS_CEILING` is untouched" is a claim about a NUMBER**, so it
    /// is pinned as one: the grace round leaves `review_rounds` exactly where
    /// the bound left it, and is funded from the bool beside it.
    #[test]
    fn the_grace_round_never_moves_the_review_round_counter() {
        let limits = DriveLimits::default();
        let mut e = at_the_bound(true);
        let spent = e.counters.review_rounds;
        assert_eq!(spent, limits.max_review_rounds, "the fixture really is at the bound");
        let step = decide(&e, &failing_at_the_bound(), &limits);
        e.take(&step, 3_000).unwrap();
        assert_eq!(e.counters.review_rounds, spent, "a grace round is not a review round");
        assert!(
            e.counters.review_rounds <= MAX_ROUNDS_CEILING,
            "and INVARIANT 9's ceiling bounds exactly what it bounded before"
        );
        assert!(e.counters.body_only_grace, "it is funded from its own one-shot budget");
        assert_eq!(e.state(), DriveState::FixWait, "and it really does hand the worker back");
    }

    /// The grace is asked **only at the bound**. Below it the ordinary round is
    /// spent and the grace is untouched — otherwise it is a discount, and a
    /// drive would arrive at the bound with it already gone.
    #[test]
    fn below_the_bound_a_body_only_fail_spends_an_ordinary_round() {
        let limits = DriveLimits::default();
        let mut e = at_the_bound(true);
        e.counters.review_rounds = limits.max_review_rounds - 1;
        let step = decide(&e, &failing_at_the_bound(), &limits);
        assert_eq!(step, DriveStep::spend(DriveState::FixWait, Counter::ReviewRounds));
        e.take(&step, 3_000).unwrap();
        assert!(!e.counters.body_only_grace, "an ordinary round leaves the grace unspent");
        assert_eq!(e.counters.review_rounds, limits.max_review_rounds);
    }

    /// All four crossings of {this lane was already briefed at the live head} x
    /// {every required lane has ANSWERED at it}, because the mark reads two
    /// signals and a mark that read either one alone is green on two of them.
    #[test]
    fn the_body_only_mark_needs_a_re_brief_at_an_unchanged_head_and_a_fully_answered_one() {
        let limits = DriveLimits::default();
        // `briefed`: the head this lane's record was last briefed at, or "" for
        // a lane with no record at all. `other`: the SECOND required lane's
        // verdict, which is what decides "has everyone answered".
        let step = |briefed: &str, other: Option<Verdict>| {
            let mut e = entry_at(DriveState::ReviewWait);
            e.head = "head-a".into();
            if !briefed.is_empty() {
                e.open_lane("rev-std", "s1", "rev-1", briefed, Some("OLD"), 1_000, false, false);
            }
            let facts = DriveFacts {
                required_lanes: Some(vec![
                    lane_fact("rev-std", Some(Verdict::Fail), "head-a", "OLD"),
                    lane_fact("rev-final", other, "head-a", "OLD"),
                ]),
                body_digest: Some("NEW".into()),
                ..facts_at("head-a")
            };
            decide(&e, &facts, &limits)
        };
        let marked = |s: &DriveStep| match s {
            DriveStep::OpenLane { body_only, .. } => *body_only,
            other => panic!("expected a lane brief, got {other:?}"),
        };
        // The one row that is #2509's case: re-briefed at this head, everyone
        // has spoken about it, and only the body moved.
        assert!(marked(&step("head-a", Some(Verdict::Pass))));
        // A lane nobody has answered at this head: the CODE here is not
        // reviewed to completion, so a later fail is a first opinion, not a
        // body finding.
        assert!(!marked(&step("head-a", None)));
        // A brief that predates this head is not a re-brief AT it.
        assert!(!marked(&step("head-b", Some(Verdict::Pass))));
        // And a lane with no record at all has never been briefed here.
        assert!(!marked(&step("", Some(Verdict::Pass))));
    }

    /// The grant's posture, which is [`LaneRecord::briefed_verify`]'s one grant
    /// over: an **exact** `(briefed_head, briefed_digest)` match, never
    /// [`lane_open_for`]'s unknown-tolerant comparison. A brief whose revision
    /// cannot be pinned grants nothing.
    #[test]
    fn the_grace_refuses_a_brief_whose_revision_it_cannot_pin() {
        let e = at_the_bound(true);
        assert!(
            body_only_grace_applies(&e, "rev-std", "head-a", Some("d1")),
            "the positive control: this fixture DOES earn the grace"
        );
        assert!(!body_only_grace_applies(&e, "rev-std", "head-b", Some("d1")), "the head moved");
        assert!(!body_only_grace_applies(&e, "rev-std", "head-a", Some("d2")), "the body moved");
        assert!(
            !body_only_grace_applies(&e, "rev-std", "head-a", None),
            "the body could not be read: unknown is never 'unbound, therefore fine'"
        );
        assert!(
            !body_only_grace_applies(&e, "rev-std", "head-a", Some("")),
            "and an empty digest is not a digest"
        );
        assert!(
            !body_only_grace_applies(&e, "rev-final", "head-a", Some("d1")),
            "another lane's brief grants this one nothing"
        );
        assert!(
            !body_only_grace_applies(&at_the_bound(false), "rev-std", "head-a", Some("d1")),
            "a brief that was never marked body-only"
        );
        let mut spent = at_the_bound(true);
        spent.counters.body_only_grace = true;
        assert!(!body_only_grace_applies(&spent, "rev-std", "head-a", Some("d1")), "already spent");
    }

    /// A repo file cannot buy a SECOND grace any more than it can buy a fourth
    /// round — the sibling of
    /// `a_repo_cannot_raise_invariant_9_by_handing_decide_a_wider_bound`, on the
    /// axis that slice could not vary because the field did not exist.
    #[test]
    fn a_repo_that_widens_its_bound_still_gets_exactly_one_grace() {
        let wide = DriveLimits { max_review_rounds: 9, ..DriveLimits::default() };
        assert_eq!(wide.max_review_rounds, 9, "the fixture really is over-bound");
        let facts = failing_at_the_bound();
        let mut e = at_the_bound(true);
        e.counters.review_rounds = MAX_ROUNDS_CEILING;
        assert_eq!(
            decide(&e, &facts, &wide),
            DriveStep::spend(DriveState::FixWait, Counter::BodyOnlyGrace),
            "the clamp puts a wide repo at the same bound, and the grace answers there"
        );
        e.counters.body_only_grace = true;
        assert_eq!(
            decide(&e, &facts, &wide),
            DriveStep::held(HeldReason::ReviewLimit),
            "and nine rounds in a repo file still buys no second grace"
        );
    }

    /// An `escalate` at the bound is still §3's judgment call, not a grace: the
    /// currency ladder answers `escalate` above the `fail` arm, and #2509 must
    /// not have quietly moved a JUDGMENT hold onto the driver's own budget.
    #[test]
    fn an_escalate_at_the_bound_is_still_a_judgment_hold_and_never_a_grace() {
        let limits = DriveLimits::default();
        let facts = DriveFacts {
            required_lanes: Some(vec![lane_fact(
                "rev-std",
                Some(Verdict::Escalate),
                "head-a",
                "d1",
            )]),
            ..facts_at("head-a")
        };
        let e = at_the_bound(true);
        assert_eq!(decide(&e, &facts, &limits), DriveStep::held(HeldReason::Escalate));
        assert!(
            !e.counters.body_only_grace,
            "and it costs the grace nothing, so a disposition can still spend it"
        );
    }

    /// §5.2's graceful-degradation posture, and the asymmetry with the three
    /// COUNTS beside it: a missing count is REFUSED because a defaulted zero
    /// re-grants a whole budget, while a missing bool grants one round once and
    /// is the true reading of a file no build with this field ever wrote.
    #[test]
    fn a_counters_block_written_before_the_grace_existed_parses_with_it_unspent() {
        let old = NOTE_EXAMPLE;
        assert!(
            !old.contains("body_only_grace"),
            "the fixture really predates the field"
        );
        let st = parse_state(old).expect("a pre-#2509 counters block still parses");
        assert!(
            st.entries.iter().all(|d| !d.counters.body_only_grace),
            "and reads as a grace nobody has spent"
        );
        // The negative control: the field is not merely being ignored on the way
        // in. A file that says the grace IS spent round-trips as spent.
        let spent = old.replace(
            r#""rebase_attempts": 0 },"#,
            r#""rebase_attempts": 0, "body_only_grace": true },"#,
        );
        assert_ne!(spent, old, "the mutation must actually land");
        let st = parse_state(&spent).expect("and the new field parses");
        assert!(st.entries.iter().all(|d| d.counters.body_only_grace));
    }
}
