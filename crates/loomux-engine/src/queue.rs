//! Pure core of the per-pane delivery queue (#445): "hold means queued,
//! never doomed." Before this, `deliver_prompt`'s three hold-cap seams
//! (box-occupied, pre-paste question, pre-Enter question) DESTROYED the
//! payload once their bounded wait expired, and told the sender the
//! delivery merely succeeded. That is safe only if the release condition
//! for the hold is expected to clear in seconds; this repo's standing
//! requirement is the opposite — the user is frequently AWAY, an agent asks
//! a question, and they answer minutes or hours later. A timeout measured
//! in seconds is structurally wrong for a release condition of "a human
//! answers."
//!
//! The fix: the hold caps stay exactly as they are (they bound *thread
//! blocking*, which is a legitimate concern — one OS thread per pending
//! delivery, forever, is not free), but the outcome AT the cap changes from
//! "destroy the payload" to "enqueue it." A per-pane, in-memory FIFO plus a
//! drainer thread (see `OrchRegistry::deliver_now`/`run_queue_drainer` in
//! `mod.rs` for the impure half) replays queued entries, oldest first, the
//! instant the pane becomes deliverable again — no timeout, no sender
//! action required.
//!
//! Everything in this module is a plain function over plain data — no
//! registry, no `gh`. Pre-#470, this module carried one deliberate
//! exception (`queue_is_non_empty`, taking the live queue-map lock
//! directly) because two SEPARATE checkpoints — the front door and
//! `deliver_now`'s pre-paste recheck — both had to consult the identical
//! live state or drift to different definitions of "this pane is already
//! spoken for." #470 removes the exception by removing the SECOND
//! checkpoint's reason to exist: every delivery is now admitted into the
//! queue at arrival, in `mod.rs`'s `enqueue_text` (impure, as admission
//! necessarily is — it mutates the live queue), and nothing downstream ever
//! needs to re-ask "is someone else already ahead of me," because the
//! queue's own front/back discipline makes that structurally impossible to
//! answer wrong. See `notify.rs`'s module doc for why this codebase
//! otherwise splits every backend feature this way (pure policy here,
//! impure wiring in `mod.rs`) and `doc/design/orchestration.md`'s "Delivery
//! queue (#445)" section — its "Ordering" subsection covers #470's redesign
//! specifically, and its "Durability (#468/#467)" subsection the on-disk
//! snapshot below — for the full design rationale.
//!
//! **Durability across a restart (#468/#467).** The queue is no longer
//! in-memory only. Every mutation of a pane's queue rewrites that group's
//! `queue.json` through `atomic_write` (the #133 precedent: temp file,
//! fsync, rename — a disk-full or a crash mid-write leaves the previous
//! good snapshot, never a truncated file), and a restart reads it back:
//! `parse_snapshot` → `split_recovered` here, `OrchRegistry::
//! recover_persisted_queue`/`readmit_recovered` in `mod.rs`. What that buys,
//! stated exactly, because the honest limits are the point (see this
//! module's `PersistedEntry` doc and `doc/design/orchestration.md`'s
//! "Durability (#468/#467)" subsection):
//!
//! - A queued `Text` payload survives the restart with its bytes intact.
//! - It is **re-admitted automatically** — same queue, same order — when the
//!   pane it was addressed to comes back with a durable identity loomux can
//!   match: the group's single orchestrator, or an agent resumed onto the
//!   same CLI session id. Neither `pty_id` nor `agent_id` is that identity:
//!   `pty_id` is re-minted at restore, and `agent_id` — durable since #524,
//!   so it never names a *different* agent — still names no LIVE pane after
//!   one, because a restore spawns a new agent with a new id.
//! - Anything else (a worker pane simply gone after the restart) is
//!   **surfaced, never silently dropped**: `queue_orphans` reports it with
//!   its payload so the orchestrator's session-start re-sync can re-derive
//!   and re-send it. `orphaned_queue_entries` below still derives the same
//!   view from `audit.jsonl` alone, which is what covers entries queued by a
//!   build older than this one (no snapshot on disk to read).
//! - A `StrandedSubmit` marker is deliberately NOT replayable: it means
//!   "text is already sitting in that pane's input box, press Enter." After
//!   a restart the box is gone, so pressing Enter would submit whatever the
//!   new session happens to have there. Recovery drops markers, audits each
//!   one, and surfaces them — see `split_recovered`.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};

/// A pane blocked for hours accumulates reports/kickoffs from at most a
/// handful of agents — 8 is generous headroom, not a measured traffic
/// figure. On overflow the NEWEST is rejected, never the oldest: the head
/// of the queue may be the kickoff everything after it depends on.
pub const QUEUE_MAX_PER_PANE: usize = 8;

/// Drainer poll cadence once a pane has a non-empty queue. There is no push
/// event from a pane anywhere in this codebase — every "question cleared /
/// box emptied" signal, including the holds this queue replaces, *is* a
/// poll over pane state — so this is not a compromise, it's the same
/// mechanism the holds already used, just continued with no cap.
pub const QUEUE_DRAIN_POLL: Duration = Duration::from_secs(2);

/// After a queue sits behind an unanswered question/box this long, a single
/// visibility notice fires once (never repeated, never destructive) so a
/// forgotten blocked pane doesn't stay invisible. This is NOT an expiry —
/// see `still_queued_notice`'s doc.
pub const QUEUE_STILL_QUEUED_NOTICE_AFTER: Duration = Duration::from_secs(30 * 60);

/// Why an entry entered the queue — carried into the audit line and (for
/// the two hold-cap reasons) the sender-facing notice.
///
/// **This is a persisted vocabulary, so adding a variant is a format change
/// in both directions (#560).** It is a field of [`QueuedDelivery`], which IS
/// the on-disk record (#468), and both directions have an answer that is a
/// decision rather than an accident:
///
/// - **New build, OLD snapshot** — every variant that has ever been written
///   is still spelled here, so an older `queue.json` parses unchanged. A
///   variant is only ever ADDED; renaming or removing one would strand the
///   entries already on disk carrying it, which is why `as_str` and the serde
///   `kebab-case` rename must keep agreeing (the audit log and the snapshot
///   are read side by side, and a reason that reads differently in the two is
///   a reason a human cannot grep for — see [`STRANDED_ORPHAN_REASON`] for
///   what that costs).
/// - **OLD build, NEW snapshot** — the old build has no such variant, so
///   `parse_snapshot`'s per-entry tolerance skips exactly that entry and
///   counts it in `skipped` (audited by the caller), leaving every sibling
///   entry intact. That is the same downgrade shape [`QueuedDelivery`]'s
///   `#[serde(default)]` fields take, minus the default: an unknown *variant*
///   has no safe value to fall back to, and inventing one (`#[serde(other)]`
///   → some existing reason) would make a downgrade SILENTLY MISLABEL an
///   entry — precisely the false audit claim this enum exists to prevent. A
///   visible skip beats an invisible lie. For [`EnqueueReason::
///   StrandedSelfHeal`] specifically the loss is nil in practice: it only
///   ever rides a `StrandedSubmit` marker, which no build replays across a
///   restart anyway (`split_recovered`) — the older build drops it one line
///   earlier, as a skip rather than as an unreplayable marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnqueueReason {
    /// A box-occupied hold gave way to the human's own content: either the
    /// pre-paste hold (#111) capped out with nothing pasted, or — since #532 —
    /// the pre-Enter occupancy gate declined the Enter with the text already
    /// in the box, leaving a `StrandedSubmit` marker to press it later.
    ///
    /// The second half of that sentence is #560's: this variant read
    /// "pre-paste … nothing pasted" while `AbortedPreEnter` had been carrying
    /// it since #532, and the marker push then recorded `Question` for it
    /// anyway. Both facts a reader needs — a human's line is in the box, and
    /// the delivery is past its paste — are true of one reason, so this stays
    /// ONE variant; what was missing was letting it reach the record.
    BoxOccupied,
    /// A pre-paste or pre-Enter interactive-question hold (#420) capped out.
    Question,
    /// The pane's queue was already non-empty when this delivery arrived —
    /// the front-door check: a fresh prompt must never overtake ones
    /// already waiting, or "here's context" / "now go" can invert.
    BehindQueue,
    /// #470: this delivery landed ALONE at the front of an idle queue — the
    /// front door's admission-time reason for the common, uncontended case
    /// that's about to be attempted immediately, never itself notice-worthy
    /// (nothing blocked it; see `queued_notice`'s doc for why this variant
    /// never reaches that function). Distinct from `BehindQueue` purely for
    /// audit-trail honesty: pre-#470, an entry this fast never touched the
    /// queue at all, so its `delivery-queued` audit line would be
    /// misleading if it claimed the SAME reason a genuinely-blocked
    /// admission gets.
    Arrival,
    /// #517: a fresh spawn's kickoff brief that the pane never received —
    /// the paste was swallowed by a CLI whose stdin reader had not attached
    /// yet, so nothing ever reached the input box and there is no stranded
    /// text for #496's Enter-press self-heal to rescue. The late-confirmation
    /// monitor re-admits the SAME text here, through this same front door,
    /// rather than writing to the pane itself. Its own reason (not
    /// `Arrival`) purely for audit-trail honesty: a re-delivery and a first
    /// delivery are different facts, and reading "arrival" twice for one
    /// brief would hide the recovery that actually happened.
    KickoffRecovery,
    /// #467/#468: this entry was read back out of a group's `queue.json`
    /// after a loomux restart and re-admitted to the pane that came back
    /// for it. Distinct from `Arrival` for the same audit-honesty reason
    /// `Arrival` is distinct from `BehindQueue`: the payload did NOT arrive
    /// now, it arrived before the restart and waited through it, and a
    /// `delivery-queued` line claiming `arrival` would put the wrong clock
    /// on it. Never reaches `queued_notice` — recovery announces itself
    /// through `recovered_notice` instead, which can say how many and how
    /// old.
    Recovered,
    /// #569: the human has PAUSED this delivery's group, so the payload is
    /// HELD in the queue until they resume rather than pasted now.
    ///
    /// Before #569 this case had no reason because it had no entry: the pause
    /// branch destroyed the payload, told the caller `Ok`, and audited
    /// `prompt-suppressed-paused` — the last remaining non-crash path where
    /// something a sender was told succeeded ceased to exist. It is an
    /// ordinary queue entry now, and it gets its own reason for the
    /// audit-honesty rule every variant above already follows: a pause hold
    /// and a blocked pane are different facts, and the flush header at drain
    /// time has to say which one it was (see [`FlushCause`]).
    GroupPaused,
    /// #569 rev-128: the resume-time notice naming what a pause LOST
    /// (`announce_pause_suppression`). The ONE admission allowed past
    /// `QUEUE_MAX_PER_PANE`, by `PAUSE_LOSS_NOTICE_HEADROOM`.
    ///
    /// **Why an exception, and why this one.** The notice exists because the
    /// cap destroyed a payload. Refusing the notice *to that same cap* means a
    /// full pane silently eats both the work and the report of the work — and
    /// it is not a rare corner: when the overflow happened on the
    /// ORCHESTRATOR's own pane (the case #569 is actually about, since that is
    /// where a fleet's reports converge) the pane is still at capacity at
    /// resume, so the notice is not merely at risk, it is CERTAIN to be
    /// refused. Worse, the pane already carries `note_queue_capacity`'s
    /// at-capacity badge by then, so `pause_badge_decision`'s
    /// never-stomp-another-badge rule suppresses the fallback too, leaving the
    /// audit tally as the only trace.
    ///
    /// Bounded, not open: `PAUSE_LOSS_NOTICE_HEADROOM` is 1, one notice is
    /// emitted per resume, and it is emitted only when something was actually
    /// lost — so the queue can exceed its cap by exactly one entry, briefly,
    /// and a second resume that finds the headroom still occupied is refused
    /// like anything else.
    PauseLossNotice,
    /// #560: a `StrandedSubmit` marker pushed by #496's self-heal — loomux
    /// noticed by itself that a delivery's text is sitting unsubmitted in a
    /// pane's box and queued the Enter press that rescues it
    /// (`admit_stranded_selfheal`).
    ///
    /// **Why it exists, which is the whole of this variant's value.** Both
    /// marker pushes go through `push_stranded_front_locked`, which hardcoded
    /// `Question` for all of them — true of the drainer's push (the pre-Enter
    /// question gate really did decline the Enter, and the marker is the
    /// remainder of that delivery) and FALSE of this one, where nothing is on
    /// screen at all: the trigger is a pane that has gone QUIET with our text
    /// stranded in its box, which is the opposite reading. So a self-heal's
    /// `delivery-queued` line claimed `question`, and a human reconstructing a
    /// wedge from `audit.jsonl` — the only record of a write loomux made on its
    /// own initiative — was told to go look for a dialog that was never there.
    /// Same class as the mislabel `AbortedPreEnter`'s own reason (#532 rev-12
    /// NB1) and `write_admission_badges_the_gate_that_actually_blocked` exist
    /// to prevent, on the one path that had no variant to be honest WITH.
    ///
    /// Audit-only, deliberately: it names why the marker was queued, never how
    /// the marker drains (that is `drain_stranded_submit`'s press, identical
    /// for both pushes) and never a notice — see `queued_notice` below.
    StrandedSelfHeal,
    /// #658: the drain-time roster naming deliveries this pane REFUSED while
    /// its queue was full (`announce_refusal_roster`).
    ///
    /// **No headroom, deliberately — this is the opposite call to
    /// [`EnqueueReason::PauseLossNotice`] and the difference is the trigger.**
    /// The loss notice fires at a resume, which says nothing about depth, so on
    /// the pane it is actually about (an orchestrator's, still at capacity) it
    /// is CERTAIN to be refused without an exemption. The roster fires only on
    /// the edge where a pane's depth just came back DOWN below the cap, so
    /// there is room by construction and an exemption would buy nothing while
    /// weakening "the cap is 8". A roster that is nonetheless refused (its slot
    /// taken by a concurrent arrival) is recorded `delivered: false`, does not
    /// advance the roster watermark, and is simply re-derived at the next
    /// drain — see `OrchRegistry::announce_refusal_roster`.
    ///
    /// Its own variant, rather than `Arrival`, for one load-bearing reason
    /// beyond audit honesty: `refusal_roster` EXCLUDES refusals carrying this
    /// reason, which is half of what stops a refused roster from becoming a
    /// line in the next roster and then in the one after that (the other half
    /// covers the refusal shapes that record no reason at all — see that
    /// function).
    RefusalRoster,
}

impl EnqueueReason {
    /// #569 rev-128: how far past `QUEUE_MAX_PER_PANE` this admission may go.
    /// Zero for everything except the pause-loss notice — see that variant.
    pub fn cap_headroom(self) -> usize {
        match self {
            EnqueueReason::PauseLossNotice => PAUSE_LOSS_NOTICE_HEADROOM,
            EnqueueReason::BoxOccupied
            | EnqueueReason::Question
            | EnqueueReason::BehindQueue
            | EnqueueReason::Arrival
            | EnqueueReason::KickoffRecovery
            | EnqueueReason::Recovered
            | EnqueueReason::GroupPaused
            | EnqueueReason::StrandedSelfHeal
            | EnqueueReason::RefusalRoster => 0,
        }
    }
}

/// #569 rev-128: how far past the cap [`EnqueueReason::PauseLossNotice`] may
/// push. One entry — enough for the single notice a resume emits, and small
/// enough that "the cap is 8" stays true for every practical purpose.
pub const PAUSE_LOSS_NOTICE_HEADROOM: usize = 1;

impl EnqueueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EnqueueReason::BoxOccupied => "box-occupied",
            EnqueueReason::Question => "question",
            EnqueueReason::BehindQueue => "behind-queue",
            EnqueueReason::Arrival => "arrival",
            EnqueueReason::KickoffRecovery => "kickoff-recovery",
            EnqueueReason::Recovered => "recovered",
            EnqueueReason::GroupPaused => "group-paused",
            EnqueueReason::PauseLossNotice => "pause-loss-notice",
            EnqueueReason::StrandedSelfHeal => "stranded-self-heal",
            EnqueueReason::RefusalRoster => "refusal-roster",
        }
    }
}

/// Why a queue was DROPPED wholesale (never a single entry — see
/// `dropped_notice`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// The queue was already at `QUEUE_MAX_PER_PANE` at the moment a
    /// hold-cap expiry needed to enqueue (rare — requires the queue filling
    /// during a single delivery's own 60-120s hold; the common overflow
    /// case is caught earlier, at the front door, and reported synchronously
    /// — see `queue_full_error`).
    QueueFull,
    /// The pane's agent died / its pty closed while entries were still
    /// queued. Today this case is silent; this PR makes it audited and
    /// notified instead.
    AgentDied,
}

impl DropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::QueueFull => "queue-full",
            DropReason::AgentDied => "agent-died",
        }
    }
}

/// The payload of one queued entry. `StrandedSubmit` (seam 3: the text was
/// already pasted before the pre-Enter question appeared — only the Enter
/// was withheld) carries no text: draining it presses Enter via the
/// existing stranded-text flush, never a fresh paste.
///
/// Serialized with an explicit tag (`{"kind":"text","text":"…"}` /
/// `{"kind":"stranded-submit"}`) rather than serde's default externally-
/// tagged shape, so a human reading a group's `queue.json` after a crash
/// sees what the entry is without knowing serde's conventions — the same
/// reason `audit.jsonl` spells its actions out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "kebab-case")]
pub enum QueuedPayload {
    Text(String),
    StrandedSubmit,
}

impl QueuedPayload {
    /// The exact text this entry would paste, or `None` for a marker —
    /// `admit`'s coalesce check compares against this, never against a
    /// marker (a marker is not a fresh payload to begin with).
    pub fn text(&self) -> Option<&str> {
        match self {
            QueuedPayload::Text(t) => Some(t.as_str()),
            QueuedPayload::StrandedSubmit => None,
        }
    }
}

/// One entry in a pane's FIFO delivery queue.
///
/// **Every field here is persisted** (#468) — this struct IS the on-disk
/// record, not a projection of one, so a field added without thought about
/// what it means after a restart is a field a recovery will read back
/// stale. The three that exist ONLY for that restart (`group`,
/// `to_orchestrator`, `session_id`) carry their own reasoning below;
/// `#[serde(default)]` on them is what lets a `queue.json` written by an
/// older build still parse (the fields simply come back empty, and such an
/// entry is surfaced as an orphan rather than re-admitted — the safe
/// direction). `delivery_kind` (#620) takes the same `#[serde(default)]` for
/// the same compatibility reason but is NOT a restart field: it exists for a
/// delivery held across a PAUSE inside one process, and its doc states what a
/// restart does with it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueuedDelivery {
    /// Per-group monotonic id (`OrchRegistry`'s `queue_seq`, an `AtomicU64`
    /// — no getrandom, CLAUDE.md constraint 2). Carried in every audit line
    /// (`delivery-queued`/`-dequeued`/`-dropped`/`-coalesced`) so one
    /// payload's whole history is reconstructible after the fact, and
    /// available to #455's dedup investigation later.
    pub id: u64,
    /// The delivery target — always the pane this queue is keyed by; kept
    /// on the entry (not just the map key) so an audit line reads the same
    /// whether logged from the front door or the drainer.
    pub agent_id: String,
    /// `deliver_prompt`'s own `from` — preserved so a drained delivery's
    /// provenance audits identically to a direct one.
    pub from: String,
    pub payload: QueuedPayload,
    pub reason: EnqueueReason,
    pub enqueued_ms: u64,
    /// How many byte-identical duplicates were coalesced into this entry
    /// (0 = none). Reported in the flush header so the RECEIVER knows it
    /// may be reading a de-duplicated ask, not a guess at which of several
    /// identical prompts is "the real one."
    pub coalesced: u32,
    /// #468: which group's `queue.json` this entry belongs in. The live
    /// queue map is keyed by `pty_id`, which is registry-global (groups
    /// share one `OrchRegistry`), so without this the persister would have
    /// to re-derive a pane's group from the agents map at write time — and
    /// an entry whose agent has already been reaped would silently fall out
    /// of every group's snapshot. Stamped at admission, where the group is
    /// never in doubt.
    /// #904: `Option<GroupId>` rather than `GroupId`, and the `#[serde(default)]`
    /// stays — this is the one field where the newtype could not simply replace
    /// the `String`, and the reason is a real behavior the suite already pinned.
    ///
    /// A pre-#468 snapshot has no `group` key at all. `GroupId` has no `Default`
    /// (deliberately — the default would be the empty string the constructor
    /// exists to refuse), so making this a bare `GroupId` would have made such an
    /// entry fail to deserialize, and `read_snapshot`'s per-entry
    /// `Err(_) => skipped += 1` would have swallowed it into an anonymous count.
    /// That is a REGRESSION, not hardening:
    /// `an_entry_from_an_older_build_parses_but_has_no_durable_identity` exists
    /// to pin that a legacy entry still *parses*, precisely so its payload is
    /// surfaced as an orphan rather than vanishing.
    ///
    /// `None` says exactly what the old empty string meant — "no recorded
    /// identity" — but says it in the type, so it cannot be confused with a group
    /// or silently joined onto a path. Every consumer filters with
    /// `as_ref() == Some(group)`, so an entry with `None` matches nothing and is
    /// never replayed into a pane it wasn't for.
    /// `deserialize_with` (rev-440 N3): `Option<GroupId>` handles the *absent*
    /// key, but a plain derive would still hard-fail on a **present but
    /// unparseable** one — a leading-dash id from the old minter, say — and
    /// `read_snapshot`'s `Err(_) => skipped += 1` would swallow the whole entry
    /// into an anonymous number. That is verbatim the regression this field's
    /// doc argues is unacceptable, arriving through the other door. An id that
    /// does not parse maps to `None`, which is exactly what `None` already means
    /// here: no recorded identity, matches nothing, never joined onto a path.
    #[serde(default, deserialize_with = "lenient_group_id")]
    pub group: Option<crate::groupid::GroupId>,
    /// #467: whether the target was this group's orchestrator — the one
    /// delivery target with an identity that outlives a restart.
    ///
    /// Neither of the obvious keys does: `pty_id` is re-minted by the
    /// terminal layer on every restore, and `agent_id` — while it no longer
    /// RECYCLES (#524 made the counter durable, so an old id can never name
    /// a *different* agent) — still names nothing LIVE after a restart,
    /// because a restore spawns a fresh agent with a fresh id rather than
    /// reviving the old one. So #468's own filing — "`agent_id` is kept so
    /// a durable follow-up could rebind by agent (durable across a
    /// restore)" — does not hold and is not what this implements. Its
    /// failure mode is simply better now: an id-keyed rebind would find no
    /// target rather than the WRONG target. A group has exactly one
    /// orchestrator, so "was this addressed to the orchestrator" survives
    /// the restart even though the name it was addressed by does not.
    #[serde(default)]
    pub to_orchestrator: bool,
    /// #467: the target agent's CLI conversation session id, when it had
    /// one. This is the durable identity for a NON-orchestrator target:
    /// `spawn_agent(resume_session, cwd)` reopens exactly this session, and
    /// the resumed pane is the same worker in every sense that matters to a
    /// delivery that was addressed to it before the restart. `None` for a
    /// copilot pane that had not yet minted its id, and for any target
    /// whose entry predates this field — both surface as orphans instead.
    #[serde(default)]
    pub session_id: Option<String>,
    /// #620: which `Delivery` kind admitted this entry — the fact
    /// `deliver_prompt` resolves at the front door and threads into the
    /// drainer's first pass as `FreshFirstAttempt` (boot wait, copilot
    /// autopilot consent, #517's late-kickoff recovery).
    ///
    /// **Why it has to live on the entry.** A pause holds a delivery for as
    /// long as the human is away, and `deliver_prompt`'s pause branch returns
    /// immediately — so by the time `flush_paused_queues` starts a drainer at
    /// resume, the only thing that still knows this was a kickoff is the entry
    /// itself. Without it the resume flushed every held delivery as a plain
    /// prompt: a `FreshKickoff` for a copilot pane under `--autopilot` then
    /// pasted its brief and pressed Enter with nobody watching for the consent
    /// dialog that appears AFTER that Enter, wedging the agent at an
    /// unanswered dialog with its brief already submitted and #517's recovery
    /// unarmed (#620; the shape #569 surfaced by making the payload survive a
    /// pause at all).
    ///
    /// **After a RESTART it means nothing, on purpose.** The field persists
    /// like every other one here, but `readmit_recovered` re-admits a
    /// recovered payload through `enqueue_text` — i.e. as `MidSession` —
    /// rather than reinstating whatever kind is on disk. That is deliberate,
    /// not an oversight: the boot this kind describes belongs to a pane that
    /// no longer exists (`pty_id` is re-minted at restore and `agent_id` names
    /// nothing live — see `to_orchestrator` above), the pane it re-binds to
    /// gets its OWN `ResumeKickoff` delivery for the boot it really did just
    /// perform, and re-arming `confirm_autopilot` across a restart would fire
    /// a stray Enter at a dialog nobody is waiting on — the same reasoning
    /// `redelivery_treatment` gives for not copying those flags onto a
    /// re-delivery. So a restart mid-pause resumes its held payloads as plain
    /// prompts, which is exactly what a record written before this field
    /// existed does via `#[serde(default)]` (`Delivery::MidSession`): old
    /// snapshots and restarts land on one behavior, and it is the one that
    /// takes no action against a live pane.
    #[serde(default)]
    pub delivery_kind: crate::model::Delivery,
}

