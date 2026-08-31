//! The review-loop driver's registry wiring (#1778 S3).
//!
//! Design note: `doc/design/review-driver.md`. Every decision this file makes
//! is made somewhere else: the state machine is [`reviewdrive::decide`], the
//! gate is [`mergeq::recheck_gate`], the `gh` reads and the notice text are
//! [`rddrive`]. What lives here is what only the registry can do — resolve the
//! policy, the gate and the verdict files, hold the state lock across the
//! read-modify-write, perform the spawns and resumes, emit the audit events,
//! and deliver the notices.
//!
//! # Why its own file rather than more of `mod.rs`
//!
//! Size is the smaller reason. The larger one is that §3.1 item 1's source
//! scan needs a scope, and CLAUDE.md's source-scanning-guard convention
//! forbids deciding one from a binding's NAME — the design note says so about
//! this scan in particular: "any scope keyed on a name — a module, an `rd_*`
//! prefix — is stepped over by a landing verb added in a function that does
//! not carry it". A **file** is not a name. Every landing verb the driver
//! could reach has to be written down somewhere, and this is the somewhere, so
//! `tests/reviewdrive.rs` can default-deny the whole of it and a rename cannot
//! move code out from under the guard.
//!
//! The residual is stated where the scan is implemented, not here, and it is
//! the one the note names: a landing verb the driver reaches through a SHARED
//! helper it does not own. [`rddrive::RdRunner`] closes the `git` half of that
//! structurally — it has no `git` method — and nothing closes the `gh` half but
//! the scan.

use std::sync::Arc;

use serde_json::{json, Value};

use super::{
    brand, mergeq, mqloop, notify, now_ms, pr_number, rddrive, render_template,
    resolve_session_ref, resolve_worker_resume_cwd, reviewdrive, workflow, AgentEntry, GroupId,
    LockExt, OrchRegistry, Role, TaskPatch,
};

// ---------- the review driver's registry-side types (#1778 S3) ----------

/// The three brief templates (§5.5), embedded like every other built-in.
///
/// **New files, so they are not `pre222` fixtures** — that set pins the four
/// role templates byte-for-byte. These are pinned instead by the goldens and the
/// key-set assertion §5.5 prescribes, over the *rendered* output rather than the
/// source, because the rendered text is what a reviewer receives.
pub const DRIVER_REVIEW_TPL: &str = include_str!("templates/driver-review.md");
pub const DRIVER_DELTA_TPL: &str = include_str!("templates/driver-delta.md");
pub const DRIVER_FIX_TPL: &str = include_str!("templates/driver-fix.md");

/// Every value the driver interpolates into a brief, scrubbed (§5.5).
///
/// **Applied at the render call site, not inside a test harness**, which §5.5
/// makes the difference between a pin and a decoration: "a test that sanitizes
/// inside its own render harness asserts only that the two functions compose,
/// and passes identically while the live call site hands `render_template` a raw
/// job name".
///
/// **Every value, not just the two that are author-controlled.** A failed job
/// name comes from a `.github/workflows` file on the PR branch and a changed
/// path is whatever the pusher chose, so those two are the ones §5.5 names — but
/// a rule applied only to the fields someone remembered to classify is a rule
/// exactly the width of that memory, and the cost of scrubbing a block id
/// orrerix minted itself is nothing.
///
/// `Lines::Collapse` rather than `Keep`: a brief is a prompt typed into a pane,
/// and a value that could open a line of its own is a value that could open a
/// line looking like an instruction. The verdict-summary path keeps newlines
/// because a reviewer's prose is the payload there; here every value is a
/// single token or a list.
fn rd_fact(s: &str) -> String {
    notify::sanitize_pane_text(s, RD_FACT_CAP, notify::Lines::Collapse)
}

/// How long one interpolated fact may be. A brief is a prompt, and a prompt is
/// the delegate's resident context — the same cost argument §6 makes for
/// notices. Long enough for a changed-file list on a real PR, short enough that
/// a pathological one cannot become the whole brief.
const RD_FACT_CAP: usize = 2_000;

/// What one driven delegate did, as the interception arms report it (§7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RdEvent {
    /// `report(done)` from the driven worker.
    WorkerDone,
    /// `report(blocked)` from the driven worker.
    WorkerBlocked,
    /// `message_orchestrator` from any driven delegate. **Never intercepted** —
    /// the delegate's own line is delivered unchanged by its own arm; this is
    /// only the routing fact beside it (§7). Carries WHICH delegate, because
    /// that is the fact the hold exists to supply.
    Messaged { by: String },
    /// `review_verdict` from a driven lane. Carries nothing: the verdict FILE is
    /// what the next tick reads, from the same parser the gate reads, so a
    /// signal carrying the word would be a second source for one fact.
    Verdict,
}

/// A driven PR's pending delegate signals, between an event and the tick that
/// acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RdSignal {
    pub worker: reviewdrive::WorkerSignal,
    pub messaged: bool,
    /// WHICH delegate called `message_orchestrator`.
    ///
    /// §2.2 says every hold names "the one fact that decides what the
    /// orchestrator does next", and for `held(messaged)` that fact is which
    /// delegate spoke: its own line is already in the pane, unchanged, and the
    /// hold is the routing fact BESIDE it — a hold that named nobody would leave
    /// the orchestrator correlating two lines by timing.
    pub messaged_by: String,
}

impl Default for RdSignal {
    fn default() -> RdSignal {
        RdSignal {
            worker: reviewdrive::WorkerSignal::Silent,
            messaged: false,
            messaged_by: String::new(),
        }
    }
}

/// The facts one entry's briefs and notices are rendered from — read once, at
/// the top of the step, so a brief and the notice beside it cannot disagree
/// about what this tick saw.
struct RdBrief {
    pr: u64,
    head: String,
    base: String,
    body_digest: String,
    ci: reviewdrive::CiObservation,
    failing_jobs: Vec<String>,
    /// The gate's required lanes at this head, in `RoutingDecision::required`'s
    /// order — the static `reviewers:` list first, then the fired rules in
    /// declaration order. Lane *k+1* opens only after lane *k* passes, which is
    /// how §4's sequenced-lane rule is expressed with no block name in the code.
    required: Vec<String>,
    lane_notices: Vec<rddrive::LaneNotice>,
    /// The lane `review-wait` is actually acting on this tick — the first whose
    /// `pass` does not stand at this revision, which is the index
    /// `decide_review_wait` itself walks to.
    ///
    /// **Carried rather than re-derived from the verdicts**, because the lane a
    /// hold is ABOUT is not always a lane that has spoken. `lane-stalled` is the
    /// case that proves it: the stalled lane has recorded nothing, so it is
    /// absent from `lane_notices` entirely, and picking "the last lane with a
    /// verdict" names a *different, passing* lane and its pane — in the one
    /// notice whose whole job §2.2 says is to name the pane.
    deciding_lane: Option<String>,
}

impl RdBrief {
    /// The digest as `open_lane` wants it: `None` when the body could not be
    /// read, which `lane_open_for` reads as "cannot tell" rather than as drift.
    fn body_digest_opt(&self) -> Option<&str> {
        (!self.body_digest.is_empty()).then_some(self.body_digest.as_str())
    }

    /// Which of the three hand-back shapes this is, for the audit line.
    fn handback_kind(&self) -> &'static str {
        match self.ci {
            reviewdrive::CiObservation::Conflicting => "conflict",
            reviewdrive::CiObservation::Red => "ci-red",
            _ => "review-findings",
        }
    }

    /// The lane that recorded `fail`, or empty — what a findings hand-back and a
    /// `review-limit` hold both name.
    fn failing_lane(&self) -> String {
        self.lane_notices
            .iter()
            .find(|l| l.verdict == workflow::Verdict::Fail)
            .map(|l| l.block.clone())
            .unwrap_or_default()
    }

    /// The lane that recorded `escalate`, or the failing one, or empty.
    fn speaking_lane(&self) -> Option<&rddrive::LaneNotice> {
        self.lane_notices
            .iter()
            .find(|l| l.verdict != workflow::Verdict::Pass)
            .or_else(|| self.lane_notices.last())
    }

    /// The notice inputs for a hold (§6).
    ///
    /// **The lane a hold is about is the DECIDING lane, not the last one that
    /// spoke.** For `escalate` and `review-limit` the two coincide, because the
    /// deciding lane is the one whose verdict caused the hold. For
    /// `lane-stalled` they do not and cannot: that lane has recorded nothing, so
    /// it is absent from `lane_notices`, and naming the last lane that answered
    /// would put a passing lane's block and pane into a notice about a stalled
    /// one. The summary still comes from whichever lane actually spoke, because
    /// there is no summary to quote from a lane that did not.
    fn held_facts(
        &self,
        entry: &reviewdrive::DriveEntry,
        limits: &reviewdrive::DriveLimits,
        messaged_by: &str,
    ) -> rddrive::HeldFacts {
        let speaking = self.speaking_lane();
        let lane = self
            .deciding_lane
            .clone()
            .or_else(|| speaking.map(|l| l.block.clone()))
            .unwrap_or_else(|| self.required.get(entry.lane_index).cloned().unwrap_or_default());
        rddrive::HeldFacts {
            head: entry.head.clone(),
            worker_session: entry.worker_session.clone(),
            lane_agent: entry.lane(&lane).map(|l| l.agent.clone()).unwrap_or_default(),
            lane_summary: speaking.map(|l| l.summary.clone()).unwrap_or_default(),
            messaged_by: messaged_by.to_string(),
            lane,
            counters: entry.counters.clone(),
            max_review_rounds: limits.max_review_rounds,
            max_ci_attempts: limits.max_ci_attempts,
            failing_jobs: self.failing_jobs.clone(),
        }
    }
}

