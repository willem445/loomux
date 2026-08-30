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
    brand, mergeq, notify, pr_number, rddrive, render_template, reviewdrive,
    resolve_worker_resume_cwd, workflow, AgentEntry, GroupId, LockExt, OrchRegistry, Role,
    TaskPatch,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RdEvent {
    /// `report(done)` from the driven worker.
    WorkerDone,
    /// `report(blocked)` from the driven worker.
    WorkerBlocked,
    /// `message_orchestrator` from any driven delegate. **Never intercepted** —
    /// the delegate's own line is delivered unchanged by its own arm; this is
    /// only the routing fact beside it (§7).
    Messaged,
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
}

impl Default for RdSignal {
    fn default() -> RdSignal {
        RdSignal { worker: reviewdrive::WorkerSignal::Silent, messaged: false }
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
    fn held_facts(
        &self,
        entry: &reviewdrive::DriveEntry,
        limits: &reviewdrive::DriveLimits,
    ) -> rddrive::HeldFacts {
        let speaking = self.speaking_lane();
        let lane = speaking
            .map(|l| l.block.clone())
            .unwrap_or_else(|| self.required.get(entry.lane_index).cloned().unwrap_or_default());
        rddrive::HeldFacts {
            head: entry.head.clone(),
            worker_session: entry.worker_session.clone(),
            lane_agent: entry.lane(&lane).map(|l| l.agent.clone()).unwrap_or_default(),
            lane_summary: speaking.map(|l| l.summary.clone()).unwrap_or_default(),
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
    lanes_opened: Vec<String>,
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
    /// `(pr, block)` for every lane briefed this tick.
    pub lanes_opened: Vec<(u64, String)>,
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
    /// **SCAFFOLD until this branch rebases onto S2 (#1784)**, and removed
    /// inside this PR rather than left standing. That PR adds
    /// `RawWorkflow::driver` and `workflow::DriverPolicy`, which do not exist on
    /// `main`, so the read below is the default and the only way to enable a
    /// drive today is `rd_policy_override`. The rebase commit replaces this body
    /// with the `wf.driver` read, deletes the override, and rewrites the tests
    /// that use it against a real `driver:` block — so no scaffold and no seam
    /// survives into the ready PR.
    fn driver_policy(&self, group: &GroupId) -> (bool, reviewdrive::DriveLimits) {
        if let Some(p) = self.rd_policy_override.lock_safe().clone() {
            return p;
        }
        let off = (false, reviewdrive::DriveLimits::default());
        let Some(g) = self.group(group) else { return off };
        if !g.guardrails.advanced_orchestrator {
            return off;
        }
        match workflow::load_workflow(&g.repo) {
            Ok(Some(_wf)) => off,
            _ => off,
        }
    }

    /// Whether this group runs a review driver at all.
    fn driver_enabled(&self, group: &GroupId) -> bool {
        self.driver_policy(group).0
    }

    /// SCAFFOLD test seam — see [`driver_policy`](Self::driver_policy).
    #[doc(hidden)] // pub for integration tests
    pub fn set_rd_policy_override(&self, p: Option<(bool, reviewdrive::DriveLimits)>) {
        *self.rd_policy_override.lock_safe() = p;
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
            RdEvent::Messaged => sig.messaged = true,
            RdEvent::Verdict => {}
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
            let prs: Vec<u64> = state.entries.iter().map(|e| e.pr).collect();
            for pr in prs {
                if let Some(o) = self.rd_step_entry(group, runner, &mut state, pr, &limits, now) {
                    outs.push(o);
                }
            }
            if outs.iter().any(|o| o.changed) {
                if let Err(e) = reviewdrive::store_state(&dir, &state) {
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
            for b in &o.lanes_opened {
                report.lanes_opened.push((o.pr, b.clone()));
            }
            if let Some((to, _)) = o.advanced {
                report.advanced.push((o.pr, to));
            }
            for n in &o.notices {
                let _ = self.deliver_to_orchestrator(group, n, brand::AUDIT_ACTOR);
                self.rd_task_note(group, o.pr, n);
                report.notices.push(n.clone());
            }
            if o.clear_signal {
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
        let observed = rddrive::gate_observation(runner, pr, obs, &spec, &obs.base);
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
                })
            })
            .collect();
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
        self.spawn_agent_bound(
            group, role, block, "", task, false, None, None, resume, cwd, None, None,
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
                    rd_fact(&brief.failing_lane())
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
        if !self.rd_reconciled.lock_safe().insert(group.clone()) {
            return Vec::new();
        }
        let dir = self.group_dir(group);
        let mut notices = Vec::new();
        let mut audits: Vec<(String, u64, bool)> = Vec::new();
        {
            let _state_guard = self.rd_state_lock.lock_safe();
            let Ok(mut state) = reviewdrive::load_state(&dir) else { return Vec::new() };
            let mut changed = false;
            let live: Vec<u64> =
                state.entries.iter().filter(|e| !e.state().is_terminal()).map(|e| e.pr).collect();
            for pr in live {
                let obs = rddrive::observe_pr(runner, pr);
                let Some(entry) = state.entry_mut(pr) else { continue };
                let on_behalf = entry.on_behalf_of.clone();
                if obs.open == Some(false)
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
        let (gate, required, lane_notices) = self.rd_gate_facts(group, runner, pr, &obs);
        let signal = self.rd_signal(group, pr);
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
        };
        let entry = state.entry_mut(pr)?;
        let step = reviewdrive::decide(entry, &facts, limits);
        let on_behalf = entry.on_behalf_of.clone();
        match &step {
            reviewdrive::DriveStep::Wait => {}
            reviewdrive::DriveStep::OpenLane { index } => {
                let Some(block) = brief.required.get(*index).cloned() else { return Some(out) };
                match self.rd_open_lane(group, entry, &block, &brief, limits, now) {
                    Ok(agent) => {
                        entry.lane_index = *index;
                        out.changed = true;
                        out.lanes_opened.push(block.clone());
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
                            Ok(agent) => out.audits.push((
                                rddrive::audit_action::HANDBACK,
                                json!({ "pr": pr, "agent": agent, "head": brief.head,
                                        "why": brief.handback_kind() }),
                            )),
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
                    let n = rddrive::held_notice(pr, r, &brief.held_facts(entry, limits));
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
}