/// A persisted group id that fails [`GroupId::parse`] reads as `None` rather
/// than failing the whole entry (rev-440 N3). See [`QueuedDelivery::group`].
fn lenient_group_id<'de, D>(de: D) -> Result<Option<crate::groupid::GroupId>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(de)?;
    Ok(raw.and_then(|s| crate::groupid::GroupId::parse(&s).ok()))
}

/// The outcome of offering a new TEXT payload to a pane's queue. Pure — no
/// lock, no registry; `OrchRegistry`'s enqueue wrapper (mod.rs) is the
/// impure caller that holds the queue-map lock and applies this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmitDecision {
    /// Append it — the common case.
    Admit,
    /// A byte-identical `Text` payload is already queued for this pane:
    /// drop the new one, bump the existing entry's `coalesced` counter
    /// instead. Scoped to exact-byte equality, never anything smarter — the
    /// queue cannot judge SEMANTIC staleness (three different task briefs
    /// must all deliver), only that a literal repeat adds zero information.
    Coalesce,
    /// The pane's queue is already at `QUEUE_MAX_PER_PANE` — reject the
    /// NEWEST, never evict the oldest.
    ///
    /// This is the one outcome that leaves NO queue entry and therefore no id
    /// (`enqueue_text` returns before it mints one), so nothing in this module
    /// can report it: `orphaned_queue_entries` and `merge_orphans` both key on
    /// an id. #579 surfaces it from `audit.jsonl` instead, as
    /// `queue_orphans`'s second list — see `mod.rs`'s `front_door_refusals`.
    RejectFull,
}

/// Decide `AdmitDecision` for a new text payload against an existing queue.
///
/// `reason` is consulted for one thing only: [`EnqueueReason::cap_headroom`],
/// which is zero for every reason but the pause-loss notice (#569 rev-128).
/// Coalescing is checked FIRST and is not affected — an exempt admission that
/// byte-matches a queued entry still folds into it rather than spending its
/// headroom, because a duplicate adds no information whatever its reason.
pub fn admit(queue: &VecDeque<QueuedDelivery>, text: &str, reason: EnqueueReason) -> AdmitDecision {
    if queue.iter().any(|q| q.payload.text() == Some(text)) {
        return AdmitDecision::Coalesce;
    }
    if queue.len() >= QUEUE_MAX_PER_PANE + reason.cap_headroom() {
        return AdmitDecision::RejectFull;
    }
    AdmitDecision::Admit
}

/// The synchronous, truthful error `deliver_prompt` returns when a pane's
/// queue is already at cap — the common overflow case (the front-door check
/// catches deliveries 2..N before this is ever consulted), and the one
/// place the sender can be told IN-BAND, at the call site, rather than via
/// a best-effort notice to someone else.
pub fn queue_full_error(agent_id: &str, depth: usize, blocked_reason: &str) -> String {
    format!(
        "delivery queue for {agent_id} full ({depth} queued; pane blocked on {blocked_reason}) — NOT queued"
    )
}

/// The `[loomux]` notice sent to the orchestrator the FIRST time a delivery
/// genuinely becomes held: a fresh delivery's OWN pre-paste/pre-Enter hold
/// (#111/#420) caps out (`BoxOccupied`/`Question` — `mod.rs`'s
/// `run_queue_drainer` gates this to that entry's very first attempt only,
/// never a later retry of the same entry). Never called with `BehindQueue`,
/// `Arrival`, `KickoffRecovery` or `Recovered` — a delivery admitted behind
/// an existing queue, admitted alone and about to be attempted immediately,
/// or re-admitted by a recovery (#517's in-process kickoff re-delivery, or
/// #467's read-back after a restart, which announces through
/// `recovered_notice` instead) was never blocked by anything at the moment
/// of admission, so there is nothing yet to
/// announce; the eventual `flush_header_text` at drain time is what informs
/// the recipient it was queued at all. Replaces the old "held: ... re-send
/// when clear" wording — the #445 honesty fix: the payload is safe and WILL
/// deliver on its own: a re-send now would just create a duplicate once the
/// drain lands.
pub fn queued_notice(agent_id: &str, reason: EnqueueReason) -> String {
    let why = match reason {
        EnqueueReason::BoxOccupied => "pane has human input",
        EnqueueReason::Question => "an interactive question is on screen",
        EnqueueReason::BehindQueue => "another delivery to this pane was already queued",
        EnqueueReason::Arrival => "just arrived",
        EnqueueReason::KickoffRecovery => "a lost kickoff is being re-delivered",
        // Unreachable in real code (`readmit_recovered` announces through
        // `recovered_notice`, which can say how old the backlog is — this
        // wording cannot). Spelled out rather than folded into a `_` arm so
        // that a FUTURE reason added to this enum is a compile error here,
        // which is what has kept every notice string in this module honest.
        EnqueueReason::Recovered => "it was queued before a loomux restart",
        // #569: also unreachable in real code — `deliver_prompt`'s pause
        // branch admits without notifying, because a notice about a paused
        // group is itself a delivery INTO that paused group and would simply
        // queue behind the payload it describes. Spelled out for the same
        // compile-time reason as `Recovered` above.
        EnqueueReason::GroupPaused => "the group is paused",
        // Also unreachable: the loss notice is loomux telling the orchestrator
        // something, never a delivery whose sender is waiting to hear it was
        // queued.
        EnqueueReason::PauseLossNotice => "it reports what a pause lost",
        // #560: also unreachable, and for a structural reason rather than a
        // convention — this reason only ever sits on a `StrandedSubmit`
        // marker, and no marker's own `reason` field is ever handed to this
        // function. The drainer notifies from the reason the ABORT carried
        // (`DeliverOutcome::AbortedPrePaste`/`AbortedPreEnter`), which is a
        // fact about the gate that just declined, not about the entry it is
        // looking at. Spelled out for the same compile-time reason as
        // `Recovered` above: the next variant added to this enum must be
        // decided here rather than defaulted.
        EnqueueReason::StrandedSelfHeal => "loomux is re-submitting text left in the box",
        // #658: also unreachable, for the same structural reason as
        // `PauseLossNotice` above — the roster is loomux telling a pane what it
        // refused, never a delivery whose sender is waiting to hear it was
        // queued. Spelled out rather than folded into a `_` arm so the next
        // variant added to this enum is still a compile error here.
        EnqueueReason::RefusalRoster => "it reports what this pane refused while full",
    };
    format!(
        "[loomux] delivery to {agent_id} queued ({why}) — delivers automatically once clear; do NOT re-send"
    )
}

/// #569: WHY a flush's constituents were queued — the one thing the header
/// cannot get wrong without misdirecting its reader.
///
/// The pre-#569 header said "queued while this pane was blocked" and that was
/// the only case there was: a pause DESTROYED its deliveries, so nothing a
/// pause held could ever reach a flush. Now it can, and "this pane was
/// blocked" would be a false statement about a pane that was never blocked at
/// all — the receiver would go looking for a box or a dialog that was never
/// there instead of reading "the human paused us and has now resumed."
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlushCause {
    /// Delivery to the pane itself was held (a box, a dialog, a queue behind
    /// one of those) — every pre-#569 flush, and still the common case.
    PaneBlocked,
    /// The human paused the group; this flush is the resume.
    GroupPaused,
}

/// #569: which cause a flush's batch should report.
///
/// `GroupPaused` wins on a MIXED batch, and deliberately: a batch mixes only
/// when entries queued behind a pane block were still sitting there when the
/// human paused, in which case the pause is both the more recent hold and the
/// one that decided when this flush happened. The pane-block wording would
/// name a condition that has since cleared; the pause wording names the event
/// the receiver just lived through. Neither is the whole story on a mixed
/// batch — the per-constituent banners carry each entry's own queue time —
/// but only one of the two can head the paste, and this is the one that
/// explains the timing.
pub fn flush_cause(entries: &[QueuedDelivery]) -> FlushCause {
    if entries.iter().any(|e| e.reason == EnqueueReason::GroupPaused) {
        FlushCause::GroupPaused
    } else {
        FlushCause::PaneBlocked
    }
}

/// The clause both headers below share: what the deliveries were waiting on.
fn flush_cause_clause(cause: FlushCause) -> &'static str {
    match cause {
        FlushCause::PaneBlocked => "queued while this pane was blocked",
        FlushCause::GroupPaused => "queued while this group was paused",
    }
}

/// The one line a drain delivers FIRST, ahead of whatever it flushes — the
/// "N deliveries queued ... re-sync" nudge the issue's own root-cause
/// analysis asked for. For an orchestrator target this doubles as the
/// re-sync prompt; it cannot loop, because it only ever fires on UNBLOCK,
/// when delivery demonstrably works again.
pub fn flush_header_text(count: usize, coalesced: usize, cause: FlushCause) -> String {
    // The verb agrees with the count (#533): this line shipped as "1
    // delivery ... ARE now delivering" and a human read it in a live pane.
    // Small, but every one of these notices is read by an agent as an
    // instruction, and a sentence that doesn't parse is one more reason to
    // skim it.
    let n = if count == 1 { "1 delivery".to_string() } else { format!("{count} deliveries") };
    let verb = if count == 1 { "is" } else { "are" };
    let why = flush_cause_clause(cause);
    if coalesced > 0 {
        format!(
            "[loomux] {n} {why} ({coalesced} coalesced) {verb} now delivering, oldest first"
        )
    } else {
        format!("[loomux] {n} {why} {verb} now delivering, oldest first")
    }
}

/// Byte budget for ONE coalesced flush paste (#533-A). A backlog bigger
/// than this splits into consecutive flushes rather than one megaprompt:
/// a paste is echoed back by the CLI and re-read by the agent, so an
/// unbounded combined payload trades the turn cost this feature saves for
/// a context cost that is strictly worse. 24 KiB is ~6k tokens — comfortably
/// several task briefs, well under any agent's context budget, and not a
/// measured traffic figure. The cap is a CEILING, never a floor: a single
/// constituent larger than it still delivers alone (see `plan_flush`), so
/// nothing is ever stuck behind its own size.
pub const QUEUE_FLUSH_MAX_BYTES: usize = 24 * 1024;

/// Per-constituent overhead `plan_flush` charges against
/// `QUEUE_FLUSH_MAX_BYTES` for the itemization banner
/// `coalesced_flush_text` will emit around each entry. Approximate on
/// purpose: the cap exists to bound a paste, and paying a fixed ~120 bytes
/// per item keeps the budget arithmetic pure (no rendering pass inside the
/// planner) while staying on the conservative side of the real banner.
/// (#632's `[loomux] ` prefix added 9 bytes to that banner; the longest one
/// this can render — every optional clause present — is still comfortably
/// inside the margin, so the charge is unchanged.)
const FLUSH_ITEM_OVERHEAD: usize = 120;

/// One constituent of a coalesced flush, as the header itemizes it — the
/// attribution that must survive N deliveries becoming one paste (#533-A).
/// Borrowed from the live `QueuedDelivery`s, never a copy of the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlushConstituent<'a> {
    pub id: u64,
    /// `deliver_prompt`'s own `from` — the origin this delivery would have
    /// been attributed to had it delivered alone.
    pub from: &'a str,
    pub enqueued_ms: u64,
    /// Byte-identical repeats folded in at admission (`AdmitDecision::
    /// Coalesce`) — reported per constituent, not just as a queue total, so
    /// the reader can tell WHICH ask was repeated.
    pub coalesced: u32,
    pub text: &'a str,
}

/// What one drain pass should submit (#533-A). Pure decision over the
/// pane's queue as it stands AT DRAIN TIME — nothing here waits for, or
/// holds back, anything: a lone live delivery yields a one-entry `batch`
/// and pastes exactly as it did pre-#533.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlushPlan {
    /// Constituents that drop OUT before coalescing, oldest first — see
    /// `superseded_entries` for what makes an entry superseded. The impure
    /// caller removes and audits these; they are never merged into `batch`.
    pub superseded: Vec<Superseded>,
    /// The entries this pass submits TOGETHER as one prompt, oldest first.
    /// Always at least one entry when anything is queued at all.
    pub batch: Vec<u64>,
    /// True when `batch` is the single `StrandedSubmit` marker at the front:
    /// press Enter via the stranded flush, never paste. A marker can never
    /// be merged with text (its payload is already sitting in the box), so
    /// it is always a batch of exactly one.
    pub stranded: bool,
    /// Entries still queued after this batch — the chunking signal. Non-zero
    /// only when the byte cap split the backlog or a marker terminated it.
    pub remaining: usize,
}

/// One constituent ruled superseded at drain time, paired with the entry
/// that supersedes it (#533-A, rev-13 F4).
///
/// The pairing is the point. `admit`'s admission-time coalesce doesn't just
/// drop a byte-identical repeat, it BUMPS the surviving entry's `coalesced`
/// counter, which is what the flush header reports as "+N identical repeats
/// coalesced". A drain-time drop that returned only the dropped id would
/// silently lose that count for exactly the case this re-check exists to
/// catch — the reader would be told about repeats folded in at admission
/// but not the one folded in at drain, in a feature whose stated point is
/// attribution completeness. Naming the survivor lets the impure caller
/// move the count instead of dropping it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Superseded {
    /// The entry being removed.
    pub id: u64,
    /// The earlier, byte-identical entry that stays and absorbs its count.
    pub by: u64,
}

/// Entries that must drop OUT before coalescing (#533-A) — the drain-time
/// byte-identical re-check.
///
/// **Scoped to what can actually be observed, deliberately.** #454's own
/// supersession is per-PANE ledger state (`last_delivery`'s
/// `submit_sent_ms`), which an entry that has never been pasted cannot
/// carry, so there is no per-entry supersession field to read here. What
/// there IS: `admit`'s byte-identical coalesce, which runs at ADMISSION
/// under the `queues` lock and is what normally guarantees two identical
/// `Text` entries never coexist. This is the same rule applied a SECOND
/// time, at drain time, over the whole batch — because coalescing changes
/// the cost of that guarantee being wrong. Pre-#533 a duplicate that
/// slipped past admission would deliver as its own prompt (visibly
/// redundant, one wasted turn); merged into a combined paste it becomes the
/// same ask stated twice INSIDE one prompt, which reads as two distinct
/// requests. The check is cheap, and it is the last point before the merge
/// where the queue can still tell them apart.
///
/// The LATER duplicate is what drops: the earlier entry keeps its queue
/// position, so order is preserved for everything that survives.
///
/// The other two members of #533-A's filter are NOT here, because neither
/// is a property of one entry: a `StrandedSubmit` marker terminating the
/// batch is `plan_flush`'s rule (a marker's text is already in the box —
/// it submits alone and whatever is behind it flushes on the next cycle),
/// and a dead target / closed pty drops the WHOLE queue through
/// `commit_exit(force: true)` before a plan is ever computed.
///
/// Stated narrowly on purpose: a rule that can never fire is worse than no
/// rule, because it reads as coverage. If a future mechanism gives a queued
/// entry its own supersession state, it belongs here, beside this one, with
/// its own test.
pub fn superseded_entries(entries: &[QueuedDelivery]) -> Vec<Superseded> {
    let mut seen: Vec<(&str, u64)> = Vec::new();
    let mut out = Vec::new();
    for e in entries {
        let Some(t) = e.payload.text() else { continue };
        match seen.iter().find(|(s, _)| *s == t) {
            Some(&(_, by)) => out.push(Superseded { id: e.id, by }),
            None => seen.push((t, e.id)),
        }
    }
    out
}

/// The banner `coalesced_flush_text` puts above one constituent — its
/// position, origin and queue age, so nothing loses attribution when N
/// deliveries become one paste (#533-A). `now_ms` is passed in (never read
/// from the clock here) so the rendering is deterministic under test.
///
/// **Marker-led, ahead of the rule (#632).** The banner is loomux's own
/// framing riding the pty, so it has to be a row `mask_loomux_notices` can
/// claim — otherwise every constituent of a coalesced flush leaves an unmasked
/// row of loomux prose in the pane's tail, and a `from`/`queued` line
/// describing a delivery is exactly the text-about-a-question shape #576 is.
/// The marker must come FIRST because `deframe` strips whitespace and
/// `│ ┃ | * ● • ◆` and not `-`: `----- [loomux] …` would still lead with the
/// dash and survive. The literal matches this module's seven other notice
/// constructors, which spell the marker out the same way;
/// `every_framing_row_of_a_coalesced_flush_is_maskable` in
/// `tests/orchestration.rs` binds them all to the real
/// `orchestration::mask_loomux_notices`, so the literal cannot drift from the
/// const unnoticed.
fn constituent_banner(pos: usize, total: usize, c: &FlushConstituent, now_ms: u64) -> String {
    let age = age_clause(now_ms.saturating_sub(c.enqueued_ms));
    let repeats = if c.coalesced > 0 {
        let n = c.coalesced;
        let s = if n == 1 { "repeat" } else { "repeats" };
        format!(" · +{n} identical {s} coalesced")
    } else {
        String::new()
    };
    format!(
        "[loomux] ----- {pos}/{total} · from {} · queued {age} (id {}, t={}){repeats} -----",
        c.from, c.id, c.enqueued_ms
    )
}

/// "3m12s ago" / "just now" — a queue age for a human/agent reader. Pure
/// over a duration in ms; no clock, no formatting crate (none is available:
/// CLAUDE.md constraint 2 keeps the dependency list minimal).
fn age_clause(ms: u64) -> String {
    let secs = ms / 1000;
    if secs == 0 {
        return "just now".to_string();
    }
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    let rem = secs % 60;
    if mins < 60 {
        return format!("{mins}m{rem:02}s ago");
    }
    format!("{}h{:02}m ago", mins / 60, mins % 60)
}

/// The ONE prompt a coalesced flush submits (#533-A): the flush header,
/// then every constituent in queue order behind its own itemization
/// banner. Order is the queue's order and is never rearranged — the reason
/// the header can promise it.
///
/// `remaining` is how many entries stay queued after this chunk (the byte
/// cap split the backlog, or a stranded marker terminated it); when
/// non-zero the header says so, so the receiver knows more is coming rather
/// than inferring the flush was everything.
///
/// Callers pass a single-constituent slice only when they genuinely mean
/// "one delivery, itemized" — `run_queue_drainer` keeps using
/// `flush_header_text` for the lone-entry case so an uncontended flush's
/// wording is unchanged from pre-#533.
///
/// **Two row classes, and the split is the whole of #632.** The header and
/// every `constituent_banner` are loomux's own framing and are marker-led, so
/// `orchestration::mask_loomux_notices` claims them. `c.text` is pushed
/// VERBATIM and stays unmarked: it is the delivery itself, byte-identical to
/// what the same entry would have pasted had it flushed alone, so it is
/// ordinary pane content that no other delivery path hides from the question
/// gate. Marking it would be a new over-mask, and teaching the mask a block
/// form ("everything between two banners") would hand any pane the power to
/// hide a live dialog behind a forged banner row — the #621 hole exactly.
/// So payload rows are left to latch the gate, which is the safe direction
/// (a hold that clears late, reported by `QuestionStale`) rather than the
/// dangerous one (an Enter released into a real question). The caller in
/// `run_queue_drainer` asserts that split through
/// `orchestration::unmaskable_framing_rows`.
pub fn coalesced_flush_text(
    items: &[FlushConstituent],
    remaining: usize,
    now_ms: u64,
    cause: FlushCause,
) -> String {
    let n = items.len();
    let more = if remaining > 0 {
        let d = if remaining == 1 { "delivery" } else { "deliveries" };
        format!(" ({remaining} further queued {d} follow in the next flush)")
    } else {
        String::new()
    };
    let total_coalesced: u32 = items.iter().map(|c| c.coalesced).sum();
    let dedup = if total_coalesced > 0 {
        format!(" {total_coalesced} byte-identical repeat(s) were folded in at admission.")
    } else {
        String::new()
    };
    let count = if n == 1 { "1 delivery".to_string() } else { format!("{n} deliveries") };
    let verb = if n == 1 { "is" } else { "are" };
    let why = flush_cause_clause(cause);
    let mut out = format!(
        "[loomux] {count} {why} {verb} being delivered TOGETHER, \
         as this one prompt, oldest first{more} — they are itemized below with their origin and \
         queue time; nothing was reordered or dropped.{dedup} Treat each item as its own message.",
    );
    for (i, c) in items.iter().enumerate() {
        out.push_str("\n\n");
        out.push_str(&constituent_banner(i + 1, n, c, now_ms));
        out.push('\n');
        out.push_str(c.text);
    }
    out
}

/// Decide what one drain pass submits (#533-A) — the pure core of the
/// coalesced flush.
///
/// `entries` is the pane's queue oldest-first as it stands at drain time,
/// `max_bytes` the combined-paste cap (`QUEUE_FLUSH_MAX_BYTES` in
/// production; a test passes its own to exercise chunking without building
/// a 24 KiB fixture).
///
/// The rules, in order:
/// 1. Superseded constituents (`superseded_entries`) drop out first — never
///    merged, never delivered.
/// 2. A `StrandedSubmit` marker is a batch of exactly one: its text is
///    already in the box, so it can only be submitted, never pasted with
///    anything else.
/// 3. Otherwise take the longest run of consecutive `Text` entries from the
///    front that fits `max_bytes`, stopping at the first marker. ALWAYS at
///    least one entry, even when that one entry alone exceeds the cap —
///    the cap must never be able to stall a queue.
///
/// Nothing here waits: the batch is whatever is ALREADY queued at this
/// instant, so a lone live delivery is planned as a one-entry batch and
/// pastes exactly as it did pre-#533. There is no batching timer and no
/// path by which a delivery is held back to be combined with a later one.
pub fn plan_flush(entries: &[QueuedDelivery], max_bytes: usize) -> FlushPlan {
    let superseded = superseded_entries(entries);
    let live: Vec<&QueuedDelivery> =
        entries.iter().filter(|e| !superseded.iter().any(|s| s.id == e.id)).collect();
    let Some(first) = live.first() else {
        return FlushPlan { superseded, batch: Vec::new(), stranded: false, remaining: 0 };
    };
    if matches!(first.payload, QueuedPayload::StrandedSubmit) {
        return FlushPlan { superseded, batch: vec![first.id], stranded: true, remaining: live.len() - 1 };
    }
    let mut batch = Vec::new();
    let mut used = 0usize;
    for e in &live {
        let Some(t) = e.payload.text() else { break };
        let cost = t.len() + FLUSH_ITEM_OVERHEAD;
        if !batch.is_empty() && used.saturating_add(cost) > max_bytes {
            break;
        }
        used = used.saturating_add(cost);
        batch.push(e.id);
    }
    let remaining = live.len() - batch.len();
    FlushPlan { superseded, batch, stranded: false, remaining }
}