/// What one entry's step produced, for the caller to emit outside the lock.
#[derive(Debug, Default)]
struct RdOut {
    pr: u64,
    changed: bool,
    backoff: bool,
    clear_signal: bool,
    on_behalf_of: String,
    advanced: Option<(reviewdrive::DriveState, Option<reviewdrive::HeldReason>)>,
    lanes_opened: Vec<(String, String)>,
    handback: Option<String>,
    audits: Vec<(&'static str, Value)>,
    notices: Vec<String>,
}

impl RdOut {
    fn new(pr: u64) -> RdOut {
        RdOut { pr, ..RdOut::default() }
    }
}

/// What one wake of the review driver did, for a test that needs to assert the
/// production path ran rather than infer it from side effects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RdDriveReport {
    /// `(pr, state)` for every entry that took an arc this tick.
    pub advanced: Vec<(u64, reviewdrive::DriveState)>,
    /// `(pr, block, agent)` for every lane briefed this tick.
    ///
    /// The agent id is here and not only the block because it is the only handle
    /// a caller has on the pane that was actually opened — `review_drive_status`
    /// deliberately does not publish one (it is a compaction-recovery surface,
    /// and a pane id is not what an orchestrator routes on), so without it
    /// nothing outside this module can ask where a driver-spawned lane landed.
    pub lanes_opened: Vec<(u64, String, String)>,
    /// `(pr, agent)` for every worker resumed with a hand-back this tick — the
    /// same handle `lanes_opened` carries for a lane, and for the same reason:
    /// it is the only way a caller can ask what the worker was actually told.
    pub handbacks: Vec<(u64, String)>,
    /// The kick-back notices delivered to the orchestrator.
    pub notices: Vec<String>,
    /// Terminal entries dropped after their notices went out (§5.2).
    pub pruned: Vec<u64>,
    /// Whether this group was backed off `RD_BACKOFF_MS` rather than merely
    /// taking its turn in the rotation.
    pub backoff: bool,
    /// Set when the tick refused outright — a `review_drives.json` this build
    /// will not read (§2.4). **Not** the same as "nothing was driven".
    pub refused: Option<&'static str>,
}

impl OrchRegistry {
    // ---------- the review-loop DRIVER (#1778) ----------

    /// This group's `driver:` policy (§5.3): whether the feature is on at all,
    /// and the bounds one drive runs against.
    ///
    /// **The two conditions are the merge queue's**, for `merge_queue_enabled`'s
    /// reason: the `advanced_orchestrator` toggle governs the workflow file, and
    /// the `driver:` block is part of that file, so a policy that read its own
    /// block alone would be live in a group that opted out of the whole
    /// workflow.
    ///
    /// **Off is the answer to every uncertainty** — no group, no advanced
    /// orchestrator, no workflow file, a file that will not parse. That is
    /// §5.3's "an absent block means the feature is off and behaviour is
    /// byte-for-byte unchanged", and it is the opposite of `board_policy`'s
    /// fail-open on purpose: a pacing discipline that fails open costs a warning
    /// nobody reads, while a driver that fails open spawns reviewers into a repo
    /// that never asked for one.
    ///
    /// **The `driver:` block, mapped through `DriveLimits::new`, which clamps.**
    /// S2 already refused an out-of-range value as it parsed; this clamps again
    /// on the values actually read, and the second is not redundant — `decide`
    /// is a `pub fn` over a plain value type that any caller in any crate can
    /// reach without passing through S2's parser, and a boundary that holds only
    /// when the expected caller is upstream is not a boundary. The two layers
    /// enforce independently on purpose; `the_two_layers_agree_on_invariant_9`
    /// is what stops them disagreeing about the VALUE.
    ///
    /// The timeouts pass through unclamped here because §5.3 does not bound them
    /// against INVARIANT 9 — they are pacing, not budget, and S2 clamps them to
    /// the notify-TTL family as it parses.
    fn driver_policy(&self, group: &GroupId) -> (bool, reviewdrive::DriveLimits) {
        let off = (false, reviewdrive::DriveLimits::default());
        let Some(g) = self.group(group) else { return off };
        if !g.guardrails.advanced_orchestrator {
            return off;
        }
        let Ok(Some(wf)) = workflow::load_workflow(&g.repo) else { return off };
        let d = wf.driver;
        (
            d.enabled,
            reviewdrive::DriveLimits::new(
                d.max_review_rounds,
                d.max_ci_attempts,
                d.max_rebase_attempts,
                d.lane_timeout_minutes as u64,
                d.fix_timeout_minutes as u64,
                d.drive_timeout_minutes as u64,
            ),
        )
    }

    /// Whether this group runs a review driver at all.
    ///
    /// `pub(super)` because `instruction_vars` in the parent module gates
    /// `{{REVIEW_DRIVER}}` on it: a private item is visible to its own module
    /// and that module's DESCENDANTS, and `orchestration` is this file's
    /// parent, not its child. Widened to exactly the parent and no further, so
    /// the one reader outside this file is the template gate — which has to
    /// read the same policy the tick does, or a group whose tools all refuse
    /// `driver-disabled` could be told in its instructions that it has a driver.
    pub(super) fn driver_enabled(&self, group: &GroupId) -> bool {
        self.driver_policy(group).0
    }

    /// Install (or clear) the canned `gh` the driver reads through —
    /// `mq_runner_override`'s twin. `None` in the app, always.
    #[doc(hidden)] // pub for integration tests
    pub fn set_rd_runner_override(&self, runner: Option<Arc<dyn rddrive::RdRunner>>) {
        *self.rd_runner_override.lock_safe() = runner;
    }

    /// Hold `group` off until `at` (§2.4's rate bound).
    fn rd_defer(&self, group: &GroupId, at: u64) {
        self.rd_service_ms.lock_safe().insert(group.clone(), at);
    }

    /// Record a driven delegate's event for the next tick (§7).
    ///
    /// **Consumed, not dropped.** The MCP arm audits `rd-consumed` before
    /// calling this, so traffic that stopped arriving as an orchestrator prompt
    /// is still on the record and still attributable.
    ///
    /// **In memory, and that is a bounded choice rather than an oversight.** A
    /// `report` is an *event*; the durable facts a drive turns on — the head,
    /// the verdict files — are re-read from GitHub and from disk every tick. An
    /// orrerix restart between a worker's `report(done)` and the next tick
    /// therefore loses only the body-only-fix shortcut (arc 8): a push is still
    /// seen as a head move (arc 7), and a drive that learns nothing degrades to
    /// `held(fix-stalled)`, which is bounded and named. Persisting an event
    /// queue would buy that one arc at the cost of a second write path into the
    /// state file, from the MCP thread.
    #[doc(hidden)] // pub for integration tests
    pub fn rd_ingest(&self, group: &GroupId, pr: u64, event: RdEvent) {
        let mut map = self.rd_signals.lock_safe();
        let sig = map.entry((group.clone(), pr)).or_default();
        match event {
            RdEvent::WorkerDone => sig.worker = reviewdrive::WorkerSignal::Done,
            // `blocked` outranks a `done` seen in the same window: the two can
            // only both be present if the worker said one and then the other,
            // and `blocked` is the one that needs a human.
            RdEvent::WorkerBlocked => sig.worker = reviewdrive::WorkerSignal::Blocked,
            RdEvent::Messaged { by } => {
                sig.messaged = true;
                sig.messaged_by = by;
            }
            RdEvent::Verdict => {}
        }
    }

    /// Which live drive, if any, this agent is a delegate of — §7's interception
    /// key, and the whole of it.
    ///
    /// **Keyed on the agent, never on text.** The id compared here is one
    /// orrerix minted at spawn and the driver recorded itself
    /// (`LaneRecord::agent`, `DriveEntry::worker_agent`), and the caller's id
    /// comes from its MCP token rather than from `args`. So a delegate cannot
    /// choose whether its report reaches the orchestrator by naming a PR
    /// number, and cannot name someone else's to redirect theirs — which is the
    /// property §7 spends a paragraph on, because a delegate that could do
    /// either is a delegate that can route around the orchestrator.
    ///
    /// **Only a LIVE drive owns anyone.** A `held` entry is parked: its
    /// delegates' traffic goes to the orchestrator exactly as it always did,
    /// which is what makes a hold a hand-back to a human rather than a quieter
    /// kind of drive. A terminal entry owns nobody for the same reason.
    ///
    /// Reads `review_drives.json` on every driven-or-not `report` and
    /// `review_verdict`. That is one small JSON read on a path that already
    /// writes a verdict file and delivers a pane prompt, and an absent file —
    /// the product default — costs a `stat` and answers `None`.
    pub fn rd_owner(
        &self,
        group: &GroupId,
        agent_id: &str,
    ) -> Option<(u64, reviewdrive::DrivenRole)> {
        let dir = self.group_dir(group);
        let _state_guard = self.rd_state_lock.lock_safe();
        let state = reviewdrive::load_state(&dir).ok()?;
        state
            .entries
            .iter()
            .filter(|e| e.state().is_live())
            .find_map(|e| e.driven_role(agent_id).map(|r| (e.pr, r)))
    }

    /// Record that a driven delegate's event was **consumed** by the driver
    /// rather than delivered to the orchestrator (§7).
    ///
    /// **Nothing is silent.** Every consumed event is audited with its kind, the
    /// agent and the PR, so traffic that stopped arriving as a prompt is still
    /// on the record and still attributable. "Consumed" is a different word from
    /// "dropped" and §5.4's vocabulary keeps them different.
    ///
    /// `event` is `None` for traffic that is consumed but carries no signal — a
    /// `report(progress)`, whose content the drive does not turn on: a drive
    /// advances on the head, the checks and the verdict files, not on a delegate
    /// saying it is still going.
    pub fn rd_consume(
        &self,
        group: &GroupId,
        pr: u64,
        agent: &str,
        kind: &str,
        event: Option<RdEvent>,
    ) {
        let on_behalf = {
            let dir = self.group_dir(group);
            let _state_guard = self.rd_state_lock.lock_safe();
            reviewdrive::load_state(&dir)
                .ok()
                .and_then(|s| s.entry(pr).map(|e| e.on_behalf_of.clone()))
                .unwrap_or_default()
        };
        self.rd_audit(
            group,
            &on_behalf,
            rddrive::audit_action::CONSUMED,
            json!({ "pr": pr, "agent": agent, "kind": kind }),
        );
        if let Some(e) = event {
            self.rd_ingest(group, pr, e);
        }
    }

