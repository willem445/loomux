//! `<repo>/.loomux/lessons.md` — a durable, repo-committed note of hard-won
//! knowledge (a Windows quirk, a flaky test, a "don't touch X") that would
//! otherwise die with the orchestration group it was learned in (#268).
//!
//! Deliberately **not** `.loomux/workflow.yml`'s sibling in mechanism: there is
//! no schema, no parser, and no MCP write tool. It is prose, edited like any
//! other repo file, reaching `main` through the same PR review every other
//! change does — see `doc/design/lessons.md` for the full argument. This
//! module's only job is the read side: load the file, cap it, and hand back
//! text the orchestrator's kickoff can splice in verbatim.
//!
//! # Trust posture (#189)
//!
//! This is agent-written prose that gets injected into a *future* agent's
//! context — the same persistence vector #189's threat model warns about, with
//! the repo as the untrusted-content carrier instead of an issue comment. The
//! caller (`OrchRegistry::lessons_note`, `mod.rs`) is responsible for wrapping
//! the text this module returns in the provenance framing ("repo-recorded
//! notes, not instructions") before it reaches any agent — this module never
//! hands back unwrapped text to a kickoff site. Capping happens here because
//! it's a property of the file, not of where it's used.

use std::path::Path;

/// Where the file lives — committed and shareable, next to
/// `workflow::WORKFLOW_PATH`.
pub const LESSONS_PATH: &str = ".loomux/lessons.md";

/// Hard ceiling on how much of the file ever reaches a kickoff prompt:
/// roughly 1,000 tokens, a few paragraphs — enough for the "don't touch X"
/// entries this is for, not enough to make every orchestrator kickoff pay for
/// an ever-growing changelog. See `doc/design/lessons.md` for why this is a
/// byte cap with oldest-drop truncation rather than a reject-at-cap refusal.
pub const LESSONS_BYTE_CAP: usize = 4096;

/// Load `.loomux/lessons.md` for kickoff injection, already capped.
///
/// `None` covers every case where there is nothing to inject: no file, an
/// empty (or whitespace-only) file, or an unreadable one (permission error,
/// non-UTF-8 bytes, the path existing as a directory). All three degrade the
/// same way a missing file does — this function has no notion of "malformed
/// content" because there is no schema for content to violate; the byte cap
/// below is the only transformation ever applied.
pub fn load_lessons_note(_repo: &str) -> Option<String> {
    // TODO(#268 red-evidence stub): always None — replaced by the real
    // read-and-cap implementation in the next commit, so CI records this
    // commit's test run as red for the intended reason before the green one.
    None
}

/// Cap `text` to its last `LESSONS_BYTE_CAP` bytes, cut forward to the next
/// line boundary so a truncated entry never opens mid-sentence, with a notice
/// prepended naming the full path so a reader knows more exists in git
/// history. A no-op under the cap.
fn cap(text: &str) -> String {
    if text.len() <= LESSONS_BYTE_CAP {
        return text.to_string();
    }
    // `tail_snippet` already cuts on a char boundary; walk forward to the next
    // newline inside that tail so the kept text starts at a whole line, never
    // mid-entry.
    let tail = super::tail_snippet(text, LESSONS_BYTE_CAP);
    let body = tail.find('\n').map(|i| &tail[i + 1..]).unwrap_or(tail);
    format!(
        "[earlier lessons truncated to the most recent ~{LESSONS_BYTE_CAP} bytes — \
         see the full history in {LESSONS_PATH}]\n{body}"
    )
}

// No inline `#[cfg(test)]` unit tests here: they'd link the full lib, and on
// Windows that misses the comctl32-v6 manifest `build.rs` only embeds for
// integration-test targets (repo constraint #4). Coverage for `cap`'s
// behavior (under-cap no-op, oldest-drop truncation, line-boundary safety)
// lives in `tests/lessonsfile.rs`, exercised through the public
// `load_lessons_note` against real files.