/// The notice for a whole queue dropped at once (`DropReason`) — always
/// names the count and the reason; never silent (today's behavior for both
/// cases this replaces).
pub fn dropped_notice(agent_id: &str, count: usize, reason: DropReason) -> String {
    let n = if count == 1 { "1 queued delivery".to_string() } else { format!("{count} queued deliveries") };
    let why = match reason {
        DropReason::QueueFull => "queue was already full when a hold expired",
        DropReason::AgentDied => "the agent's pane closed",
    };
    format!("[loomux] {n} to {agent_id} DROPPED ({why}) — not delivered, not recoverable")
}

/// The one-shot "still queued" visibility notice at
/// `QUEUE_STILL_QUEUED_NOTICE_AFTER`. Deliberately NOT an expiry: a queue
/// behind an unanswered question is the DESIGNED case for this feature, not
/// a leak — see the design note's "No age-based expiry" section. This just
/// makes a forgotten blocked pane visible without destroying anything.
pub fn still_queued_notice(agent_id: &str, depth: usize, minutes: u64) -> String {
    let entries = if depth == 1 { "1 delivery".to_string() } else { format!("{depth} deliveries") };
    format!(
        "[loomux] still queued: {entries} to {agent_id}, waiting {minutes} min behind an unanswered \
         question/input box — nothing lost, delivers automatically once clear"
    )
}

/// #590: the `from` a delivery carries when **loomux itself** is the sender.
/// `deliver_prompt`'s `from` for every host-originated payload — a fired
/// `notify_when` notice, a channel note, a compact nudge, a kickoff brief.
///
/// Not forgeable by an agent, which is the whole reason it is worth reading:
/// an MCP-relayed `message_agent` carries the CALLING agent's id here, never
/// this string, because `mcp.rs` passes `&caller.agent_id` and no tool lets a
/// caller choose the field.
pub const LOOMUX_SENDER: &str = "loomux";

/// #590: is this queued entry one of loomux's OWN `[loomux] …` notices — the
/// payload class whose entire purpose is to tell the pane's agent something it
/// cannot learn any other way, and which is therefore the one payload whose
/// non-delivery can deadlock the agent that is waiting on it?
///
/// **Both halves are load-bearing, and each covers the other's blind spot.**
///
/// - `from` alone OVER-matches. A kickoff brief is also `from: "loomux"` and
///   is *work*, not a notice: a kickoff that never lands is #517/#585's
///   subject, with its own recovery, and counting it here would put a
///   deadlock diagnosis on an ordinary busy pane.
/// - The marker alone UNDER-guards. Agent text is relayed verbatim, and
///   [`crate::text::LOOMUX_NOTICE_MARKER`]'s own doc states the limit in these
///   words: a marker row is evidence that *someone wrote a notice-shaped
///   row*, never proof that loomux wrote this one. An agent that opens a
///   `message_agent` with `[loomux]` would otherwise be able to make loomux
///   report its own text as a stuck host notice.
///
/// Requiring both means the answer rests on a field an agent cannot set AND a
/// prefix loomux always writes. A `StrandedSubmit` marker carries no text and
/// is never a notice.
pub fn is_loomux_notice(entry: &QueuedDelivery) -> bool {
    entry.from == LOOMUX_SENDER
        && entry.payload.text().is_some_and(|t| {
            // Lowercased and left-trimmed for the same reason
            // `mask_loomux_notices` de-frames and lowercases: the claim is
            // "this row leads with the marker", and neither indentation nor a
            // re-cased prefix changes that. No de-framing, because this is a
            // payload loomux constructed, not a row read back off a pane.
            t.trim_start().to_lowercase().starts_with(crate::text::LOOMUX_NOTICE_MARKER)
        })
}

/// #590: how many of `entries` are loomux's own notices ([`is_loomux_notice`]).
pub fn queued_notice_count(entries: &[QueuedDelivery]) -> usize {
    entries.iter().filter(|e| is_loomux_notice(e)).count()
}

/// #563: the depth at which a pane's queue stops being "backed up" and starts
/// being "about to lose work".
///
/// Six of eight, i.e. two admissions of headroom. Sized against what the
/// warning is FOR rather than against traffic: at `QUEUE_DRAIN_POLL` (2s) a
/// held pane admits new deliveries as fast as its senders produce them, so a
/// threshold that leaves only one slot would routinely be crossed and
/// exhausted inside the same poll tick and the warning would arrive with the
/// loss rather than before it. Two slots is the smallest headroom that can
/// still be a *warning*. Below `QUEUE_MAX_PER_PANE` by construction — see
/// `capacity_state`'s `debug_assert`.
pub const QUEUE_NEAR_FULL_AT: usize = 6;

/// #563: how close a pane's delivery queue is to dropping work.
///
/// Exists because "the queue filled completely" was, before this, a fact
/// loomux only ever learned at the instant it was already losing a payload.
/// There was no state between "fine" and "a delivery has just been thrown
/// away", so nothing could be said in advance and nothing said it afterwards
/// on the one pane (an orchestrator's) where the in-band notice channel is
/// structurally suppressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapacityState {
    /// Room to spare.
    Normal,
    /// At or past `QUEUE_NEAR_FULL_AT` — still accepting, but the next few
    /// arrivals will be rejected. This is the state that has to be loud.
    Approaching,
    /// At `QUEUE_MAX_PER_PANE`. Every further arrival is dropped.
    Full,
}

impl CapacityState {
    /// Stable audit token, kept separate from any human-facing wording for the
    /// same reason `StrandedBlocker::as_str` is.
    pub fn as_str(self) -> &'static str {
        match self {
            CapacityState::Normal => "normal",
            CapacityState::Approaching => "approaching",
            CapacityState::Full => "full",
        }
    }
}

/// Classify a pane's queue depth (#563). Pure, so the thresholds are pinnable
/// and so the caller's edge-trigger ("fire only when this goes UP") is a
/// comparison of two of these rather than a second spelling of the constants.
pub fn capacity_state(depth: usize) -> CapacityState {
    debug_assert!(
        QUEUE_NEAR_FULL_AT < QUEUE_MAX_PER_PANE,
        "a near-full threshold at or past the cap could never warn in advance"
    );
    if depth >= QUEUE_MAX_PER_PANE {
        CapacityState::Full
    } else if depth >= QUEUE_NEAR_FULL_AT {
        CapacityState::Approaching
    } else {
        CapacityState::Normal
    }
}

/// The longest a dropped payload's preview may run in an audit line (#563).
/// Long enough to identify which delivery was lost, short enough that a pane
/// dropping repeatedly cannot bloat `audit.jsonl` (which rotates, and is the
/// only record of the loss).
pub const DROPPED_PREVIEW_MAX: usize = 160;

/// A one-line, bounded preview of a payload that could NOT be queued (#563).
///
/// A drop that names only `{to, reason, depth}` — the pre-#563 audit line —
/// tells a reader that something was lost but not WHAT, which is the
/// difference between a recoverable incident and an unrecoverable one: the
/// sender can re-send a delivery it can identify. Newlines are collapsed so
/// the preview stays one JSON string a human can read in a log viewer, and
/// truncation is marked (`…`) rather than silent, because a preview that looks
/// complete but isn't is the same unbacked claim `.loomux/lessons.md` catalogues.
///
/// Truncation is by CHARS, not bytes — slicing a UTF-8 string at an arbitrary
/// byte offset panics, and this runs on the delivery path.
pub fn dropped_payload_preview(text: &str) -> String {
    clamp_preview(&text.split_whitespace().collect::<Vec<_>>().join(" "), DROPPED_PREVIEW_MAX)
}

/// Cut an already-one-line preview to `max` CHARS, marking the cut (#658).
///
/// Split out of [`dropped_payload_preview`] rather than re-spelled, because a
/// second consumer arrived with a tighter budget: the #658 refusal roster is a
/// SINGLE line carrying several previews at once, so it re-clamps each one
/// (`mod.rs`'s `ROSTER_PREVIEW_MAX`) instead of pasting four 160-char previews
/// into one row. Two copies of "truncate on a char boundary and say so" is one
/// copy too many — and the char-boundary half is not a style preference, it is
/// what stops a UTF-8 payload from panicking a delivery path.
///
/// Idempotent for the roster's purposes: clamping an already-clamped preview
/// to a smaller max simply cuts further, and the `…` the first cut appended is
/// an ordinary char to the second.
pub fn clamp_preview(one_line: &str, max: usize) -> String {
    if one_line.chars().count() <= max {
        return one_line.to_string();
    }
    let kept: String = one_line.chars().take(max).collect();
    format!("{kept}…")
}

/// The "this pane's queue is nearly full" notice (#563). Names the numbers
/// rather than saying "nearly", so a reader can tell how much room is left
/// without knowing loomux's constants.
///
/// **Says only what depth establishes (rev-10 finding 1).** The first cut read
/// "it is held and backing up" and told the reader to press Enter or answer
/// what was on screen. `note_queue_capacity` decides on **depth alone** — it
/// never reads `write_admission` or any hold state — so a pane whose drainer is
/// perfectly healthy but whose senders simply outrun it (a fleet of workers
/// reporting into one orchestrator pane) reaches this threshold with no hold of
/// any kind, and the reader would go looking for a dialog that does not exist.
/// A badge that sends someone hunting for nothing is the same "trains a human
/// to ignore the real one" failure the drainer's `ChipGuard` exists to prevent,
/// arrived at from the other side. So the hold is stated as a CONDITION to
/// check, never as a fact — and the backlog itself, which depth does establish,
/// is what the sentence asserts.
///
/// Goes to the group's orchestrator via `notify_queue` — which means it is
/// suppressed when the pane in question IS the orchestrator's. That is exactly
/// why this notice is never the only channel: the caller also raises an
/// attention badge, which no role suppresses. See `StrandedBlocker::QueueNearFull`.
pub fn pressure_notice(agent_id: &str, depth: usize, cap: usize) -> String {
    format!(
        "[loomux] {agent_id}'s delivery queue is {depth}/{cap} and backing up — deliveries are \
         arriving faster than that pane accepts them. If the pane is held (unsubmitted text in \
         the box, or a question on screen), releasing it drains the backlog. At {cap} further \
         deliveries are DROPPED, not queued"
    )
}

/// The "this pane's queue is FULL" notice (#563) — the transition INTO
/// `CapacityState::Full`, fired before anything has actually been rejected
/// (the entry that filled the last slot was accepted). Says what happens next
/// rather than claiming a loss that has not happened yet; the loss itself gets
/// its own `delivery-dropped` record naming the payload.
///
/// "until it drains", not "until the pane is released" (rev-10 finding 1): the
/// caller knows the depth and nothing else, and a full queue does not imply a
/// held pane — see `pressure_notice`'s doc for the case where it is simply
/// arriving faster than it delivers.
pub fn at_capacity_notice(agent_id: &str, cap: usize) -> String {
    format!(
        "[loomux] {agent_id}'s delivery queue is FULL ({cap}/{cap}) — every further delivery \
         to it is DROPPED, not queued, until it drains"
    )
}

/// #560: since when has this pane had work it has not delivered — the clock
/// [`should_fire_still_queued_notice`] is measured from.
///
/// **Both terms are load-bearing, and each covers the other's blind spot.**
///
/// - `oldest_entry_ms` alone is what shipped before #560, taken from the queue
///   FRONT. A `StrandedSubmit` marker carries a fresh `now_ms()` and is pushed
///   in FRONT of everything, so after a pasted-but-unsubmitted delivery the
///   front is the *youngest* entry in the queue — and the 30-minute notice was
///   pushed out by the very event that proves the pane is stuck. Taking the
///   minimum over the whole queue fixes the front-vs-oldest half of that (and
///   makes the parameter's name true, which it was not); it does not fix the
///   case where the marker is the ONLY entry left, because the batch it stands
///   for was already popped.
/// - `hold_since_ms` (the pane's hold episode) covers exactly that case, and
///   only that case — but on its own it would lose a backlog that outlives
///   individual successes, since a delivery landing ends the episode while
///   entries the flush could not fit (`plan.remaining`) stay queued.
///
/// So: the earlier of the two, and `None` when no episode is open.
pub fn undelivered_since(oldest_entry_ms: u64, hold_since_ms: Option<u64>) -> u64 {
    match hold_since_ms {
        Some(held) => oldest_entry_ms.min(held),
        None => oldest_entry_ms,
    }
}

/// Whether this pane's undelivered work freshly crosses the still-queued notice
/// threshold this poll tick — pure edge trigger so the impure caller can
/// fire the notice exactly once per queue lifetime rather than every 2s
/// poll once past 30 minutes. `undelivered_since_ms` comes from
/// [`undelivered_since`] (#560; it was the front entry's `enqueued_ms` before,
/// which a stranded marker could restart); `already_notified` tracks whether
/// this exact queue already fired it.
pub fn should_fire_still_queued_notice(
    undelivered_since_ms: u64,
    now_ms: u64,
    already_notified: bool,
) -> bool {
    !already_notified
        && now_ms.saturating_sub(undelivered_since_ms) >= QUEUE_STILL_QUEUED_NOTICE_AFTER.as_millis() as u64
}

/// #814: how long a pane's oldest queued delivery may sit before the header
/// badge calls the queue **stalled** rather than merely busy.
///
/// One minute, and the two neighbours it sits between are what size it. Below:
/// `QUEUE_DRAIN_POLL` is 2 s, so a healthy pane clears an entry within a few
/// polls and an ordinary burst of worker reports is gone long before this — a
/// threshold in the seconds would call normal traffic stalled and train the
/// human to ignore the badge, which is the failure mode `ChipGuard` and
/// `pressure_notice` both exist to avoid. Above: `QUEUE_STILL_QUEUED_NOTICE_AFTER`
/// is 30 minutes, and that notice is the *agent-facing* channel of last resort;
/// a human watching the window should not have to wait half an hour to be told
/// what a glance could tell them, which is the whole complaint #814 was filed
/// for. So: long enough that flowing traffic never trips it, short enough that
/// a stuck prompt is visible while the human is still at the machine.
///
/// Measured against [`undelivered_since`], not against the queue front — see
/// that function for why the front can be the *youngest* entry on exactly the
/// pane that is most stuck.
pub const QUEUE_STALLED_AFTER: Duration = Duration::from_secs(60);

/// #814: the resolution the queue badge's age is reported at — 1 s while the
/// wait is under a minute, 1 min once it is over.
///
/// **This is a rate bound, not cosmetics.** The badge is refreshed by re-pushing
/// the reading on the attention tick (`OrchRegistry::queue_depth_push`), and that
/// push is skipped when the new set is identical to the last one sent. A raw
/// millisecond age is never identical, so every tick would emit — and a Tauri
/// emit is a JS compile on the webview thread (`doc/design/performance.md` §1),
/// paid whether or not the human could see any difference. Coarsening to what
/// the badge can actually *render* differently makes the skip real: a pane stuck
/// for an hour emits about once a minute instead of once every 3 s.
///
/// Flooring, never rounding: a badge must not claim a wait is longer than it is.
pub fn coarsen_waiting_ms(waiting_ms: u64) -> u64 {
    const MINUTE: u64 = 60_000;
    if waiting_ms < MINUTE {
        waiting_ms - waiting_ms % 1_000
    } else {
        waiting_ms - waiting_ms % MINUTE
    }
}

/// #814: one pane's live delivery-queue reading, as the frontend's header badge
/// shows it — an item of the `orch-queue-depth` event's payload.
///
/// **Why the wait is a duration and not a timestamp.** An `enqueued_ms` would be
/// the more primitive fact, and it would also be a *frozen* one on screen: the
/// frontend has no clock of its own here (#814 deliberately adds no timer — see
/// `test/perfpolicy.test.ts`'s INV-4 manifest), so an age computed once at
/// arrival would still read "2m" an hour later. The backend re-derives the wait
/// on each attention tick instead, coarsened by [`coarsen_waiting_ms`], which is
/// what keeps the number on screen true without a frontend cadence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QueueDepthItem {
    /// The pane. Keyed by pty like `orch-delivery-held`, because the badge is a
    /// property of a terminal and a plain pane has no agent id.
    pub pty_id: u32,
    /// Whose pane it is, for the tooltip; empty when nothing can name it.
    pub agent_id: String,
    /// Deliveries waiting right now, 1..=`QUEUE_MAX_PER_PANE` (a pane with an
    /// empty queue has no item at all).
    pub depth: usize,
    /// `QUEUE_MAX_PER_PANE`, carried rather than duplicated frontend-side so
    /// "3/8" cannot drift from the cap the backend actually enforces.
    pub cap: usize,
    /// How long the oldest undelivered work has been waiting, coarsened.
    pub waiting_ms: u64,
    /// Past [`QUEUE_STALLED_AFTER`] — "this is not flowing", the state #814's
    /// incident needed at a glance.
    pub stalled: bool,
}

/// #814/#560: the oldest stamp actually in this queue, or `None` if it is empty.
///
/// The **minimum**, never `front().enqueued_ms` — a `StrandedSubmit` marker is
/// pushed to the front carrying a fresh stamp, so on exactly the pane whose
/// prompt has been sitting unsubmitted the longest, the front is the *youngest*
/// entry (see [`undelivered_since`], which fixed the same defect for the
/// 30-minute notice). Named here so the badge and that notice read one rule
/// rather than two copies of it.
pub fn oldest_enqueued_ms(entries: &VecDeque<QueuedDelivery>) -> Option<u64> {
    entries.iter().map(|e| e.enqueued_ms).min()
}

/// #814: build a pane's badge reading, or `None` when its queue is empty.
///
/// Pure over facts a caller can read out of the queue map cheaply — a count and
/// a minimum stamp ([`oldest_enqueued_ms`]) — plus the pane's open hold episode,
/// so the badge's clock is the SAME clock the 30-minute still-queued notice is
/// measured from ([`undelivered_since`]): a human reading "12m" on the badge and
/// an orchestrator reading that notice are then talking about one number, not two
/// that happen to be near each other.
///
/// `hold_since_ms` is the pane's open hold episode (`hold_episode_since`), and it
/// is the other half of making a stranded pane's badge honest — the entries left
/// queued behind a pasted-but-unsubmitted delivery can all be young while the
/// pane itself has been stuck for half an hour.
pub fn queue_depth_item(
    pty_id: u32,
    agent_id: &str,
    depth: usize,
    oldest_entry_ms: u64,
    hold_since_ms: Option<u64>,
    now_ms: u64,
) -> Option<QueueDepthItem> {
    if depth == 0 {
        return None;
    }
    let waiting_since = undelivered_since(oldest_entry_ms, hold_since_ms);
    // `saturating_sub`, not a subtraction: `enqueued_ms` and `now_ms` come from
    // the same wall clock, which can step BACKWARD (an NTP correction, a manual
    // change), and the honest reading then is "no wait yet" rather than a wrap
    // to ~584 million years and a badge that says the queue is stalled.
    let waiting_ms = now_ms.saturating_sub(waiting_since);
    Some(QueueDepthItem {
        pty_id,
        agent_id: agent_id.to_string(),
        depth,
        cap: QUEUE_MAX_PER_PANE,
        waiting_ms: coarsen_waiting_ms(waiting_ms),
        stalled: waiting_ms >= QUEUE_STALLED_AFTER.as_millis() as u64,
    })
}

/// The on-disk snapshot format version (#468). Bumped only for a change a
/// reader cannot absorb through `#[serde(default)]`; `parse_snapshot`
/// refuses a version it does not know rather than guessing at the shape,
/// because the failure direction of guessing is replaying the wrong bytes
/// into somebody's terminal.
pub const SNAPSHOT_VERSION: u32 = 1;

/// One entry as it sits in a group's `queue.json` (#468): the live entry
/// plus the pane it was queued for.
///
/// `pty_id` is recorded for FORENSICS ONLY and is deliberately never used to
/// re-bind on recovery — it is stale the instant the process exits (the
/// terminal layer re-mints pty ids on every restore), and a recovery that
/// trusted it would paste one pane's backlog into whatever unrelated pane
/// inherited its number. Rebinding goes through `QueuedDelivery`'s
/// `to_orchestrator`/`session_id` instead. It stays in the file because a
/// human reading a post-crash snapshot alongside `audit.jsonl` (whose
/// `agent-bind` lines carry the same number) needs to see which pane an
/// entry was for.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedEntry {
    pub pty_id: u32,
    #[serde(flatten)]
    pub delivery: QueuedDelivery,
}

/// A whole group's queued deliveries, as written to `queue.json` (#468).
///
/// **A snapshot, not a journal.** Every mutation rewrites the whole file
/// through `atomic_write` — the #133 precedent (temp file, fsync, rename),
/// which is what makes a crash or a full disk leave the PREVIOUS good
/// snapshot rather than a truncated file. An append-only journal (#240's
/// precedent, which `audit.jsonl` uses) was the other candidate and is
/// deliberately not what this is: a journal of queue events would need
/// replay logic to reconstruct the current state, compaction to stay
/// bounded, and would leave a half-replayed queue reachable if a record
/// were ever lost — three failure modes bought for nothing, since a pane's
/// queue is capped at `QUEUE_MAX_PER_PANE` (8) entries and the whole file is
/// therefore tiny. The audit log remains the append-only event history;
/// this file is only ever "what is queued right now."
///
/// `entries` are in exact drain order: per pane, front first, and panes in
/// ascending `pty_id` so two snapshots of the same state are byte-identical
/// (a stable file is a diffable one).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub version: u32,
    pub written_ms: u64,
    pub entries: Vec<PersistedEntry>,
}

/// Serialize a group's live queues for `atomic_write`. Pretty-printed
/// deliberately: this file is read by humans doing post-crash forensics far
/// more often than by the program, and it is at most a few KB.
pub fn serialize_snapshot(written_ms: u64, entries: Vec<PersistedEntry>) -> String {
    let snap = QueueSnapshot { version: SNAPSHOT_VERSION, written_ms, entries };
    // `to_string_pretty` on a plain struct of plain data cannot fail; the
    // fallback keeps that from being an `unwrap` on a durability path.
    serde_json::to_string_pretty(&snap).unwrap_or_else(|_| String::from("{\"version\":1,\"written_ms\":0,\"entries\":[]}"))
}

/// Read a `queue.json` back. **Tolerant per entry, strict about version.**
///
/// A snapshot is written by a process that may have been killed mid-life,
/// on a disk that may have been full, and it is read at startup where a
/// panic or a hard error would take the whole orchestration down with it —
/// so a file that will not parse at all, or whose `version` this build does
/// not know, yields nothing (the caller then falls back to the audit-derived
/// orphan view, which needs no snapshot), and a single malformed entry costs
/// only that entry rather than every entry beside it. Returns the entries it
/// could read plus the number it had to skip, so the caller can audit the
/// skips instead of losing them silently.
pub fn parse_snapshot(text: &str) -> (Vec<PersistedEntry>, usize) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return (Vec::new(), 0);
    };
    let version = value.get("version").and_then(serde_json::Value::as_u64).unwrap_or(0);
    if version != SNAPSHOT_VERSION as u64 {
        return (Vec::new(), 0);
    }
    let Some(raw) = value.get("entries").and_then(serde_json::Value::as_array) else {
        return (Vec::new(), 0);
    };
    let mut out = Vec::with_capacity(raw.len());
    let mut skipped = 0usize;
    for item in raw {
        match serde_json::from_value::<PersistedEntry>(item.clone()) {
            Ok(e) => out.push(e),
            Err(_) => skipped += 1,
        }
    }
    (out, skipped)
}

/// What a restart can and cannot do with a recovered snapshot — the split
/// `OrchRegistry::recover_persisted_queue` acts on.
#[derive(Clone, Debug, Default)]
pub struct RecoverySplit {
    /// `Text` payloads, in the file's own (drain) order. Held for the pane
    /// they were addressed to and re-admitted if it comes back.
    pub replayable: Vec<PersistedEntry>,
    /// `StrandedSubmit` markers, which a restart makes meaningless — see
    /// `split_recovered`'s doc. Audited and surfaced, never replayed.
    pub markers: Vec<PersistedEntry>,
}

