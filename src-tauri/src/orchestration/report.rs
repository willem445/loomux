//! Pure core of the decision-grade report protocol (#398): the structured
//! `outcome` vocabulary, the status it implies for idle/attention bookkeeping,
//! the hard cap on `note` (structural enforcement over prose — a cap the TOOL
//! enforces beats a guideline the template merely asks for), and the notice
//! text composed for the orchestrator's pane. `mcp.rs`'s `"report"` dispatch
//! arm is the impure half: argument extraction, the idle/attention side
//! effects (`set_agent_idle`, `note_report_attention`), and delivery.
//!
//! The legacy shape (`status` + free-text `summary`) is unchanged and stays
//! legal forever (soft-deprecated: accepted, but the role templates stop
//! teaching it) — see `mcp.rs` for how the two shapes coexist in one tool.
//!
//! Since #850 this module also owns the cap on the OTHER agent-authored text
//! that lands in the orchestrator's pane: the courtesy notice a recorded
//! verdict types there (`verdict_notice_summary`). Same principle, same
//! truncation primitive — what an agent writes into another agent's pane is
//! resident context the recipient re-pays for on every subsequent turn, so the
//! notice is a wake-up signal and the record lives where a reader can go get
//! it.

/// Legal `outcome` values — a superset of the legacy `status` enum. `approved`
/// / `request_changes` let a reviewer's report classify itself without
/// borrowing the worker-shaped `done`/`blocked` vocabulary for something that
/// isn't a worker completion.
pub const OUTCOMES: [&str; 5] = ["done", "blocked", "approved", "request_changes", "progress"];

/// Legal legacy `status` values — unchanged from the pre-#398 tool.
pub const STATUSES: [&str; 3] = ["progress", "done", "blocked"];

/// Hard cap on `note`'s length, in **characters** (never bytes — a cap
/// measured in bytes could split a multi-byte codepoint mid-character).
/// ~500 chars is a decision-grade paragraph, not an essay; `truncate_note`
/// enforces it structurally and states the truncation rather than silently
/// dropping text.
pub const NOTE_CHAR_CAP: usize = 500;

/// The idle/attention-facing status implied by `outcome`, for a report that
/// supplies `outcome` but omits the legacy `status` — a reviewer's `approved`
/// or `request_changes` both mean "this agent's turn is over, it's idle
/// again", exactly like a worker's `done`. Never called with a value outside
/// `OUTCOMES` — the caller validates that first.
pub fn status_for_outcome(outcome: &str) -> &'static str {
    match outcome {
        "blocked" => "blocked",
        "progress" => "progress",
        _ => "done", // done | approved | request_changes
    }
}

/// Hard cap, in **characters**, on how much of a recorded verdict's summary is
/// copied into the orchestrator's pane by the courtesy notice `review_verdict`
/// types there (#850).
///
/// A summary may be up to `workflow::MAX_SUMMARY_CHARS` (4000) and every
/// character of it used to be typed into the orchestrator's pane — then
/// restated a second time by the reviewer's own `report(...)`. That pane text
/// becomes the orchestrator's *resident* context, re-sent on every subsequent
/// API call, which makes it the most expensive prose in the system: one review
/// round of eight verdict events measured ≈15k duplicated tokens.
///
/// ~400 characters is a paragraph — enough to route on (what class of finding,
/// how bad, whether to send it back or ask a human) without being the record.
/// The record is unchanged and complete in three places a reader can reach on
/// demand: the verdict file, `list_verdicts`, and the review posted on the PR.
pub const VERDICT_NOTICE_SUMMARY_CAP: usize = 400;

/// Truncate `note` to `NOTE_CHAR_CAP` characters, appending a marker that
/// states the truncation happened and points at `detail_url` for the rest —
/// never a silent cut. A no-op (returns `note` unchanged) when already under
/// the cap, so a short note round-trips byte-for-byte.
pub fn truncate_note(note: &str) -> String {
    truncate_chars(note, NOTE_CHAR_CAP, |total| {
        format!(" […truncated, {total} chars total — see detail_url]")
    })
}

/// The summary as the orchestrator's pane receives it: capped at
/// `VERDICT_NOTICE_SUMMARY_CAP` characters with a **fixed pointer** at where
/// the whole thing lives (#850). Untouched when it already fits, so a reviewer
/// writing the ~100 words the templates ask for round-trips byte-for-byte and
/// never sees a marker.
///
/// The pointer names `list_verdicts` and the PR rather than "see above",
/// because a truncated notice is read by an agent that has to *decide* whether
/// it needs the rest — and the tool call that gets it is the actionable half.
/// Nothing else is filtered here (newlines and the like are already handled by
/// `workflow::sanitize_summary` at the write): this is a length cap, and a
/// second responsibility bolted on would make the boundary tests below stop
/// describing one thing.
pub fn verdict_notice_summary(summary: &str) -> String {
    truncate_chars(summary, VERDICT_NOTICE_SUMMARY_CAP, |total| {
        format!(" […truncated, {total} chars total — full summary on the PR and via list_verdicts]")
    })
}

