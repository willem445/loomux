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
//! Everything in this module is a plain function over plain data: no lock,
//! no registry, no `gh`. See `notify.rs`'s module doc for why this codebase
//! splits every backend feature this way (pure policy here, impure wiring
//! in `mod.rs`) and `doc/design/orchestration.md`'s "Delivery queue (#445)"
//! section for the full design rationale, including the honestly-argued
//! limits of the in-memory choice.
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

use std::collections::VecDeque;
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
/// enters the queue via a hold-cap expiry (never for `BehindQueue` — that is
/// the normal, expected case once a queue exists, and needs no notice of
/// its own). Replaces the old "held: ... re-send when clear" wording — the
/// #445 honesty fix: the payload is safe and WILL deliver on its own: a
/// re-send now would just create a duplicate once the drain lands.
pub fn queued_notice(agent_id: &str, reason: EnqueueReason) -> String {
    let why = match reason {
        EnqueueReason::BoxOccupied => "pane has human input",
        EnqueueReason::Question => "an interactive question is on screen",
        EnqueueReason::BehindQueue => "queue not empty", // never actually sent — see doc above
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