/// Split a recovered snapshot into what may be replayed and what may not.
///
/// The one thing here that is a JUDGEMENT and not bookkeeping: a
/// `StrandedSubmit` marker means "this pane's input box already holds the
/// text; press Enter." Its whole meaning is a live pty's box contents, which
/// a restart destroys — the pane is gone, and the pane that comes back has
/// its own (possibly human-typed) box. Replaying the marker would press
/// Enter on whatever that is. So markers are dropped from the replay set,
/// deliberately and loudly (the caller audits each one), and the text they
/// referred to is genuinely lost: it was already pasted into a terminal that
/// no longer exists, so unlike a `Text` entry there are no bytes left to
/// re-send. That loss is REPORTED rather than papered over — see
/// `stranded_lost_notice`.
pub fn split_recovered(entries: Vec<PersistedEntry>) -> RecoverySplit {
    let mut split = RecoverySplit::default();
    for e in entries {
        match e.delivery.payload {
            QueuedPayload::Text(_) => split.replayable.push(e),
            QueuedPayload::StrandedSubmit => split.markers.push(e),
        }
    }
    split
}

// ---------- the staged-orphan archive (#547) ----------

/// How long a staged orphan stays in the HOT snapshot before it rolls off
/// into the append-only archive (#547).
///
/// **Why an archive at all, and why this is not the age cutoff #468
/// refused.** `persist_queues` rewrites and fsyncs the WHOLE `queue.json` on
/// every admission, and since #523 that file carries the staged set as well
/// as the live queues. Staged entries are deliberately never cleared (see
/// `OrchRegistry::queue_orphans`), and worker panes do not survive a restart
/// — so every restart permanently adds that restart's unbindable backlog,
/// and the per-delivery write cost grows without bound. Rolling the tail into
/// `queue-orphans-archive.jsonl` moves those bytes off the write path
/// *without moving them off the recovery path*: `queue_orphans` reads the
/// archive, `readmit_archived` re-binds out of it, and every roll is audited
/// per entry. Nothing is dropped, capped away, or inferred to have been
/// acknowledged — which is what #547 explicitly rules out and what #468's
/// "no age cutoff" bullet is about. This is a change of *file*, not of
/// disposition.
///
/// 24h because the rebind window is minutes, not days: a pane rebinds when
/// its session is resumed, which happens in the restore that follows the
/// restart. A day keeps a full working day of orphans in the file a human
/// opens first, and still bounds the steady state to one day's restarts
/// rather than to the life of the install.
pub const STAGED_ARCHIVE_AFTER_MS: u64 = 24 * 60 * 60 * 1000;

/// Hard backstop on how many staged entries stay hot (#547), applied
/// **regardless of age** — because a bound with an "unless it is recent"
/// exception is not a bound, and a crash loop stages a fresh backlog every
/// few seconds. 64 is eight full panes at `QUEUE_MAX_PER_PANE`, so one whole
/// fleet's worth of one restart's backlog still rides in the hot file.
pub const STAGED_HOT_MAX_ENTRIES: usize = 64;

/// Hard backstop on staged *payload bytes* kept hot (#547). The entry count
/// alone does not bound the write, because a queued payload is a whole task
/// brief and nothing clamps what is STORED — `ORPHAN_TEXT_CAP_BYTES` clamps
/// only what `queue_orphans` hands back. 64 KiB is ~8 full briefs; past that
/// the per-admission fsync stops being the "few KB" the snapshot design
/// assumed.
pub const STAGED_HOT_MAX_BYTES: usize = 64 * 1024;

/// Why one staged entry rolled off the hot snapshot (#547). Carried in the
/// archive line and in the `queue-orphan-archived` audit line, because "it
/// moved" is not enough for a human reconstructing what happened — the three
/// causes have different follow-ups (a slow leak vs. a burst).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveReason {
    /// Staged longer than [`STAGED_ARCHIVE_AFTER_MS`].
    Age,
    /// The hot set was over [`STAGED_HOT_MAX_ENTRIES`]; this was among the
    /// oldest.
    Entries,
    /// The hot set was over [`STAGED_HOT_MAX_BYTES`]; this was among the
    /// oldest.
    Bytes,
}

impl ArchiveReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ArchiveReason::Age => "staged-past-hot-window",
            ArchiveReason::Entries => "staged-entry-backstop",
            ArchiveReason::Bytes => "staged-byte-backstop",
        }
    }
}

/// The three numbers the archive decision is made on (#547), so the decision
/// itself is pure and testable and the caller never has to CLONE the staged
/// set to ask the question — the whole point is to stop paying per admission
/// for entries nobody is reading.
#[derive(Clone, Copy, Debug)]
pub struct ArchivePolicy {
    pub max_age_ms: u64,
    pub max_entries: usize,
    pub max_bytes: usize,
}

impl Default for ArchivePolicy {
    fn default() -> Self {
        Self {
            max_age_ms: STAGED_ARCHIVE_AFTER_MS,
            max_entries: STAGED_HOT_MAX_ENTRIES,
            max_bytes: STAGED_HOT_MAX_BYTES,
        }
    }
}

/// One staged entry, reduced to what [`plan_archive`] needs. Built by walking
/// the staging maps under their lock; deliberately `Copy`-cheap so that walk
/// is a projection rather than a copy of every payload.
#[derive(Clone, Copy, Debug)]
pub struct StagedCost {
    pub id: u64,
    pub enqueued_ms: u64,
    /// Payload bytes this entry contributes to `queue.json`. A marker
    /// contributes 0 — it has no text — which is correct: a marker costs the
    /// write almost nothing and is the one entry with no bytes left to lose.
    pub bytes: usize,
}

/// Decide which staged ids roll off the hot snapshot, oldest first (#547).
///
/// Pure, and returns *ids* rather than entries so the caller can plan under
/// the staging lock without cloning anything it is not about to move.
///
/// Order of the three rules is the argument, not an implementation detail:
///
/// 1. **Age first**, because an entry past the window rolls whether or not
///    the set is over any cap — that is the steady-state leak #547 filed.
/// 2. **Then the entry backstop, oldest id first.** Ids are monotonic per
///    group, so lowest id is oldest ask, which is the one with the weakest
///    remaining claim on a live pane.
/// 3. **Then the byte backstop**, over what survived (2), same order.
///
/// A single entry larger than `max_bytes` rolls the whole hot set to empty
/// rather than looping — bounded, and the entry is in the archive either way.
pub fn plan_archive(
    staged: &[StagedCost],
    now_ms: u64,
    policy: &ArchivePolicy,
) -> Vec<(u64, ArchiveReason)> {
    let mut hot: Vec<&StagedCost> = Vec::with_capacity(staged.len());
    let mut rolled: Vec<(u64, ArchiveReason)> = Vec::new();
    let mut by_age: Vec<&StagedCost> = staged.iter().collect();
    by_age.sort_by_key(|c| c.id);
    for c in by_age {
        if now_ms.saturating_sub(c.enqueued_ms) >= policy.max_age_ms {
            rolled.push((c.id, ArchiveReason::Age));
        } else {
            hot.push(c);
        }
    }
    let mut cut = 0usize;
    let mut bytes: usize = hot.iter().map(|c| c.bytes).sum();
    while hot.len() - cut > policy.max_entries {
        bytes = bytes.saturating_sub(hot[cut].bytes);
        rolled.push((hot[cut].id, ArchiveReason::Entries));
        cut += 1;
    }
    while bytes > policy.max_bytes && cut < hot.len() {
        bytes = bytes.saturating_sub(hot[cut].bytes);
        rolled.push((hot[cut].id, ArchiveReason::Bytes));
        cut += 1;
    }
    rolled.sort_by_key(|(id, _)| *id);
    rolled
}

/// The on-disk version of ONE archive line (#547). Per line, not per file:
/// the archive is append-only, so a build that changes the record shape has
/// to coexist with lines an older build already wrote — a file-level version
/// could only say what the NEWEST writer thought.
pub const ARCHIVE_LINE_VERSION: u32 = 1;

fn archive_line_version() -> u32 {
    ARCHIVE_LINE_VERSION
}

/// One line of a group's `queue-orphans-archive.jsonl` (#547): the staged
/// entry exactly as `queue.json` held it, plus when and why it rolled off.
///
/// **Compatible in both directions, following `QueuedDelivery`'s serde
/// precedent.** Forward: `v` carries `#[serde(default)]` so a line written
/// before it existed still parses, and [`parse_archive`] skips-and-counts a
/// line whose `v` this build does not know rather than guessing at its shape
/// (`parse_snapshot`'s stance, for the same reason — the failure direction of
/// guessing is replaying the wrong bytes into a terminal). Backward: an older
/// build simply never opens this file; it reads a `queue.json` that is a
/// valid, smaller snapshot of the same schema, so a downgrade loses SIGHT of
/// archived entries but destroys nothing — they are still on disk and the
/// audit-derived orphan view still names their ids.
///
/// `entry` is nested rather than `#[serde(flatten)]`ed. `PersistedEntry`
/// already flattens `QueuedDelivery`, and a second flatten around it would
/// buy a slightly flatter object at the cost of a nested-flatten shape; an
/// explicit `entry` object also reads better in the file a human opens after
/// a crash, which is what `QueuedPayload`'s explicit tag is about too.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchivedEntry {
    #[serde(default = "archive_line_version")]
    pub v: u32,
    /// When the roll happened — distinct from `enqueued_ms`, which is when
    /// the delivery was first queued and is what staleness is judged on.
    pub archived_ms: u64,
    /// [`ArchiveReason::as_str`].
    pub why: String,
    pub entry: PersistedEntry,
}

/// Build one archive record. Split from [`archive_line`] so the roll's two
/// halves — deciding what a record says, and turning it into a line — stay
/// separable and separately testable.
///
/// **Nothing re-serializes a record it read back.** A rewrite carries the
/// original line through verbatim ([`scan_archive`]); re-emitting through
/// this type would silently normalize away anything the reading build did
/// not understand, which is the defect [`scan_archive`]'s doc describes.
pub fn new_archive_record(archived_ms: u64, why: ArchiveReason, entry: PersistedEntry) -> ArchivedEntry {
    ArchivedEntry { v: ARCHIVE_LINE_VERSION, archived_ms, why: why.as_str().to_string(), entry }
}

/// Serialize one archive record as its file line, WITHOUT the newline — the
/// caller owns batching, because an append is atomic per `write_all` syscall
/// and a batch has to reach the OS as one buffer (`append_audit`'s rule 1).
///
/// Compact, not pretty-printed, unlike `serialize_snapshot`: this file is one
/// record per line by construction, and a pretty-printed record would span
/// lines and stop being one.
pub fn archive_line(rec: &ArchivedEntry) -> String {
    // Same fallback discipline as `serialize_snapshot`: a durability path
    // must not carry an `unwrap`. A record that will not serialize is skipped
    // by the caller (an empty line parses as nothing) rather than panicking
    // the delivery that triggered the roll.
    serde_json::to_string(rec).unwrap_or_default()
}

/// Read a `queue-orphans-archive.jsonl` back, tolerant per line and strict
/// about the per-line version — `parse_snapshot`'s stance, applied to an
/// append-only file.
///
/// **Deduped by delivery id, first occurrence wins.** The archive can
/// legitimately hold an id twice: the roll appends BEFORE it removes from
/// staging (so a crash in between leaves a copy in both stores, never
/// neither), and the next roll re-appends the entry the crash left staged.
/// Both records describe the same delivery; the first is the original.
/// Returns what it could read plus the number of lines it had to skip, so the
/// caller audits the skip instead of coming up silently short.
pub fn parse_archive(text: &str) -> (Vec<ArchivedEntry>, usize) {
    let mut out: Vec<ArchivedEntry> = Vec::new();
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut skipped = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ArchivedEntry>(line) {
            Ok(rec) if rec.v == ARCHIVE_LINE_VERSION => {
                if seen.insert(rec.entry.delivery.id) {
                    out.push(rec);
                }
            }
            _ => skipped += 1,
        }
    }
    (out, skipped)
}

/// One raw archive line, kept **verbatim**, plus the little this build can
/// tell about it without claiming to understand it (#547 review B1).
///
/// See [`scan_archive`] for why this exists at all.
pub struct ArchiveLine<'a> {
    /// The line exactly as it sits in the file. What a rewrite writes back
    /// when it is not deliberately removing this record — byte for byte, so
    /// a record this build cannot interpret is not silently normalized into
    /// what this build would have written.
    pub raw: &'a str,
    /// The delivery id, read **without interpreting the record**: probed
    /// straight out of the JSON rather than through [`ArchivedEntry`], so it
    /// works across versions this build does not know. `None` for a line
    /// that is not JSON at all, or whose shape has moved far enough that the
    /// id is no longer where it was — and a line with no id can never be
    /// matched, so it can never be removed.
    pub id: Option<u64>,
}

/// Read the archive as **lines**, not as records (#547 review B1).
///
/// **Why this exists, and why [`parse_archive`] cannot be used for it.**
/// `parse_archive` is deliberately lossy: it skips a line whose version this
/// build does not know, because interpreting one would mean guessing at a
/// shape. That is the right stance for every path that ACTS on records — the
/// orphan report, the rebind — and exactly the wrong one for the path that
/// REWRITES the file. Reconstructing the file from parsed records deletes
/// every line the parse dropped, which turns the one mechanism that exists to
/// survive version skew into the thing that destroys it: a newer build writes
/// `v: 2` lines, a rollback follows, and the next bind that re-admits
/// anything erases them. It also blinds the `queue_seq` seed to the ids on
/// those lines, re-opening the id collision the seed exists to prevent.
///
/// So the rewrite and the seed both read the file this way instead: every
/// line is carried, and parsing is used only to answer "is THIS the record I
/// am deliberately removing?". Nothing is dropped because this build could
/// not read it — which is the guarantee the archive is for, made structural
/// rather than dependent on the reader's vocabulary.
///
/// Blank lines are skipped: they carry nothing and re-emitting them would
/// grow the file by one byte per rewrite.
pub fn scan_archive(text: &str) -> Vec<ArchiveLine<'_>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|raw| {
            let id = serde_json::from_str::<serde_json::Value>(raw)
                .ok()
                .and_then(|v| v.get("entry").and_then(|e| e.get("id")).and_then(serde_json::Value::as_u64));
            ArchiveLine { raw, id }
        })
        .collect()
}

/// Whether a recovered entry has a durable identity to re-bind to when
/// `agent` comes back (#467). Pure so the matching rule is stated in one
/// place and tested directly, rather than being an `if` buried in a bind
/// callback: an entry re-binds when it was addressed to this group's
/// orchestrator and `agent` IS that orchestrator, or when both carry the
/// SAME non-empty CLI session id. Everything else — including two entries
/// that merely share an `agent_id`, which a restore never revives (#524
/// stopped it naming a different agent; it still names no live one) and
/// which therefore proves nothing — does not match.
pub fn rebinds_to(entry: &QueuedDelivery, agent_is_orchestrator: bool, agent_session: Option<&str>) -> bool {
    if entry.to_orchestrator && agent_is_orchestrator {
        return true;
    }
    match (entry.session_id.as_deref(), agent_session) {
        (Some(a), Some(b)) => !a.is_empty() && a == b,
        _ => false,
    }
}

/// The `[loomux]` notice announcing that a restart's queued deliveries were
/// re-admitted to a pane that came back for them (#467). Says how many and
/// how long they waited, because "delivers automatically" is not enough
/// information when the wait spanned a restart: the recipient needs to judge
/// whether an hours-old ask still applies, which is exactly the staleness
/// judgement #468's filing said durability should leave to a human rather
/// than resolve with an age cutoff.
pub fn recovered_notice(agent_id: &str, count: usize, oldest_minutes: u64) -> String {
    let n = if count == 1 { "1 delivery".to_string() } else { format!("{count} deliveries") };
    format!(
        "[loomux] {n} to {agent_id} queued before a loomux restart {oldest_minutes} min ago have been \
         re-queued in their original order and are delivering now — judge staleness before acting on them"
    )
}

/// The notice for recovered entries that have NO pane to go back to (#467):
/// the common case after a restart, since worker panes do not survive one.
/// Not a drop — the payloads are intact and reported by `queue_orphans`,
/// which is what the orchestrator's session-start re-sync reads — so the
/// wording must not read like `dropped_notice`'s "not recoverable."
pub fn orphaned_notice(count: usize) -> String {
    let n = if count == 1 { "1 delivery".to_string() } else { format!("{count} deliveries") };
    format!(
        "[loomux] {n} queued before the last loomux restart could not be re-bound to a live pane — \
         call queue_orphans() to read them (payloads intact) and re-send what still applies"
    )
}

/// The notice for `StrandedSubmit` markers a restart made unreplayable —
/// the one genuinely lossy case in recovery, so it says so plainly rather
/// than folding into `orphaned_notice`'s "payloads intact." See
/// `split_recovered`'s doc for why a marker cannot survive a restart.
pub fn stranded_lost_notice(count: usize) -> String {
    let n = if count == 1 { "1 delivery".to_string() } else { format!("{count} deliveries") };
    format!(
        "[loomux] {n} had already been typed into a pane and was waiting only for Enter when loomux \
         restarted — that pane is gone, so the text is NOT recoverable; check the `prompt` audit lines \
         for what it was and re-send if it still applies"
    )
}

/// The `reason` a surfaced `StrandedSubmit` marker carries (#467), instead
/// of the `EnqueueReason` it was admitted under — the admission reason
/// ("question") would say why it was queued, when what the reader needs is
/// why it cannot be replayed. Paired with `text: null`, which for this one
/// case does not mean "an older build queued it" but "the bytes went into a
/// terminal that no longer exists."
///
/// **One string, every channel** (review round 1, finding 3). This is the
/// value in the `queue-stranded-unreplayable` audit line, in the orphan
/// row's `reason`, and in the MCP tool's own description — it shipped for
/// review as `stranded-marker-not-replayable` in the audit and
/// `stranded-submit-not-replayable` everywhere else, so a human grepping the
/// audit log for the string the tool had just shown them found nothing.
pub const STRANDED_ORPHAN_REASON: &str = "stranded-submit-not-replayable";

/// Where a surfaced orphan was reconstructed from (#467). Both sources are
/// real and neither subsumes the other: the snapshot carries the payload
/// bytes but only exists for entries queued by a build that writes one; the
/// audit derivation works on any group's history back to whenever
/// `delivery-queued` started being written, but knows only that an id was
/// queued and never resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrphanSource {
    /// Read out of the group's `queue.json` — payload included.
    Snapshot,
    /// Derived from `audit.jsonl` alone — no payload.
    Audit,
    /// Read out of the group's `queue-orphans-archive.jsonl` (#547) —
    /// payload included, exactly like `Snapshot`.
    ///
    /// **A third value on an existing field, added additively** (the #579
    /// precedent for `refused`): a reader written against the two-value shape
    /// sees no change on the rows it already knew, and the new value is
    /// documented in the tool description rather than inferred. It is a
    /// distinct value rather than more `Snapshot` rows because the two say
    /// different things to a human doing forensics — which FILE to open — and
    /// because collapsing them would make the archive invisible in the one
    /// view that exists to make lost work visible.
    Archive,
}

impl OrphanSource {
    pub fn as_str(self) -> &'static str {
        match self {
            OrphanSource::Snapshot => "snapshot",
            OrphanSource::Audit => "audit",
            OrphanSource::Archive => "archive",
        }
    }
}

/// One delivery that entered the queue and never reached a terminal event —
/// a restart caught it mid-wait. Derived from a group's `queue.json`
/// snapshot (payload intact) or, for entries with no snapshot to read, from
/// `audit.jsonl` alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrphanedQueueEntry {
    pub id: u64,
    pub agent_id: String,
    pub enqueued_ms: u64,
    pub reason: String,
    /// The payload bytes, when the snapshot had them (`OrphanSource::
    /// Snapshot`). `None` from the audit derivation — `audit.jsonl`'s
    /// `delivery-queued` line carries the id, target and reason but not the
    /// text; the paired `prompt` line does, which is what the tool
    /// description points a reader at rather than this field pretending to.
    pub text: Option<String>,
    pub source: OrphanSource,
}

/// How much of a recovered payload `queue_orphans` hands back verbatim
/// (#467). Generous on purpose — the tool exists so the orchestrator can
/// RE-SEND lost work, and a brief cut to a preview is not re-sendable — but
/// still a cap, because a pane's queue can hold eight full task briefs and
/// an orchestrator's context is this codebase's scarcest resource. A cut is
/// always named in-band; see `OrchRegistry::queue_orphans_json`.
pub const ORPHAN_TEXT_CAP_BYTES: usize = 8192;

/// Truncate `text` to at most `cap` bytes on a char boundary, appending a
/// marker that says so. Never silently short: a caller handing this to an
/// agent must be able to tell a complete payload from a clipped one, which
/// is the same "a claim is a deliverable" rule the rest of this module's
/// notice strings follow.
pub fn clamp_payload(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[… truncated at {cap} bytes of {} — the full payload is in this group's audit log, on the `prompt` line for this delivery]", &text[..end], text.len())
}

/// Merge the two orphan views into the one an orchestrator reads (#467).
///
/// Snapshot entries WIN on a shared id: both describe the same delivery, and
/// only the snapshot has its payload. Sorted by id, which for a monotonic
/// `queue_seq` is enqueue order — so the list reads oldest-ask-first, the
/// order it would have delivered in.
///
/// **`live` is what keeps this a list of LOSSES rather than a list of
/// pending work**, and it is not an optimization. The audit derivation's
/// rule — queued, never resolved — is satisfied by every entry sitting in
/// the queue right now, because an entry that has not been delivered yet has
/// not been audited as delivered yet either. So without this filter the tool
/// would hand the orchestrator its own in-flight deliveries and tell it they
/// were lost, and the documented response to a non-empty result is to
/// RE-SEND — turning the recovery feature into a duplicate-delivery
/// generator. Anything currently in memory is excluded, including entries
/// this same restart just re-admitted (which are delivering, not lost).
pub fn merge_orphans(
    snapshot: Vec<OrphanedQueueEntry>,
    audit: Vec<OrphanedQueueEntry>,
    live: &std::collections::HashSet<u64>,
) -> Vec<OrphanedQueueEntry> {
    let mut out = snapshot;
    let seen: std::collections::HashSet<u64> = out.iter().map(|e| e.id).collect();
    out.extend(audit.into_iter().filter(|e| !seen.contains(&e.id)));
    out.retain(|e| !live.contains(&e.id));
    out.sort_by_key(|e| e.id);
    out
}

/// One audit-derived fact about a delivery that entered the queue but has
/// no matching terminal event (`delivery-dequeued` / `delivery-dropped` /
/// `delivery-coalesced`) for the same `id` — i.e. a restart happened while
/// it was still waiting. Kept as the fallback view for groups with no
/// `queue.json` to read (a snapshot written by an older build, or none at
/// all): `OrchRegistry::queue_orphans` merges it under the snapshot
/// derivation, which carries payloads this one cannot.

/// One audit line's shape, as far as this function needs it — deliberately
/// minimal (not the full `AuditEntry`) so this stays testable with hand-built
/// fixtures and doesn't need to import `serde_json::Value` parsing here.
pub struct QueueAuditLine<'a> {
    pub action: &'a str,
    pub id: Option<u64>,
    pub agent_id: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub ts_ms: u64,
}