/// Keep the first `cap` **characters** of `text`, appending `marker(total)`
/// when (and only when) something was actually dropped.
///
/// Characters, never bytes: a byte cut mid-codepoint either panics on a slice
/// boundary or corrupts the string, and every text reaching this module is
/// agent-authored prose that can carry any UTF-8 at all. Shared by both caps so
/// there is one truncation to get right rather than two that drift.
fn truncate_chars(text: &str, cap: usize, marker: impl FnOnce(usize) -> String) -> String {
    let char_count = text.chars().count();
    if char_count <= cap {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(cap).collect();
    truncated.push_str(&marker(char_count));
    truncated
}

/// Compose the decision-grade notice line for a **structured** report
/// (`outcome` supplied). `body` is the already-truncated note (or, for a
/// caller that mixed `outcome` with the legacy `summary`, the raw summary —
/// callers pass whichever they resolved). `ref_` and `detail_url` are both
/// optional: a `blocked` report may have neither yet.
pub fn structured_notice(agent_id: &str, outcome: &str, body: &str, ref_: Option<&str>, detail_url: Option<&str>) -> String {
    let mut msg = format!("[loomux] {agent_id} reports {outcome}");
    if let Some(r) = ref_.filter(|s| !s.is_empty()) {
        msg.push_str(&format!(" ({r})"));
    }
    msg.push_str(&format!(": {body}"));
    if let Some(u) = detail_url.filter(|s| !s.is_empty()) {
        msg.push_str(&format!(" — see {u}"));
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_for_outcome_maps_reviewer_outcomes_to_done() {
        assert_eq!(status_for_outcome("approved"), "done");
        assert_eq!(status_for_outcome("request_changes"), "done");
        assert_eq!(status_for_outcome("done"), "done");
    }

    #[test]
    fn status_for_outcome_preserves_blocked_and_progress() {
        assert_eq!(status_for_outcome("blocked"), "blocked");
        assert_eq!(status_for_outcome("progress"), "progress");
    }

    #[test]
    fn truncate_note_is_a_no_op_under_the_cap() {
        let note = "PR #12 is up, CI green.";
        assert_eq!(truncate_note(note), note);
    }

    #[test]
    fn truncate_note_is_exact_at_the_cap() {
        let note = "x".repeat(NOTE_CHAR_CAP);
        assert_eq!(truncate_note(&note), note, "exactly at the cap must not be marked truncated");
    }

    #[test]
    fn truncate_note_states_the_marker_and_original_length_over_the_cap() {
        let note = "x".repeat(NOTE_CHAR_CAP + 137);
        let out = truncate_note(&note);
        assert!(out.starts_with(&"x".repeat(NOTE_CHAR_CAP)), "must keep the first NOTE_CHAR_CAP chars verbatim");
        assert!(out.contains("truncated"), "truncation must be STATED, not silent: {out}");
        assert!(out.contains(&(NOTE_CHAR_CAP + 137).to_string()), "must state the original length: {out}");
    }

    #[test]
    fn truncate_note_counts_characters_not_bytes() {
        // Each 'é' is 2 bytes in UTF-8; a byte-based cap would split one in half
        // and either panic or corrupt the string. 500 of them is over the char
        // cap but under a naive byte cap of 500.
        let note = "é".repeat(NOTE_CHAR_CAP + 10);
        let out = truncate_note(&note);
        assert!(out.contains("truncated"), "must truncate at {} chars: {out}", NOTE_CHAR_CAP + 10);
        // Must not have split a codepoint — every char in the kept prefix is
        // still a whole 'é', so counting chars (not bytes) in the output's
        // pre-marker prefix recovers exactly NOTE_CHAR_CAP.
        let prefix_chars = out.chars().take_while(|&c| c == 'é').count();
        assert_eq!(prefix_chars, NOTE_CHAR_CAP);
    }

    #[test]
    fn structured_notice_includes_outcome_ref_and_detail_url() {
        let n = structured_notice("w-2", "done", "CI green, ready for review", Some("#412"), Some("https://github.com/o/r/pull/412"));
        assert!(n.starts_with("[loomux] w-2 reports done (#412)"), "got: {n}");
        assert!(n.contains("CI green"), "got: {n}");
        assert!(n.contains("https://github.com/o/r/pull/412"), "got: {n}");
    }

    #[test]
    fn structured_notice_omits_absent_ref_and_detail_url() {
        let n = structured_notice("w-2", "blocked", "waiting on human decision", None, None);
        assert!(!n.contains("()"), "an absent ref must not leave an empty parenthesis: {n}");
        assert_eq!(n, "[loomux] w-2 reports blocked: waiting on human decision");
    }

    #[test]
    fn outcomes_and_statuses_never_silently_default() {
        // The dispatcher rejects anything outside these lists rather than
        // coercing it — pinning the vocabulary itself so a typo'd enum value
        // added to one list but not validated against isn't a passing test.
        assert!(OUTCOMES.contains(&"request_changes"));
        assert!(!OUTCOMES.contains(&"request-changes"), "hyphen vs underscore is a real distinction the caller must get right");
        assert!(STATUSES.contains(&"progress"));
        assert!(!STATUSES.contains(&"approved"), "approved is an outcome, not a legacy status");
    }
}
