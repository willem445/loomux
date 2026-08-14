//! Pure string helpers shared by more than one part of the engine.
//!
//! Nothing here is a policy or a feature; each function is a cut or a parse
//! over a `&str` with no state, no IO and no error type. They live in their own
//! module for a structural reason rather than a tidiness one: both were
//! defined in `src-tauri/src/orchestration/mod.rs`, and both are reached from
//! modules that moved into this crate (#888 slice A2). A module in here calling
//! back into `src-tauri` would point the dependency arrow the wrong way — the
//! one rule the extraction exists to hold — so the helper moves ahead of its
//! callers, and `orchestration::mod.rs` re-exports each under the name it
//! always had.
//!
//! Deliberately **not** filed next to either caller. Both have consumers beyond
//! the module that forced the lift: `tail_snippet` is the char-safe cut behind
//! a pty exit notice as well as the lessons byte-cap fallback, and `pr_number`
//! is reached from the merge-grant path, the board's PR lookup and the MCP
//! argument parsers. Filing a shared helper inside one of its consumers would
//! make every other consumer depend on that consumer for a reason the code does
//! not have.

/// Last `n` bytes of `s`, cut on a char boundary (never mid-UTF8) — a short
/// diagnostic snippet for an exit notice, never the whole captured tail.
///
/// Shared rather than re-derived: `lessons::cap` (#268) needs the identical
/// char-safe cut for its headingless byte-suffix fallback, and two independent
/// spellings of "walk back to a boundary" is exactly the pair that drifts.
pub fn tail_snippet(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let start = s.len() - n;
    let boundary = (start..=s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len());
    &s[boundary..]
}

/// Extract the numeric PR id from a board task's `pr` field, which may be a bare
/// number (`7`), a `#7`, or a full PR URL (`…/pull/7`). `None` if no number is
/// found. Pure so the normalization is testable; the grant file is keyed `pr-<N>`.
pub fn pr_number(pr: &str) -> Option<u64> {
    // A PR URL ends in `/pull/<n>`; otherwise take the last run of digits.
    let tail = pr.rsplit(['/', '#', ' ']).find(|s| !s.is_empty()).unwrap_or(pr);
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}