/// Scan a group's audit lines (oldest first, as `parse_audit_lines` already
/// returns them) for `delivery-queued` ids with no later
/// `delivery-dequeued`/`delivery-dropped`/`delivery-recovered` for that SAME
/// id — a queue entry that a restart caught mid-wait. `delivery-coalesced`
/// entries are not scanned for orphans: a coalesced payload was never
/// independently queued — it was folded into the entry it duplicated, whose
/// own id is what to check.
///
/// `delivery-recovered` (#467) is terminal here, and **only ever written at
/// the moment an entry actually leaves staging** — `readmit_recovered`, once
/// a FRESH `delivery-queued` id is tracking the same payload. Closing it any
/// earlier is a real defect, caught in review round 1: the first version
/// wrote it when an entry was merely STAGED, which made this derivation —
/// the one view that needs no snapshot to work — blind to exactly the
/// entries that had not been re-bound. Paired with a snapshot that then held
/// live queues only, a second restart lost them with no trace in either
/// channel: strictly worse than never having persisted at all, since
/// pre-#468 this scan would at least have kept reporting the ids forever.
///
/// So the rule this encodes: **an id is closed here only once its
/// disposition is durable somewhere else.** Until then both views report it
/// and `merge_orphans` dedupes them, snapshot first (it has the payload).
/// That is also why a `StrandedSubmit` marker a restart made unreplayable
/// audits as the NON-terminal `queue-stranded-unreplayable` rather than
/// `delivery-dropped`: it never leaves staging, so it must never stop being
/// reported.
pub fn orphaned_queue_entries(lines: &[QueueAuditLine]) -> Vec<OrphanedQueueEntry> {
    let mut open: std::collections::HashMap<u64, (String, u64, String)> = std::collections::HashMap::new();
    for l in lines {
        match l.action {
            "delivery-queued" => {
                if let Some(id) = l.id {
                    open.insert(
                        id,
                        (
                            l.agent_id.unwrap_or_default().to_string(),
                            l.ts_ms,
                            l.reason.unwrap_or_default().to_string(),
                        ),
                    );
                }
            }
            "delivery-dequeued" | "delivery-dropped" | "delivery-recovered" => {
                if let Some(id) = l.id {
                    open.remove(&id);
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<OrphanedQueueEntry> = open
        .into_iter()
        .map(|(id, (agent_id, enqueued_ms, reason))| OrphanedQueueEntry {
            id,
            agent_id,
            enqueued_ms,
            reason,
            text: None,
            source: OrphanSource::Audit,
        })
        .collect();
    out.sort_by_key(|e| e.id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_entry(id: u64, text: &str) -> QueuedDelivery {
        QueuedDelivery {
            id,
            agent_id: "w-1".into(),
            from: "loomux".into(),
            payload: QueuedPayload::Text(text.to_string()),
            reason: EnqueueReason::Question,
            enqueued_ms: 1_000,
            coalesced: 0,
            group: Some("g-1".try_into().unwrap()),
            to_orchestrator: false,
            session_id: None,
            delivery_kind: crate::model::Delivery::MidSession,
        }
    }

    /// #569: `text_entry` with the admission reason chosen — what
    /// `flush_cause` reads, and the only field that distinguishes a
    /// pause-held entry from a pane-blocked one.
    fn reason_entry(id: u64, text: &str, reason: EnqueueReason) -> QueuedDelivery {
        QueuedDelivery { reason, ..text_entry(id, text) }
    }

    // ---------- admit ----------

    #[test]
    fn admit_appends_to_an_empty_queue() {
        let q: VecDeque<QueuedDelivery> = VecDeque::new();
        assert_eq!(admit(&q, "hello", EnqueueReason::Arrival), AdmitDecision::Admit);
    }

    #[test]
    fn admit_coalesces_a_byte_identical_repeat() {
        let mut q = VecDeque::new();
        q.push_back(text_entry(1, "give me a status update"));
        assert_eq!(admit(&q, "give me a status update", EnqueueReason::Arrival), AdmitDecision::Coalesce);
    }

    #[test]
    fn admit_does_not_coalesce_semantically_similar_but_different_text() {
        // The design's whole argument: three DIFFERENT task briefs must all
        // deliver. Only an exact byte match collapses.
        let mut q = VecDeque::new();
        q.push_back(text_entry(1, "give me a status update"));
        assert_eq!(admit(&q, "give me a status update please", EnqueueReason::Arrival), AdmitDecision::Admit);
    }

    #[test]
    fn admit_never_coalesces_against_a_stranded_submit_marker() {
        let mut q = VecDeque::new();
        q.push_back(QueuedDelivery {
            id: 1,
            agent_id: "w-1".into(),
            from: "loomux".into(),
            payload: QueuedPayload::StrandedSubmit,
            reason: EnqueueReason::Question,
            enqueued_ms: 1_000,
            coalesced: 0,
            group: Some("g-1".try_into().unwrap()),
            to_orchestrator: false,
            session_id: None,
            delivery_kind: crate::model::Delivery::MidSession,
        });
        // Empty string would trivially "match" a bad comparison — pin the
        // marker is simply never a coalesce target at all.
        assert_eq!(admit(&q, "", EnqueueReason::Arrival), AdmitDecision::Admit);
    }

    #[test]
    fn admit_rejects_newest_at_the_cap() {
        let mut q = VecDeque::new();
        for i in 0..QUEUE_MAX_PER_PANE {
            q.push_back(text_entry(i as u64, &format!("distinct-{i}")));
        }
        assert_eq!(admit(&q, "one-more", EnqueueReason::Arrival), AdmitDecision::RejectFull);
    }

    #[test]
    fn admit_coalesce_takes_priority_over_a_full_queue() {
        // A duplicate of an already-queued entry must coalesce even when
        // the queue happens to be at cap — dropping it costs nothing (it's
        // already represented) and coalescing is strictly better than a
        // loud rejection for a payload that adds no information.
        let mut q = VecDeque::new();
        for i in 0..QUEUE_MAX_PER_PANE {
            q.push_back(text_entry(i as u64, &format!("distinct-{i}")));
        }
        assert_eq!(admit(&q, "distinct-0", EnqueueReason::Arrival), AdmitDecision::Coalesce);
    }

    #[test]
    fn only_the_pause_loss_notice_may_exceed_the_cap_and_only_by_its_headroom() {
        // #569 rev-128. The notice reports payloads the cap destroyed, so
        // refusing it to that same cap loses both the work and the record of
        // it — and on the orchestrator's own pane that refusal is certain, not
        // unlucky. The exemption is one entry, so "the cap is 8" stays true for
        // every other purpose.
        let mut q = VecDeque::new();
        for i in 0..QUEUE_MAX_PER_PANE {
            q.push_back(text_entry(i as u64, &format!("distinct-{i}")));
        }
        for ordinary in [
            EnqueueReason::Arrival,
            EnqueueReason::BehindQueue,
            EnqueueReason::BoxOccupied,
            EnqueueReason::Question,
            EnqueueReason::KickoffRecovery,
            EnqueueReason::Recovered,
            EnqueueReason::GroupPaused,
            EnqueueReason::StrandedSelfHeal,
            // #658: the refusal roster is deliberately NOT exempt — it fires on
            // the edge where depth just came back down, so it has room by
            // construction and needs no bypass. See the variant's own doc.
            EnqueueReason::RefusalRoster,
        ] {
            assert_eq!(
                admit(&q, "one-more", ordinary),
                AdmitDecision::RejectFull,
                "{ordinary:?} must not be exempt — the cap is the cap"
            );
            assert_eq!(ordinary.cap_headroom(), 0, "{ordinary:?}");
        }
        assert_eq!(
            admit(&q, "one-more", EnqueueReason::PauseLossNotice),
            AdmitDecision::Admit,
            "the loss notice gets in"
        );

        // …and the headroom is a bound, not a bypass: once spent, the next one
        // is refused like anything else.
        q.push_back(text_entry(99, "the notice"));
        assert_eq!(q.len(), QUEUE_MAX_PER_PANE + PAUSE_LOSS_NOTICE_HEADROOM);
        assert_eq!(
            admit(&q, "a second notice", EnqueueReason::PauseLossNotice),
            AdmitDecision::RejectFull,
            "the exemption is one entry deep, not an open door"
        );
    }

    #[test]
    fn an_exempt_admission_still_coalesces_rather_than_spending_its_headroom() {
        // Coalescing is checked before the cap for every reason, and must stay
        // that way here: a byte-identical repeat adds no information, so
        // spending the one-entry headroom on it would leave the real notice
        // with nowhere to go.
        let mut q = VecDeque::new();
        for i in 0..QUEUE_MAX_PER_PANE {
            q.push_back(text_entry(i as u64, &format!("distinct-{i}")));
        }
        assert_eq!(
            admit(&q, "distinct-3", EnqueueReason::PauseLossNotice),
            AdmitDecision::Coalesce
        );
    }

    // ---------- notice text ----------

    #[test]
    fn queued_notice_never_says_hold_or_re_send() {
        let n = queued_notice("w-5", EnqueueReason::Question);
        assert!(n.contains("queued"), "got: {n}");
        assert!(!n.to_lowercase().contains("held"), "must not use the misleading old word: {n}");
        assert!(n.contains("do NOT re-send"), "must warn against the duplicate-generating re-send: {n}");
    }

    #[test]
    fn queued_notice_names_why_by_reason() {
        assert!(queued_notice("w-5", EnqueueReason::BoxOccupied).contains("human input"));
        assert!(queued_notice("w-5", EnqueueReason::Question).contains("interactive question"));
    }

    #[test]
    fn flush_header_verb_agrees_with_its_count() {
        // #533: shipped as "1 delivery ... are now delivering" and a human
        // read it in a live pane. Both headers are pinned, since the
        // coalesced one repeats the construction.
        // The count and the verb are NOT adjacent — "1 delivery queued
        // while this pane was blocked is now delivering" — so each is
        // asserted where it actually sits. (An earlier draft asserted
        // "1 delivery is now" and reddened on the real, correct string.)
        let blocked = FlushCause::PaneBlocked;
        assert!(flush_header_text(1, 0, blocked).contains("1 delivery queued"), "{}", flush_header_text(1, 0, blocked));
        assert!(flush_header_text(1, 0, blocked).contains("blocked is now delivering"), "{}", flush_header_text(1, 0, blocked));
        assert!(flush_header_text(2, 0, blocked).contains("2 deliveries queued"), "{}", flush_header_text(2, 0, blocked));
        assert!(flush_header_text(2, 0, blocked).contains("blocked are now delivering"), "{}", flush_header_text(2, 0, blocked));
        assert!(flush_header_text(1, 1, blocked).contains("coalesced) is now"), "{}", flush_header_text(1, 1, blocked));
        let one = [FlushConstituent { id: 1, from: "w-2", enqueued_ms: 0, coalesced: 0, text: "x" }];
        assert!(coalesced_flush_text(&one, 0, 0, blocked).contains("1 delivery queued while this pane was blocked is being"),
            "{}", coalesced_flush_text(&one, 0, 0, blocked));
        let two = [
            FlushConstituent { id: 1, from: "w-2", enqueued_ms: 0, coalesced: 0, text: "x" },
            FlushConstituent { id: 2, from: "w-3", enqueued_ms: 0, coalesced: 0, text: "y" },
        ];
        assert!(coalesced_flush_text(&two, 0, 0, blocked).contains("2 deliveries queued while this pane was blocked are being"),
            "{}", coalesced_flush_text(&two, 0, 0, blocked));
    }

    #[test]
    fn flush_header_singular_and_plural_and_coalesced_clause() {
        let blocked = FlushCause::PaneBlocked;
        assert!(flush_header_text(1, 0, blocked).contains("1 delivery "), "{}", flush_header_text(1, 0, blocked));
        assert!(flush_header_text(3, 0, blocked).contains("3 deliveries"), "{}", flush_header_text(3, 0, blocked));
        let h = flush_header_text(3, 1, blocked);
        assert!(h.contains("1 coalesced"), "got: {h}");
        assert!(!flush_header_text(3, 0, blocked).contains("coalesced"), "must omit the clause when nothing coalesced");
        assert!(flush_header_text(3, 0, blocked).contains("oldest first"));
    }

    // ---------- #569: the flush header names the RIGHT hold ----------

    #[test]
    fn a_pause_held_flush_says_paused_not_pane_blocked() {
        // The whole point of `FlushCause`: a pause-held delivery arriving under
        // "queued while this pane was blocked" would send its reader hunting
        // for a box or a dialog that never existed. Both headers are pinned
        // because the coalesced one repeats the construction.
        let paused = FlushCause::GroupPaused;
        let h = flush_header_text(2, 0, paused);
        assert!(h.contains("queued while this group was paused"), "got: {h}");
        assert!(!h.contains("pane was blocked"), "must not claim a pane block that never happened: {h}");
        let items = [
            FlushConstituent { id: 1, from: "w-2", enqueued_ms: 0, coalesced: 0, text: "x" },
            FlushConstituent { id: 2, from: "w-3", enqueued_ms: 0, coalesced: 0, text: "y" },
        ];
        let c = coalesced_flush_text(&items, 0, 0, paused);
        assert!(c.contains("2 deliveries queued while this group was paused are being"), "got: {c}");
        assert!(!c.contains("pane was blocked"), "got: {c}");
    }

    #[test]
    fn flush_cause_is_paused_when_any_constituent_was_pause_held() {
        // An all-pane-blocked batch keeps the pre-#569 wording verbatim...
        let blocked = [reason_entry(1, "a", EnqueueReason::Question), reason_entry(2, "b", EnqueueReason::BehindQueue)];
        assert_eq!(flush_cause(&blocked), FlushCause::PaneBlocked);
        // ...and an empty batch cannot claim a pause it has no evidence for.
        assert_eq!(flush_cause(&[]), FlushCause::PaneBlocked);
        // A MIXED batch reports the pause: entries that had been sitting behind
        // a pane block were still there when the human paused, and the pause is
        // what decided when this flush happened. See `flush_cause`'s doc.
        let mixed = [
            reason_entry(1, "a", EnqueueReason::BoxOccupied),
            reason_entry(2, "b", EnqueueReason::GroupPaused),
        ];
        assert_eq!(flush_cause(&mixed), FlushCause::GroupPaused);
        let all_paused = [reason_entry(1, "a", EnqueueReason::GroupPaused)];
        assert_eq!(flush_cause(&all_paused), FlushCause::GroupPaused);
    }

    // ---------- #533-A: coalesced flush ----------

    fn marker_entry(id: u64) -> QueuedDelivery {
        // #468's three durability fields carry no meaning for #533-A's
        // flush-planning tests (a marker is never replayed across a restart),
        // but they are stamped rather than defaulted-away so this helper stays
        // a faithful `QueuedDelivery` — see the struct's own doc on why every
        // field here is persisted.
        QueuedDelivery {
            id,
            agent_id: "w-1".into(),
            from: "loomux".into(),
            payload: QueuedPayload::StrandedSubmit,
            reason: EnqueueReason::Question,
            enqueued_ms: 1_000,
            coalesced: 0,
            group: Some("g-1".try_into().unwrap()),
            to_orchestrator: false,
            session_id: None,
            delivery_kind: crate::model::Delivery::MidSession,
        }
    }

    /// The economy property #533 exists for, stated as the observable
    /// outcome rather than as an implementation detail: ONE drain pass
    /// submits the entire flushable backlog. Pre-#533 the drainer planned
    /// exactly the front entry per pass, so this same assertion read
    /// `batch == [1]` — which is what the red-evidence branch pins (see the
    /// PR body): identical test text, failing on `plan_flush` returning a
    /// one-entry batch, passing here.
    #[test]
    fn plan_flush_combines_the_whole_flushable_backlog_into_one_batch_in_order() {
        let entries = vec![
            text_entry(1, "first: here is the context"),
            text_entry(2, "second: the actual task"),
            text_entry(3, "third: and one correction"),
        ];
        let plan = plan_flush(&entries, QUEUE_FLUSH_MAX_BYTES);
        assert_eq!(plan.batch, vec![1, 2, 3], "one pass must take the whole flushable backlog");
        assert_eq!(plan.remaining, 0);
        assert!(plan.superseded.is_empty());
        assert!(!plan.stranded);
    }

    #[test]
    fn plan_flush_never_holds_back_a_lone_live_delivery() {
        // The issue's explicit constraint: never wait for a second entry in
        // order to batch. A queue of one plans a batch of one, immediately.
        let entries = vec![text_entry(7, "go")];
        let plan = plan_flush(&entries, QUEUE_FLUSH_MAX_BYTES);
        assert_eq!(plan.batch, vec![7]);
        assert_eq!(plan.remaining, 0);
    }

    #[test]
    fn plan_flush_leaves_nothing_to_do_for_an_empty_queue() {
        let plan = plan_flush(&[], QUEUE_FLUSH_MAX_BYTES);
        assert!(plan.batch.is_empty());
        assert_eq!(plan.remaining, 0);
    }

    #[test]
    fn plan_flush_submits_a_front_marker_alone_and_never_merges_it() {
        // A marker's text is ALREADY in the box — pasting anything with it
        // would deliver that text twice. It submits by itself; whatever is
        // behind it flushes on the next pass, order preserved.
        let entries = vec![marker_entry(1), text_entry(2, "next"), text_entry(3, "and next")];
        let plan = plan_flush(&entries, QUEUE_FLUSH_MAX_BYTES);
        assert!(plan.stranded, "a front marker is a stranded submit, not a paste");
        assert_eq!(plan.batch, vec![1], "exactly one entry — never merged with text behind it");
        assert_eq!(plan.remaining, 2, "the text behind it is still queued, in order");
    }

    #[test]
    fn plan_flush_stops_the_batch_at_a_marker_it_reaches_mid_queue() {
        let entries = vec![text_entry(1, "a"), marker_entry(2), text_entry(3, "c")];
        let plan = plan_flush(&entries, QUEUE_FLUSH_MAX_BYTES);
        assert_eq!(plan.batch, vec![1], "the marker terminates the batch");
        assert!(!plan.stranded);
        assert_eq!(plan.remaining, 2);
    }

    #[test]
    fn plan_flush_chunks_a_backlog_that_exceeds_the_byte_cap() {
        // Equal-length but DISTINCT texts: byte-identical entries would be
        // ruled superseded before the cap ever got a say (as an earlier
        // draft of this test discovered the hard way).
        let big = |i: u64| format!("{}{i}", "x".repeat(199));
        let entries: Vec<QueuedDelivery> = (1..=4).map(|i| text_entry(i, &big(i))).collect();
        // Budget for ~2 entries (each costs len + FLUSH_ITEM_OVERHEAD).
        let cap = 2 * (200 + FLUSH_ITEM_OVERHEAD);
        let plan = plan_flush(&entries, cap);
        assert_eq!(plan.batch, vec![1, 2], "cap splits the backlog rather than pasting a megaprompt");
        assert_eq!(plan.remaining, 2, "the rest stays queued for the next flush — never dropped");
    }

    #[test]
    fn plan_flush_never_stalls_on_a_single_entry_larger_than_the_cap() {
        // The cap is a ceiling, never a floor: an entry bigger than the
        // whole budget must still deliver, alone, or it is stuck forever.
        let huge = "y".repeat(5_000);
        let entries = vec![text_entry(1, &huge), text_entry(2, "small")];
        let plan = plan_flush(&entries, 100);
        assert_eq!(plan.batch, vec![1], "the oversized entry still goes, by itself");
        assert_eq!(plan.remaining, 1);
    }

    #[test]
    fn plan_flush_drops_a_byte_identical_duplicate_before_coalescing_it() {
        // `admit` normally prevents this at admission; the drain-time
        // re-check is what keeps a duplicate that raced a pop from being
        // merged into ONE prompt, where it would read as two distinct asks.
        let entries = vec![
            text_entry(1, "status?"),
            text_entry(2, "different"),
            text_entry(3, "status?"),
        ];
        let plan = plan_flush(&entries, QUEUE_FLUSH_MAX_BYTES);
        assert_eq!(
            plan.superseded,
            vec![Superseded { id: 3, by: 1 }],
            "the LATER duplicate drops, naming the earlier entry that keeps its place and absorbs it"
        );
        assert_eq!(plan.batch, vec![1, 2], "a superseded entry never merges in");
        assert_eq!(plan.remaining, 0);
    }

    #[test]
    fn superseded_entries_keeps_the_first_occurrence_and_ignores_markers() {
        let entries = vec![text_entry(1, "a"), marker_entry(2), text_entry(3, "a")];
        assert_eq!(superseded_entries(&entries), vec![Superseded { id: 3, by: 1 }]);
    }

    #[test]
    fn superseded_entries_names_the_surviving_entry_so_its_repeat_count_can_be_moved() {
        // rev-13 F4: the pairing is what keeps a drain-time drop from losing
        // the count `admit` would have bumped had it caught the duplicate at
        // admission. Third and fourth copies both name the FIRST entry, not
        // each other — the survivor absorbs every one of them.
        let entries = vec![
            text_entry(1, "same"),
            text_entry(2, "other"),
            text_entry(3, "same"),
            text_entry(4, "same"),
        ];
        assert_eq!(
            superseded_entries(&entries),
            vec![Superseded { id: 3, by: 1 }, Superseded { id: 4, by: 1 }]
        );
    }

    #[test]
    fn coalesced_flush_text_preserves_order_and_itemizes_origin_and_queue_time() {
        let now = 1_000_000u64;
        let items = [
            FlushConstituent { id: 11, from: "orchestrator", enqueued_ms: now - 300_000, coalesced: 0, text: "FIRST-BODY" },
            FlushConstituent { id: 12, from: "w-7", enqueued_ms: now - 45_000, coalesced: 0, text: "SECOND-BODY" },
        ];
        let out = coalesced_flush_text(&items, 0, now, FlushCause::PaneBlocked);
        let first = out.find("FIRST-BODY").expect("constituent 1 present");
        let second = out.find("SECOND-BODY").expect("constituent 2 present");
        assert!(first < second, "queue order must survive the merge:\n{out}");
        assert!(out.contains("1/2"), "each constituent is positioned: {out}");
        assert!(out.contains("2/2"), "{out}");
        assert!(out.contains("from orchestrator"), "origin must survive: {out}");
        assert!(out.contains("from w-7"), "origin must survive: {out}");
        assert!(out.contains("5m00s ago"), "queue time must survive: {out}");
        assert!(out.contains("45s ago"), "queue time must survive: {out}");
        assert!(out.contains("id 11") && out.contains("id 12"), "audit-joinable ids: {out}");
        assert!(out.contains("nothing was reordered or dropped"), "{out}");
    }

    #[test]
    fn coalesced_flush_text_announces_a_further_chunk_when_one_remains() {
        let items = [FlushConstituent { id: 1, from: "loomux", enqueued_ms: 0, coalesced: 0, text: "b" }];
        let out = coalesced_flush_text(&items, 3, 1_000, FlushCause::PaneBlocked);
        assert!(out.contains("3 further queued deliveries follow"), "chunking must be stated: {out}");
        let none = coalesced_flush_text(&items, 0, 1_000, FlushCause::PaneBlocked);
        assert!(!none.contains("follow in the next flush"), "no phantom chunk clause: {none}");
    }

    #[test]
    fn coalesced_flush_text_reports_admission_coalesced_repeats_per_constituent() {
        let items = [
            FlushConstituent { id: 1, from: "w-2", enqueued_ms: 0, coalesced: 2, text: "ping" },
            FlushConstituent { id: 2, from: "w-3", enqueued_ms: 0, coalesced: 0, text: "pong" },
        ];
        let out = coalesced_flush_text(&items, 0, 1_000, FlushCause::PaneBlocked);
        assert!(out.contains("+2 identical repeats coalesced"), "per-constituent repeat count: {out}");
        assert_eq!(
            out.matches("repeats coalesced").count(),
            1,
            "only the constituent that actually repeated is annotated: {out}"
        );
    }

    #[test]
    fn draining_a_backlog_takes_one_pass_not_one_per_entry() {
        // The turn-economy property end to end: repeatedly plan-and-remove
        // until the queue is empty and count the passes. Four queued
        // deliveries used to cost four agent turns; they now cost one.
        let mut q: Vec<QueuedDelivery> = (1..=4).map(|i| text_entry(i, &format!("brief-{i}"))).collect();
        let mut passes = 0;
        while !q.is_empty() {
            passes += 1;
            assert!(passes <= 4, "not converging");
            let plan = plan_flush(&q, QUEUE_FLUSH_MAX_BYTES);
            q.retain(|e| {
                !plan.batch.contains(&e.id) && !plan.superseded.iter().any(|s| s.id == e.id)
            });
        }
        assert_eq!(passes, 1, "the whole flushable backlog must submit in ONE pass");
    }

    #[test]
    fn dropped_notice_names_count_and_reason_never_silent() {
        let n = dropped_notice("w-5", 2, DropReason::AgentDied);
        assert!(n.contains("2 queued deliveries"), "got: {n}");
        assert!(n.contains("DROPPED"), "must be loud, got: {n}");
        assert!(n.contains("pane closed"), "must name the reason, got: {n}");

        let n = dropped_notice("w-5", 1, DropReason::QueueFull);
        assert!(n.contains("1 queued delivery"), "singular, got: {n}");
        assert!(n.contains("already full"), "got: {n}");
    }

    #[test]
    fn still_queued_notice_is_informative_not_alarming_and_never_says_expired() {
        let n = still_queued_notice("w-5", 2, 30);
        assert!(n.contains("still queued"), "got: {n}");
        assert!(n.contains("nothing lost"), "must reassure, not alarm: {n}");
        assert!(!n.to_lowercase().contains("expir"), "this is NOT an expiry: {n}");
    }

    // ---------- should_fire_still_queued_notice ----------

    #[test]
    fn still_queued_notice_fires_once_at_threshold_not_before_not_again() {
        let start = 1_000_000u64;
        let threshold_ms = QUEUE_STILL_QUEUED_NOTICE_AFTER.as_millis() as u64;
        assert!(!should_fire_still_queued_notice(start, start + threshold_ms - 1, false), "not yet");
        assert!(should_fire_still_queued_notice(start, start + threshold_ms, false), "at threshold");
        assert!(
            !should_fire_still_queued_notice(start, start + threshold_ms + 10_000, true),
            "already notified — must not re-fire"
        );
    }

    // ---------- queue_full_error ----------

    #[test]
    fn queue_full_error_is_synchronous_and_truthful() {
        let e = queue_full_error("w-5", 8, "an unanswered question since t");
        assert!(e.contains("full"), "got: {e}");
        assert!(e.contains("8 queued"), "got: {e}");
        assert!(e.contains("NOT queued"), "must say plainly it was rejected, got: {e}");
    }

    // ---------- orphaned_queue_entries ----------

    fn line<'a>(action: &'a str, id: Option<u64>, agent: Option<&'a str>, reason: Option<&'a str>, ts: u64) -> QueueAuditLine<'a> {
        QueueAuditLine { action, id, agent_id: agent, reason, ts_ms: ts }
    }

    #[test]
    fn orphan_scan_finds_a_queued_entry_never_dequeued_or_dropped() {
        let lines = vec![line("delivery-queued", Some(1), Some("w-5"), Some("question"), 1_000)];
        let orphans = orphaned_queue_entries(&lines);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].id, 1);
        assert_eq!(orphans[0].agent_id, "w-5");
        assert_eq!(orphans[0].reason, "question");
    }

    #[test]
    fn orphan_scan_excludes_a_dequeued_entry() {
        let lines = vec![
            line("delivery-queued", Some(1), Some("w-5"), Some("question"), 1_000),
            line("delivery-dequeued", Some(1), Some("w-5"), None, 2_000),
        ];
        assert!(orphaned_queue_entries(&lines).is_empty());
    }

    #[test]
    fn orphan_scan_excludes_a_dropped_entry() {
        let lines = vec![
            line("delivery-queued", Some(2), Some("w-5"), Some("box-occupied"), 1_000),
            line("delivery-dropped", Some(2), Some("w-5"), Some("agent-died"), 2_000),
        ];
        assert!(orphaned_queue_entries(&lines).is_empty());
    }

    #[test]
    fn orphan_scan_matches_by_id_not_by_agent_or_order() {
        // Two different ids for the same agent: one resolved, one not —
        // must tell them apart by id, never by agent name or position.
        let lines = vec![
            line("delivery-queued", Some(1), Some("w-5"), Some("question"), 1_000),
            line("delivery-queued", Some(2), Some("w-5"), Some("box-occupied"), 1_500),
            line("delivery-dequeued", Some(1), Some("w-5"), None, 2_000),
        ];
        let orphans = orphaned_queue_entries(&lines);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].id, 2);
    }

    #[test]
    fn orphan_scan_ignores_coalesced_entries() {
        // A coalesced delivery never had its own `delivery-queued` line (it
        // was folded into an existing one) — nothing here to orphan.
        let lines = vec![line("delivery-coalesced", Some(1), Some("w-5"), None, 1_000)];
        assert!(orphaned_queue_entries(&lines).is_empty());
    }

    #[test]
    fn orphan_scan_is_empty_for_no_audit_history() {
        assert!(orphaned_queue_entries(&[]).is_empty());
    }

    #[test]
    fn orphan_scan_treats_recovery_as_terminal() {
        // #467: once a restart reads an entry back out of the snapshot, the
        // snapshot derivation owns it — either staged (reported WITH its
        // payload) or re-admitted under a fresh id. Left open here it would
        // be reported twice, forever, under an id nothing will ever close.
        let lines = vec![
            line("delivery-queued", Some(7), Some("w-5"), Some("arrival"), 1_000),
            line("delivery-recovered", Some(7), Some("w-5"), None, 2_000),
        ];
        assert!(orphaned_queue_entries(&lines).is_empty());
    }

    // ---------- #468/#467: durability ----------

    fn persisted(id: u64, text: &str) -> PersistedEntry {
        PersistedEntry { pty_id: 42, delivery: text_entry(id, text) }
    }

    #[test]
    fn a_snapshot_round_trips_payload_bytes_exactly() {
        // The whole value of persisting is that the bytes come back
        // re-sendable. Non-ASCII and newlines are in here because a task
        // brief has both and a lossy encoder would only show up on one.
        let original = vec![persisted(1, "line one\nline two — em dash, ünïcode"), persisted(2, "")];
        let (back, skipped) = parse_snapshot(&serialize_snapshot(1_234, original.clone()));
        assert_eq!(skipped, 0);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].delivery.payload.text(), Some("line one\nline two — em dash, ünïcode"));
        assert_eq!(back[1].delivery.payload.text(), Some(""), "an empty payload is not the same as a missing one");
        assert_eq!(back[0].delivery.id, 1);
        assert_eq!(back[0].pty_id, 42);
    }

    #[test]
    fn a_snapshot_preserves_order() {
        let (back, _) = parse_snapshot(&serialize_snapshot(0, vec![persisted(3, "c"), persisted(1, "a"), persisted(2, "b")]));
        let texts: Vec<Option<&str>> = back.iter().map(|e| e.delivery.payload.text()).collect();
        assert_eq!(texts, [Some("c"), Some("a"), Some("b")],
            "file order IS drain order — parsing must never re-sort it");
    }

    #[test]
    fn parsing_a_snapshot_skips_only_the_unreadable_entries() {
        let good = serialize_snapshot(0, vec![persisted(1, "keep"), persisted(2, "also keep")]);
        let mut v: serde_json::Value = serde_json::from_str(&good).unwrap();
        v["entries"].as_array_mut().unwrap().insert(1, serde_json::json!({ "pty_id": "nonsense" }));
        let (back, skipped) = parse_snapshot(&v.to_string());
        assert_eq!(skipped, 1, "the caller must be able to audit what it lost");
        assert_eq!(back.len(), 2, "one bad entry must not cost the good ones beside it");
    }

    #[test]
    fn parsing_refuses_garbage_and_unknown_versions_without_panicking() {
        assert_eq!(parse_snapshot("").0.len(), 0);
        assert_eq!(parse_snapshot("not json at all").0.len(), 0);
        assert_eq!(parse_snapshot("{}").0.len(), 0);
        let future = serialize_snapshot(0, vec![persisted(1, "x")])
            .replace("\"version\": 1", &format!("\"version\": {}", SNAPSHOT_VERSION + 1));
        assert_eq!(parse_snapshot(&future).0.len(), 0,
            "an unknown shape must be refused, not guessed at — the failure direction is pasting wrong bytes");
    }

    #[test]
    fn an_entry_from_an_older_build_parses_but_has_no_durable_identity() {
        // Forward compatibility in the safe direction: a snapshot written
        // before `group`/`to_orchestrator`/`session_id` existed must still
        // read back (so its payload is surfaced as an orphan) while matching
        // nothing (so it is never replayed into a pane it wasn't for).
        let legacy = serde_json::json!({
            "version": SNAPSHOT_VERSION,
            "written_ms": 1,
            "entries": [{
                "pty_id": 9, "id": 1, "agent_id": "w-1", "from": "orch-1",
                "payload": { "kind": "text", "text": "hello" },
                "reason": "arrival", "enqueued_ms": 5, "coalesced": 0,
            }],
        });
        let (back, skipped) = parse_snapshot(&legacy.to_string());
        assert_eq!((back.len(), skipped), (1, 0));
        assert_eq!(back[0].delivery.payload.text(), Some("hello"));
        assert!(!rebinds_to(&back[0].delivery, true, Some("some-session")),
            "an entry with no recorded identity must match nothing, not everything");
    }

    #[test]
    fn split_recovered_replays_text_and_refuses_markers() {
        let marker = PersistedEntry {
            pty_id: 1,
            delivery: QueuedDelivery {
                payload: QueuedPayload::StrandedSubmit,
                ..text_entry(2, "unused")
            },
        };
        let split = split_recovered(vec![persisted(1, "replay me"), marker, persisted(3, "me too")]);
        assert_eq!(split.replayable.len(), 2);
        assert_eq!(split.markers.len(), 1);
        let texts: Vec<Option<&str>> = split.replayable.iter().map(|e| e.delivery.payload.text()).collect();
        assert_eq!(texts, [Some("replay me"), Some("me too")], "removing a marker must not reorder what's left");
    }

    #[test]
    fn rebinding_matches_the_orchestrator_or_the_same_session_and_nothing_else() {
        let to_orch = QueuedDelivery { to_orchestrator: true, ..text_entry(1, "x") };
        let to_worker = QueuedDelivery { session_id: Some("sess-a".into()), ..text_entry(2, "x") };

        assert!(rebinds_to(&to_orch, true, None), "the group's one orchestrator is a durable target");
        assert!(!rebinds_to(&to_orch, false, Some("sess-a")),
            "an orchestrator-targeted entry must not land in a worker pane");
        assert!(rebinds_to(&to_worker, false, Some("sess-a")), "same resumed session = same worker");
        assert!(!rebinds_to(&to_worker, false, Some("sess-b")), "a different session is a different pane");
        assert!(!rebinds_to(&to_worker, false, None), "a pane with no session id matches nothing");
        assert!(!rebinds_to(&to_worker, true, None),
            "being the orchestrator does not entitle a pane to a worker's backlog");

        // `agent_id` is deliberately NOT a key: a restore never revives one
        // (and since #524 never re-mints one either), so two entries sharing
        // one prove nothing about a live pane. Both entries here carry
        // agent_id "w-1" (from `text_entry`) and neither matches on it.
        let anonymous = QueuedDelivery { session_id: Some(String::new()), ..text_entry(3, "x") };
        assert!(!rebinds_to(&anonymous, false, Some("")),
            "two empty session ids are not a match — they are two absences");
    }

    #[test]
    fn merging_orphans_prefers_the_derivation_that_has_the_payload() {
        let snap = OrphanedQueueEntry {
            id: 1, agent_id: "w-1".into(), enqueued_ms: 10, reason: "arrival".into(),
            text: Some("the real payload".into()), source: OrphanSource::Snapshot,
        };
        let from_audit = |id: u64| OrphanedQueueEntry {
            id, agent_id: "w-1".into(), enqueued_ms: 10, reason: "arrival".into(),
            text: None, source: OrphanSource::Audit,
        };
        let merged = merge_orphans(vec![snap.clone()], vec![from_audit(1), from_audit(2)], &Default::default());
        assert_eq!(merged.len(), 2, "the same delivery must not be reported twice");
        assert_eq!(merged[0].id, 1);
        assert_eq!(merged[0].text.as_deref(), Some("the real payload"),
            "the snapshot derivation wins — it is the only one that can be re-sent");
        assert_eq!(merged[1].source, OrphanSource::Audit, "an audit-only id is still reported, payload-less");

        // An id still in the live queue is PENDING, not lost — reporting it
        // would invite a re-send of something about to arrive.
        let live: std::collections::HashSet<u64> = [2].into_iter().collect();
        let filtered = merge_orphans(vec![snap], vec![from_audit(1), from_audit(2)], &live);
        assert_eq!(filtered.iter().map(|e| e.id).collect::<Vec<_>>(), [1],
            "an in-flight delivery must never be reported as lost work");
    }

    #[test]
    fn clamping_a_payload_says_so_and_never_splits_a_char() {
        let short = "fits fine";
        assert_eq!(clamp_payload(short, 100), short, "an uncapped payload is returned untouched");

        // A cap landing mid-character must not panic or produce invalid
        // UTF-8 — em dashes are three bytes and task briefs are full of them.
        let wide = "—".repeat(10);
        let cut = clamp_payload(&wide, 10);
        assert!(cut.starts_with("———"), "must cut on a char boundary: {cut}");
        assert!(cut.contains("truncated"), "a cut payload must SAY it was cut: {cut}");
        assert!(cut.contains("audit log"), "and where the full copy is: {cut}");
    }
}

