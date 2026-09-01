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
    resolve_session_ref, resolve_worker_resume_cwd, reviewdrive, tail_snippet, workflow,
    AgentEntry, AgentStatus, Delivery, GroupId, LockExt, OrchRegistry, Role, TaskPatch,
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
    /// `report(progress)` from the driven worker's CURRENT pane (#1959).
    ///
    /// **It is not a drive signal and must never become one.** A drive advances
    /// on the head, the checks and the verdict files, and a worker saying it is
    /// still going is none of those — reading it as "the fix is in" would brief a
    /// reviewer over unfinished work. What it IS is evidence the worker thinks
    /// it has finished and has said so in the wrong word, which is what the
    /// dogfood measured: a body-only fix (nothing to push, no new checks) whose
    /// worker read "push, and report when the checks are green" literally and
    /// picked `progress`. The drive consumed it and did nothing for ten minutes,
    /// until the idle watchdog woke the ORCHESTRATOR — the turn the driver exists
    /// to remove. So the tick answers it in the worker's own pane instead, once
    /// per hand-back, and the drive does not move.
    WorkerProgress,
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
    /// The driven worker's current pane called `report(progress)` (#1959).
    ///
    /// Deliberately NOT folded into `worker`: [`reviewdrive::WorkerSignal`] is
    /// what `decide` turns on, and this must not be able to move a drive. It is
    /// read by the tick alone, to answer the worker in its own pane.
    pub worker_progress: bool,
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
            worker_progress: false,
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
        refusal: &str,
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
            refusal: refusal.to_string(),
            panes: entry.owned_panes(),
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
    /// `(agent, text)` for a kick-back this tick owes the WORKER's own pane
    /// (#1959). Carried out of the step like [`RdOut::notices`] and for the same
    /// reason: a delivery is an enqueue and an enqueue re-enters registry locks,
    /// so it must happen with `rd_state_lock` released.
    kickback: Option<(String, String)>,
    audits: Vec<(&'static str, Value)>,
    notices: Vec<String>,
    /// What refused, when this tick's hold is about a refusal (#1961) — the
    /// spawn error, or the line the resumed pane exited on. Empty otherwise,
    /// which renders as no clause; see [`rddrive::HeldFacts::refusal`].
    refusal: String,
}

impl RdOut {
    fn new(pr: u64) -> RdOut {
        RdOut { pr, ..RdOut::default() }
    }
}