    /// This drive's pending delegate signals, **without clearing them**.
    ///
    /// Cleared only once an arc has been taken, because a tick can decline to
    /// act for reasons that have nothing to do with the signal — an unresolved
    /// head, a runner failure — and a signal consumed by a tick that then did
    /// nothing is a hand-back the drive never learns about.
    fn rd_signal(&self, group: &GroupId, pr: u64) -> RdSignal {
        self.rd_signals.lock_safe().get(&(group.clone(), pr)).cloned().unwrap_or_default()
    }

    /// One audit line in the driver's vocabulary, carrying §3's `on_behalf_of`.
    ///
    /// The **actor** stays `brand::AUDIT_ACTOR`, so it is this detail key — not
    /// the actor — that distinguishes a driver action from any other host
    /// action, and it is what an audit reader filters on.
    fn rd_audit(&self, group: &GroupId, on_behalf_of: &str, action: &str, mut detail: Value) {
        if let Some(obj) = detail.as_object_mut() {
            obj.insert(rddrive::ON_BEHALF_OF.to_string(), Value::from(on_behalf_of));
        }
        self.audit(group, brand::AUDIT_ACTOR, action, detail);
    }

    /// The one group this wake will service, or `None`.
    ///
    /// `next_mq_group`'s three filters, cheapest first, each of them a reason
    /// not to spend a subprocess: not inside a backoff window, has a
    /// `review_drives.json` at all (a **file check** rather than a parse, so an
    /// unreadable file still reaches the driver's own loud handling), and the
    /// repo declares the driver.
    fn next_rd_group(&self, now: u64) -> Option<GroupId> {
        let all: Vec<GroupId> = self.groups.lock_safe().keys().cloned().collect();
        let service = self.rd_service_ms.lock_safe().clone();
        let mut due: Vec<(u64, GroupId)> = all
            .into_iter()
            .filter(|g| service.get(g).map(|t| now >= *t).unwrap_or(true))
            .filter(|g| reviewdrive::state_path(&self.group_dir(g)).exists())
            .filter(|g| self.driver_enabled(g))
            .map(|g| (service.get(&g).copied().unwrap_or(0), g))
            .collect();
        // Oldest-serviced first; the group id breaks a tie deterministically so
        // two never-serviced groups do not alternate on HashMap iteration order.
        due.sort();
        due.into_iter().next().map(|(_, g)| g)
    }

    /// One driver step, for at most one group — the fifth step in
    /// `gh_poll_tick` (§2.4), clock injected.
    ///
    /// **At most one group per wake, oldest-serviced first**, which is
    /// `mq_driver_tick`'s bound and structural rather than a counter: this loop
    /// is shared with every `notify_when` watch in the fleet, and a driver that
    /// serviced N groups on one wake would put N groups' worth of `gh`
    /// round-trips inside one tick.
    pub fn rd_driver_tick(&self, now: u64) -> Option<GroupId> {
        let group = self.next_rd_group(now)?;
        let injected = self.rd_runner_override.lock_safe().clone();
        match injected {
            Some(r) => {
                self.rd_drive_group_with(&group, r.as_ref(), now);
            }
            None => {
                let repo = self.group(&group).map(|g| g.repo)?;
                let runner = rddrive::runner_for(std::path::Path::new(&repo));
                self.rd_drive_group_with(&group, &runner, now);
            }
        }
        Some(group)
    }

    /// Drive one group with the `gh` seam injected — the seam
    /// `tests/reviewdrive.rs` uses to exercise the whole production path
    /// without spawning a child (CLAUDE.md constraint 3).
    ///
    /// **This is wiring, not logic.** Every state decision is
    /// `reviewdrive::decide`'s and every gate answer is
    /// `mergeq::recheck_gate`'s. What lives here is what only the registry can
    /// do: resolve the policy, the gate and the verdict files, hold the state
    /// lock across the read-modify-write, perform the spawns and resumes, emit
    /// the audit events, and deliver the notices.
    ///
    /// **The lock spans the spawn and never a delivery**, which is §2.4's
    /// prescription with its one ambiguity resolved rather than ignored. §2.4
    /// wants the load-decide-store to span the spawn, because a `drive_review`
    /// landing inside that window would read the pre-spawn file and write it
    /// back, erasing the entry; #467/#468 want no registry lock held across a
    /// notice, because a delivery enqueues and an enqueue re-enters registry
    /// locks. Both hold here: `rd_state_lock` is taken by exactly four call
    /// sites — this tick and the three MCP tools — none of them reachable from a
    /// pane delivery, so a spawn's own kickoff cannot cycle back onto it; and
    /// the orchestrator notices this produces are delivered below, outside it.
    #[doc(hidden)] // pub for integration tests
    pub fn rd_drive_group_with(
        &self,
        group: &GroupId,
        runner: &dyn rddrive::RdRunner,
        now: u64,
    ) -> RdDriveReport {
        let mut report = RdDriveReport::default();
        let (enabled, limits) = self.driver_policy(group);
        if !enabled {
            // Byte-for-byte unchanged behaviour with no `driver:` block (§5.3).
            // Checked here as well as in `next_rd_group` because this method is
            // the test seam, and a seam that skipped the product's own opt-in
            // would be testing something the product cannot do.
            return report;
        }
        for n in self.rd_reconcile_with(group, runner) {
            let _ = self.deliver_to_orchestrator(group, &n, brand::AUDIT_ACTOR);
            report.notices.push(n);
        }
        let dir = self.group_dir(group);
        let mut outs: Vec<RdOut> = Vec::new();
        // Whether this tick's decisions actually reached disk. A signal is a
        // ONE-SHOT, and consuming one for a transition the next restart forgets
        // is the same shape as latching a once-only flag on a read that failed:
        // the arc is rolled back by the failed write, the delegate's event is
        // gone, and the drive never learns of it again. CLAUDE.md's
        // multi-tenant-store rule is the settled form — a failed read declines
        // rather than defaulting, and is not latched, so one transient rejection
        // does not disable the mechanism. Same rule, applied to a failed WRITE
        // consuming an event.
        let mut persisted = true;
        {
            let _state_guard = self.rd_state_lock.lock_safe();
            let mut state = match reviewdrive::load_state(&dir) {
                Ok(s) => s,
                Err(e) => {
                    // §2.4: refuse the tick, audit, back off. Never repaired and
                    // never deleted — a record orrerix will not read is one
                    // whose live drives it cannot account for, and guessing
                    // would resume a drive against state nobody wrote.
                    self.audit(
                        group,
                        brand::AUDIT_ACTOR,
                        rddrive::audit_action::STATE_UNREADABLE,
                        json!({ "detail": format!("{e:?}") }),
                    );
                    self.rd_defer(group, now.saturating_add(rddrive::RD_BACKOFF_MS));
                    report.backoff = true;
                    report.refused = Some(rddrive::audit_action::STATE_UNREADABLE);
                    return report;
                }
            };
            // One tick, one answer per base branch — see `rd_base_green`.
            let mut base_green: std::collections::HashMap<String, Option<bool>> =
                std::collections::HashMap::new();
            let prs: Vec<u64> = state.entries.iter().map(|e| e.pr).collect();
            for pr in prs {
                if let Some(o) =
                    self.rd_step_entry(group, runner, &mut state, pr, &limits, now, &mut base_green)
                {
                    outs.push(o);
                }
            }
            if outs.iter().any(|o| o.changed) {
                if let Err(e) = reviewdrive::store_state(&dir, &state) {
                    persisted = false;
                    // A transition the next restart forgets is not a transition.
                    // Audited loudly and backed off; reconcile fixes the record
                    // on the next start.
                    self.audit(
                        group,
                        brand::AUDIT_ACTOR,
                        rddrive::audit_action::STATE_UNREADABLE,
                        json!({ "reason": "review_drives.json could not be written",
                                "detail": e }),
                    );
                    report.backoff = true;
                }
            }
        }
        for o in &outs {
            for (action, detail) in &o.audits {
                self.rd_audit(group, &o.on_behalf_of, action, detail.clone());
            }
            for (b, a) in &o.lanes_opened {
                report.lanes_opened.push((o.pr, b.clone(), a.clone()));
            }
            if let Some(a) = &o.handback {
                report.handbacks.push((o.pr, a.clone()));
            }
            if let Some((to, _)) = o.advanced {
                report.advanced.push((o.pr, to));
            }
            for n in &o.notices {
                let _ = self.deliver_to_orchestrator(group, n, brand::AUDIT_ACTOR);
                self.rd_task_note(group, o.pr, n);
                report.notices.push(n.clone());
            }
            // Only once the arc that consumed it is durable — see `persisted`.
            if o.clear_signal && persisted {
                self.rd_signals.lock_safe().remove(&(group.clone(), o.pr));
            }
            if o.backoff {
                report.backoff = true;
            }
        }
        // §5.2's retention, and it runs HERE — after the notices — because that
        // rule is an ordering one: "terminal entries are pruned once their
        // notice has been delivered", and `prune_terminal`'s own doc says the
        // caller owns that because the function cannot enforce it.
        let pruned = {
            let _state_guard = self.rd_state_lock.lock_safe();
            match reviewdrive::load_state(&dir) {
                Ok(mut state) => {
                    let dropped = reviewdrive::prune_terminal(&mut state);
                    if dropped.is_empty() || reviewdrive::store_state(&dir, &state).is_ok() {
                        dropped
                    } else {
                        Vec::new()
                    }
                }
                Err(_) => Vec::new(),
            }
        };
        for pr in &pruned {
            self.rd_audit(group, "", rddrive::audit_action::PRUNED, json!({ "pr": pr }));
            self.rd_signals.lock_safe().remove(&(group.clone(), *pr));
        }
        report.pruned = pruned;
        if report.backoff {
            self.rd_defer(group, now.saturating_add(rddrive::RD_BACKOFF_MS));
        } else {
            // Not a backoff — just this group's turn in the rotation, so N
            // groups with live drives share the wakes instead of the first one
            // alphabetically taking every tick.
            self.rd_defer(group, now);
        }
        report
    }