/// #445 rev-35 B1 — the ordering PROPERTY, exhaustively, not one scenario.
///
/// **Superseded by #470 — kept as a historical record, not deleted.** This
/// module models the algorithm `deliver_now`'s pre-paste recheck implemented:
/// a raw per-pty mutex race for a fresh delivery, PLUS a live recheck right
/// before pasting to defer to a queue that formed underneath it. Proven
/// correct here for exactly 2 simultaneous contenders; the 3-contender
/// residual this module ALSO proves (below) is exactly the defect #470's
/// unified-admission redesign closes — see `unified_admission_property`
/// (this file) for the model of what replaced this mechanism, and
/// `doc/design/orchestration.md`'s Ordering subsection for the argument.
/// Every test in this module still passes unmodified: they are pure math
/// about an algorithm this PR stops running in production, not a live
/// regression pin — keeping them intact is what lets `unified_admission_
/// property`'s own mutation test point back at a REAL example of the
/// class of bug being closed, instead of asserting it in the abstract.
///
/// The review's finding: `deliver_now` never re-checked the queue after its
/// own hold, so a delivery already in flight (holding the pane's delivery
/// mutex through a pre-paste hold) when another delivery timed out and
/// queued could still paste directly once ITS OWN hold cleared — "now go"
/// landing before "here's the context." The fix is the pre-paste recheck in
/// `deliver_now` (mod.rs): immediately before pasting, re-consult
/// `queue_is_non_empty` and defer to the queue if it now says yes.
///
/// `deliver_now`'s live pipeline cannot run in this test suite (no real
/// pty/`AppHandle` — see the module doc's "Scope" section and every other
/// `deliver_now`-adjacent doc comment for the same boundary), so this
/// cannot execute real threads racing a real mutex. What it CAN do, and
/// what a single hand-picked scenario test cannot: model the algorithm's
/// actual decision rule (front door defers iff the queue is non-empty at
/// arrival; the in-flight recheck defers iff the queue is non-empty
/// immediately before paste) and exhaustively search every reachable
/// interleaving of {a fresh delivery arriving, the current mutex holder's
/// hold resolving — by timing out OR by clearing, both explored — and
/// rechecking, the drainer popping the queue's front}, checking rev-35's
/// own wording for the property on every terminal state: **a delivery that
/// arrived later never lands before an earlier [arrived] delivery.**
///
/// **Scope: exactly 2 contending deliveries, not 3+ — argued, not assumed.**
/// With exactly two deliveries to one pane, the per-pty delivery mutex has
/// AT MOST one waiter at any moment: whichever arrives second is
/// unambiguously "next" once the first releases the mutex, so arrival order
/// and mutex-acquisition order coincide by construction — this is precisely
/// rev-35's own 2-delivery scenario, generalized over every interleaving of
/// arrival/hold/release for those two. With THREE OR MORE simultaneously
/// contending, `std::sync::Mutex` grants to waiters in an unspecified
/// order — if X and Y both arrive while a third delivery holds the mutex
/// and the queue is still empty, whichever of X/Y the scheduler happens to
/// grant the mutex to next runs its own hold-and-decide cycle first,
/// independent of which of them arrived first. That is a DIFFERENT defect
/// (mutex-acquisition fairness among simultaneous waiters, not a missing
/// queue recheck) that a localized "recheck before paste" cannot fix — see
/// `pre_470_algorithm_three_way_contention_inverted_arrival_order`,
/// below (renamed by #470 review B3 — see that test's own doc), which
/// documents it rather than silently dropping it. Restricting
/// THIS property's exhaustive search to 2 is what keeps it non-vacuous: an
/// earlier draft ran it at 3 and found violations in the FIXED run too —
/// not because the fix was wrong, but because the property as phrased
/// (raw arrival order, no scoping) is provably unachievable by any
/// localized fix once 3+ contenders are possible.
///
/// **Deliberately MORE adversarial than the real system, not less** (within
/// its 2-delivery scope). In production the drainer's own paste attempt
/// shares the SAME per-pty delivery mutex as a direct delivery
/// (`deliver_now`'s `lock` parameter), so a drain pop is actually
/// serialized behind whatever currently holds it. This model does NOT
/// impose that extra constraint — a `Drain` event is available any time the
/// queue is non-empty, mutex state notwithstanding. That only WIDENS the
/// set of interleavings explored beyond what the real mutex would ever
/// allow; the real system's reachable states are a subset of this model's.
/// A property proven here therefore holds a fortiori in production — this
/// is an over-approximation of danger, not an under-approximation of it.
///
/// **Mutation-verified** (rev-35's ask): `defer_check_upheld_for_two_
/// contenders` runs the search with the actual fix's decision rule and must
/// find zero violations; `defer_check_disabled_finds_a_real_inversion` runs
/// the IDENTICAL search with the recheck neutralized (always "paste, never
/// defer" — base-branch behavior) and must find at least one. If the
/// second test ever started passing (zero violations even with the recheck
/// disabled), the first test would be proven vacuous — passing regardless
/// of whether the fix does anything — so it is exactly as load-bearing as
/// the property test itself.
#[cfg(test)]
mod b1_ordering_property {
    use std::collections::HashMap;

    #[derive(Clone)]
    struct SimState {
        queue: std::collections::VecDeque<char>,
        mutex_holder: Option<char>,
        waiting: Vec<char>,
        not_yet_arrived: Vec<char>,
        arrived_at: HashMap<char, usize>,
        delivered: Vec<char>,
        step: usize,
    }

    /// One violation of the ordering property, formatted for a failing
    /// assertion to print directly — no need to re-run anything by hand to
    /// see which interleaving broke it. Pure arrival-vs-arrival comparison
    /// — see the module doc's "Scope" section for why this is valid (only)
    /// for exactly 2 contenders, and never both queued-vs-arrival (an
    /// earlier draft tried anchoring on `x`'s own queued-timestamp instead
    /// of its arrival — it never fired at all, in either the fixed OR the
    /// disabled run, because in the exact reported scenario the "victim"
    /// always arrives BEFORE the "blocker" finishes timing out and
    /// enqueuing; anchoring on the blocker's queued moment made the check
    /// vacuous in both directions).
    fn check_terminal_state(state: &SimState, violations: &mut Vec<String>) {
        for (&x, &arrived_at_x) in &state.arrived_at {
            for (&y, &arrived_at_y) in &state.arrived_at {
                if x == y || arrived_at_y <= arrived_at_x {
                    continue;
                }
                let px = state.delivered.iter().position(|&c| c == x);
                let py = state.delivered.iter().position(|&c| c == y);
                match (px, py) {
                    (Some(px), Some(py)) if px > py => {
                        violations.push(format!(
                            "{x} arrived (step {arrived_at_x}) before {y} (step {arrived_at_y}), \
                             but {y} was delivered before {x} — final order {:?}",
                            state.delivered
                        ));
                    }
                    (None, _) | (_, None) => violations.push(format!(
                        "{x} or {y} never delivered — simulation bug, not a real finding: {state:?}"
                    )),
                    _ => {}
                }
            }
        }
    }

    impl std::fmt::Debug for SimState {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SimState")
                .field("queue", &self.queue)
                .field("mutex_holder", &self.mutex_holder)
                .field("delivered", &self.delivered)
                .finish()
        }
    }

    /// After a mutex holder resolves (either way — see the two branches in
    /// `explore` below), the delivery mutex is free: pick every possible
    /// next holder from `waiting` (`std::sync::Mutex` gives no fairness
    /// guarantee, so ALL of them are explored, not just arrival order) and
    /// recurse from each resulting state.
    fn release_mutex_and_continue(s: SimState, defer_check: fn(bool) -> bool, violations: &mut Vec<String>) {
        if s.waiting.is_empty() {
            explore(s, defer_check, violations);
        } else {
            for i in 0..s.waiting.len() {
                let mut s2 = s.clone();
                let next = s2.waiting.remove(i);
                s2.mutex_holder = Some(next);
                explore(s2, defer_check, violations);
            }
        }
    }

    /// Exhaustively explore every event enabled from `state`, recursing on
    /// each. `defer_check(queue_non_empty_at_recheck)` is the ONE thing that
    /// varies between the fixed and unfixed runs — everything else (the
    /// front door's own rule, the queue, the mutex) is identical.
    fn explore(state: SimState, defer_check: fn(bool) -> bool, violations: &mut Vec<String>) {
        let mut any_event = false;

        // Event: a not-yet-arrived delivery arrives.
        for i in 0..state.not_yet_arrived.len() {
            any_event = true;
            let mut s = state.clone();
            let id = s.not_yet_arrived.remove(i);
            s.step += 1;
            s.arrived_at.insert(id, s.step);
            if !s.queue.is_empty() {
                // Front door: queue already non-empty — enqueue behind it.
                s.queue.push_back(id);
            } else if s.mutex_holder.is_none() {
                s.mutex_holder = Some(id);
            } else {
                s.waiting.push(id);
            }
            explore(s, defer_check, violations);
        }

        // Events: the current mutex holder's hold resolves — TWO distinct,
        // mutually exclusive ways, both real and both explored:
        //
        // - `HoldTimesOut`: the pre-paste hold's OWN cap (60-120s, seam 1/2
        //   — pre-existing, unrelated to B1) expires with nothing having
        //   cleared it. This UNCONDITIONALLY enqueues, in both the fixed
        //   and the "disabled" runs — it is what SEEDS the queue at all,
        //   and B1's recheck is not even reached on this path in the real
        //   code (the seam-1/2 abort returns before ever getting there).
        // - `HoldClears`: the pane becomes deliverable (question answered /
        //   box emptied) before the cap. THIS is where the pre-paste
        //   recheck (rev-35 B1) runs — `defer_check` is the ONE thing that
        //   varies between the fixed and disabled runs.
        //
        // Collapsing these into one `defer_check`-gated event (an earlier
        // draft of this model did exactly that) is wrong: it makes NOTHING
        // able to seed the queue in the "disabled" run (nothing to compare
        // a later arrival against), so the mutation test would vacuously
        // find no violation regardless of whether the recheck exists.
        if let Some(holder) = state.mutex_holder {
            any_event = true;

            let mut timed_out = state.clone();
            timed_out.step += 1;
            timed_out.mutex_holder = None;
            timed_out.queue.push_back(holder);
            release_mutex_and_continue(timed_out, defer_check, violations);

            let mut cleared = state.clone();
            cleared.step += 1;
            cleared.mutex_holder = None;
            if defer_check(!cleared.queue.is_empty()) {
                cleared.queue.push_back(holder);
            } else {
                cleared.delivered.push(holder);
            }
            release_mutex_and_continue(cleared, defer_check, violations);
        }

        // Event: the drainer pops the queue's front (see the module doc
        // above for why this is intentionally NOT gated on `mutex_holder`).
        if !state.queue.is_empty() {
            any_event = true;
            let mut s = state.clone();
            s.step += 1;
            let id = s.queue.pop_front().expect("just checked non-empty");
            s.delivered.push(id);
            explore(s, defer_check, violations);
        }

        if !any_event {
            check_terminal_state(&state, violations);
        }
    }

    fn run_exhaustive_search(ids: &[char], defer_check: fn(bool) -> bool) -> Vec<String> {
        let mut violations = Vec::new();
        let initial = SimState {
            queue: std::collections::VecDeque::new(),
            mutex_holder: None,
            waiting: Vec::new(),
            not_yet_arrived: ids.to_vec(),
            arrived_at: HashMap::new(),
            delivered: Vec::new(),
            step: 0,
        };
        explore(initial, defer_check, &mut violations);
        violations
    }

    #[test]
    fn defer_check_upheld_for_two_contenders_no_inversion_across_any_reachable_interleaving() {
        let violations = run_exhaustive_search(&['A', 'B'], |queue_non_empty_at_recheck| queue_non_empty_at_recheck);
        assert!(
            violations.is_empty(),
            "the FIXED algorithm produced {} ordering violation(s) across the exhaustive 2-contender search:\n{}",
            violations.len(),
            violations.join("\n")
        );
    }

    #[test]
    fn defer_check_disabled_finds_a_real_inversion_for_two_contenders() {
        // Mutation (rev-35's ask): neutralize the recheck (base-branch
        // behavior — always paste, never defer). The IDENTICAL exhaustive
        // 2-contender search must find at least one violation, or the test
        // above is vacuous — passing whether or not the fix does anything.
        let violations = run_exhaustive_search(&['A', 'B'], |_queue_non_empty_at_recheck| false);
        assert!(
            !violations.is_empty(),
            "disabling the recheck must reproduce a real inversion somewhere in the 2-contender search \
             space — if it doesn't, `defer_check_upheld_for_two_contenders_...` isn't actually testing \
             anything"
        );
    }

    #[test]
    fn pre_470_algorithm_three_way_contention_inverted_arrival_order() {
        // #470 review (rev-31), B3: renamed from
        // `three_way_contention_can_still_invert_arrival_order_known_residual`.
        // That name was accurate the day it was written but became a
        // claim-vs-reality hazard the moment #470 shipped a REPLACEMENT
        // algorithm (`unified_admission_property`, this file) that closes
        // this exact gap: a passing test whose name says an inversion "can
        // still" happen, read by `cargo test` output, CI logs, or a grep
        // for "residual" with no other context, states the bug is LIVE in
        // current code. It is not — it describes the algorithm THIS PR
        // REPLACED. The assertion body is intentionally unchanged (the
        // model, and the defect it demonstrates, are still exactly as real
        // as they were the day this was written — see the module doc's new
        // "Superseded by #470" note above); only the name and this comment
        // are updated so the claim travels correctly wherever the name
        // alone is read.
        //
        // What this still proves, past tense: with 3 simultaneous
        // contenders, this exhaustive search found an inversion even with
        // the B1 recheck fully applied — not because the recheck was wrong
        // (each individual delivery, once it held the mutex, correctly
        // deferred to whatever was already queued), but because
        // `std::sync::Mutex` could grant the freed mutex to EITHER of two
        // simultaneous waiters regardless of which arrived first, and that
        // choice alone could put the later-arriving one through its own
        // hold-and-decide cycle first. This is the concrete, executable
        // evidence that #470's redesign had a real defect to close — see
        // `unified_admission_property::unified_admission_closes_ordering_
        // at_three_contenders` (this file) for the SAME property, SAME
        // contender count, now proven closed under the replacement
        // algorithm.
        let violations =
            run_exhaustive_search(&['A', 'B', 'C'], |queue_non_empty_at_recheck| queue_non_empty_at_recheck);
        assert!(
            !violations.is_empty(),
            "this test models the PRE-#470 algorithm (raced mutex + front-door bypass), which #470 \
             replaced — it must keep finding this violation, since it exists to document that the \
             replaced algorithm genuinely had one. If this ever starts passing, the model has \
             drifted from the algorithm it's supposed to describe; fix the model, don't just delete \
             the coverage"
        );
    }
}

