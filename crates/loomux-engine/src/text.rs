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

/// The marker every notice loomux writes into a pane opens with, and the whole
/// basis of `mask_loomux_notices` (#576).
///
/// It is trustworthy for that job because it **cannot be forged from outside**:
/// `notify::sanitize_gh_text` rewrites `[`→`(` and `]`→`)` in every untrusted
/// field before it is formatted into a notice, and `intake`'s own test pins
/// that a third-party issue title can never produce this string. So a rendered
/// row opening with it was written by loomux.
///
/// **Unforgeable only in the delivery direction, and that asymmetry is the
/// whole design constraint here.** Nothing an agent sends *through* loomux can
/// carry it. But an agent's pane output is not sanitized at all, so an agent
/// can print these bytes itself — echoing a notice back, quoting one in a
/// summary, or induced to by a hostile prompt. A marker row is therefore
/// evidence that *someone wrote a notice-shaped row*, never proof that loomux
/// wrote this one, and `mask_loomux_notices` is scoped to exactly what that
/// weaker claim can support.
///
/// #888 slice A3 batch 8: moved here (unchanged, still `pub`) alongside
/// `pr_number` above — same shape, a pure string constant with consumers
/// beyond the module that forced its lift (`mod.rs`, `queue.rs`'s
/// `is_loomux_notice` check, and the integration suite, all reached through
/// `mod.rs`'s re-export under this const's original name).
///
/// **Demoted, deliberately: the two `[mask_loomux_notices]` intra-doc links
/// the original `src-tauri` doc above used are now plain backtick-quoted
/// code text (`mask_loomux_notices`).** `mask_loomux_notices` stayed behind
/// in `mod.rs` — this is `text`, a leaf with no edge back into `src-tauri` —
/// so a real intra-doc link here would not resolve (its target is a
/// different crate) and would either dead-link or need an external URL this
/// crate has no business hardcoding. Plain code text says the same thing
/// without promising a link rustdoc cannot make good on.
pub const LOOMUX_NOTICE_MARKER: &str = "[loomux]";