/// What one pass of the owed-notice flush did (#1857) — see
/// [`OrchRegistry::rd_flush_notices`].
#[derive(Debug, Default)]
struct RdFlush {
    /// Owed notices that reached the orchestrator's pane this pass.
    notices: Vec<String>,
    /// PRs whose notice is still owed when the pass ends.
    undelivered: Vec<u64>,
    /// Entries §5.2's retention dropped.
    pruned: Vec<u64>,
    /// `(pr, notice)` for the entries dropped at the retention **ceiling** with
    /// the notice never delivered. The text is here so the caller can put it on
    /// the audit log, which is then the only record of it.
    dropped: Vec<(u64, String)>,
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
    /// `(pr, agent)` for every kick-back this tick typed into a WORKER's own
    /// pane (#1959) — the answer to a `report(progress)` in `fix-wait`.
    ///
    /// **Attempted, not delivered**, for [`notices`](RdDriveReport::notices)'
    /// reason and with the same honesty: the entry is marked before the
    /// delivery, so a pane that could not be reached costs the line rather than
    /// re-emitting on every later tick.
    pub kickbacks: Vec<(u64, String)>,
    /// The kick-back notices this tick **produced and attempted** — a hold's,
    /// and every terminal exit's.
    ///
    /// **Attempted, not delivered**, and the wording is the correction rather
    /// than a hedge: this field's doc used to say "delivered to the
    /// orchestrator" while being pushed regardless of what
    /// `deliver_to_orchestrator` answered, which was a claim the code did not
    /// make (#1857). Whether a terminal exit's notice actually landed is
    /// [`notice_undelivered`](RdDriveReport::notice_undelivered)'s question, and
    /// it is the one with a consequence — a notice still owed keeps its entry.
    ///
    /// It stays "attempted" rather than being narrowed to `Ok` deliveries
    /// because the two are not distinguishable from a caller's side for a HOLD,
    /// whose notice is not owed on the entry and has no retry: reporting only
    /// the ones that landed would silently drop a hold's notice out of this
    /// field with nowhere else for it to appear.
    pub notices: Vec<String>,
    /// PRs whose terminal notice is **still owed** at the end of this tick — the
    /// delivery was attempted and failed, so the entry is retained and a later
    /// tick tries again (#1857).
    pub notice_undelivered: Vec<u64>,
    /// Terminal entries dropped after their notices went out (§5.2), plus the
    /// ones given up on at the retention ceiling — which are audited
    /// `rd-notice-dropped` beside their `rd-pruned`.
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
            // Its own field, never `sig.worker` — see [`RdEvent::WorkerProgress`].
            RdEvent::WorkerProgress => sig.worker_progress = true,
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
    /// (`LaneRecord::agent`, `DriveEntry::worker_agent`, and the superseded
    /// panes beside each), and the caller's id comes from its MCP token rather
    /// than from `args`. So a delegate cannot choose whether its report reaches
    /// the orchestrator by naming a PR number, and cannot name someone else's to
    /// redirect theirs — which is the property §7 spends a paragraph on, because
    /// a delegate that could do either is a delegate that can route around the
    /// orchestrator.
    ///
    /// **Every pane the drive opened, not only the latest** (#1871 B2), and the
    /// answer says which: [`reviewdrive::DrivenPane::current`] is what keeps
    /// "this pane is mine" from being read as "take its word". See that type.
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
    ) -> Option<(u64, reviewdrive::DrivenPane)> {
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
    /// `event` is `None` for traffic that is consumed but carries no signal at
    /// all — a LANE's `report`, whose word to the drive is its verdict file, and
    /// any report from a superseded pane.
    ///
    /// A current WORKER's `report(progress)` used to be in that set and is not
    /// any more (#1959): it still moves nothing — a drive advances on the head,
    /// the checks and the verdict files, not on a delegate saying it is still
    /// going — but it arrives as [`RdEvent::WorkerProgress`], on a field
    /// `reviewdrive::decide` cannot read, so the tick can answer it in the
    /// worker's own pane instead of waiting out `fix-stalled`.
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
    /// locks. Both hold here, on a property of the LOCK rather than a count of
    /// its callers: no site that takes `rd_state_lock` is reachable from a pane
    /// delivery, so a spawn's own kickoff cannot cycle back onto it — and the
    /// orchestrator notices this produces are delivered below, outside it.
    ///
    /// The sites today are the tick (here and at the prune), the restart
    /// reconcile, the three MCP tools of §5.1, and the two interception helpers
    /// `rd_owner` / `rd_consume` — eight acquisitions across seven functions.
    /// The last two are the ones worth naming: they run on a delegate's own tool
    /// call, which is a later turn scheduled by the runtime, never a frame the
    /// delivery itself pushes, and both drop the lock before auditing. A ninth
    /// caller owes this argument again rather than inheriting it.
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
        // Reconcile owes its cancellations onto their entries; the flush below
        // is what delivers them (#1857).
        self.rd_reconcile_with(group, runner, now);
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
            // #1959: into the WORKER's pane, not the orchestrator's — the whole
            // point is that this costs the orchestrator no turn. Outside the
            // state lock, like the notices below: a delivery is an enqueue and
            // an enqueue re-enters registry locks.
            if let Some((agent, text)) = &o.kickback {
                let _ = self.deliver_prompt(agent, text, brand::AUDIT_ACTOR, Delivery::MidSession);
                report.kickbacks.push((o.pr, agent.clone()));
            }
            if let Some((to, _)) = o.advanced {
                report.advanced.push((o.pr, to));
            }
            // A HOLD's notice, delivered directly. A failure here still loses
            // the line, and that is argued where the split is made — in
            // `rd_step_entry`, at the exits: a parked entry is never pruned, so
            // the drive survives and `review_drive_status()` still carries it. A
            // terminal exit's notice does not come through here at all; it is
            // owed on the entry and delivered by the flush below (#1857).
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
        // **§5.2's ordering rule, implemented** (#1857): every owed notice is
        // attempted, and only then does retention run — over a `prune_terminal`
        // that will not drop an entry still owing one.
        //
        // This is a SEPARATE PASS rather than a relaxation of `rd_step_entry`'s
        // `is_parked() || is_terminal()` early return, and the choice is
        // deliberate. That early return is a cost bound §2.4 wants: it declines
        // a resting entry *before* any `gh` read, and a terminal entry admitted
        // into the step path would have to be threaded past `observe_pr`,
        // `rd_gate_facts` and `decide` — none of which has anything to say about
        // a drive that is over — to arrive at a branch that only re-sends a
        // string. A pass that walks the file and delivers what it owes costs no
        // round trip at all, has one condition, and cannot advance anything.
        //
        // It also covers the two producers the step path never sees: reconcile's
        // startup cancellations, and `cancel_review_drive`'s, both of which owe
        // onto the entry for exactly this to pick up.
        //
        // A failed `store_state` above left `persisted == false`, and the flush
        // re-reads from DISK — so an arc that did not reach the file owes
        // nothing here either. That is the same rule the signal clearing uses: a
        // transition the next restart forgets is not a transition, and it must
        // not announce itself as one.
        let flush = self.rd_flush_notices(group, &dir, now);
        report.notices.extend(flush.notices);
        report.notice_undelivered = flush.undelivered;
        report.pruned = flush.pruned;
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

    /// **Deliver every owed notice, then run §5.2's retention over what is
    /// left** (#1857) — the pass that makes "a terminal entry is pruned once its
    /// notice has been delivered" a fact about the code rather than a sentence
    /// in a design note.
    ///
    /// # The three phases, and why they are three
    ///
    /// **Read under the lock, attempt with NO lock held, record under the lock
    /// again.** The middle phase is the constraint: #467/#468 make a delivery an
    /// enqueue and an enqueue re-enters registry locks, so holding
    /// `rd_state_lock` across one is the deadlock `rd_drive_group_with`'s own
    /// doc spends a paragraph keeping out. Splitting the file access in two is
    /// the cost of that, and it is safe for the same reason the tick's own
    /// split is: the second phase re-reads, so an entry a concurrent
    /// `drive_review` or `cancel_review_drive` changed in between is read as it
    /// now is, and `entry_mut` simply finds nothing for one that has gone.
    ///
    /// # What a failure at each step does
    ///
    /// A torn file returns empty — the tick above has already audited and backed
    /// off, and refusing to act on a record orrerix cannot read is §2.4's rule,
    /// not a special case. A failed `store_state` returns without reporting
    /// anything pruned, because nothing WAS pruned: the write covers the
    /// delivery marks and the retention together, so the file is exactly as it
    /// was and the next tick re-attempts. The worst case there is a duplicate
    /// line in the pane, which is the direction to fail in — #1857 is about the
    /// other one.
    fn rd_flush_notices(&self, group: &GroupId, dir: &std::path::Path, now: u64) -> RdFlush {
        // Phase 1 — what is owed, and whether this is its first attempt.
        let owed: Vec<(u64, String, bool)> = {
            let _state_guard = self.rd_state_lock.lock_safe();
            match reviewdrive::load_state(dir) {
                Ok(s) => s
                    .entries
                    .iter()
                    .filter_map(|e| e.owed_notice().map(|n| (e.pr, n.text.clone(), n.failures == 0)))
                    .collect(),
                Err(_) => return RdFlush::default(),
            }
        };
        if owed.is_empty() {
            // Nothing owed — but retention still has to run, or a delivered
            // entry never leaves the file.
            let r = self.rd_retain(dir, now, &[], &[]);
            return self.rd_audit_retention(group, r);
        }
        // Phase 2 — attempt each, outside the lock.
        let mut out = RdFlush::default();
        let (mut ok, mut failed) = (Vec::new(), Vec::new());
        for (pr, text, first_attempt) in owed {
            if first_attempt {
                // The board row's note is a SECOND record, and it is written on
                // the first attempt rather than on a successful one: the notice
                // whose row most needs to carry it is precisely the one that
                // never reached a pane. Written once, so a retry does not
                // rewrite the row on every tick.
                self.rd_task_note(group, pr, &text);
            }
            // **`Ok` is exactly "the notice is on the pane's queue and a drainer
            // will paste it", and `Err` is exactly "it is not".** Every refusal
            // in `deliver_prompt_as` either answers before the admission
            // (unknown/dead agent, no terminal, a manager pane, a full queue) or
            // WITHDRAWS the admission it made (`withdraw_unprocessable`, for a
            // missing app handle) — so an `Err` never leaves a payload queued,
            // and re-sending on the next tick cannot duplicate a line that is
            // already going to arrive.
            let landed = self.deliver_to_orchestrator(group, &text, brand::AUDIT_ACTOR).is_ok();
            out.notices.push(text);
            if landed {
                ok.push(pr);
            } else {
                failed.push(pr);
            }
        }
        // Phase 3 — record the outcome and prune, in one write.
        let retained = self.rd_audit_retention(group, self.rd_retain(dir, now, &ok, &failed));
        out.undelivered = retained.undelivered;
        out.pruned = retained.pruned;
        out.dropped = retained.dropped;
        out
    }

    /// The audit lines retention owes, emitted **inside the flush** so every
    /// caller of it gets them — the tick, and `cancel_review_drive`, which runs
    /// the same flush rather than a second delivery path of its own (#1857).
    ///
    /// A drop at the ceiling is audited FIRST and separately from the `rd-pruned`
    /// beside it: `rd-notice-dropped` carries the notice text, and once the entry
    /// is gone that line is the only record that could produce it. Auditing both
    /// is not redundant — `rd-pruned` is the retention event and a filter looking
    /// for a lost notice must not have to read every one of them to find the few
    /// that lost anything.
    fn rd_audit_retention(&self, group: &GroupId, flush: RdFlush) -> RdFlush {
        for (pr, text) in &flush.dropped {
            self.rd_audit(
                group,
                "",
                rddrive::audit_action::NOTICE_DROPPED,
                json!({ "pr": pr, "reason": "retention-ceiling", "notice": text }),
            );
        }
        for pr in &flush.pruned {
            self.rd_audit(group, "", rddrive::audit_action::PRUNED, json!({ "pr": pr }));
            self.rd_signals.lock_safe().remove(&(group.clone(), *pr));
        }
        flush
    }

    /// Phase 3 of [`rd_flush_notices`](OrchRegistry::rd_flush_notices), and the
    /// no-op tick's whole of it: mark the deliveries, then prune.
    ///
    /// One critical section and **one write**, so the two halves cannot come
    /// apart — a file that recorded a delivery and failed to prune, or pruned
    /// and failed to record, is a state neither this function nor the next tick
    /// has a story for.
    fn rd_retain(&self, dir: &std::path::Path, now: u64, ok: &[u64], failed: &[u64]) -> RdFlush {
        let mut out = RdFlush { undelivered: failed.to_vec(), ..RdFlush::default() };
        let _state_guard = self.rd_state_lock.lock_safe();
        let Ok(mut state) = reviewdrive::load_state(dir) else { return out };
        for pr in ok {
            if let Some(e) = state.entry_mut(*pr) {
                e.notice_delivered();
            }
        }
        for pr in failed {
            if let Some(e) = state.entry_mut(*pr) {
                e.notice_delivery_failed();
            }
        }
        let pruned =
            reviewdrive::prune_terminal(&mut state, now, reviewdrive::NOTICE_RETENTION_MS);
        if ok.is_empty() && failed.is_empty() && pruned.is_empty() {
            // Nothing moved. Not writing is not an optimization here: an
            // unconditional rewrite would touch `review_drives.json` on every
            // wake of every group forever, for no change.
            return out;
        }
        if reviewdrive::store_state(dir, &state).is_err() {
            // Nothing landed, so nothing is claimed. Everything that was owed
            // is still owed, including what this pass just delivered — it will
            // be sent again, which is the duplicate-line direction.
            out.undelivered = ok.iter().chain(failed.iter()).copied().collect();
            out.undelivered.sort_unstable();
            return out;
        }
        for p in pruned {
            out.pruned.push(p.pr);
            if let Some(text) = p.undelivered {
                out.dropped.push((p.pr, text));
            }
        }
        out
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
        // **The resume names the lane's own block** (#1961). A resume that
        // named neither `kind` nor `block` reached `spawn_agent_bound`, which
        // has no session-inheritance rule of its own and falls straight through
        // to `block_for(role)` — the roster's DEFAULT reviewer block. So a
        // `rev-final` lane came back as `rev-std` on its second round: wrong
        // persona, wrong model, and on a CLI that may not be able to open the
        // transcript at all. This lane's block is not something to look up — it
        // is the key the lane is filed under.
        // Reuse before spawn (#1960), the lane's half: a lane briefed a second
        // time whose reviewer is idle in a live pane is re-briefed IN that pane
        // rather than in a second one on the same conversation. Five of the six
        // panes holding the cap in the measured incident were idle lanes and
        // superseded workers the driver itself had opened.
        let fresh = |this: &Self| {
            this.rd_spawn(group, Role::Reviewer, Some(block.to_string()), None, &text)
        };
        let (agent, session_id) = match prior {
            Some(session) => match self.rd_reuse_pane(group, &session, &text) {
                Some(a) => (a, session),
                None => {
                    let sp = self
                        .rd_spawn(
                            group,
                            Role::Reviewer,
                            Some(block.to_string()),
                            Some(session),
                            &text,
                        )
                        .or_else(|_| fresh(self))?;
                    (sp.id, sp.session_id.unwrap_or_default())
                }
            },
            None => {
                let sp = fresh(self)?;
                (sp.id, sp.session_id.unwrap_or_default())
            }
        };
        entry.open_lane(block, &session_id, &agent, &brief.head, brief.body_digest_opt(), now);
        Ok(agent)
    }

    /// **Reuse before spawn** (#1960): resume `session` into the live idle pane
    /// already running it, by typing the brief into that pane, and answer which
    /// pane took it. `None` = there is none, so the caller opens one.
    ///
    /// # Why this rather than releasing what a new pane supersedes
    ///
    /// The driver opened a NEW pane per resume and closed none, so every round
    /// cost net +1 or +2 live panes and three concurrent drives exhausted the
    /// six-delegate cap in a round and a half — on the driver's own panes, five
    /// of the six idle. §3.1's other candidate fix was to kill the pane a new
    /// one supersedes, and this is chosen over it for two reasons.
    ///
    /// It does not need §3.1 item 5 narrowed. "The driver never kills a pane"
    /// stays a closed guarantee rather than becoming "never kills a pane it did
    /// not open", which is a guarantee a reader has to hold a second fact to
    /// evaluate — and the exception would have been useless for the pane that
    /// actually squatted the cap in the measured incident: the ORIGINAL worker
    /// pane, opened by the orchestrator, which a release-what-you-superseded
    /// rule may not touch. Reuse takes that pane over on the first hand-back
    /// and never creates the second one at all.
    ///
    /// And it is what an orchestrator driving by hand does: it types the next
    /// instruction into the worker's pane. A drive whose worker is idle in a
    /// live pane has nothing to gain from a second pane on the same
    /// conversation, and #338/#359 makes the second one actively worse — two
    /// panes, one session, one worktree.
    ///
    /// A failed delivery falls through to the spawn rather than failing the
    /// hand-back: `deliver_prompt` can refuse for reasons that say nothing
    /// about the drive (a pane that died between the lookup and the write),
    /// and the spawn is the path that already existed.
    fn rd_reuse_pane(&self, group: &GroupId, session: &str, text: &str) -> Option<String> {
        let agent = self.idle_pane_on_session(group, session)?;
        self.deliver_prompt(&agent, text, brand::AUDIT_ACTOR, Delivery::MidSession).ok()?;
        Some(agent)
    }

    /// Whether the pane this drive resumed its worker into is **dead**, and
    /// what it died saying (#1961).
    ///
    /// §2.1's `fix-wait` row waits for a push or a `report`, and neither ever
    /// arrives from a pane that exited on boot: the measured incident had a
    /// resumed pane exit 5.4 seconds after spawn with `Invalid session ID`, and
    /// the drive then sat in `fix-wait` until `fix-stalled` — a whole fix
    /// timeout spent waiting on a process that was already gone, and the exit
    /// notice routed to the ORCHESTRATOR, which is the turn the driver exists to
    /// remove. This is what lets `fix-wait` learn it instead.
    ///
    /// **`Dead` and nothing else counts.** An agent this registry has no record
    /// of is "we could not check", which is not "it is dead" — the same
    /// asymmetry [`reviewdrive::DriveEntry::forget_dead_panes`] states, and the
    /// same fail-direction: an emptied map (a restart) would otherwise park
    /// every live drive in `fix-wait` on a hold about panes that are fine.
    fn rd_pane_exit(&self, agent_id: &str) -> Option<String> {
        let a = self.agent(agent_id)?;
        if a.status != AgentStatus::Dead {
            return None;
        }
        let tail = a.last_exit_tail.as_deref().unwrap_or("").trim().to_string();
        Some(if tail.is_empty() {
            format!(
                "the pane it resumed the worker into ({agent_id}) exited before reporting, \
                 with no output"
            )
        } else {
            format!(
                "the pane it resumed the worker into ({agent_id}) exited before reporting; \
                 last output: {}",
                tail_snippet(&tail, 200)
            )
        })
    }

    /// The block a driver-initiated resume of `session` must run under (#1961).
    ///
    /// **The driver resolves this rather than leaving it to be defaulted, and
    /// that is the whole of #1961's root cause.** `rd_spawn` calls
    /// `spawn_agent_bound`, which has no session-inheritance rule of its own —
    /// #254's lives in the MCP `spawn_agent` arm — so a `block: None` resume
    /// falls through to `block_for(Role::Worker)`, the roster's DEFAULT worker
    /// block. Every drive whose worker is not the default block therefore had
    /// its fix handed back to the wrong persona on the wrong CLI: measured, a
    /// `worker-adv` (Claude) session reopened by opencode, which exited 5.4s
    /// later with `Invalid session ID` and left the drive in `fix-wait` with a
    /// dead worker.
    ///
    /// **A session this group has no record of is refused**, not defaulted.
    /// §5.1 already says resolving a session id is not proving it resumable,
    /// and "we do not know this session's capability class" is #544's own rule
    /// (never guess one) reaching the driver: the refusal becomes
    /// `held(worker-unresumable)` naming what could not be established, which
    /// is a line an orchestrator can act on. Defaulting is what produced the
    /// pane that could not open.
    ///
    /// **An empty recorded block is a pre-#222 row** — a role and no block
    /// identity — and inherits that class's default block, exactly as the MCP
    /// arm's own pre-#222 branch does. `None` here means precisely that, never
    /// "we did not look".
    ///
    /// A recorded block that is no longer declared is left for
    /// `spawn_agent_bound` to refuse, so the sentence an orchestrator reads is
    /// the one that knows the roster (`unknown block "x". Blocks in this group:
    /// …`). That is a deliberate divergence from the session browser's rejoin,
    /// which DEGRADES a stale block to the class default: there a human is
    /// present and losing the persona beats losing the session, while here
    /// nobody is watching and a silently re-personad worker is #1961.
    fn rd_resume_block(&self, group: &GroupId, session: &str) -> Result<Option<String>, String> {
        let rec = self.session_identity_record(group, session).ok_or_else(|| {
            format!(
                "no roster record maps session {session:?} to a block, so the class it must \
                 resume under cannot be established — refusing to guess one"
            )
        })?;
        let block = rec.block.trim();
        Ok((!block.is_empty()).then(|| block.to_string()))
    }

    /// Hand the PR back to its worker (§2.1's `fix-wait` row), resuming the
    /// session `drive_review` resolved and recorded **under that session's own
    /// block** — see [`rd_resume_block`](Self::rd_resume_block).
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
        // Reuse before spawn (#1960). Asked BEFORE the block is resolved: a
        // session with a live idle pane is already running under the right
        // block by construction, and refusing that hand-back because the
        // roster no longer declares that block would be a refusal with a live,
        // correct, typeable pane sitting in front of it.
        let agent = match self.rd_reuse_pane(group, &session, &text) {
            Some(a) => a,
            None => {
                let block = self.rd_resume_block(group, &session)?;
                self.rd_spawn(group, Role::Worker, block, Some(session), &text)?.id
            }
        };
        // Through the method, never a field write: the pane this supersedes is
        // still the drive's and still live (#1871 B2). Idempotent when the pane
        // reused IS the one already recorded — `retain_panes` drops it from the
        // superseded list on the way past — so a second hand-back into one pane
        // does not file that pane as its own predecessor.
        entry.record_worker_pane(&agent);
        Ok(agent)
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
        // **The CI line states what this tick OBSERVED**, and it is rendered
        // rather than asserted because the driver had the fact in hand and the
        // templates were claiming green unconditionally.
        //
        // A lane is normally briefed out of `review-wait`, which `ci-wait`
        // reaches only on green — but arc 8 moves `fix-wait -> review-wait` on a
        // worker's `report(done)` at an unchanged head WITHOUT consulting
        // `facts.ci`, which is the "that failure was unrelated" turn. A brief
        // that told the reviewer the checks were green there was stating as fact
        // something this tick had just read as false. It cannot produce an
        // unsafe landing — `gate-check` re-evaluates `ci-green` through
        // `recheck_gate` — so it costs a misled reviewer and a wasted round,
        // which is exactly what a driven review is for saving.
        // **One paragraph, one line each.** A backslash-n plus the source
        // indent ships both into a reviewer's brief, and a `.contains` of any
        // single fragment passes straight over it because no asserted substring
        // straddles the break. Written without continuations at all so there is
        // nothing to collapse, and the SHAPE is pinned beside the content in
        // `a_lane_brief_is_one_paragraph_per_sentence`.
        let ci = match brief.ci {
            reviewdrive::CiObservation::Green => "This PR's checks are green at that head.",
            reviewdrive::CiObservation::Red => "This PR's checks are RED at that head. Review the change on its merits; the failure is the worker's to answer.",
            reviewdrive::CiObservation::Conflicting => "This PR does not merge cleanly at that head. Review the change on its merits; the conflict is the worker's to answer.",
            // Pending and Unknown share one sentence on purpose: §8 says unknown
            // is never reported as a fact about the PR, and not-green-yet is the
            // only thing true of both.
            reviewdrive::CiObservation::Pending | reviewdrive::CiObservation::Unknown => "This PR's checks are not green at that head (orrerix could not read a settled result).",
        };
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
                        ("CI", ci),
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
                        ("CI", ci),
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
    fn rd_reconcile_with(&self, group: &GroupId, runner: &dyn rddrive::RdRunner, now: u64) {
        if self.rd_reconciled.lock_safe().contains(group) {
            return;
        }
        let dir = self.group_dir(group);
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
            let Ok(mut state) = reviewdrive::load_state(&dir) else { return };
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
                    // Owed onto the entry, not returned for a fire-and-forget
                    // delivery (#1857). Reconcile runs at startup — the exact
                    // moment an orchestrator pane is most likely to be missing
                    // or still coming up — so this is the producer whose notice
                    // was most likely to be the one that vanished. The caller's
                    // flush delivers it from disk, on this tick or a later one.
                    //
                    // #1871 B3's pane list is threaded into the construction
                    // rather than dropped: this hunk is a replace-vs-augment,
                    // and keeping "both sides" would restore the direct push
                    // #1857 deleted while ALSO owing the notice — the line
                    // twice. The panes are read before `owe_notice`'s mutable
                    // borrow.
                    let panes = entry.owned_panes();
                    let n = rddrive::cancelled_notice(pr, rddrive::CancelCause::PrGone, &panes);
                    entry.owe_notice(&n, now);
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
    /// one here and not a *notice*: no site that takes `rd_state_lock` is
    /// reachable from a pane delivery, so a spawn's own kickoff cannot cycle
    /// back onto it. The orchestrator notices this produces are still delivered
    /// by the caller, outside the lock, for the #467/#468 reason. The full site
    /// list, and why the two interception helpers do not break it, is on
    /// [`Registry::rd_drive_group_with`].
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
        // **The driver watches the pane it resumed** (#1961), and only where
        // the answer can mean anything: in `fix-wait`, on the CURRENT worker
        // pane, and only while that pane has said nothing.
        //
        // A `done` or a `blocked` already in hand outranks it — a worker that
        // reported and then exited is a worker that finished, and reading its
        // exit as a failure would throw away the arc its own report earned.
        // Nor is this asked in any other state: `review-wait` has
        // `lane-stalled` for its own panes, and a worker pane exiting outside a
        // hand-back is not this drive's business.
        let worker_exit = (here == reviewdrive::DriveState::FixWait
            && signal.worker == reviewdrive::WorkerSignal::Silent)
            .then(|| state.entry(pr).map(|e| e.worker_agent.clone()).unwrap_or_default())
            .filter(|a| !a.is_empty())
            .and_then(|a| self.rd_pane_exit(&a));
        let facts = reviewdrive::DriveFacts {
            now_ms: now,
            pr_open: obs.open,
            head: obs.head.clone(),
            body_digest: obs.body_digest.clone(),
            required_lanes: required.clone(),
            ci: obs.ci,
            // A dead resumed pane IS `worker-unresumable`, learned one tick
            // after the hand-back instead of one fix timeout after it. It rides
            // the existing signal rather than a new arc because §2.1 already
            // routes `Unresumable` from `fix-wait` to exactly this hold; what
            // was missing was anything that could ever produce it after the
            // hand-back itself had succeeded.
            worker: match worker_exit {
                Some(_) => reviewdrive::WorkerSignal::Unresumable,
                None => signal.worker,
            },
            gate,
            messaged: signal.messaged,
        };
        let mut out = RdOut::new(pr);
        out.backoff = obs.runner_failed;
        if let Some(why) = &worker_exit {
            out.refusal = why.clone();
        }
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
            reviewdrive::DriveStep::Wait => {
                // **#1959: a worker's `report(progress)` in `fix-wait` is
                // answered, in the worker's own pane.**
                //
                // The drive does not move on it and must not — a drive advances
                // on the head, the checks and the verdict files, and treating
                // "still going" as "the fix is in" would brief a reviewer over
                // unfinished work. But swallowing it is #1857's shape one arm
                // over: the measured round was a BODY-ONLY fix, so there was
                // nothing to push and no new checks, the worker read the
                // brief's "report when the checks are green" literally and sent
                // `progress`, and the drive sat for ten minutes until the idle
                // watchdog woke the ORCHESTRATOR — the turn the driver exists to
                // remove. One line back into the worker's pane costs that turn
                // nothing.
                //
                // Under `Wait` alone, so it can never displace an arc: a tick
                // that has something to DO does it, and a worker whose
                // `report(done)` arrived in the same window is advanced rather
                // than lectured.
                if signal.worker_progress && entry.kickback_owed() {
                    let agent = entry.worker_agent.clone();
                    if !agent.is_empty() {
                        // Marked BEFORE the delivery, which happens outside this
                        // lock. A delivery that fails therefore costs the line —
                        // the same asymmetry §5.2 already draws for a hold's
                        // notice, and the right direction here: the drive stays
                        // bounded by `fix-stalled` either way, while a mark
                        // written only on success would re-emit on every tick
                        // for as long as a pane stayed unreachable.
                        entry.record_kickback(now);
                        out.changed = true;
                        out.kickback = Some((agent.clone(), rddrive::fix_kickback_notice(pr)));
                        out.audits.push((
                            rddrive::audit_action::KICKBACK,
                            json!({ "pr": pr, "agent": agent }),
                        ));
                    }
                }
            }
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
                                    // #1960: whether the CAP is what refused,
                                    // on the row itself. A reader chasing a
                                    // drive that opened no lane for twenty
                                    // minutes was reading a free-text `detail`
                                    // to find out, and the answer decides
                                    // whether anything is wrong at all — a
                                    // capped lane retries and clears itself.
                                    "cap": super::is_live_cap_refusal(&why),
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
                                // The refusal reaches the NOTICE as well as the
                                // audit row (#1961). It used to reach the audit
                                // alone, and the pane got one fixed sentence
                                // diagnosing a session that no longer resolves
                                // — for a block that had left the roster, for a
                                // pane that opened and died on `Invalid session
                                // ID`, and (#1960) for a cap refusal.
                                //
                                // **And a cap refusal is now its own reason**
                                // (#1960): the session resolves fine, a slot is
                                // what is exhausted, and the two remedies are
                                // different actions. Classified on the shared
                                // literal `live_cap_refusal` writes, so the
                                // producer and this reader cannot drift.
                                let reason = if super::is_live_cap_refusal(&why) {
                                    reviewdrive::HeldReason::CapRefused
                                } else {
                                    reviewdrive::HeldReason::WorkerUnresumable
                                };
                                out.refusal = why.clone();
                                out.audits.push((
                                    rddrive::audit_action::REFUSED,
                                    json!({ "pr": pr, "reason": reason.as_str(),
                                            "detail": why }),
                                ));
                                // **The arc's result decides what the notice may
                                // claim.** Discarding it let `out.advanced`
                                // announce a hold the entry had not taken — the
                                // notice says parked, the file says `fix-wait`,
                                // and the next tick hands back again. A value
                                // computed and dropped at a boundary, which is
                                // the axis this round is about.
                                match entry.advance(
                                    reviewdrive::DriveState::Held,
                                    Some(reason),
                                    None,
                                    now,
                                ) {
                                    Ok(()) => {
                                        out.advanced =
                                            Some((reviewdrive::DriveState::Held, Some(reason)));
                                    }
                                    Err(bad) => {
                                        // Unreachable — `fix-wait -> held` is arc
                                        // 12 — and handled rather than claimed:
                                        // the drive stays where it is and says so.
                                        out.advanced = None;
                                        out.audits.push((
                                            rddrive::audit_action::REFUSED,
                                            json!({ "pr": pr, "reason": "invalid-transition",
                                                    "detail": bad.to_string() }),
                                        ));
                                    }
                                }
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
        // **What bounds the superseded-pane lists (#1871 B2, rev-final).** They
        // are pruned by LIVENESS and never by size: a size cap can only evict by
        // age, and the oldest superseded pane is one that is still running, still
        // on this session and still able to `report` — so evicting it un-owns it
        // exactly as the single slot did, which is B2 reproduced by the record
        // that fixes B2. `DriveEntry::forget_dead_panes` argues why a DEAD pane
        // is safe to forget instead.
        //
        // Liveness is the registry's fact, so the predicate is supplied here
        // rather than being reached for next door. `agent()` answers `None` for
        // an id that is gone; both that and `Dead` are states in which
        // `resolve_token` refuses the caller, so neither can reach the MCP seam.
        if entry.forget_dead_panes(&|id| {
            self.agent(id).is_some_and(|a| a.status != AgentStatus::Dead)
        }) {
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
        //
        // **A TERMINAL exit's notice is written ONTO the entry, inside this same
        // load-decide-store, and delivered from there** (#1857). It is not
        // handed to the caller as a string, because a string handed to a
        // delivery that answers `Err` is gone — and §5.2's retention then drops
        // the only record that could reproduce it, which is a drive that ends
        // with nothing in the pane and nothing to say why. `owe_notice` makes
        // the obligation durable before anything attempts it, and
        // `prune_terminal` will not drop an entry that still owes one.
        //
        // A `held` exit keeps the direct path deliberately, and the asymmetry is
        // the one §5.2 already draws: a parked entry is NEVER pruned, so the
        // drive survives its own lost notice — `review_drive_status()` lists it
        // and §2.3's resume re-reads it. What a hold can lose is a line; what a
        // terminal exit loses is the whole record. The mechanism is here if a
        // later change wants the stronger guarantee for a hold too.
        if let Some((to, reason)) = out.advanced {
            match (to, reason) {
                (reviewdrive::DriveState::Satisfied, _) => {
                    let n = rddrive::satisfied_notice(
                        pr,
                        &entry.head,
                        &entry.body_digest,
                        &brief.lane_notices,
                        &entry.counters,
                        &entry.owned_panes(),
                    );
                    out.audits.push((
                        rddrive::audit_action::SATISFIED,
                        json!({ "pr": pr, "head": entry.head }),
                    ));
                    entry.owe_notice(&n, now);
                }
                (reviewdrive::DriveState::Held, Some(r)) => {
                    let refusal = out.refusal.clone();
                    let n = rddrive::held_notice(
                        pr,
                        r,
                        &brief.held_facts(entry, limits, &messaged_by, &refusal),
                    );
                    // The refusal rides the `rd-held` row rather than a
                    // `rd-refused` row of its own, and only when there is one:
                    // it is a detail OF this hold, and a separate row pushed
                    // where the refusal was learned would be a claim about an
                    // arc a later condition (age, a closed PR) could still
                    // outrank — §5.4's "a filter looking for the thing that
                    // happened must not match the thing that did not".
                    let mut detail =
                        json!({ "pr": pr, "reason": r.as_str(), "head": entry.head });
                    if !refusal.is_empty() {
                        detail["refusal"] = Value::String(refusal);
                    }
                    out.audits.push((rddrive::audit_action::HELD, detail));
                    out.notices.push(n);
                }
                (reviewdrive::DriveState::Cancelled, _) => {
                    out.audits.push((rddrive::audit_action::CANCELLED, json!({ "pr": pr })));
                    // Replace-vs-augment, resolved as reconcile's is: #1871 B3's
                    // panes thread into the construction, and #1857's owe
                    // replaces the direct push rather than sitting beside it.
                    let panes = entry.owned_panes();
                    let n = rddrive::cancelled_notice(pr, rddrive::CancelCause::PrGone, &panes);
                    entry.owe_notice(&n, now);
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
            now_ms(),
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
        now: u64,
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
        // Notices owed by an entry this call is about to displace — audited
        // after the lock is dropped, for `rd_audit`'s own reason (#1857).
        let mut dropped_notices: Vec<(u64, String)> = Vec::new();
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
                // Read BEFORE `advance`, which clears `held_reason` on the way
                // out of `held`. Used by the lane re-open below.
                let was_lane_stalled =
                    entry.held_reason == Some(reviewdrive::HeldReason::LaneStalled);
                if entry
                    .advance(reviewdrive::DriveState::CiWait, None, None, now)
                    .is_err()
                {
                    return self.rd_refuse(group, pr, r::STATE_UNREADABLE);
                }
                // **The age clocks restart on a resume, or arc 11 is a no-op for
                // exactly the holds it exists to recover.**
                //
                // §2.2 makes `drive-stalled` the drive's AGE — `now -
                // started_ms`, "never an idle clock reset by each state
                // advance" — and `decide` checks it BEFORE any per-state logic.
                // Left alone, a drive parked longer than `drive_timeout_minutes`
                // re-holds `drive-stalled` on its very first tick after the
                // resume, and every hold a human takes their time over is
                // exactly that old. Four shipped surfaces promise the opposite.
                //
                // Resetting HERE does not reintroduce the idle clock that row
                // forbids: the ban is on a stamp written by each state advance,
                // and nothing on the tick path touches this. It moves only on a
                // deliberate, role-gated, audited `drive_review` — the same
                // event §2.3 already lets clear the counters. A drive being
                // restarted is a drive whose age starts again; the counters,
                // which are the budget, still carry unless `reset_counters` says
                // otherwise.
                //
                // Each lane's `spawned_ms` is re-armed for the same reason and
                // by the same argument: `lane-stalled` fires at 60 minutes, so
                // without this a resumed drive re-holds on the FIRST tick for a
                // lane the orchestrator has just looked at and chosen to resume.
                entry.started_ms = now;
                for l in entry.lanes.iter_mut() {
                    l.spawned_ms = now;
                }
                // **`lane-stalled` needs its lane RE-BRIEFED, not merely
                // re-timed** — and the clock re-stamp just above is what makes
                // that visible rather than fixing it. `decide_review_wait`
                // re-opens a lane only when `lane_open_for` is false, and at a
                // stable head it stays true, so the lane the notice named is
                // never spoken to again: before the re-stamp the drive re-held
                // instantly, after it the drive waits the full
                // `lane_timeout_minutes` in silence and re-holds then. Neither
                // is the recovery `held(lane-stalled)`'s own notice instructs,
                // and a hold ON A WAIT that its printed remedy cannot clear is
                // the defect arc 11 exists to not have. Holds parked on a
                // JUDGMENT are deliberately outside that rule — §2.2 names the
                // two and why resuming them re-holds by design.
                //
                // Clearing `briefed_head` puts the outstanding lane back in
                // `lane_open_for`'s false branch, so the next tick takes
                // `OpenLane`. `rd_open_lane` resumes the session recorded for
                // that lane when there is one and spawns a fresh reviewer when
                // there is not; either way the record is re-pointed at the pane
                // that now holds the lane, so §7's interception stays keyed on a
                // live pane rather than on an abandoned one.
                //
                // Scoped to this hold because it is the only one it can change.
                // A lane holding `escalate` or `review-limit` carries a verdict
                // that `decide_review_wait` answers before it ever consults the
                // lane record — SO LONG AS that verdict is still bound to the
                // revision in front of it, which is the half #1871 B1 added and
                // this sentence used to state flat. Once the head has moved the
                // verdict decides nothing, `lane_verdict_is_current` reads it as
                // absent, and the lane is re-briefed by the ordinary path with
                // no clearing needed here. Either way clearing in this arm would
                // be a no-op; and a lane that is legitimately mid-review must not
                // be re-briefed merely because some OTHER hold on the same drive
                // was resumed.
                if was_lane_stalled {
                    for l in entry.lanes.iter_mut() {
                        l.briefed_head.clear();
                    }
                }
                // **A new session means the recorded PANES are stale**, and a
                // stale pane is not merely useless — it is an interception key.
                // `driven_role` matches on `worker_agent` and on every pane it
                // superseded, so leaving them would have this drive consume the
                // traffic of a worker it no longer owns, while the worker it
                // DOES own reports to the orchestrator as if undriven. Cleared
                // on a change, kept when the orchestrator resumes with the same
                // session (the common case), where the panes are still this
                // worker's.
                if entry.worker_session != session {
                    entry.forget_worker_panes();
                }
                entry.worker_session = session.clone();
                entry.on_behalf_of = on_behalf_of.to_string();
            } else {
                // A `satisfied` or `cancelled` entry that retention has not yet
                // pruned starts a FRESH drive with fresh counters — the queue's
                // own "comes back as a NEW entry" behaviour.
                //
                // **If that entry still owed a notice, this is the one other way
                // one is given up on** (#1857), and it is deliberately not
                // silent: the retained entry becomes reachable far more often
                // now that retention holds it for an undelivered notice, and a
                // re-drive discarding the previous drive's ending would be the
                // same silence with a different cause. Audited with the text,
                // exactly as the ceiling is, so the record survives the entry.
                // The notice is NOT carried onto the new entry: it describes a
                // drive that is over, and delivering it beside a fresh drive's
                // own traffic would read as this drive ending.
                let superseded: Vec<(u64, String)> = state
                    .entries
                    .iter()
                    .filter(|e| e.pr == pr)
                    .filter_map(|e| e.owed_notice().map(|n| (e.pr, n.text.clone())))
                    .collect();
                for (dropped_pr, text) in superseded {
                    dropped_notices.push((dropped_pr, text));
                }
                state.entries.retain(|e| e.pr != pr);
                // **The clock is the caller's, and that is what makes the age
                // bound testable at all.** `started_ms` is the anchor §2.2
                // measures `drive-stalled` from, and stamping it from the wall
                // clock here while the tick advances on an injected `now` put
                // the two on different scales: `age_ms` saturated to zero for
                // every synthetic clock, so `drive-stalled` could not fire in a
                // test and never had. That is most of why B2 shipped.
                state.entries.push(reviewdrive::DriveEntry::new(
                    pr,
                    &session,
                    on_behalf_of,
                    reviewdrive::Counters::seeded(rounds_already_spent),
                    now,
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
        for (dropped_pr, text) in &dropped_notices {
            self.rd_audit(
                group,
                on_behalf_of,
                rddrive::audit_action::NOTICE_DROPPED,
                json!({ "pr": dropped_pr, "reason": "superseded", "notice": text }),
            );
        }
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
    /// only way an orchestrator stops a live drive short of `satisfied`.
    ///
    /// Not the only way one REACHES `cancelled`: a drive whose PR reads closed
    /// is cancelled on the tick that observes it, with no tool call at all,
    /// which is why [`CancelCause::PrGone`] exists.
    #[doc(hidden)] // pub for integration tests
    pub fn cancel_review_drive(&self, group: &GroupId, pr: u64, on_behalf_of: &str) -> Value {
        self.cancel_review_drive_with(group, pr, on_behalf_of, now_ms())
    }

    /// `cancel_review_drive` with the clock injected — `drive_review` /
    /// `drive_review_with`'s twin convention, and here for that pair's exact
    /// reason (#1857).
    ///
    /// **A bound measured against a clock a test cannot set is a bound no test
    /// can perform.** This function stamps [`OwedNotice::owed_ms`], and the
    /// retention ceiling is measured from it by a tick running on the caller's
    /// `now`. With the wall clock hard-coded here, a test driving the tick on a
    /// synthetic clock compared a small `now` against a wall-clock anchor,
    /// `saturating_sub` answered zero, and the ceiling could never fire on this
    /// path — so the one thing the ceiling promises was, for the tool-cancel
    /// producer, a documented counterfactual rather than a pinned one. #1841's
    /// B2 shipped out of the same shape one function over: "the clock is the
    /// caller's, and that is what makes the age bound testable at all".
    ///
    /// The only production caller passes `now_ms()`, so nothing about live
    /// behaviour changes; what changes is that
    /// `the_ceiling_fires_on_a_tool_cancelled_notice_too` can exist.
    #[doc(hidden)] // pub for integration tests
    pub fn cancel_review_drive_with(
        &self,
        group: &GroupId,
        pr: u64,
        on_behalf_of: &str,
        now: u64,
    ) -> Value {
        use rddrive::refusal as r;
        if !self.driver_enabled(group) {
            return self.rd_refuse(group, pr, r::DRIVER_DISABLED);
        }
        let dir = self.group_dir(group);
        // #1871 B3: read out of the entry BEFORE the lock is dropped, and it is
        // the ONLY thing that survives it. A cancel is the one exit whose caller
        // is a tool rather than a notice, so the panes have to reach two places —
        // the notice, and this tool's own return value, which is what an
        // orchestrator acts on without waiting for a prompt to arrive.
        let panes: Vec<(String, reviewdrive::DrivenRole)>;
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
            if entry.advance(reviewdrive::DriveState::Cancelled, None, None, now).is_err() {
                return self.rd_refuse(group, pr, r::STATE_UNREADABLE);
            }
            // Both sides are wanted here — this hunk's base side is EMPTY, so
            // it is add/add rather than the replace-vs-augment above: #1871 B3
            // needs the panes out before the lock drops, and #1857 needs the
            // notice built and owed before the store.
            panes = entry.owned_panes();
            // Owed BEFORE the store, so the obligation is on disk in the same
            // write as the cancellation itself (#1857). The delivery is
            // attempted below, outside the lock; if it fails, the entry keeps
            // owing and the next tick's flush re-sends it. Before this, the
            // notice was built after the write and handed to a `let _ =`, so a
            // cancel into a pane that was down was a drive that vanished with no
            // line and nothing to reproduce one from.
            let notice = rddrive::cancelled_notice(pr, rddrive::CancelCause::Tool, &panes);
            entry.owe_notice(&notice, now);
            if reviewdrive::store_state(&dir, &state).is_err() {
                return self.rd_refuse(group, pr, r::STATE_UNWRITABLE);
            }
        }
        self.rd_signals.lock_safe().remove(&(group.clone(), pr));
        self.rd_audit(
            group,
            on_behalf_of,
            rddrive::audit_action::CANCELLED,
            json!({ "pr": pr, "panes": panes.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>() }),
        );
        // The same flush the tick runs, so this tool has exactly one delivery
        // path rather than a second one that would have to be kept in step. It
        // also prunes: a cancel whose notice lands is an entry that leaves here,
        // which is what §5.2 already promised. The notice itself was built and
        // owed above, inside the lock (#1857).
        //
        // `now`, not `now_ms()`: the flush runs the retention ceiling, and a
        // ceiling measured against a different clock from the anchor above is
        // the untestable bound this seam exists to close (#1857).
        let _ = self.rd_flush_notices(group, &dir, now);
        // **The panes ride in the RESULT as well as in the notice.** #1871 B3
        // argued this from "a notice whose delivery fails is lost (nothing here
        // recovers it — #1857)", and that premise is no longer true: the notice
        // is owed on the entry and re-sent until it lands or the retention
        // ceiling drops it with an audit line. The conclusion still holds on a
        // narrower reason — this is the one exit whose caller is holding a
        // return value at the moment the panes stop being anyone's, and a
        // return value is synchronous where a notice is a prompt that arrives
        // whenever the pane next drains.
        json!({
            "cancelled": true,
            "panes": panes
                .iter()
                .map(|(agent, role)| json!({
                    "agent": agent,
                    "role": match role {
                        reviewdrive::DrivenRole::Worker => "worker".to_string(),
                        reviewdrive::DrivenRole::Lane(b) => b.clone(),
                    },
                }))
                .collect::<Vec<_>>(),
        })
    }

    /// Mark an agent `Dead`, so the liveness prune's DEAD side can be reached
    /// from a test (#1871 B2, rev-final).
    ///
    /// **Not a kill path, and deliberately unable to become one.** `kill_agent`
    /// needs a bound pty and performs a real `PtyManager::kill`; a
    /// driver-spawned pane in this harness has neither, so the state this
    /// predicate turns on is otherwise unreachable from an integration test.
    /// This sets the one field the predicate reads and touches nothing else — no
    /// initiator stamp, no exit notice, no pty — so it cannot stand in for
    /// `kill_agent` in a test that means to exercise killing, and a reader
    /// cannot mistake it for the production route.
    ///
    /// Answers whether an agent by that id existed to mark.
    #[doc(hidden)] // pub for integration tests
    pub fn mark_agent_dead_for_test(&self, agent_id: &str) -> bool {
        let mut agents = self.agents.lock_safe();
        match agents.get_mut(agent_id) {
            Some(a) => {
                a.status = AgentStatus::Dead;
                true
            }
            None => false,
        }
    }

    /// Corrupt this group's drive record, so the FAULT paths that read it can
    /// be exercised from outside the crate.
    ///
    /// **It hands out no path**, which is the whole point. CLAUDE.md constraint
    /// 6 keeps `group_dir_at` the single join and keeps it private, and a
    /// `group_dir_for_test` returning a `PathBuf` would hand every future test
    /// exactly the thing that rule exists to withhold. This takes a validated
    /// `GroupId`, writes a fixed payload, and answers whether it wrote — so a
    /// test can reach "the record exists and cannot be parsed" without ever
    /// reaching the directory. The payload is not a parameter for the same
    /// reason: a caller that can choose the bytes is a caller that can write a
    /// VALID record, which is a state seeder rather than a fault injector.
    #[doc(hidden)] // pub for integration tests
    pub fn corrupt_drive_record_for_test(&self, group: &GroupId) -> bool {
        let path = reviewdrive::state_path(&self.group_dir(group));
        std::fs::write(path, b"{ not json").is_ok()
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