/// #470 — the ordering PROPERTY under UNIFIED ADMISSION, exhaustively.
///
/// This models `mod.rs`'s post-#470 design: `deliver_prompt`'s front door no
/// longer has two separate admission paths (race a raw mutex when the queue
/// looks empty; append directly when it doesn't) — EVERY delivery pushes to
/// the BACK of the SAME queue, atomically with the check for whether the
/// queue was empty (`enqueue_text`'s `was_first`). Whichever push observes
/// an empty queue is the one that starts processing; every other push, no
/// matter how it got here, is already correctly positioned behind whatever
/// arrived first.
///
/// **Why this closes what a fair mutex alone does not (NB-A).** A reviewer
/// of the original #470 filing proved that simply making the OLD per-pty
/// mutex fair is insufficient: `b1_ordering_property`'s recheck-and-
/// defer-to-TAIL mechanism loses a delivery's arrival position whenever a
/// LATER arrival used the old front door's "queue already non-empty ->
/// append directly" bypass — a path that never touched the mutex, fair or
/// not, at all. This model has no such bypass to lose position to: there is
/// exactly one admission function, and it is the SAME push whether the
/// queue is empty or not. A delivery's position in `queue` is fixed the
/// instant it's admitted and never changes until it's delivered — there is
/// no "defer to tail" step left to model, because nothing can ever cut in
/// front of an already-admitted entry.
///
/// **Why a hold-cap TIMEOUT needs no event of its own (unlike
/// `b1_ordering_property`'s `HoldTimesOut`/`HoldClears` split).** Pre-#470,
/// a timeout had to perform a state transition (enqueue the holder, THEN
/// release the mutex to a waiter) because the timed-out delivery had no
/// queue representation until that moment. Post-#470 it already has one —
/// it's sitting at the front of `queue` from the moment it arrived — so a
/// timeout changes nothing observable: the entry stays exactly where it
/// is and the same thread simply tries again later. The only state-changing
/// resolution left is delivering: popping the front and recording it, which
/// this model represents as a single `processing`-gated event enabled
/// whenever something is currently being attempted.
///
/// **The mutation knob.** `deliver_from_back` is this model's ONE toggle,
/// in the same spirit as `b1_ordering_property`'s `defer_check`: `false` is
/// the real #470 algorithm (always resolve the FRONT — genuine FIFO);
/// `true` breaks the one line that makes admission order equal delivery
/// order, popping the BACK instead, to prove the exhaustive search actually
/// notices an inversion rather than passing vacuously. See
/// `unified_admission_mutation_pop_from_back_finds_inversions`, below.
#[cfg(test)]
mod unified_admission_property {
    use std::collections::{HashMap, VecDeque};

    #[derive(Clone, Debug)]
    struct SimState {
        queue: VecDeque<char>,
        processing: bool,
        not_yet_arrived: Vec<char>,
        arrived_at: HashMap<char, usize>,
        delivered: Vec<char>,
        step: usize,
    }

    /// Identical in spirit to `b1_ordering_property::check_terminal_state`:
    /// every pair of contenders where one arrived strictly after the other
    /// must deliver in that same order.
    fn check_terminal_state(state: &SimState, violations: &mut Vec<String>) {
        for (&x, &arrived_at_x) in &state.arrived_at {
            for (&y, &arrived_at_y) in &state.arrived_at {
                if x == y || arrived_at_y <= arrived_at_x {
                    continue;
                }
                let px = state.delivered.iter().position(|&c| c == x);
                let py = state.delivered.iter().position(|&c| c == y);
                match (px, py) {
                    (Some(px), Some(py)) if px > py => {
                        violations.push(format!(
                            "{x} arrived (step {arrived_at_x}) before {y} (step {arrived_at_y}), \
                             but {y} was delivered before {x} — final order {:?}",
                            state.delivered
                        ));
                    }
                    (None, _) | (_, None) => violations.push(format!(
                        "{x} or {y} never delivered — simulation bug, not a real finding: {state:?}"
                    )),
                    _ => {}
                }
            }
        }
    }

    fn explore(state: SimState, deliver_from_back: bool, violations: &mut Vec<String>) {
        let mut any_event = false;

        // Event: a not-yet-arrived delivery arrives. #470: unconditionally
        // pushed to the BACK — no more "queue empty -> race a mutex
        // instead" branch. If nothing is currently being processed, this
        // admission is what starts processing; in the real code that
        // decision (`was_first`) is made ATOMICALLY under the same lock as
        // the push, so modeling it as an immediate, deterministic
        // transition (never a race between two "was_first" claimants) is
        // faithful, not optimistic.
        for i in 0..state.not_yet_arrived.len() {
            any_event = true;
            let mut s = state.clone();
            let id = s.not_yet_arrived.remove(i);
            s.step += 1;
            s.arrived_at.insert(id, s.step);
            s.queue.push_back(id);
            if !s.processing {
                s.processing = true;
            }
            explore(s, deliver_from_back, violations);
        }

        // Event: the currently-processing entry's attempt resolves by
        // delivering. A hold-cap TIMEOUT is deliberately NOT a separate
        // event here (see the module doc) — it changes nothing this model
        // can observe, so omitting it loses no reachable state, only a
        // self-loop.
        if state.processing {
            any_event = true;
            let mut s = state.clone();
            s.step += 1;
            let id = if deliver_from_back { s.queue.pop_back() } else { s.queue.pop_front() }
                .expect("`processing` is only ever true while `queue` is non-empty");
            s.delivered.push(id);
            s.processing = !s.queue.is_empty();
            explore(s, deliver_from_back, violations);
        }

        if !any_event {
            check_terminal_state(&state, violations);
        }
    }

    fn run_exhaustive_search(ids: &[char], deliver_from_back: bool) -> Vec<String> {
        let mut violations = Vec::new();
        let initial = SimState {
            queue: VecDeque::new(),
            processing: false,
            not_yet_arrived: ids.to_vec(),
            arrived_at: HashMap::new(),
            delivered: Vec::new(),
            step: 0,
        };
        explore(initial, deliver_from_back, &mut violations);
        violations
    }

    #[test]
    fn unified_admission_closes_ordering_at_three_contenders() {
        // Formerly proven OPEN by `b1_ordering_property::
        // pre_470_algorithm_three_way_contention_inverted_arrival_order`
        // (renamed by #470 review B3 from `..._can_still_invert_..._known_
        // residual` — a passing test naming a residual THIS test proves
        // closed was its own claim-vs-reality hazard; kept, unmodified
        // otherwise — see that module's doc for why it still exists and
        // still passes). This is the flip issue #470 asked for: the SAME
        // property, at the SAME contender count that used to
        // invert, now proven closed — not by widening the old recheck, but
        // by removing the admission bypass that made widening it
        // insufficient (NB-A).
        let violations = run_exhaustive_search(&['A', 'B', 'C'], false);
        assert!(
            violations.is_empty(),
            "unified admission produced {} ordering violation(s) at 3 contenders:\n{}",
            violations.len(),
            violations.join("\n")
        );
    }

    #[test]
    fn unified_admission_closes_ordering_at_four_and_five_contenders() {
        // #470 DoD: prove this at 3 AND 4+, not just re-run the old 3-only
        // scope. Unlike `b1_ordering_property` (whose 2-contender ceiling
        // was load-bearing — the property was provably unachievable past
        // it under the old algorithm), unified admission has no such
        // ceiling: FIFO-by-construction doesn't get harder at higher N.
        for ids in [&['A', 'B', 'C', 'D'][..], &['A', 'B', 'C', 'D', 'E'][..]] {
            let violations = run_exhaustive_search(ids, false);
            assert!(
                violations.is_empty(),
                "unified admission produced {} ordering violation(s) at {} contenders:\n{}",
                violations.len(),
                ids.len(),
                violations.join("\n")
            );
        }
    }

    #[test]
    fn unified_admission_mutation_pop_from_back_finds_inversions() {
        // Mutation test, matching `b1_ordering_property::
        // defer_check_disabled_finds_a_real_inversion_for_two_contenders`'s
        // style: flip the ONE line that makes admission order equal
        // delivery order (resolve the back instead of the front) and
        // confirm the SAME exhaustive search, SAME property checker, finds
        // real violations. If this ever started passing, the two `_closes_
        // ordering_` tests above would be proven vacuous.
        let violations = run_exhaustive_search(&['A', 'B', 'C'], true);
        assert!(
            !violations.is_empty(),
            "popping from the BACK must reproduce a real inversion somewhere in the 3-contender \
             search space — if it doesn't, the `_closes_ordering_` tests aren't actually testing \
             anything"
        );
    }

    /// #470 B1 (review round 1, rev-31) — drainer LIFECYCLE, exhaustively:
    /// not just delivery ORDER (proven above), but whether every admitted
    /// delivery is EVER claimed by a drainer at ALL.
    ///
    /// The ordering model above is faithful for ordering but silently
    /// assumed a spawned drainer always gets around to every entry — true
    /// enough for an ordering property (nothing can skip ahead of an
    /// unclaimed entry) but false for LIVENESS, which review caught: a real
    /// drainer that privately decides to exit (finds the queue empty) does
    /// not deregister from `queue_draining` in that same instant — the
    /// `DrainerGuard`'s `Drop` runs later, an unbounded OS-scheduling-width
    /// window later, not a rare corner. A FRESH admission landing in that
    /// window (`was_first: true`, sender already told `Ok`) calls
    /// `ensure_drainer`, finds the pty still registered, and no-ops — the
    /// drainer that registration refers to will never look again. The
    /// delivery sits in the queue forever: not destroyed, but never
    /// claimed either — exactly the "queued means safe" guarantee
    /// #445/#451 exist to make structural, broken by the PR meant to
    /// strengthen it.
    ///
    /// `OrchRegistry::commit_exit` (mod.rs) is round 1's fix: the "is the
    /// queue empty" check and the `queue_draining` deregistration happen in
    /// ONE critical section on `queues`, so there is no window between them
    /// for an admission to land in. `atomic_exit` below models that fix
    /// (mirroring `deliver_from_back`, above): `true` is `commit_exit` as
    /// shipped; `false` reproduces the round-1 pre-fix split.
    ///
    /// **Round 2 (rev-37): this model itself had a gap.** It modeled
    /// `commit_exit`'s atomicity but not the REAL `DrainerGuard`'s `Drop` —
    /// an RAII cleanup that fires on every return REGARDLESS of whether
    /// `commit_exit` already deregistered, because the guard has no way to
    /// know that. The `atomic_exit: true` variant modeled a world where
    /// commit is the ONLY deregistration, which is not the code as shipped
    /// — the guard still ran afterward and (round-1-shipped) removed
    /// `pty_id` UNCONDITIONALLY a second time, capable of erasing a
    /// SUCCESSOR drainer's live registration (spawned in the window between
    /// commit and the guard's drop) and running TWO drainers on the same
    /// queue concurrently — the SAME entry pasted twice. A model that omits
    /// a real event cannot exclude the bugs that event causes; this is why
    /// the guard-drop event below is now UNCONDITIONAL (every drainer
    /// instance eventually gets one, always) and `guard_checks_generation`
    /// — not the event's mere presence — is the round-2 mutation knob.
    ///
    /// `OrchRegistry`'s actual fix (round 2): `queue_draining` became a
    /// generation map (`pty_id -> u64`), not a bare membership set. Both
    /// `commit_exit` and `DrainerGuard::drop` remove `pty_id` ONLY if the
    /// CURRENTLY stored generation is still their own — so whichever of
    /// them acts first performs the real removal and the other is
    /// inherently a no-op, structurally, with no call site needing to
    /// remember to "arm" or "disarm" anything.
    mod drainer_lifecycle {
        use std::collections::{HashMap, VecDeque};

        /// One drainer thread's own lifecycle, tracked explicitly so
        /// MULTIPLE instances can coexist in the model exactly as they can
        /// (only when buggy) in reality — this is what lets the checker
        /// observe "two drainers concurrently processing," the round-2
        /// defect's direct symptom, rather than only its downstream
        /// consequences.
        #[derive(Clone, Debug)]
        struct DrainerInstance {
            generation: u64,
            /// Whether this instance will still loop around and touch the
            /// queue again (pop+deliver, or decide-to-exit).
            processing: bool,
            /// Only meaningful when `atomic_exit` is false: this instance
            /// found the queue empty and stopped processing, but its
            /// (still generation-checked) deregistration hasn't run yet —
            /// a separate, later event.
            commit_pending: bool,
            /// Whether this instance's `DrainerGuard` has already dropped.
            /// Once true this instance contributes no further events —
            /// fully retired.
            guard_dropped: bool,
        }

        #[derive(Clone, Debug)]
        struct SimState {
            queue: VecDeque<char>,
            /// Mirrors `OrchRegistry::queue_draining`'s CURRENT value for
            /// this pty: `None` if nothing registered, `Some(generation)`
            /// if that generation currently holds it.
            registered_gen: Option<u64>,
            next_gen: u64,
            /// Every drainer instance spawned so far that hasn't fully
            /// retired (`guard_dropped`) yet. In the fully-fixed algorithm
            /// this never exceeds length 1 at any point where more than
            /// one entry is `processing` — proven, not assumed.
            instances: Vec<DrainerInstance>,
            not_yet_arrived: Vec<char>,
            arrived_at: HashMap<char, usize>,
            delivered: Vec<char>,
            step: usize,
        }

        /// Ordering (same predicate as the enclosing module's) PLUS
        /// liveness (nothing arrived may go undelivered — round 1) PLUS
        /// uniqueness (nothing may be delivered MORE than once — round 2's
        /// direct symptom, alongside the mid-execution concurrency check in
        /// `explore`).
        fn check_terminal_state(state: &SimState, violations: &mut Vec<String>) {
            let mut ids: Vec<&char> = state.arrived_at.keys().collect();
            ids.sort();
            for &id in &ids {
                let count = state.delivered.iter().filter(|&d| d == id).count();
                if count == 0 {
                    violations.push(format!(
                        "{id} was admitted (arrived at step {}) but never delivered — STRANDED. \
                         final: queue={:?} registered_gen={:?} delivered={:?}",
                        state.arrived_at[id], state.queue, state.registered_gen, state.delivered
                    ));
                } else if count > 1 {
                    violations.push(format!(
                        "{id} was delivered {count} times — DUPLICATED. \
                         final: queue={:?} registered_gen={:?} delivered={:?}",
                        state.queue, state.registered_gen, state.delivered
                    ));
                }
            }
            for (&x, &arrived_at_x) in &state.arrived_at {
                for (&y, &arrived_at_y) in &state.arrived_at {
                    if x == y || arrived_at_y <= arrived_at_x {
                        continue;
                    }
                    let px = state.delivered.iter().position(|&c| c == x);
                    let py = state.delivered.iter().position(|&c| c == y);
                    if let (Some(px), Some(py)) = (px, py) {
                        if px > py {
                            violations.push(format!(
                                "{x} arrived (step {arrived_at_x}) before {y} (step {arrived_at_y}), \
                                 but {y} was delivered before {x} — final order {:?}",
                                state.delivered
                            ));
                        }
                    }
                }
            }
        }

        fn explore(
            state: SimState,
            atomic_exit: bool,
            guard_checks_generation: bool,
            violations: &mut Vec<String>,
        ) {
            // Invariant check on EVERY visited state, not just terminal
            // ones — mutual exclusion is a property of the WHOLE run, not
            // just its end. This is rev-37's own signal ("drainers=2") made
            // structural: two instances simultaneously `processing` is
            // exactly what lets the same front entry be independently
            // popped and delivered by each.
            let concurrent: Vec<u64> =
                state.instances.iter().filter(|i| i.processing).map(|i| i.generation).collect();
            if concurrent.len() >= 2 {
                violations.push(format!(
                    "{} drainer instances (generations {concurrent:?}) concurrently processing the \
                     same pty at step {} — MUTUAL EXCLUSION BROKEN. queue={:?} delivered={:?}",
                    concurrent.len(), state.step, state.queue, state.delivered
                ));
            }

            let mut any_event = false;

            // Event: an arrival. Always pushes to the back — then, matching
            // `deliver_prompt`'s real shape where EVERY admission attempts
            // `ensure_drainer` (one owns it outright, the other
            // best-effort), tries to claim the pty: succeeds only if
            // nothing is currently registered, minting a fresh generation
            // and spawning a NEW instance (mirrors `ensure_drainer` exactly
            // — a fresh `u64` per successful claim, never reused).
            for i in 0..state.not_yet_arrived.len() {
                any_event = true;
                let mut s = state.clone();
                let id = s.not_yet_arrived.remove(i);
                s.step += 1;
                s.arrived_at.insert(id, s.step);
                s.queue.push_back(id);
                if s.registered_gen.is_none() {
                    let gen = s.next_gen;
                    s.next_gen += 1;
                    s.registered_gen = Some(gen);
                    s.instances.push(DrainerInstance {
                        generation: gen,
                        processing: true,
                        commit_pending: false,
                        guard_dropped: false,
                    });
                }
                explore(s, atomic_exit, guard_checks_generation, violations);
            }

            // Event: a `processing` instance takes one step — pop+deliver
            // if the queue is non-empty, or decide to exit if it's empty.
            for idx in 0..state.instances.len() {
                if !state.instances[idx].processing {
                    continue;
                }
                any_event = true;
                let mut s = state.clone();
                s.step += 1;
                if let Some(id) = s.queue.pop_front() {
                    s.delivered.push(id);
                } else {
                    s.instances[idx].processing = false;
                    if atomic_exit {
                        // `commit_exit` as shipped: generation-checked
                        // removal in the SAME step as deciding to exit.
                        let gen = s.instances[idx].generation;
                        if s.registered_gen == Some(gen) {
                            s.registered_gen = None;
                        }
                    } else {
                        // Round-1 pre-fix split: decided to stop, but the
                        // (still generation-checked) removal is a
                        // SEPARATE, later event — `CommitDeregisters`,
                        // below.
                        s.instances[idx].commit_pending = true;
                    }
                }
                explore(s, atomic_exit, guard_checks_generation, violations);
            }

            // Event (`atomic_exit: false` only): the pending commit's
            // deregistration finally runs, generation-checked exactly like
            // the atomic case — only the TIMING relative to other events is
            // what `atomic_exit` varies, never whether it's generation-safe.
            for idx in 0..state.instances.len() {
                if !state.instances[idx].commit_pending {
                    continue;
                }
                any_event = true;
                let mut s = state.clone();
                s.step += 1;
                let gen = s.instances[idx].generation;
                if s.registered_gen == Some(gen) {
                    s.registered_gen = None;
                }
                s.instances[idx].commit_pending = false;
                explore(s, atomic_exit, guard_checks_generation, violations);
            }

            // Event: a no-longer-processing instance's `DrainerGuard`
            // finally drops. UNCONDITIONAL — every instance gets exactly
            // one of these, always, in both variants; this is the event
            // round 1's model omitted (see the module doc's "Round 2"
            // note). Requires `!commit_pending` because within ONE
            // thread's own sequential execution, `commit_exit` fully
            // returns before the function returns and the guard drops —
            // only DIFFERENT instances' events may interleave freely.
            for idx in 0..state.instances.len() {
                let inst = &state.instances[idx];
                if inst.processing || inst.commit_pending || inst.guard_dropped {
                    continue;
                }
                any_event = true;
                let mut s = state.clone();
                s.step += 1;
                let gen = s.instances[idx].generation;
                if guard_checks_generation {
                    // Round-2 fix: only remove if still this generation.
                    if s.registered_gen == Some(gen) {
                        s.registered_gen = None;
                    }
                } else {
                    // Round-1-shipped bug: unconditional — erases
                    // whatever is CURRENTLY registered, even a live
                    // successor's.
                    s.registered_gen = None;
                }
                s.instances[idx].guard_dropped = true;
                explore(s, atomic_exit, guard_checks_generation, violations);
            }

            if !any_event {
                check_terminal_state(&state, violations);
            }
        }

        fn run_exhaustive_search(ids: &[char], atomic_exit: bool, guard_checks_generation: bool) -> Vec<String> {
            let mut violations = Vec::new();
            let initial = SimState {
                queue: VecDeque::new(),
                registered_gen: None,
                next_gen: 1,
                instances: Vec::new(),
                not_yet_arrived: ids.to_vec(),
                arrived_at: HashMap::new(),
                delivered: Vec::new(),
                step: 0,
            };
            explore(initial, atomic_exit, guard_checks_generation, &mut violations);
            violations
        }

        #[test]
        fn fully_fixed_leaves_no_delivery_unclaimed_or_duplicated_across_any_reachable_interleaving() {
            // `atomic_exit: true, guard_checks_generation: true` — the code
            // as shipped after BOTH review rounds.
            for ids in [&['A', 'B'][..], &['A', 'B', 'C'][..]] {
                let violations = run_exhaustive_search(ids, true, true);
                assert!(
                    violations.is_empty(),
                    "the fully-fixed algorithm produced {} violation(s) at {} contenders:\n{}",
                    violations.len(),
                    ids.len(),
                    violations.join("\n")
                );
            }
        }