    /// The gate, read the way §4 requires: **through the readers that already
    /// exist**, never a re-derivation.
    ///
    /// `workflow::route_reviewers` gives the required lane list `review-wait`
    /// walks; `mergeq::recheck_gate` gives `gate-check` its answer, and that is
    /// where `ci-green`, `body-unchanged`, `base-green`, `max_diff_lines` and
    /// routing-unaccountable are each already decided once. Merge-queue §6 is
    /// explicit that a third *implementation* of the gate decision is a defect
    /// rather than an optimization; this makes the driver a third **reader**.
    ///
    /// The one thing decided here is which of `recheck_gate`'s answers is a
    /// `gate-unreadable` hold and which is an ordinary not-satisfied-yet,
    /// because that mapping is the driver's own vocabulary and nothing else
    /// needs it.
    fn rd_gate_facts(
        &self,
        group: &GroupId,
        runner: &dyn rddrive::RdRunner,
        pr: u64,
        obs: &rddrive::PrObservation,
        want_gate: bool,
        base_green_memo: &mut std::collections::HashMap<String, Option<bool>>,
    ) -> (reviewdrive::GateOutcome, Option<Vec<reviewdrive::LaneFact>>, Vec<rddrive::LaneNotice>) {
        let spec = match self.merge_queue_gate(group) {
            Ok(s) => s,
            Err(_) => {
                // The file is on disk and orrerix could not read it. §2.2's
                // `gate-unreadable`, and NOT `gate-not-configured` — a wrong
                // label sends the reader somewhere else (#681's posture, which
                // the queue keeps and the driver inherits).
                return (reviewdrive::GateOutcome::Unreadable, None, Vec::new());
            }
        };
        let gate = match &spec {
            mergeq::GateSpec::Declared(g) => g.clone(),
            // Present and unparseable. §2.2 words `gate-unreadable` as an I/O
            // error; a file whose contents will not parse is read no better than
            // one that would not open, the `gh` shim refuses every merge on
            // exactly this state, and announcing satisfied over it is the
            // outcome §3.1 calls "a bypass with better telemetry". The design
            // note's row is widened to say so in this PR rather than the reading
            // being left implicit here.
            mergeq::GateSpec::Malformed => {
                return (reviewdrive::GateOutcome::Unreadable, None, Vec::new())
            }
            // §5.1 makes an absent gate a drive-time DECLINE, so a live entry
            // can only be here if the repo deleted its gate under a running
            // drive. Not satisfied — arc 10 back to `ci-wait`, then the drive's
            // own age bound — which is §8's degradation for a gate that cannot
            // be reached, honestly labelled, rather than a hold reason §2.2's
            // closed enum does not have.
            mergeq::GateSpec::Absent => {
                return (reviewdrive::GateOutcome::Unsatisfied, None, Vec::new())
            }
        };
        // `base-green` is a fact about a BRANCH, so it is fetched at most once
        // per tick and shared by every entry on that base — `mqloop`'s own
        // driver memoizes it per pass for the same reason. Fetched only when
        // this state evaluates the gate AND the gate declares the condition;
        // `None` is the value that refuses, so declining to fetch can only make
        // the gate harder to satisfy.
        let base_green =
            if want_gate && rddrive::declares_base_green(&spec) && !obs.base.is_empty() {
                match base_green_memo.get(&obs.base) {
                    Some(cached) => *cached,
                    None => {
                        let answer = rddrive::base_ci_green(runner, &obs.base);
                        base_green_memo.insert(obs.base.clone(), answer);
                        answer
                    }
                }
            } else {
                None
            };
        let observed = rddrive::gate_observation(runner, pr, obs, &spec, base_green);
        let Some(routed) = workflow::route_reviewers(&gate, observed.changed_files.as_deref())
        else {
            // `route_reviewers` refused: the changed-file list could not be
            // shown complete, so WHICH reviewers are required is unknown.
            // `required_lanes: None` is what `decide` turns into
            // `held(routing-unaccountable)` from every state that reads it.
            return (reviewdrive::GateOutcome::NotEvaluated, None, Vec::new());
        };
        let verdicts = self.verdict_map(group, pr);
        let lanes: Vec<reviewdrive::LaneFact> = routed
            .required
            .iter()
            .map(|b| reviewdrive::LaneFact { block: b.clone(), verdict: verdicts.get(b).cloned() })
            .collect();
        let notices: Vec<rddrive::LaneNotice> = lanes
            .iter()
            .filter_map(|l| {
                l.verdict.as_ref().map(|v| rddrive::LaneNotice {
                    block: l.block.clone(),
                    verdict: v.verdict,
                    summary: v.summary.clone(),
                    at_head: v.head.clone(),
                })
            })
            .collect();
        if !want_gate {
            // `review-wait` needs the lane list and nothing else. Evaluating the
            // gate here would be a second answer to a question this state does
            // not ask, on a tick that has to re-ask it at `gate-check` anyway.
            return (reviewdrive::GateOutcome::NotEvaluated, Some(lanes), notices);
        }
        let head = (!obs.head.is_empty()).then_some(obs.head.as_str());
        let outcome = match mergeq::recheck_gate(&spec, &verdicts, head, &observed) {
            mergeq::GateRecheck::Ok => reviewdrive::GateOutcome::Satisfied,
            mergeq::GateRecheck::Malformed | mergeq::GateRecheck::NotConfigured => {
                reviewdrive::GateOutcome::Unreadable
            }
            mergeq::GateRecheck::RoutingUnaccountable => reviewdrive::GateOutcome::NotEvaluated,
            // Arc 10, deliberately wider than "stale": a stale pass, an
            // unsatisfied `also:` condition, a red default branch, a PR over the
            // size clause. Each is "the gate is not satisfied for this
            // revision", which is `ci-wait` and then the drive's own age bound.
            _ => reviewdrive::GateOutcome::Unsatisfied,
        };
        (outcome, Some(lanes), notices)
    }

    /// Brief one reviewer lane — a fresh spawn by block id, or a resume of the
    /// session this lane already has (§4: the driver does make the
    /// spawn-versus-reuse choice *within* a lane it was already told to run).
    ///
    /// **A resume that will not resolve falls back to a fresh spawn by block
    /// id**, which is §8's reaped-reviewer row: the entry stores the full
    /// resolved session so the next round resumes it, and a lane whose session
    /// no longer resolves respawns fresh. That respawn does **not** consume a
    /// `review_rounds` increment — that counter counts rounds of *findings*, and
    /// a reaped reviewer produced none.
    fn rd_open_lane(
        &self,
        group: &GroupId,
        entry: &mut reviewdrive::DriveEntry,
        block: &str,
        brief: &RdBrief,
        limits: &reviewdrive::DriveLimits,
        now: u64,
    ) -> Result<String, String> {
        if block.is_empty() {
            return Err("no lane at that index".into());
        }
        let text = self.rd_lane_brief(entry, block, brief, limits);
        let prior = entry.lane(block).map(|l| l.session.clone()).filter(|s| !s.is_empty());
        let spawned = match prior {
            Some(session) => self
                .rd_spawn(group, Role::Reviewer, None, Some(session), &text)
                .or_else(|_| {
                    self.rd_spawn(group, Role::Reviewer, Some(block.to_string()), None, &text)
                }),
            None => self.rd_spawn(group, Role::Reviewer, Some(block.to_string()), None, &text),
        }?;
        entry.open_lane(
            block,
            spawned.session_id.as_deref().unwrap_or(""),
            &spawned.id,
            &brief.head,
            brief.body_digest_opt(),
            now,
        );
        Ok(spawned.id)
    }

    /// Hand the PR back to its worker (§2.1's `fix-wait` row), resuming the
    /// session `drive_review` resolved and recorded.
    fn rd_handback(
        &self,
        group: &GroupId,
        entry: &mut reviewdrive::DriveEntry,
        brief: &RdBrief,
        limits: &reviewdrive::DriveLimits,
    ) -> Result<String, String> {
        let text = self.rd_fix_brief(entry, brief, limits);
        let session = entry.worker_session.clone();
        if session.is_empty() {
            return Err("this drive has no recorded worker session".into());
        }
        let spawned = self.rd_spawn(group, Role::Worker, None, Some(session), &text)?;
        entry.worker_agent = spawned.id.clone();
        Ok(spawned.id)
    }

    /// The workspace a driver-initiated resume inherits.
    ///
    /// **`resolve_worker_resume_cwd` is the shared decision, and this is a
    /// caller of it rather than a second copy of it.** A worker or reviewer
    /// resume must never fall back to the main clone — that is the human's
    /// environment (#338/#359) — and which workspace it inherits instead (the
    /// roster's recorded cwd if it still exists, else the session located in its
    /// CLI's own store by id) is written once, in the function the `spawn_agent`
    /// MCP arm and the session browser both call.
    ///
    /// What is *not* shared is that arm's role classification, and that is the
    /// point rather than an omission: the arm has to decide whether the thing
    /// being resumed even needs a dedicated workspace, because an orchestrator
    /// may resume a planner. The driver resumes exactly two roles, both of which
    /// always need one, so the branch has no subject here. Re-deriving it would
    /// be a second answer to a containment question; leaving it out is not.
    fn rd_resume_cwd(
        &self,
        group: &GroupId,
        session: &str,
        block: Option<&str>,
    ) -> Result<Option<String>, String> {
        let Some(g) = self.group(group) else { return Err("no such group".into()) };
        // The last-touched roster record naming this session — the same record
        // the MCP arm's own cwd inheritance reads, chosen the same way.
        let owner = self
            .merged_records(group)
            .into_iter()
            .filter(|r| r.session.as_deref() == Some(session))
            .max_by_key(|r| r.updated_ms);
        let blk = block
            .and_then(|id| g.guardrails.block(id).cloned())
            .or_else(|| owner.as_ref().and_then(|o| g.guardrails.block(&o.block).cloned()));
        let cli = match blk.as_ref() {
            Some(b) => workflow::cli_of(b, &g.guardrails.agent_cli).to_string(),
            None => g.guardrails.agent_cli.clone(),
        };
        let db = self.opencode_db_path(group);
        resolve_worker_resume_cwd(
            &cli,
            session,
            owner.as_ref().map(|o| o.cwd.as_str()),
            &g.repo,
            Some(&db),
        )
        .map(Some)
    }

