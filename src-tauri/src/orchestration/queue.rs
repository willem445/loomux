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
//! registry, no `gh` — with ONE deliberate exception: `queue_is_non_empty`
//! takes the live queue-map lock directly, because it is the single
//! decision BOTH the front door and `deliver_now`'s pre-paste recheck must
//! consult (rev-35 review, B1) — splitting it into "pure decision" plus
//! "impure caller" would let the two checkpoints drift to different
//! definitions of "this pane is already spoken for," which is exactly the
//! class of bug it exists to prevent. See `notify.rs`'s module doc for why
//! this codebase otherwise splits every backend feature this way (pure
//! policy here, impure wiring in `mod.rs`) and `doc/design/orchestration.md`'s
//! "Delivery queue (#445)" section for the full design rationale, including
//! the honestly-argued limits of the in-memory choice.
//!
//! **Scope, stated plainly (the persistence limitation):** this queue is
//! in-memory only. A loomux restart during a blocked window loses whatever
//! is queued at that instant — nothing replays it. `deliver_prompt` audits
//! the `prompt` action with the full payload text before a delivery thread
//! ever runs, and `orphaned_queue_entries` (below) can mechanically
//! reconstruct which of those payloads were queued but never resolved
//! (`delivery-dequeued`/`delivery-dropped`) by reading `audit.jsonl` alone —
//! but as of this PR nothing in the orchestrator's session-start re-sync
//! calls it, so the derivation being POSSIBLE does not yet make the loss
//! RECOVERABLE. Do not claim otherwise; a follow-up issue wires this
//! function into something the orchestrator actually reads.

use crate::obs::LockExt;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueReason {
    /// The pre-paste box-occupied hold (#111) capped out — nothing pasted.
    BoxOccupied,
    /// A pre-paste or pre-Enter interactive-question hold (#420) capped out.
    Question,
    /// The pane's queue was already non-empty when this delivery arrived —
    /// the front-door check: a fresh prompt must never overtake ones
    /// already waiting, or "here's context" / "now go" can invert.
    BehindQueue,
}

impl EnqueueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EnqueueReason::BoxOccupied => "box-occupied",
            EnqueueReason::Question => "question",
            EnqueueReason::BehindQueue => "behind-queue",
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug)]
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