        #[test]
        fn non_atomic_commit_still_reproduces_the_round_1_lost_wakeup() {
            // Round-1 regression guard (review round 2's explicit ask):
            // with the round-2 fix ON (`guard_checks_generation: true`) but
            // `commit_exit`'s OWN atomicity broken, the search must STILL
            // find round 1's STRANDED bug — proving this restructured,
            // more-faithful model didn't accidentally weaken what round 1
            // already proved.
            let violations = run_exhaustive_search(&['A', 'B'], false, true);
            assert!(
                !violations.is_empty(),
                "reproducing the round-1 pre-fix non-atomic commit must find a stranded delivery \
                 somewhere in the 2-contender search space — if it doesn't, the fully-fixed test \
                 isn't actually testing round 1's property anymore"
            );
            assert!(
                violations.iter().any(|v| v.contains("STRANDED")),
                "expected a STRANDED-delivery violation specifically, got: {violations:?}"
            );
        }

        #[test]
        fn unconditional_guard_removal_reproduces_the_round_2_double_drain() {
            // Mutation test (rev-37's ask): reintroduce the round-2 bug
            // exactly as shipped after round 1 — `commit_exit` atomic and
            // correct (`atomic_exit: true`), but the guard's OWN removal
            // unconditional rather than generation-checked
            // (`guard_checks_generation: false`) — and confirm THIS
            // checker catches it. If this ever started passing,
            // `fully_fixed_...` above would be proven vacuous for the
            // round-2 dimension specifically.
            let violations = run_exhaustive_search(&['A', 'B', 'C'], true, false);
            assert!(
                !violations.is_empty(),
                "an unconditional guard removal must reproduce a concurrent-drainer or duplicate- \
                 delivery violation somewhere in the 3-contender search space — if it doesn't, the \
                 fully-fixed test isn't actually testing the round-2 fix"
            );
            assert!(
                violations.iter().any(|v| v.contains("MUTUAL EXCLUSION BROKEN") || v.contains("DUPLICATED")),
                "expected a mutual-exclusion or duplicate-delivery violation specifically, got: \
                 {violations:?}"
            );
        }
    }
}

/// #496 PR-C (rev-47 B1) — the stranded self-heal's ADMISSION, exhaustively.
///
/// The heal pushes a `StrandedSubmit` marker to the FRONT of a pane's queue,
/// which is the one place in this design where anything other than the
/// drainer touches the front. That is safe only while no drainer OWNS the
/// front entry: a drainer inside `deliver_now` has already peeked its entry
/// and finishes with `pop_front_dequeued(that id)`, which pops ONLY on an id
/// match. A marker slipped in front makes that pop match nothing, leaving an
/// ALREADY-DELIVERED entry queued for a second, duplicate delivery — the
/// exact hazard class #470 B1's own model exists for, one layer up.
///
/// The first shipped gate checked `queue_draining` and pushed in SEPARATE
/// lock scopes, so a drainer registering in the gap re-opened the hazard
/// (review rev-47 B1). The fix fuses the check and the push into one
/// critical section on `queues`, reading `queue_draining` while holding it.
/// This model is the proof, and — like every property module in this file —
/// carries its own mutation control so it cannot pass vacuously.
///
/// **The mutation knob.** `fused` is the ONE toggle: `true` is the real
/// (post-fix) algorithm, where "observe whether a drainer is registered" and
/// "push the marker" are a single indivisible step; `false` is the
/// shipped-then-fixed shape, where the observation is taken, other threads
/// may run, and the push happens later against a stale observation. See
/// `unfused_admission_reproduces_the_duplicate_delivery` below.
///
/// **What is modeled**, one event each, in every order the search can reach:
/// a delivery arriving (push to BACK — the front door's only move), its
/// drainer registering, that drainer peeking the front (taking ownership),
/// that drainer finishing (popping ONLY on an id match, exactly as
/// `pop_front_dequeued` does), and the heal's gate/push. Timeouts and
/// retries are omitted for the same reason the module above omits them: they
/// are self-loops this property cannot observe.
#[cfg(test)]
mod stranded_admission_property {
    use std::collections::VecDeque;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Item {
        /// An ordinary delivery, identified so a mismatched pop is visible.
        Text(u32),
        /// The self-heal's `StrandedSubmit` marker.
        Marker,
    }

    #[derive(Clone, Debug)]
    struct SimState {
        queue: VecDeque<Item>,
        /// The entry a drainer has peeked and is delivering, if any.
        owned: Option<Item>,
        drainer_registered: bool,
        /// The delivery the front door has not admitted yet (`None` once in).
        pending_arrival: Option<u32>,
        /// The heal's observation, once taken but not yet acted on — only
        /// reachable in the UNFUSED variant, which is the whole point.
        gate_saw_drainer: Option<bool>,
        heal_done: bool,
        /// Entries a drainer finished delivering.
        delivered: Vec<Item>,
    }

    /// The property: nothing already delivered may still be sitting in the
    /// queue, because that entry would be delivered a SECOND time.
    fn check(state: &SimState, violations: &mut Vec<String>) {
        for d in &state.delivered {
            if state.queue.contains(d) {
                violations.push(format!(
                    "DUPLICATE DELIVERY: {d:?} was delivered but is still queued {:?} — a \
                     mismatched `pop_front_dequeued` left it behind",
                    state.queue
                ));
            }
        }
    }

    fn explore(state: SimState, fused: bool, depth: u32, violations: &mut Vec<String>) {
        // A duplicate becomes inevitable the moment it appears, so check
        // EVERY state, not just terminal ones — it makes the counter-example
        // readable at the point of appearance.
        check(&state, violations);
        // Depth guard: the model is acyclic by construction (every event
        // consumes a one-shot flag or drains an entry), so this can only
        // ever catch a modeling bug, never a real interleaving.
        assert!(depth < 32, "runaway interleaving search — modeling bug: {state:?}");

        // Event: the front door admits a delivery — always to the BACK.
        if let Some(id) = state.pending_arrival {
            let mut s = state.clone();
            s.pending_arrival = None;
            s.queue.push_back(Item::Text(id));
            explore(s, fused, depth + 1, violations);
        }

        // Event: `ensure_drainer` registers a drainer. It registers BEFORE
        // its thread spawns, which is what makes the fused check sufficient.
        if !state.drainer_registered && !state.queue.is_empty() {
            let mut s = state.clone();
            s.drainer_registered = true;
            explore(s, fused, depth + 1, violations);
        }

        // Event: the drainer peeks the front and takes ownership of it. It
        // must hold `queues` to do this, which is precisely why the fused
        // gate can never interleave between an observation and a push.
        if state.drainer_registered && state.owned.is_none() {
            if let Some(&front) = state.queue.front() {
                let mut s = state.clone();
                s.owned = Some(front);
                explore(s, fused, depth + 1, violations);
            }
        }

        // Event: the drainer finishes. It pops the front ONLY if the front
        // is still the entry it owns — `pop_front_dequeued`'s id match.
        if let Some(owned) = state.owned {
            let mut s = state.clone();
            if s.queue.front() == Some(&owned) {
                s.queue.pop_front();
            }
            s.delivered.push(owned);
            s.owned = None;
            s.drainer_registered = !s.queue.is_empty();
            explore(s, fused, depth + 1, violations);
        }

        // Event(s): the heal admits its marker.
        if !state.heal_done {
            if fused {
                // ONE indivisible step: observe and act under the same lock.
                let mut s = state.clone();
                s.heal_done = true;
                if !s.drainer_registered {
                    s.queue.push_front(Item::Marker);
                }
                explore(s, fused, depth + 1, violations);
            } else {
                // TWO steps, with anything able to run in between — the
                // shipped-then-fixed shape review rev-47 B1 caught.
                match state.gate_saw_drainer {
                    None => {
                        let mut s = state.clone();
                        s.gate_saw_drainer = Some(s.drainer_registered);
                        explore(s, fused, depth + 1, violations);
                    }
                    Some(saw) => {
                        let mut s = state.clone();
                        s.heal_done = true;
                        if !saw {
                            s.queue.push_front(Item::Marker);
                        }
                        explore(s, fused, depth + 1, violations);
                    }
                }
            }
        }
    }

    fn run(fused: bool) -> Vec<String> {
        let mut violations = Vec::new();
        explore(
            SimState {
                queue: VecDeque::new(),
                owned: None,
                drainer_registered: false,
                pending_arrival: Some(1),
                gate_saw_drainer: None,
                heal_done: false,
                delivered: Vec::new(),
            },
            fused,
            0,
            &mut violations,
        );
        violations
    }

    #[test]
    fn fused_admission_never_duplicates_a_delivery() {
        let violations = run(true);
        assert!(
            violations.is_empty(),
            "fusing the gate check and the marker push into one `queues` critical section must \
             make a duplicate delivery unreachable, but the search found {}:\n{}",
            violations.len(),
            violations.join("\n")
        );
    }

    #[test]
    fn unfused_admission_reproduces_the_duplicate_delivery() {
        // The mutation control: the shape this PR originally shipped. If it
        // ever stops finding a violation, the fused test is vacuous and this
        // model has stopped modeling the hazard.
        let violations = run(false);
        assert!(
            !violations.is_empty(),
            "checking `queue_draining` and pushing in SEPARATE lock scopes must reproduce the \
             duplicate delivery somewhere in the search space — if it doesn't, the fused test \
             isn't testing anything"
        );
        assert!(
            violations.iter().any(|v| v.contains("DUPLICATE DELIVERY")),
            "expected a duplicate-delivery violation specifically, got: {violations:?}"
        );
    }
}

/// #454 — supersession vs. a newer delivery's IN-FLIGHT window, exhaustively.
///
/// #451 B1 gave the late-confirmation monitor a supersession rule: if this
/// pane's recorded `DeliveryOutcome` no longer carries this monitor's own
/// `submit_sent_ms`, a newer delivery owns the pane and the monitor exits
/// writing and notifying nothing. That rule is right; its TRIGGER was late.
/// The ledger was written only when the newer delivery's confirm window
/// resolved — under a second when its hook confirms in-window, ~9s worst
/// case — while the `promptsubmit` record that delivery's Enter produces
/// exists almost immediately. In between, a stale monitor reads "still mine"
/// from the ledger and matches the NEWER delivery's record as its own: a
/// misattributed `delivery-confirmed-late` audit row, and — if that monitor
/// had already declared `Failed` — a "no re-send needed" correction about the
/// very re-send that is the only reason anything landed. On a Copilot pane it
/// does not even need the two deliveries to share text: `PromptLandedMatch::
/// Existence` matches any record past the monitor's baseline, whatever it
/// says.
///
/// The fix is an ORDERING, and this model is its proof. Two knobs, each the
/// pre-#454 shape of one half, so neither half can pass vacuously:
///
/// - `publish_before_enter` — `deliver_now` calls `record_inflight_delivery`
///   immediately BEFORE its first Enter (`true`), rather than claiming the
///   ledger only at its outcome insert (`false`, as shipped).
/// - `ledger_read_after_hook` — the monitor takes its ledger observation
///   AFTER its hook read (`true`, via `observe_ledger`), rather than before
///   it (`false`, as shipped).
///
/// Both together give a happens-before chain — ledger claim, then Enter, then
/// the hook record, then any tick that can see that record, then that tick's
/// own ledger read — so a record a tick can act on is always checked against
/// a ledger state at least as new as the record itself. Either knob alone
/// leaves a window: with only the reader fix the ledger claim still lands
/// seconds late; with only the writer fix the gap shrinks to one tick's own
/// work but a violation is still reachable. `run` explores every interleaving
/// of both, and the tests below pin exactly that — fixed is clean, each
/// single-knob mutation reddens, and the as-shipped shape (both off) reddens.
///
/// **What is modeled**, one event each, in every order the search can reach:
/// the newer delivery's program in strict order (claim the ledger — only when
/// `publish_before_enter` — press Enter, record its outcome), a genuinely
/// LATE `promptsubmit` record for the OLD delivery arriving on its own (the
/// case the monitor exists to catch, so the property cannot be satisfied by
/// simply never confirming anything), and the old monitor's tick — its two
/// reads, individually interleavable, then its decision, following
/// `late_monitor_tick`'s real precedence (superseded beats even a hook
/// match). Timeouts, retries and the pane-quiet/question inputs are omitted
/// for the same reason the sibling modules omit theirs: they are self-loops
/// this property cannot observe.
///
/// **Deliberately more adversarial than production.** Nothing here forces the
/// monitor's two reads to be close together, or the newer delivery's steps to
/// be quick — the search happily runs a whole monitor tick between the newer
/// delivery's Enter and its next step. Production's reachable states are a
/// subset of this model's, so a property proven here holds a fortiori.
#[cfg(test)]
mod supersession_race_property {
    /// Who the pane's ledger entry currently names.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Owner {
        /// The OLD delivery — the one whose late monitor is still running.
        Old,
        /// The NEWER delivery (the re-send).
        New,
    }

    /// One step of the newer delivery's own (single-threaded) program.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Op {
        /// `record_inflight_delivery` — present only when the writer half of
        /// the fix is on.
        ClaimLedger,
        /// The first Enter. Its `promptsubmit` record exists from here on.
        PressEnter,
        /// The end-of-window outcome insert, which also claims the ledger —
        /// the ONLY claim in the pre-#454 shape.
        RecordOutcome,
    }

    fn newer_delivery_program(publish_before_enter: bool) -> Vec<Op> {
        let mut ops = Vec::new();
        if publish_before_enter {
            ops.push(Op::ClaimLedger);
        }
        ops.push(Op::PressEnter);
        ops.push(Op::RecordOutcome);
        ops
    }

    /// What one `poll_promptsubmit_hook` call returned. The monitor cannot
    /// tell WHOSE record it matched — that is the defect — so the model
    /// tracks provenance only for the checker's benefit, never as an input to
    /// the modeled decision.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct HookRead {
        /// A record past this monitor's baseline exists.
        saw_record: bool,
        /// ...and the only record that exists is the newer delivery's, so a
        /// confirm off this read is necessarily a misattribution.
        newer_only: bool,
    }

    /// Where the old monitor is within ONE tick. The two reads are separate
    /// events precisely so everything else can run between them.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Tick {
        Start,
        /// The first read (whichever the knob orders first) has been taken;
        /// the other is still pending, and stays `None` until it is.
        Half { ledger_superseded: Option<bool>, hook: Option<HookRead> },
        /// Both reads taken — the decision is the next event.
        Ready { ledger_superseded: bool, hook: HookRead },
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Monitor {
        Ticking { tick: Tick, ticks_left: u32 },
        /// `MonitorAction::Superseded` (or the lifetime cap) — exits writing
        /// and notifying nothing.
        Exited,
        /// `MonitorAction::Confirm` — writes the ledger, audits
        /// `delivery-confirmed-late`, and (when it had already declared
        /// `Failed`) sends the correction notice.
        Confirmed { misattributed: bool },
    }

    #[derive(Clone, Debug)]
    struct SimState {
        ledger: Owner,
        /// Set by `Op::PressEnter` — the newer delivery's own hook record.
        newer_record: bool,
        /// A genuinely late record for the OLD delivery, arriving on its own.
        /// One-shot, available at any point (`older_record_possible`).
        older_record: bool,
        newer_pc: usize,
        monitor: Monitor,
    }

    #[derive(Default)]
    struct Outcomes {
        violations: Vec<String>,
        /// Whether the search ever reached a CORRECT late confirm — the
        /// liveness half, so "never confirm anything" cannot pass as a fix.
        legit_confirm_reachable: bool,
    }

    struct Cfg {
        publish_before_enter: bool,
        ledger_read_after_hook: bool,
        /// Whether a newer delivery happens at all.
        newer_delivery: bool,
        /// Whether the old delivery's own late record may arrive.
        older_record_possible: bool,
    }

    fn explore(state: SimState, cfg: &Cfg, program: &[Op], depth: u32, out: &mut Outcomes) {
        // The model is acyclic by construction — every event either advances
        // a program counter, consumes a one-shot flag, or spends a tick from
        // a finite budget — so this can only ever catch a modeling bug.
        assert!(depth < 96, "runaway interleaving search — modeling bug: {state:?}");

        match state.monitor {
            Monitor::Confirmed { misattributed: true } => {
                out.violations.push(format!(
                    "MISATTRIBUTED CONFIRM: the old delivery's monitor resolved itself off the \
                     NEWER delivery's promptsubmit record, on a tick whose ledger sample said \
                     \"still mine\" (live ledger now names {:?}; newer delivery at step {} of \
                     {}) — a false delivery-confirmed-late, and a \"no re-send needed\" \
                     correction about the re-send that made it land",
                    state.ledger,
                    state.newer_pc,
                    program.len(),
                ));
                return;
            }
            Monitor::Confirmed { misattributed: false } => {
                out.legit_confirm_reachable = true;
                return;
            }
            // Nothing this monitor does afterwards is observable.
            Monitor::Exited => return,
            Monitor::Ticking { .. } => {}
        }

        // Event: the newer delivery takes its next step. Strictly ordered —
        // it is one thread running one function.
        if cfg.newer_delivery && state.newer_pc < program.len() {
            let mut s = state.clone();
            match program[s.newer_pc] {
                Op::ClaimLedger | Op::RecordOutcome => s.ledger = Owner::New,
                Op::PressEnter => s.newer_record = true,
            }
            s.newer_pc += 1;
            explore(s, cfg, program, depth + 1, out);
        }

        // Event: the OLD delivery's own hook record finally lands — the late
        // confirmation this monitor exists to catch.
        if cfg.older_record_possible && !state.older_record {
            let mut s = state.clone();
            s.older_record = true;
            explore(s, cfg, program, depth + 1, out);
        }

        // Event(s): the monitor's tick — two reads, then a decision.
        let Monitor::Ticking { tick, ticks_left } = state.monitor else { return };
        let read_ledger = |s: &SimState| s.ledger != Owner::Old;
        let read_hook = |s: &SimState| HookRead {
            saw_record: s.older_record || s.newer_record,
            newer_only: s.newer_record && !s.older_record,
        };
        match tick {
            Tick::Start => {
                let mut s = state.clone();
                s.monitor = Monitor::Ticking {
                    tick: if cfg.ledger_read_after_hook {
                        Tick::Half { ledger_superseded: None, hook: Some(read_hook(&state)) }
                    } else {
                        Tick::Half { ledger_superseded: Some(read_ledger(&state)), hook: None }
                    },
                    ticks_left,
                };
                explore(s, cfg, program, depth + 1, out);
            }
            Tick::Half { ledger_superseded, hook } => {
                let mut s = state.clone();
                let (superseded, hook) = match (ledger_superseded, hook) {
                    (None, Some(h)) => (read_ledger(&state), h),
                    (Some(l), None) => (l, read_hook(&state)),
                    other => unreachable!("a half-taken tick has exactly one read: {other:?}"),
                };
                s.monitor =
                    Monitor::Ticking { tick: Tick::Ready { ledger_superseded: superseded, hook }, ticks_left };
                explore(s, cfg, program, depth + 1, out);
            }
            Tick::Ready { ledger_superseded, hook } => {
                let mut s = state.clone();
                // `late_monitor_tick`'s real precedence: superseded first,
                // before even a hook match.
                s.monitor = if ledger_superseded {
                    Monitor::Exited
                } else if hook.saw_record {
                    Monitor::Confirmed { misattributed: hook.newer_only }
                } else if ticks_left == 0 {
                    // The lifetime cap — `MonitorAction::Expired`, not a
                    // violation: an unresolved delivery timing out is a
                    // documented outcome.
                    Monitor::Exited
                } else {
                    Monitor::Ticking { tick: Tick::Start, ticks_left: ticks_left - 1 }
                };
                explore(s, cfg, program, depth + 1, out);
            }
        }
    }

    fn run(cfg: Cfg) -> Outcomes {
        let program = newer_delivery_program(cfg.publish_before_enter);
        let mut out = Outcomes::default();
        explore(
            SimState {
                ledger: Owner::Old,
                newer_record: false,
                older_record: false,
                newer_pc: 0,
                monitor: Monitor::Ticking {
                    tick: Tick::Start,
                    // More ticks than there are progress events, so every
                    // "the monitor polls between two of the other thread's
                    // steps" ordering is reachable rather than budget-capped.
                    ticks_left: 5,
                },
            },
            &cfg,
            &program,
            0,
            &mut out,
        );
        out
    }

    fn cfg(publish_before_enter: bool, ledger_read_after_hook: bool) -> Cfg {
        Cfg { publish_before_enter, ledger_read_after_hook, newer_delivery: true, older_record_possible: true }
    }

    #[test]
    fn fixed_ordering_never_misattributes_a_newer_deliverys_hook_record() {
        // Both halves of #454's fix on — the code as shipped by this PR.
        let out = run(cfg(true, true));
        assert!(
            out.violations.is_empty(),
            "claiming the ledger before the Enter AND reading the ledger after the hook must make \
             a misattributed confirm unreachable, but the search found {}:\n{}",
            out.violations.len(),
            out.violations.join("\n")
        );
    }

    #[test]
    fn claiming_the_ledger_only_at_the_outcome_reproduces_the_454_race() {
        // Mutation control #1: the writer half reverted to the pre-#454 shape
        // (the ledger claimed only by the end-of-window outcome insert), the
        // reader half still fixed. This is #454's own description of the gap,
        // and it must stay reachable — otherwise `fixed_ordering_...` is
        // vacuous for the `record_inflight_delivery` dimension.
        let out = run(cfg(false, true));
        assert!(
            !out.violations.is_empty(),
            "claiming the ledger only at the outcome insert must leave the in-flight window open \
             somewhere in the search space — if it doesn't, the fixed test isn't testing the \
             record_inflight_delivery half"
        );
        assert!(
            out.violations.iter().any(|v| v.contains("MISATTRIBUTED CONFIRM")),
            "expected a misattributed-confirm violation specifically, got: {:?}",
            out.violations
        );
    }

    #[test]
    fn reading_the_ledger_before_the_hook_reproduces_the_454_race() {
        // Mutation control #2: the writer half fixed, the reader half
        // reverted — the monitor samples the ledger first, then the hook, so
        // it can act on evidence newer than the state it validated against.
        // The window is one tick's own work rather than a whole confirm
        // window, which is exactly why narrowing it is not closing it.
        let out = run(cfg(true, false));
        assert!(
            !out.violations.is_empty(),
            "sampling the ledger BEFORE the hook must leave a reachable misattribution even with \
             the in-flight claim in place — if it doesn't, the fixed test isn't testing the \
             observe_ledger-after-the-hook half"
        );
        assert!(
            out.violations.iter().any(|v| v.contains("MISATTRIBUTED CONFIRM")),
            "expected a misattributed-confirm violation specifically, got: {:?}",
            out.violations
        );
    }

    #[test]
    fn pre_454_shape_as_shipped_reproduces_the_race() {
        // Both halves reverted — main's algorithm exactly, as #451 B1 left
        // it. #454's claim that the residual is real, re-derived against the
        // current machinery rather than taken on trust from the filing.
        let out = run(cfg(false, false));
        assert!(
            out.violations.iter().any(|v| v.contains("MISATTRIBUTED CONFIRM")),
            "the shape shipped before #454 must reproduce the misattribution, got: {:?}",
            out.violations
        );
    }

    #[test]
    fn the_fix_still_lets_a_genuinely_late_confirmation_through() {
        // The liveness half: a monitor that simply never confirms would pass
        // every test above. With the fix on and NO newer delivery, the old
        // delivery's own late hook record must still resolve it — that is the
        // entire reason `run_late_confirmation_monitor` exists (#112's live
        // episode needed 38 minutes to land).
        let out = run(Cfg {
            publish_before_enter: true,
            ledger_read_after_hook: true,
            newer_delivery: false,
            older_record_possible: true,
        });
        assert!(out.violations.is_empty(), "no newer delivery means nothing to misattribute");
        assert!(
            out.legit_confirm_reachable,
            "with the fix on and no newer delivery, a late record for THIS delivery must still \
             reach MonitorAction::Confirm — otherwise #454's fix has broken #112's whole feature"
        );
    }

    #[test]
    fn a_correct_late_confirm_survives_even_alongside_a_newer_delivery() {
        // Sharper than the test above: with the fix on AND a newer delivery
        // racing, the search must STILL reach a correct confirm (the old
        // delivery's own record arriving before the newer delivery claims the
        // pane). The fix must suppress misattributed confirms only, never
        // every confirm that happens to have company.
        let out = run(cfg(true, true));
        assert!(
            out.legit_confirm_reachable,
            "the fix must not suppress a correct late confirm just because a newer delivery is \
             also in flight"
        );
    }
}