    /// The one spawn or resume the driver performs.
    ///
    /// **A fresh lane gets a dedicated workspace, and that is #338/#359 rather
    /// than a preference.** `spawn_agent_ex` cuts a worktree only when
    /// `use_worktree` is set and no `cwd_override` is given; a fresh reviewer
    /// spawned with neither falls through to the per-role default, which is the
    /// group's **main clone** — the human's own checkout, and the exact conflict
    /// #359 exists to prevent (two reviewers, or a reviewer and the
    /// orchestrator's fetch traffic, contending on one checkout's state). The
    /// MCP `spawn_agent` surface defaults this ON for worker and reviewer kinds
    /// for the same reason, so the driver matches it rather than inventing a
    /// quieter default.
    ///
    /// A **resume** passes `false`, and it is not a second policy: the branch
    /// above is unreachable when `cwd_override` is `Some`, which a resume always
    /// is — [`rd_resume_cwd`](Self::rd_resume_cwd) resolves the workspace the
    /// session already had. Passing `false` there says "this spawn does not cut
    /// anything" rather than relying on a later branch to ignore a `true`.
    fn rd_spawn(
        &self,
        group: &GroupId,
        role: Role,
        block: Option<String>,
        resume: Option<String>,
        task: &str,
    ) -> Result<AgentEntry, String> {
        let cwd = match resume.as_deref() {
            Some(s) => self.rd_resume_cwd(group, s, block.as_deref())?,
            None => None,
        };
        let fresh = cwd.is_none();
        self.spawn_agent_bound(
            group, role, block, "", task, fresh, None, None, resume, cwd, None, None,
        )
    }

    /// Render one lane's brief (§5.5), **sanitizing every interpolated value at
    /// this call site**.
    ///
    /// §5.5 makes that placement the difference between a pin and a decoration:
    /// "a test that sanitizes inside its own render harness asserts only that
    /// the two functions compose, and passes identically while the live call
    /// site hands `render_template` a raw job name". So [`rd_fact`] wraps every
    /// value here, and the hostile-value test calls this function.
    fn rd_lane_brief(
        &self,
        entry: &reviewdrive::DriveEntry,
        block: &str,
        brief: &RdBrief,
        limits: &reviewdrive::DriveLimits,
    ) -> String {
        let round = entry.counters.review_rounds.saturating_add(1).to_string();
        let max = limits.max_review_rounds.to_string();
        let head = rd_fact(&brief.head);
        let pr = brief.pr.to_string();
        match entry.lane(block).filter(|l| !l.at_head.is_empty()) {
            // A lane that has answered before gets the delta — the line an
            // orchestrator typed by hand nine times on one PR.
            Some(rec) => {
                let prev_head = rd_fact(&rec.at_head);
                let prev =
                    rec.last_verdict.map(|v| v.as_str()).unwrap_or("unrecorded").to_string();
                let digest_state = if rec.briefed_digest.is_empty() || brief.body_digest.is_empty()
                {
                    // "Cannot tell" is not "changed" — the asymmetry
                    // `ReviewVerdict::body_changed` encodes.
                    "of unknown drift (orrerix could not compare the two digests)"
                } else if rec.briefed_digest == brief.body_digest {
                    "unchanged"
                } else {
                    "changed"
                };
                let moved = if rec.at_head == brief.head {
                    "What moved: the head has not moved and the PR body has. Re-read the body, \
                     not the diff."
                        .to_string()
                } else {
                    // **What this brief does NOT claim.** orrerix does not
                    // compute the per-round delta: the driver's seam is
                    // `gh`-only by construction (§3.1 item 1, made structural in
                    // `RdRunner`), so it has no `git diff` to run. It names the
                    // two revisions and points at the command that answers the
                    // question exactly — facts it read plus an instruction,
                    // rather than a delta it invented.
                    format!(
                        "What moved: the head moved from {prev_head} to {head}. orrerix does not \
                         compute the per-round delta; `git diff {prev_head}..{head}` in your \
                         worktree does."
                    )
                };
                render_template(
                    DRIVER_DELTA_TPL,
                    &[
                        ("PR", &pr),
                        ("HEAD", &head),
                        ("PREV_VERDICT", &rd_fact(&prev)),
                        ("PREV_HEAD", &prev_head),
                        ("PREV_DIGEST_STATE", digest_state),
                        ("WHAT_MOVED", &moved),
                        ("ROUND", &round),
                        ("MAX_ROUNDS", &max),
                    ],
                )
            }
            None => {
                let prior: Vec<String> = brief
                    .lane_notices
                    .iter()
                    .take_while(|l| l.block != block)
                    .map(|l| {
                        format!(
                            "{} recorded {}",
                            rd_fact(&l.block),
                            l.verdict.as_str().to_uppercase()
                        )
                    })
                    .collect();
                let prior = if prior.is_empty() {
                    String::new()
                } else {
                    // How a final lane learns it is validating a review as well
                    // as the work, with no block name anywhere in the code —
                    // §4's sequenced-lane rule expressed as an ordered list.
                    format!(" Lanes before yours at this revision: {}.", prior.join("; "))
                };
                render_template(
                    DRIVER_REVIEW_TPL,
                    &[
                        ("PR", &pr),
                        ("HEAD", &head),
                        ("BASE", &rd_fact(&brief.base)),
                        ("LANES", &rd_fact(&brief.required.join(", "))),
                        ("LANE", &rd_fact(block)),
                        ("ROUND", &round),
                        ("MAX_ROUNDS", &max),
                        ("PRIOR_LANES", &prior),
                    ],
                )
            }
        }
    }

    /// Render the worker's hand-back brief (§5.5).
    ///
    /// `{{WHAT}}` is **loomux-authored text chosen from a closed set of three**,
    /// with facts orrerix read interpolated into it — never delegate- or
    /// repo-authored prose (§3.1 item 4). The three are the three ways a PR
    /// comes back: a lane's findings, a red run, and a conflict.
    fn rd_fix_brief(
        &self,
        entry: &reviewdrive::DriveEntry,
        brief: &RdBrief,
        limits: &reviewdrive::DriveLimits,
    ) -> String {
        let base = rd_fact(&brief.base);
        let (what, attempt, max) = match brief.ci {
            reviewdrive::CiObservation::Conflicting => (
                format!(
                    "It is CONFLICTING against {base}. Rebase onto origin/{base}, resolve, and \
                     push."
                ),
                entry.counters.rebase_attempts,
                limits.max_rebase_attempts,
            ),
            reviewdrive::CiObservation::Red => (
                format!(
                    "CI is red at that head. Failing checks: {}. Read them with `gh pr checks \
                     {}`, fix, and push.",
                    rd_fact(&brief.failing_jobs.join(", ")),
                    brief.pr
                ),
                entry.counters.ci_attempts,
                limits.max_ci_attempts,
            ),
            _ => (
                format!(
                    "Review requested changes: {} recorded FAIL. The findings are on the PR. \
                     Address all of them, or answer on the PR why one is not a defect, then \
                     push.",
                    // The DECIDING lane, which is the one `decide_review_wait`
                    // actually acted on — not "the first lane with a `fail`".
                    // The two agree today, because the deciding lane is the
                    // first whose pass does not stand and a `fail` is what put
                    // it there. They agree by an argument rather than by
                    // construction, and naming the wrong lane in a hand-back
                    // sends a worker to the wrong review.
                    rd_fact(
                        &brief
                            .deciding_lane
                            .clone()
                            .unwrap_or_else(|| brief.failing_lane())
                    )
                ),
                entry.counters.review_rounds,
                limits.max_review_rounds,
            ),
        };
        render_template(
            DRIVER_FIX_TPL,
            &[
                ("PR", &brief.pr.to_string()),
                ("HEAD", &rd_fact(&brief.head)),
                ("BASE", &base),
                ("WHAT", &what),
                ("ATTEMPT", &attempt.to_string()),
                ("MAX_ATTEMPTS", &max.to_string()),
            ],
        )
    }

    /// §2.4's restart reconcile, once per group per process, before driving.
    ///
    /// The `recover_persisted_queue` posture: a PR **positively established** as
    /// closed or merged becomes `cancelled` with its notice; anything else
    /// resumes from disk and is re-evaluated against the **live** head on the
    /// tick that follows, never against the head the file remembers. A PR whose
    /// state could not be determined is neither — `None` is "the world does not
    /// match", never "probably fine".
    ///
    /// An unresolvable *session* is deliberately not held here, though §2.4
    /// names it: that is what the first hand-back discovers
    /// (`held(worker-unresumable)`), and holding at reconcile would park every
    /// drive whose worker pane merely has not been re-registered yet at startup
    /// — a race the reconcile cannot distinguish from a genuinely lost session,
    /// where the hand-back can.
    fn rd_reconcile_with(&self, group: &GroupId, runner: &dyn rddrive::RdRunner) -> Vec<String> {
        if self.rd_reconciled.lock_safe().contains(group) {
            return Vec::new();
        }
        let dir = self.group_dir(group);
        let mut notices = Vec::new();
        let mut audits: Vec<(String, u64, bool)> = Vec::new();
        {
            let _state_guard = self.rd_state_lock.lock_safe();
            // **The once-only latch is set below, on a reconcile that actually
            // READ the file — not here, on one that merely attempted it.**
            // Latching first is the obvious spelling and it means a torn
            // `review_drives.json` at startup costs this group its reconcile for
            // the life of the process, including after a human fixes the file:
            // the flag says "done" and nothing ever revisits it. §2.4 already
            // makes an unreadable file a loud, rate-bounded "a human has to look
            // at this", and a human who then looks and fixes it should get the
            // reconcile they were owed.
            let Ok(mut state) = reviewdrive::load_state(&dir) else { return Vec::new() };
            self.rd_reconciled.lock_safe().insert(group.clone());
            let mut changed = false;
            let live: Vec<u64> =
                state.entries.iter().filter(|e| !e.state().is_terminal()).map(|e| e.pr).collect();
            for pr in live {
                // `pr_is_open`, not `observe_pr`: reconcile reads nothing but
                // this, and `observe_pr` would spend a second round trip on
                // checks it never looks at, per live entry, at startup.
                let open = rddrive::pr_is_open(runner, pr);
                let Some(entry) = state.entry_mut(pr) else { continue };
                let on_behalf = entry.on_behalf_of.clone();
                if open == Some(false)
                    && entry.advance(reviewdrive::DriveState::Cancelled, None, None, 0).is_ok()
                {
                    changed = true;
                    notices.push(rddrive::cancelled_notice(pr, rddrive::CancelCause::PrGone));
                    audits.push((on_behalf, pr, true));
                } else {
                    audits.push((on_behalf, pr, false));
                }
            }
            if changed {
                let _ = reviewdrive::store_state(&dir, &state);
            }
        }
        for (on_behalf, pr, cancelled) in audits {
            let action = if cancelled {
                rddrive::audit_action::CANCELLED
            } else {
                rddrive::audit_action::RECOVERED
            };
            self.rd_audit(group, &on_behalf, action, json!({ "pr": pr, "at": "reconcile" }));
        }
        notices
    }

