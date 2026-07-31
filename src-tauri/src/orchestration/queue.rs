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
//! specifically — for the full design rationale, including the
//! honestly-argued limits of the in-memory choice.
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
//!   same CLI session id. Neither `pty_id` nor `agent_id` is that identity;
//!   both are re-minted at restore.
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

use serde::{Deserialize, Serialize};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnqueueReason {
    /// The pre-paste box-occupied hold (#111) capped out — nothing pasted.
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
}

impl EnqueueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EnqueueReason::BoxOccupied => "box-occupied",
            EnqueueReason::Question => "question",
            EnqueueReason::BehindQueue => "behind-queue",
            EnqueueReason::Arrival => "arrival",
            EnqueueReason::KickoffRecovery => "kickoff-recovery",
            EnqueueReason::Recovered => "recovered",
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
/// direction).
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
    #[serde(default)]
    pub group: String,
    /// #467: whether the target was this group's orchestrator — the one
    /// delivery target with an identity that outlives a restart.
    ///
    /// Neither of the obvious keys does: `pty_id` is re-minted by the
    /// terminal layer on every restore, and `agent_id` is re-minted too
    /// (`orch-{seq}` / `w-{seq}` off a fresh in-memory counter), which is
    /// why #468's own filing — "`agent_id` is kept so a durable follow-up
    /// could rebind by agent (durable across a restore)" — does not hold
    /// and is not what this implements. A group has exactly one
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
    RejectFull,
}

/// Decide `AdmitDecision` for a new text payload against an existing queue.
pub fn admit(queue: &VecDeque<QueuedDelivery>, text: &str) -> AdmitDecision {
    if queue.iter().any(|q| q.payload.text() == Some(text)) {
        return AdmitDecision::Coalesce;
    }
    if queue.len() >= QUEUE_MAX_PER_PANE {
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
    };
    format!(
        "[loomux] delivery to {agent_id} queued ({why}) — delivers automatically once clear; do NOT re-send"
    )
}

/// The one line a drain delivers FIRST, ahead of whatever it flushes — the
/// "N deliveries queued ... re-sync" nudge the issue's own root-cause
/// analysis asked for. For an orchestrator target this doubles as the
/// re-sync prompt; it cannot loop, because it only ever fires on UNBLOCK,
/// when delivery demonstrably works again.
pub fn flush_header_text(count: usize, coalesced: usize) -> String {
    let n = if count == 1 { "1 delivery".to_string() } else { format!("{count} deliveries") };
    if coalesced > 0 {
        format!(
            "[loomux] {n} queued while this pane was blocked ({coalesced} coalesced) are now delivering, oldest first"
        )
    } else {
        format!("[loomux] {n} queued while this pane was blocked are now delivering, oldest first")
    }
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

/// Whether `depth` (queue length) freshly crosses the still-queued notice
/// threshold this poll tick — pure edge trigger so the impure caller can
/// fire the notice exactly once per queue lifetime rather than every 2s
/// poll once past 30 minutes. `oldest_enqueued_ms` is the FRONT entry's
/// timestamp (the one that's been waiting longest); `already_notified`
/// tracks whether this exact queue already fired it.
pub fn should_fire_still_queued_notice(
    oldest_enqueued_ms: u64,
    now_ms: u64,
    already_notified: bool,
) -> bool {
    !already_notified
        && now_ms.saturating_sub(oldest_enqueued_ms) >= QUEUE_STILL_QUEUED_NOTICE_AFTER.as_millis() as u64
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

/// Whether a recovered entry has a durable identity to re-bind to when
/// `agent` comes back (#467). Pure so the matching rule is stated in one
/// place and tested directly, rather than being an `if` buried in a bind
/// callback: an entry re-binds when it was addressed to this group's
/// orchestrator and `agent` IS that orchestrator, or when both carry the
/// SAME non-empty CLI session id. Everything else — including two entries
/// that merely share an `agent_id`, which is re-minted at restore and
/// therefore proves nothing — does not match.
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
}

impl OrphanSource {
    pub fn as_str(self) -> &'static str {
        match self {
            OrphanSource::Snapshot => "snapshot",
            OrphanSource::Audit => "audit",
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
/// `delivery-recovered` (#467) is terminal here for the same reason the
/// other two are: from the moment a restart reads an entry back out of
/// `queue.json`, the snapshot derivation owns it — it is either staged (and
/// reported by `OrchRegistry::queue_orphans`'s snapshot half, with its
/// payload) or re-admitted under a FRESH id whose own `delivery-queued` line
/// tracks it from there. Leaving it open here would report every recovered
/// entry twice, forever, under an id nothing will ever close.
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
            group: "g-1".into(),
            to_orchestrator: false,
            session_id: None,
        }
    }

    // ---------- admit ----------

    #[test]
    fn admit_appends_to_an_empty_queue() {
        let q: VecDeque<QueuedDelivery> = VecDeque::new();
        assert_eq!(admit(&q, "hello"), AdmitDecision::Admit);
    }

    #[test]
    fn admit_coalesces_a_byte_identical_repeat() {
        let mut q = VecDeque::new();
        q.push_back(text_entry(1, "give me a status update"));
        assert_eq!(admit(&q, "give me a status update"), AdmitDecision::Coalesce);
    }

    #[test]
    fn admit_does_not_coalesce_semantically_similar_but_different_text() {
        // The design's whole argument: three DIFFERENT task briefs must all
        // deliver. Only an exact byte match collapses.
        let mut q = VecDeque::new();
        q.push_back(text_entry(1, "give me a status update"));
        assert_eq!(admit(&q, "give me a status update please"), AdmitDecision::Admit);
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
            group: "g-1".into(),
            to_orchestrator: false,
            session_id: None,
        });
        // Empty string would trivially "match" a bad comparison — pin the
        // marker is simply never a coalesce target at all.
        assert_eq!(admit(&q, ""), AdmitDecision::Admit);
    }

    #[test]
    fn admit_rejects_newest_at_the_cap() {
        let mut q = VecDeque::new();
        for i in 0..QUEUE_MAX_PER_PANE {
            q.push_back(text_entry(i as u64, &format!("distinct-{i}")));
        }
        assert_eq!(admit(&q, "one-more"), AdmitDecision::RejectFull);
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
        assert_eq!(admit(&q, "distinct-0"), AdmitDecision::Coalesce);
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
    fn flush_header_singular_and_plural_and_coalesced_clause() {
        assert!(flush_header_text(1, 0).contains("1 delivery "), "{}", flush_header_text(1, 0));
        assert!(flush_header_text(3, 0).contains("3 deliveries"), "{}", flush_header_text(3, 0));
        let h = flush_header_text(3, 1);
        assert!(h.contains("1 coalesced"), "got: {h}");
        assert!(!flush_header_text(3, 0).contains("coalesced"), "must omit the clause when nothing coalesced");
        assert!(flush_header_text(3, 0).contains("oldest first"));
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

        // `agent_id` is deliberately NOT a key: it is re-minted at restore,
        // so two agents sharing one proves nothing. Both entries here carry
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
