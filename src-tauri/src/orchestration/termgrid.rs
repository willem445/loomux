//! `get_output`'s capture path (#520) — **pre-fix behavior, deliberately**.
//!
//! This commit exists to make the new `get_output` tests in
//! `tests/orchestration.rs` fail against the pipeline as it is TODAY, so the
//! fix that follows has evidence rather than an assertion that it was needed.
//! `render_screen` here just forwards to [`super::strip_ansi`] — exactly what
//! `agent_output_tail` does on `main` — and the byte cap is declared but not
//! applied. The next commit replaces this module with the composed-grid
//! replay and enforces the cap.
//!
//! Nothing else in the tree calls this yet.

/// Fallback geometry when the live pty size can't be read.
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;

/// Pre-fix: hands back the raw write stream with escape sequences deleted,
/// which is what floods (#520). Replaced in the following commit.
pub fn render_screen(bytes: &[u8], _cols: u16, _rows: u16) -> String {
    super::strip_ansi(bytes)
}