    /// Put a `TaskNote` on the board row whose `pr` matches, so a human sees the
    /// drive where they see the work (§3.2).
    ///
    /// **The driver writes to the board and reads nothing from it.** `Task::pr`
    /// is agent-writable, so a driver that took its worker session or its gate
    /// from a row would be letting the thing being checked answer the check.
    /// Matching a row in order to write a note on it is not that: a wrong match
    /// costs a note on the wrong row, never an authorization.
    fn rd_task_note(&self, group: &GroupId, pr: u64, text: &str) {
        let Some(id) = self
            .tasks(group)
            .into_iter()
            .find(|t| t.pr.as_deref().and_then(pr_number) == Some(pr))
            .map(|t| t.id)
        else {
            return;
        };
        let _ = self.upsert_task(
            group,
            brand::AUDIT_ACTOR,
            Some(&id),
            TaskPatch { note: Some(text.to_string()), ..TaskPatch::default() },
        );
    }

    /// One entry, one tick, **at most one advance** (§2.4).
    ///
    /// Runs with `rd_state_lock` held, and that includes the spawn — §2.4 says
    /// so in as many words ("the load-decide-store spans a spawn, and a
    /// `drive_review` landing inside that window would otherwise read the
    /// pre-spawn file and write it back, erasing the entry"). It is safe to span
    /// one here and not a *notice*: `rd_state_lock` is taken by exactly four
    /// call sites — this tick and the three MCP tools — and none of them is
    /// reachable from a pane delivery, so a spawn's own kickoff cannot cycle
    /// back onto it. The orchestrator notices this produces are still delivered
    /// by the caller, outside the lock, for the #467/#468 reason.
    fn rd_step_entry(
        &self,
        group: &GroupId,
        runner: &dyn rddrive::RdRunner,
        state: &mut reviewdrive::ReviewDrivesState,
        pr: u64,
        limits: &reviewdrive::DriveLimits,
        now: u64,
        base_green_memo: &mut std::collections::HashMap<String, Option<bool>>,
    ) -> Option<RdOut> {
        let resting =
            state.entry(pr).map(|e| e.state().is_parked() || e.state().is_terminal())?;
        if resting {
            // §2.1's `held` row: "nothing; the tick does not advance it". Bailing
            // BEFORE the reads rather than after — `decide` would answer `Wait`
            // anyway, but a parked drive can sit here for days, and spending
            // `gh` round-trips per parked entry per tick to be told so is the
            // cost §2.4's one-group bound exists to keep down.
            return None;
        }
        let obs = rddrive::observe_pr(runner, pr);
        // **Only the states that READ these facts pay for them.** `decide` reads
        // `required_lanes` in `review-wait` and `gate-check` and `gate` in
        // `gate-check` alone; `ci-wait` and `fix-wait` read neither. Resolving
        // them unconditionally would spend a `pr view --json files` on every
        // routing gate and a pair of `gh` reads on every `base-green` gate, per
        // entry, per tick, for answers nothing consults — on the loop that also
        // delivers every `notify_when` notice in the fleet (§2.4). The queue's
        // own driver gates the same two reads on the same principle
        // (`declares_base_green`: a value nothing consults is not worth a round
        // trip, and an unfetched value is `None`, which refuses).
        let here = state.entry(pr).map(|e| e.state())?;
        let want_lanes = matches!(
            here,
            reviewdrive::DriveState::ReviewWait | reviewdrive::DriveState::GateCheck
        );
        let want_gate = here == reviewdrive::DriveState::GateCheck;
        let (gate, required, lane_notices) = if want_lanes {
            self.rd_gate_facts(group, runner, pr, &obs, want_gate, base_green_memo)
        } else {
            // `NotEvaluated` is the honest value here and not a stand-in for
            // "satisfied": §2.1's `gate-check` row treats it as "the tick reached
            // this state without evaluating the gate", which is `Wait`. Neither
            // of the two states that land here can read it at all.
            (reviewdrive::GateOutcome::NotEvaluated, None, Vec::new())
        };
        let signal = self.rd_signal(group, pr);
        let messaged_by = signal.messaged_by.clone();
        let facts = reviewdrive::DriveFacts {
            now_ms: now,
            pr_open: obs.open,
            head: obs.head.clone(),
            body_digest: obs.body_digest.clone(),
            required_lanes: required.clone(),
            ci: obs.ci,
            worker: signal.worker,
            gate,
            messaged: signal.messaged,
        };
        let mut out = RdOut::new(pr);
        out.backoff = obs.runner_failed;
        let brief = RdBrief {
            pr,
            head: obs.head.clone(),
            base: obs.base.clone(),
            body_digest: obs.body_digest.clone().unwrap_or_default(),
            ci: obs.ci,
            failing_jobs: obs.failing_jobs.clone(),
            required: required
                .as_ref()
                .map(|r| r.iter().map(|l| l.block.clone()).collect())
                .unwrap_or_default(),
            lane_notices,
            // The index `decide_review_wait` walks to, computed with the same
            // pure function it uses rather than guessed from the verdicts.
            deciding_lane: required.as_deref().and_then(|r| {
                r.get(reviewdrive::first_stale_lane(
                    r,
                    &obs.head,
                    obs.body_digest.as_deref(),
                ))
                .map(|l| l.block.clone())
            }),
        };
        let entry = state.entry_mut(pr)?;
        // What each lane's verdict file said this tick, recorded onto that lane
        // BEFORE the decision — not as an input to it (nothing decides from a
        // recorded verdict; the live file is re-read every tick through the
        // gate's own parser) but because `at_head` is what tells a lane that has
        // ANSWERED from one that has only been ASKED. Without it a re-briefed
        // lane looks like a first-time lane forever and §5.5's delta template is
        // unreachable, which is the defect this line closes.
        for l in &brief.lane_notices {
            if entry.record_verdict_seen(&l.block, l.verdict, &l.at_head) {
                out.changed = true;
            }
        }
        let step = reviewdrive::decide(entry, &facts, limits);
        let on_behalf = entry.on_behalf_of.clone();
        // §2.1's `review-wait` row writes "the current lane index", and the
        // current lane is the DECIDING one — the first whose pass does not stand
        // — whether or not this tick had to open it. Writing it only on a
        // successful spawn leaves it lagging every time the drive waits on a
        // lane that is already open, which is most ticks of most rounds. It is
        // only a display and last-resort-fallback field today, so the lag was
        // not reachable as a defect; a field that is silently wrong is how the
        // next reader is misled.
        if entry.state() == reviewdrive::DriveState::ReviewWait {
            if let Some(k) = brief
                .deciding_lane
                .as_deref()
                .and_then(|b| brief.required.iter().position(|r| r == b))
            {
                if entry.lane_index != k {
                    entry.lane_index = k;
                    out.changed = true;
                }
            }
        }
        match &step {
            reviewdrive::DriveStep::Wait => {}
            reviewdrive::DriveStep::OpenLane { index } => {
                // `decide` only ever names an index into the list it was handed,
                // so `None` is unreachable — and it is handled by falling
                // THROUGH rather than returning, because an early return here
                // would skip the head persistence below, which is the one write
                // this function exists to get right. An unreachable branch that
                // skips a load-bearing write is how the reachable one gets
                // broken later.
                let block = brief.required.get(*index).cloned().unwrap_or_default();
                match self.rd_open_lane(group, entry, &block, &brief, limits, now) {
                    Ok(agent) => {
                        entry.lane_index = *index;
                        out.changed = true;
                        out.lanes_opened.push((block.clone(), agent.clone()));
                        out.audits.push((
                            rddrive::audit_action::LANE_SPAWNED,
                            json!({ "pr": pr, "block": block, "agent": agent,
                                    "head": brief.head, "round": entry.counters.review_rounds + 1 }),
                        ));
                    }
                    Err(why) => {
                        // §8's live-delegate-cap row: a refused spawn is a
                        // runner-class outcome. Back off and retry on a later
                        // tick, counted only against `drive_timeout_minutes` —
                        // and NEVER kill a pane to make room (§3.1 item 5).
                        out.backoff = true;
                        out.audits.push((
                            rddrive::audit_action::REFUSED,
                            json!({ "pr": pr, "block": block, "reason": "lane-spawn-refused",
                                    "detail": why }),
                        ));
                    }
                }
            }
            reviewdrive::DriveStep::Advance { to, held_reason, .. } => {
                // The CI observation that caused this arc is audited BEFORE the
                // arc is taken, so a green and a red are separate actions in the
                // order they were observed (§5.4: a filter looking for the thing
                // that happened must not match the thing that did not).
                if entry.state() == reviewdrive::DriveState::CiWait {
                    match obs.ci {
                        reviewdrive::CiObservation::Green => out.audits.push((
                            rddrive::audit_action::CI_GREEN,
                            json!({ "pr": pr, "head": brief.head }),
                        )),
                        reviewdrive::CiObservation::Red => out.audits.push((
                            rddrive::audit_action::CI_RED,
                            json!({ "pr": pr, "head": brief.head, "failing": brief.failing_jobs }),
                        )),
                        reviewdrive::CiObservation::Conflicting => out.audits.push((
                            rddrive::audit_action::CONFLICTING,
                            json!({ "pr": pr, "base": brief.base }),
                        )),
                        _ => {}
                    }
                }
                if let Err(bad) = entry.take(&step, now) {
                    // Unreachable through `decide`, which only proposes arcs the
                    // table names — and handled rather than unwrapped, because an
                    // unwind out of the shared poll thread would take every watch
                    // in the fleet down with it.
                    out.audits.push((
                        rddrive::audit_action::REFUSED,
                        json!({ "pr": pr, "reason": "invalid-transition", "detail": bad.to_string() }),
                    ));
                    return Some(out);
                }
                out.changed = true;
                out.clear_signal = true;
                out.advanced = Some((*to, *held_reason));
                match to {
                    reviewdrive::DriveState::FixWait => {
                        match self.rd_handback(group, entry, &brief, limits) {
                            Ok(agent) => {
                                out.handback = Some(agent.clone());
                                out.audits.push((
                                    rddrive::audit_action::HANDBACK,
                                    json!({ "pr": pr, "agent": agent, "head": brief.head,
                                            "why": brief.handback_kind() }),
                                ));
                            }
                            Err(why) => {
                                // A worker that will not resume is §2.2's
                                // `worker-unresumable`, learned exactly here —
                                // §5.1 says so, and says the deferral is
                                // deliberate: resolving a session at drive time
                                // is not the same as proving it resumable.
                                out.audits.push((
                                    rddrive::audit_action::REFUSED,
                                    json!({ "pr": pr, "reason": "worker-unresumable",
                                            "detail": why }),
                                ));
                                let _ = entry.advance(
                                    reviewdrive::DriveState::Held,
                                    Some(reviewdrive::HeldReason::WorkerUnresumable),
                                    None,
                                    now,
                                );
                                out.advanced = Some((
                                    reviewdrive::DriveState::Held,
                                    Some(reviewdrive::HeldReason::WorkerUnresumable),
                                ));
                            }
                        }
                    }
                    reviewdrive::DriveState::GateCheck | reviewdrive::DriveState::Satisfied => {
                        for l in &brief.lane_notices {
                            out.audits.push((
                                rddrive::audit_action::VERDICT,
                                json!({ "pr": pr, "block": l.block,
                                        "verdict": l.verdict.as_str(), "head": brief.head }),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        // **THE HEAD, PERSISTED — the line two reviewers named on S1 as the one
        // that would be forgotten.** `DriveEntry::head` is only ever *compared*
        // against the live head (arc 6 in `review-wait`, arc 7 in `fix-wait`), so
        // a tick that records it once at `drive_review` time and never again
        // makes that comparison permanently true: the drive takes arc 6 to
        // `ci-wait`, goes green, comes back to `review-wait`, takes arc 6 again,
        // forever — a PR that is never reviewed and never gated.
        //
        // **After `decide`, never before.** Arc 6 *is* `entry.head !=
        // facts.head`; writing first would make the two equal before anything
        // compared them, and the arc would be unreachable rather than permanent.
        //
        // **And only when the head actually resolved.** An empty `facts.head` is
        // a FAILED READ, not a head — the same class as `fix_handback_ms == 0`
        // meaning "ancient" rather than "unset". Writing it would leave every
        // later tick comparing a real live head against a stored `""`, taking
        // arc 6 on every wake; and in `review-wait` a stored `""` makes
        // `lane_open_for` refuse every record briefed at a real head, which is
        // `OpenLane{k}` on every tick — a reviewer spawned per tick, each brief
        // re-arming `spawned_ms` so `lane-stalled` can never fire. `decide`
        // refuses to dispatch at all on an empty LIVE head, which bounds the
        // damage this tick; this is what stops the ENTRY being poisoned so the
        // next read, successful or not, still misbehaves.
        if !obs.head.is_empty() && entry.head != obs.head {
            entry.head = obs.head.clone();
            out.changed = true;
        }
        if let Some(d) = obs.body_digest.as_deref() {
            if entry.body_digest != d {
                entry.body_digest = d.to_string();
                out.changed = true;
            }
        }
        // The exits (§2.2). Built here, where the entry's counters and lanes are
        // in hand; delivered by the caller, outside the lock.
        if let Some((to, reason)) = out.advanced {
            match (to, reason) {
                (reviewdrive::DriveState::Satisfied, _) => {
                    let n = rddrive::satisfied_notice(
                        pr,
                        &entry.head,
                        &entry.body_digest,
                        &brief.lane_notices,
                        &entry.counters,
                    );
                    out.audits.push((
                        rddrive::audit_action::SATISFIED,
                        json!({ "pr": pr, "head": entry.head }),
                    ));
                    out.notices.push(n);
                }
                (reviewdrive::DriveState::Held, Some(r)) => {
                    let n =
                        rddrive::held_notice(pr, r, &brief.held_facts(entry, limits, &messaged_by));
                    out.audits.push((
                        rddrive::audit_action::HELD,
                        json!({ "pr": pr, "reason": r.as_str(), "head": entry.head }),
                    ));
                    out.notices.push(n);
                }
                (reviewdrive::DriveState::Cancelled, _) => {
                    out.audits.push((rddrive::audit_action::CANCELLED, json!({ "pr": pr })));
                    out.notices
                        .push(rddrive::cancelled_notice(pr, rddrive::CancelCause::PrGone));
                }
                _ => {}
            }
        }
        out.on_behalf_of = on_behalf;
        Some(out)
    }

    // ---------- §5.1's three MCP tools ----------

    /// `drive_review(pr, worker_session, reset_counters?, rounds_already_spent?)`
    /// — §3.2's second key, and the only thing that starts a drive.
    ///
    /// **Never automatic**, and in particular it does not fire on a worker's
    /// `report(done)`: INVARIANT 8 makes *what starts* the orchestrator's call,
    /// and the PRs where a drive is wrong are ordinary — a scratch or
    /// red-evidence PR, a release bump, a PR the human said they would read
    /// themselves.
    ///
    /// **The session is resolved once, and what is persisted is what came
    /// back** (§3.2). `resolve_session_ref` is a resolution against *this
    /// group's roster at the moment of the call*: a prefix that resolves
    /// uniquely today can become ambiguous tomorrow as the roster grows, and the
    /// entry outlives both the call and the process. So the **resolved** id goes
    /// into `review_drives.json`, never the caller's raw string.
    ///
    /// Refusals are §5.1's closed vocabulary, in two classes: the driver
    /// declining, and orrerix having failed. The order they are checked in is
    /// cheapest-first among equals, with one exception that is not: the PR's
    /// state is read **last** among the checks, because it is the only one that
    /// spends a `gh` round trip.
    #[doc(hidden)] // pub for integration tests
    pub fn drive_review(
        &self,
        group: &GroupId,
        pr: u64,
        worker_session: &str,
        reset_counters: bool,
        rounds_already_spent: u32,
        on_behalf_of: &str,
    ) -> Value {
        let injected = self.rd_runner_override.lock_safe().clone();
        let owned;
        let runner: &dyn rddrive::RdRunner = match injected.as_deref() {
            Some(r) => r,
            None => {
                let Some(repo) = self.group(group).map(|g| g.repo) else {
                    return self.rd_refuse(group, pr, rddrive::refusal::UNAVAILABLE);
                };
                owned = rddrive::runner_for(std::path::Path::new(&repo));
                &owned
            }
        };
        self.drive_review_with(
            group,
            runner,
            pr,
            worker_session,
            reset_counters,
            rounds_already_spent,
            on_behalf_of,
        )
    }

    /// [`drive_review`](Self::drive_review) with the `gh` seam injected.
    #[doc(hidden)] // pub for integration tests
    pub fn drive_review_with(
        &self,
        group: &GroupId,
        runner: &dyn rddrive::RdRunner,
        pr: u64,
        worker_session: &str,
        reset_counters: bool,
        rounds_already_spent: u32,
        on_behalf_of: &str,
    ) -> Value {
        use rddrive::refusal as r;
        if !self.driver_enabled(group) {
            return self.rd_refuse(group, pr, r::DRIVER_DISABLED);
        }
        // The session, resolved once (§3.2). An empty string gets its own name
        // rather than leaking `resolve_session_ref`'s untagged prose.
        if worker_session.trim().is_empty() {
            return self.rd_refuse(group, pr, r::RESUME_SESSION_EMPTY);
        }
        let session = match resolve_session_ref(&self.merged_records(group), worker_session) {
            Ok(s) => s,
            Err(e) if e.starts_with("resume-ambiguous") => {
                return self.rd_refuse(group, pr, r::RESUME_AMBIGUOUS)
            }
            Err(_) => return self.rd_refuse(group, pr, r::RESUME_NOT_FOUND),
        };
        // The gate, from the same two files the shim reads. `gate-unreadable` is
        // NOT `gate-not-configured`: a wrong label sends the reader somewhere
        // else, which is #681's own lesson and the queue's posture.
        let spec = match self.merge_queue_gate(group) {
            Ok(s) => s,
            Err(_) => return self.rd_refuse(group, pr, r::GATE_UNREADABLE),
        };
        let gate = match &spec {
            mergeq::GateSpec::Declared(g) => g.clone(),
            mergeq::GateSpec::Malformed => return self.rd_refuse(group, pr, r::GATE_UNREADABLE),
            mergeq::GateSpec::Absent => {
                return self.rd_refuse(group, pr, r::GATE_NOT_CONFIGURED)
            }
        };
        // A gate requiring a reviewer the roster does not declare is answerable
        // here, from two files. Left unanswered it becomes `held(lane-stalled)`
        // an hour later instead of an immediate refusal.
        if let Some(g) = self.group(group) {
            if !workflow::gate_missing_blocks(&gate, &g.guardrails.blocks).is_empty() {
                return self.rd_refuse(group, pr, r::GATE_NAMES_NO_SUCH_BLOCK);
            }
        }
        // §8.1's mutual refusal: the two loops both move a PR's head and both
        // read its verdicts, and neither was designed expecting the other to be
        // doing so concurrently. The intended sequence is serial and has a
        // direction — a drive ends at `satisfied`, the orchestrator dispositions
        // the findings, and *then* it queues.
        if let Ok(q) = mqloop::load_state(&self.group_dir(group)) {
            if q.entry(pr).map(|e| !e.state().is_terminal()).unwrap_or(false) {
                return self.rd_refuse(group, pr, r::IN_MERGE_QUEUE);
            }
        }
        // **`already-driven`, checked once cheaply BEFORE the `gh` call.** A
        // second `drive_review` on a live PR is the ordinary duplicate — an
        // orchestrator retrying, or re-reading its own state after a compact —
        // and spending a `gh` round trip to answer it is a round trip on the
        // loop that also delivers every `notify_when` notice in the fleet. The
        // AUTHORITATIVE check is still the one under the lock below: this read
        // is unsynchronized, so it can only ever be stale in the direction of
        // doing more work, never of starting a second drive.
        if reviewdrive::load_state(&self.group_dir(group)).map(|s| s.is_driven(pr)).unwrap_or(false)
        {
            return self.rd_refuse(group, pr, r::ALREADY_DRIVEN);
        }
        // Last, because it is the only check that spends a `gh` round trip — and
        // it spends exactly ONE: `drive_review` reads whether the PR is open and
        // nothing else, so `observe_pr`'s second call on `gh pr checks` would be
        // an answer this path never looks at.
        match rddrive::pr_is_open(runner, pr) {
            Some(true) => {}
            Some(false) => return self.rd_refuse(group, pr, r::PR_NOT_OPEN),
            // The remote did not answer. Unknown is never treated as safe, and
            // it is never treated as a fact about the PR either.
            None => return self.rd_refuse(group, pr, r::PR_UNVERIFIABLE),
        }
        let dir = self.group_dir(group);
        let (audit_action, detail) = {
            let _state_guard = self.rd_state_lock.lock_safe();
            let mut state = match reviewdrive::load_state(&dir) {
                Ok(s) => s,
                // NOT `not-driven`: that would assert something orrerix cannot
                // know, and `already-driven` is unevaluable here, so an unnamed
                // failure becomes a second drive on one PR.
                Err(_) => return self.rd_refuse(group, pr, r::STATE_UNREADABLE),
            };
            if state.is_driven(pr) {
                return self.rd_refuse(group, pr, r::ALREADY_DRIVEN);
            }
            let resumed = match state.entry(pr).map(|e| e.state()) {
                // A parked drive RESUMES, carrying its counters — §2.3's
                // default, and the whole reason §2.1 makes `held` parked rather
                // than terminal. Clearing them is an explicit, audited argument.
                Some(reviewdrive::DriveState::Held) => true,
                _ => false,
            };
            if resumed {
                let entry = match state.entry_mut(pr) {
                    Some(e) => e,
                    None => return self.rd_refuse(group, pr, r::STATE_UNREADABLE),
                };
                if reset_counters {
                    entry.counters = reviewdrive::Counters::seeded(rounds_already_spent);
                }
                if entry
                    .advance(reviewdrive::DriveState::CiWait, None, None, now_ms())
                    .is_err()
                {
                    return self.rd_refuse(group, pr, r::STATE_UNREADABLE);
                }
                // **A new session means the recorded PANE is stale**, and a
                // stale pane is not merely useless — it is an interception key.
                // `driven_role` matches on `worker_agent`, so leaving the old
                // one would have this drive consume the traffic of a worker it
                // no longer owns, while the worker it DOES own reports to the
                // orchestrator as if undriven. Cleared on a change, kept when
                // the orchestrator resumes with the same session (the common
                // case), where the pane is still the right one.
                if entry.worker_session != session {
                    entry.worker_agent = String::new();
                }
                entry.worker_session = session.clone();
                entry.on_behalf_of = on_behalf_of.to_string();
            } else {
                // A `satisfied` or `cancelled` entry that retention has not yet
                // pruned starts a FRESH drive with fresh counters — the queue's
                // own "comes back as a NEW entry" behaviour.
                state.entries.retain(|e| e.pr != pr);
                state.entries.push(reviewdrive::DriveEntry::new(
                    pr,
                    &session,
                    on_behalf_of,
                    reviewdrive::Counters::seeded(rounds_already_spent),
                    now_ms(),
                ));
            }
            if reviewdrive::store_state(&dir, &state).is_err() {
                return self.rd_refuse(group, pr, r::STATE_UNWRITABLE);
            }
            if resumed {
                (
                    rddrive::audit_action::RESUMED,
                    json!({ "pr": pr, "reset_counters": reset_counters,
                            "rounds_already_spent": rounds_already_spent }),
                )
            } else {
                (
                    rddrive::audit_action::STARTED,
                    json!({ "pr": pr, "rounds_already_spent": rounds_already_spent }),
                )
            }
        };
        // A resume that carried a stale signal would re-hold on the reason it
        // was resumed out of — `messaged` most obviously.
        self.rd_signals.lock_safe().remove(&(group.clone(), pr));
        self.rd_audit(group, on_behalf_of, audit_action, detail);
        // Service this group on the very next wake rather than after a backoff
        // window that predates the drive.
        self.rd_service_ms.lock_safe().remove(group);
        json!({ "driving": true, "state": reviewdrive::DriveState::CiWait.as_str() })
    }

    /// `cancel_review_drive(pr)` — one of `held`'s two outgoing arcs, and the
    /// only way a live drive stops without reaching `satisfied`.
    #[doc(hidden)] // pub for integration tests
    pub fn cancel_review_drive(&self, group: &GroupId, pr: u64, on_behalf_of: &str) -> Value {
        use rddrive::refusal as r;
        if !self.driver_enabled(group) {
            return self.rd_refuse(group, pr, r::DRIVER_DISABLED);
        }
        let dir = self.group_dir(group);
        {
            let _state_guard = self.rd_state_lock.lock_safe();
            let mut state = match reviewdrive::load_state(&dir) {
                Ok(s) => s,
                // **NOT `not-driven`.** A torn file cannot tell you a PR is not
                // driven; it can only tell you orrerix cannot say. §5.1 gives
                // this its own name for exactly the confusion the queue's own
                // contract uses capitals to prevent.
                Err(_) => return self.rd_refuse(group, pr, r::STATE_UNREADABLE),
            };
            let live = state.entry(pr).map(|e| !e.state().is_terminal()).unwrap_or(false);
            if !live {
                return self.rd_refuse(group, pr, r::NOT_DRIVEN);
            }
            let Some(entry) = state.entry_mut(pr) else {
                return self.rd_refuse(group, pr, r::NOT_DRIVEN);
            };
            if entry
                .advance(reviewdrive::DriveState::Cancelled, None, None, now_ms())
                .is_err()
            {
                return self.rd_refuse(group, pr, r::STATE_UNREADABLE);
            }
            if reviewdrive::store_state(&dir, &state).is_err() {
                return self.rd_refuse(group, pr, r::STATE_UNWRITABLE);
            }
        }
        self.rd_signals.lock_safe().remove(&(group.clone(), pr));
        self.rd_audit(group, on_behalf_of, rddrive::audit_action::CANCELLED, json!({ "pr": pr }));
        let notice = rddrive::cancelled_notice(pr, rddrive::CancelCause::Tool);
        let _ = self.deliver_to_orchestrator(group, &notice, brand::AUDIT_ACTOR);
        self.rd_task_note(group, pr, &notice);
        json!({ "cancelled": true })
    }

    /// `review_drive_status()` — the surface a **compacted** orchestrator
    /// recovers its drives from, which is why §5.1 puts it in the re-sync list
    /// beside `list_tasks`, `list_agents` and `get_state`.
    ///
    /// **It does not list terminal entries**, exactly as `merge_queue_status`
    /// does not: they would flow into the orchestrator's resident context, which
    /// is the cost this whole feature exists to remove. Parked entries ARE
    /// listed — a `held` drive is the one thing an orchestrator most needs to
    /// see, and §5.2 never prunes one.
    #[doc(hidden)] // pub for integration tests
    pub fn review_drive_status(&self, group: &GroupId) -> Value {
        let enabled = self.driver_enabled(group);
        let dir = self.group_dir(group);
        let state = {
            let _state_guard = self.rd_state_lock.lock_safe();
            match reviewdrive::load_state(&dir) {
                Ok(s) => s,
                // The same distinction the two mutating tools make: "orrerix
                // cannot read the record" is not "there is nothing in it".
                Err(_) => {
                    return json!({ "enabled": enabled,
                                   "refused": rddrive::refusal::STATE_UNREADABLE })
                }
            }
        };
        let now = now_ms();
        let drives: Vec<Value> = state
            .entries
            .iter()
            .filter(|e| !e.state().is_terminal())
            .map(|e| {
                json!({
                    "pr": e.pr,
                    "state": e.state().as_str(),
                    "held_reason": e.held_reason.map(|r| r.as_str()),
                    "head": e.head,
                    "lanes": e.lanes.iter().map(|l| json!({
                        "block": l.block,
                        "last_verdict": l.last_verdict.map(|v| v.as_str()),
                    })).collect::<Vec<_>>(),
                    "counters": {
                        "review_rounds": e.counters.review_rounds,
                        "ci_attempts": e.counters.ci_attempts,
                        "rebase_attempts": e.counters.rebase_attempts,
                    },
                    // Derived, never stored: a stored AGE is stale the instant
                    // it is written and meaningless across a restart, which is
                    // the queue's own split between `enqueued_ms` and
                    // `status_view`'s `since_ms`.
                    "since_ms": e.age_ms(now),
                })
            })
            .collect();
        json!({ "enabled": enabled, "drives": drives })
    }

    /// One refusal, audited then returned — so `rd-refused` and what the caller
    /// was told cannot come apart.
    fn rd_refuse(&self, group: &GroupId, pr: u64, reason: &'static str) -> Value {
        self.rd_audit(
            group,
            "",
            rddrive::audit_action::REFUSED,
            json!({ "pr": pr, "reason": reason,
                    "orrerix_fault": rddrive::refusal::is_orrerix_fault(reason) }),
        );
        json!({ "refused": reason })
    }

}