/// Whether `pty_id`'s queue is non-empty right now — the ONE check both the
/// front door (`deliver_prompt`, at a fresh delivery's arrival) and
/// `deliver_now`'s pre-paste recheck (rev-35 review, B1) consult, so the two
/// checkpoints can never silently drift to different definitions of "this
/// pane is already spoken for."
///
/// **Why a SECOND check is required at all.** The front door alone only
/// protects deliveries that ARRIVE after a queue already exists — it says
/// nothing about a delivery already in flight (holding the pane's per-pty
/// delivery mutex through its own pre-paste hold, `lock` in `deliver_now`)
/// when the queue FORMS underneath it: an earlier-arriving delivery can
/// time out its own hold, enqueue, and spawn a drainer while a
/// later-arriving one is still blocked on the mutex or mid-hold, having
/// already passed the front door back when the queue was still empty. Live
/// evidence of exactly this shape is what the #445 review (rev-35) reported
/// against the shipped PR: "now go" landing before "here's the context"
/// because the second delivery never re-checked. Calling this again
/// immediately before the paste closes that window; the residual gap
/// between this call and the actual `write_bytes` is a few function calls,
/// not a 60-120s hold.
pub fn queue_is_non_empty(queues: &Mutex<HashMap<u32, VecDeque<QueuedDelivery>>>, pty_id: u32) -> bool {
    queues.lock_safe().get(&pty_id).is_some_and(|q| !q.is_empty())
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
/// enters the queue via a hold-cap expiry OR the rev-35 B1 pre-paste recheck
/// (`BehindQueue` reaching here means the queue filled DURING this
/// delivery's own hold — see `queue_is_non_empty`'s doc — which is
/// notice-worthy exactly like the other two reasons; only the SILENT
/// front-door append, at plain arrival with no hold at all, sends no notice
/// of its own — that path never calls this function). Replaces the old
/// "held: ... re-send when clear" wording — the #445 honesty fix: the
/// payload is safe and WILL deliver on its own: a re-send now would just
/// create a duplicate once the drain lands.
pub fn queued_notice(agent_id: &str, reason: EnqueueReason) -> String {
    let why = match reason {
        EnqueueReason::BoxOccupied => "pane has human input",
        EnqueueReason::Question => "an interactive question is on screen",
        EnqueueReason::BehindQueue => "another delivery to this pane was already queued",
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

/// One audit-derived fact about a delivery that entered the queue but has
/// no matching terminal event (`delivery-dequeued` / `delivery-dropped` /
/// `delivery-coalesced`) for the same `id` — i.e. a restart happened while
/// it was still waiting. See the module doc's "Scope" section: this
/// function makes the derivation REAL, but nothing calls it from the
/// orchestrator's own re-sync yet, so the loss is forensically visible, not
/// yet recovered automatically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrphanedQueueEntry {
    pub id: u64,
    pub agent_id: String,
    pub enqueued_ms: u64,
    pub reason: String,
}

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
/// `delivery-dequeued`/`delivery-dropped` for that SAME id — a queue entry
/// that a restart caught mid-wait. `delivery-coalesced` entries are not
/// scanned for orphans: a coalesced payload was never independently
/// queued — it was folded into the entry it duplicated, whose own id is
/// what to check.
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
            "delivery-dequeued" | "delivery-dropped" => {
                if let Some(id) = l.id {
                    open.remove(&id);
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<OrphanedQueueEntry> = open
        .into_iter()
        .map(|(id, (agent_id, enqueued_ms, reason))| OrphanedQueueEntry { id, agent_id, enqueued_ms, reason })
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

    // ---------- queue_is_non_empty (rev-35 B1) ----------

    #[test]
    fn queue_is_non_empty_reads_true_only_while_something_is_actually_queued() {
        let queues: Mutex<HashMap<u32, VecDeque<QueuedDelivery>>> = Mutex::new(HashMap::new());
        assert!(!queue_is_non_empty(&queues, 42), "no map entry at all — must read empty");
        queues.lock_safe().insert(42, VecDeque::new());
        assert!(!queue_is_non_empty(&queues, 42), "an empty VecDeque — must still read empty");
        queues.lock_safe().get_mut(&42).unwrap().push_back(text_entry(1, "hi"));
        assert!(queue_is_non_empty(&queues, 42));
        // A DIFFERENT pty's queue must never leak into this one's answer.
        assert!(!queue_is_non_empty(&queues, 43), "an unrelated pty must read empty");
        queues.lock_safe().get_mut(&42).unwrap().pop_front();
        assert!(!queue_is_non_empty(&queues, 42), "draining back to empty must flip the answer back");
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
}

/// #445 rev-35 B1 — the ordering PROPERTY, exhaustively, not one scenario.
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
/// `three_way_contention_can_still_invert_arrival_order_known_residual`,
/// below, which documents it rather than silently dropping it. Restricting
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
    fn three_way_contention_can_still_invert_arrival_order_known_residual() {
        // NOT a regression to fix in this PR — a DISCOVERED, DOCUMENTED
        // residual, reported rather than silently dropped (per the module
        // doc's "Scope" section). With 3 simultaneous contenders, this
        // exhaustive search finds an inversion even with the B1 recheck
        // fully applied — not because the recheck is wrong (each individual
        // delivery, once it holds the mutex, correctly defers to whatever
        // is already queued), but because `std::sync::Mutex` can grant the
        // freed mutex to EITHER of two simultaneous waiters regardless of
        // which arrived first, and that choice alone can put the
        // later-arriving one through its own hold-and-decide cycle first.
        // Fixing this would mean replacing raw mutex contention with a real
        // FIFO ticket for ACQUIRING the delivery mutex itself — a
        // materially bigger change than "re-check the queue at the paste
        // point," and specifically NOT what was asked for as a localized
        // fix. This test exists so the gap stays visible (asserted, not
        // merely mentioned in a comment that can rot) rather than silently
        // reappearing as a surprise later.
        let violations =
            run_exhaustive_search(&['A', 'B', 'C'], |queue_non_empty_at_recheck| queue_non_empty_at_recheck);
        assert!(
            !violations.is_empty(),
            "if this ever starts passing, the 3-way mutex-fairness gap has been closed somehow — \
             replace this test with a proper property assertion at N=3 and say how it happened, \
             rather than deleting the coverage"
        );
    }
}
