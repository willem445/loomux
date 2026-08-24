//! #1239: a usage poll must parse only what an agent has WRITTEN since the
//! previous poll.
//!
//! `compute_group_usage` runs on the app's hottest poll — at most once per
//! `USAGE_POLL_MAX_AGE` (1 s) — and for every live agent it re-read and
//! re-parsed the ENTIRE transcript from byte zero: tens of MiB on a multi-day
//! session, `serde_json` over every line, the message-id dedupe set rebuilt
//! from scratch, to advance four totals by a few lines. #1218/#1237 bounded
//! that read's MEMORY (it streams) and deliberately left the WORK alone.
//!
//! So the property under test here is not "the totals are right" — the
//! whole-file re-parse got those right too. It is **a tick's work is
//! proportional to what was appended**, plus the correctness story that makes
//! resuming from a byte offset safe: a replaced or truncated file must reset
//! the cursor, and a half-written trailing line must not be consumed until its
//! newline arrives.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint #4: a
//! unit-test binary linking the full lib misses the comctl32-v6 manifest
//! `build.rs` embeds only for integration-test targets.

use loomux_lib::usage::{parse_claude_transcript, TranscriptCursors};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A Claude transcript at `<root>/<project>/<session>.jsonl` — the layout
/// `claude_transcript_path` scans for — that a test can append to, replace,
/// and re-stat.
struct Transcript {
    dir: tempfile::TempDir,
    session: String,
    path: PathBuf,
}

impl Transcript {
    fn new(session: &str, body: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("C--tmp-repo");
        fs::create_dir_all(&project).expect("create project dir");
        let path = project.join(format!("{session}.jsonl"));
        fs::write(&path, body).expect("write transcript");
        Transcript { dir, session: session.to_string(), path }
    }

    fn root(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// Append bytes exactly as a JSONL writer would — no truncation, no
    /// rewrite of anything already there.
    fn append(&self, text: &str) {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .expect("open for append");
        f.write_all(text.as_bytes()).expect("append");
        f.flush().expect("flush");
    }

    /// Replace the file's whole content in place (truncate + write), the shape
    /// a rotation or an external rewrite takes.
    fn replace(&self, text: &str) {
        fs::write(&self.path, text).expect("replace transcript");
    }

    fn len(&self) -> u64 {
        fs::metadata(&self.path).expect("stat").len()
    }

    fn text(&self) -> String {
        fs::read_to_string(&self.path).expect("read transcript")
    }

    /// Force the modification time strictly forward.
    ///
    /// Without this a same-length rewrite is not reliably distinguishable at
    /// the STAT level: the system clock tick is coarse enough (~15 ms on
    /// Windows) that two writes microseconds apart can share an mtime, and the
    /// cursor would then serve its cached totals — the residual blind spot
    /// documented on `TranscriptCursor::stat_verdict`. Moving the mtime makes
    /// the test about the ANCHOR (the thing that catches a same-length
    /// rewrite) rather than about the host's clock granularity.
    fn bump_mtime(&self) {
        let was = fs::metadata(&self.path).expect("stat").modified().expect("mtime");
        let f = fs::OpenOptions::new().write(true).open(&self.path).expect("open");
        f.set_times(fs::FileTimes::new().set_modified(was + Duration::from_secs(2)))
            .expect("set mtime");
    }
}

/// Bytes of padding per transcript line. Real Claude lines run from a few
/// hundred bytes to tens of kilobytes; 1 KiB sits at the small end, which is
/// the conservative direction for a work-bound assertion.
const LINE_FILLER: usize = 1024;

/// One assistant transcript line, newline-terminated, padded to roughly
/// [`LINE_FILLER`] bytes so a file of them is big enough for the difference
/// between "reads the appended bytes" and "reads the file" to be an order of
/// magnitude rather than a rounding error.
fn line(id: &str, model: &str, input: u64, output: u64) -> String {
    let mut s = serde_json::json!({
        "type": "assistant",
        "message": {
            "id": id,
            "model": model,
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            }
        },
        "filler": "x".repeat(LINE_FILLER),
    })
    .to_string();
    s.push('\n');
    s
}

/// A transcript body of `n` assistant lines, ids `<prefix>0..<prefix>n`.
fn body(prefix: &str, n: usize) -> String {
    (0..n).map(|i| line(&format!("{prefix}{i}"), "claude-opus-4-8", 10, 5)).collect()
}

// ---------------------------------------------------------------------------
// Totals
// ---------------------------------------------------------------------------

#[test]
fn appended_lines_fold_onto_the_cursor_and_reach_a_full_reparse_answer() {
    // A first tick over 5 lines, then two appends folded on top. What the
    // cursor must carry ACROSS the boundary is everything the fold accumulates:
    // the four totals, the dollar cost, the best-priced model, and the dedupe
    // set — so the fixture makes each of those observable across it.
    let t = Transcript::new("sess-fold", &body("a", 5));
    let cursors = TranscriptCursors::default();

    let (first, w1) = cursors
        .session_usage_measured(t.root(), &t.session)
        .expect("the transcript is found and parsed");
    assert_eq!(first.tokens.input_tokens, 50, "5 lines at 10 input tokens each");
    assert!(w1.bytes_read >= t.len(), "a first tick reads the whole file");

    // A NEW message, a REPEAT of one already folded (a `--resume` re-emit —
    // the dedupe set must survive the tick boundary or it double-counts), and
    // a switch to a model with more output tokens (the best-model pick must
    // survive it too, and be re-decided against what came before).
    t.append(&line("a9", "claude-opus-4-8", 100, 20));
    t.append(&line("a0", "claude-opus-4-8", 10, 5));
    t.append(&line("s1", "claude-sonnet-5", 7, 900));

    let (second, w2) = cursors
        .session_usage_measured(t.root(), &t.session)
        .expect("still found");
    assert!(!w2.reset, "an append is an extend, never a reset");

    let full = parse_claude_transcript(&t.text());
    assert_eq!(second.tokens, full.tokens, "totals match a full re-parse");
    assert_eq!(second.model, full.model, "and so does the priced-model pick");
    assert_eq!(
        second.cost_usd.map(|c| format!("{c:.10}")),
        full.cost_usd.map(|c| format!("{c:.10}")),
        "and so does the accrued cost"
    );

    // Non-vacuity for the three properties the fixture was built around: the
    // assertions above would also hold if the appends had been ignored
    // wholesale, so pin what each one actually moved.
    assert_eq!(
        second.tokens.input_tokens,
        50 + 100 + 7,
        "the new lines counted, and the re-emitted `a0` did NOT count twice"
    );
    assert_eq!(second.model.as_deref(), Some("claude-sonnet-5"), "the model switch is seen");
    assert!(second.cost_usd.unwrap() > first.cost_usd.unwrap(), "cost accrued, not restarted");
}

// ---------------------------------------------------------------------------
// The work bound — the property this whole change exists for
// ---------------------------------------------------------------------------

#[test]
fn a_tick_reads_the_appended_bytes_and_not_the_file() {
    let t = Transcript::new("sess-work", &body("w", 300));
    let cursors = TranscriptCursors::default();

    let file_len = t.len();
    // Non-vacuity: a tiny fixture would make the ceiling below trivially true.
    assert!(
        file_len > 200 * 1024,
        "fixture is {file_len} B — too small for the ceiling to mean anything"
    );

    let (_, first) = cursors
        .session_usage_measured(t.root(), &t.session)
        .expect("first read");
    assert!(
        first.bytes_read >= file_len,
        "positive control: the first tick has no cursor, so it must read the whole \
         {file_len} B file (read {} B)",
        first.bytes_read
    );
    assert!(first.scanned_root, "and it must scan the projects root to find the file");

    let before = t.len();
    t.append(&line("w-new-1", "claude-opus-4-8", 1, 1));
    t.append(&line("w-new-2", "claude-opus-4-8", 1, 1));
    let appended = t.len() - before;
    assert!(appended > 0, "positive control: the append actually landed");

    let (usage, second) = cursors
        .session_usage_measured(t.root(), &t.session)
        .expect("second read");

    // The bound. Slack covers the 64-byte anchor re-read and nothing else;
    // it is not a fudge factor for a re-parse, which would be ~150x over.
    assert!(
        second.bytes_read <= appended + 1024,
        "#1239: a tick must read only what was appended ({appended} B, +anchor), \
         but it read {} B off a {file_len} B file — that is a whole-file re-parse",
        second.bytes_read
    );
    assert!(!second.reset, "nothing about an append invalidates the cursor");
    assert!(!second.scanned_root, "and the transcript path is not re-resolved either");
    assert_eq!(
        usage.tokens,
        parse_claude_transcript(&t.text()).tokens,
        "and the cheap tick still reaches the full re-parse answer"
    );
}

#[test]
fn an_unchanged_transcript_is_not_read_at_all() {
    let t = Transcript::new("sess-idle", &body("i", 20));
    let cursors = TranscriptCursors::default();

    let (first, w1) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert!(w1.bytes_read > 0, "positive control: the first tick did read the file");

    let (second, w2) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert_eq!(w2.bytes_read, 0, "an idle transcript costs one stat and no read");
    assert!(w2.served_cached, "the totals came from the cursor");
    assert_eq!(second.tokens, first.tokens, "and they are the same totals");
}

// ---------------------------------------------------------------------------
// What invalidates a cursor
// ---------------------------------------------------------------------------

#[test]
fn a_truncated_transcript_resets_the_cursor_and_re_reads_from_zero() {
    let t = Transcript::new("sess-trunc", &body("t", 40));
    let cursors = TranscriptCursors::default();

    let (first, _) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert_eq!(first.tokens.input_tokens, 400, "positive control: 40 lines were folded");

    // Rewritten shorter — a rotation, or the file replaced by a different
    // session's. Resuming from the old offset would fold whatever now sits
    // there onto totals that describe content no longer in the file.
    t.replace(&body("z", 3));

    let (after, w) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(w.reset, "a shrink must throw the cursor away");
    assert_eq!(
        after.tokens,
        parse_claude_transcript(&t.text()).tokens,
        "and the answer is the NEW file's, re-read from byte zero"
    );
    assert_eq!(after.tokens.input_tokens, 30, "which is 3 lines, not 40 and not 43");
}

#[test]
fn a_same_length_rewrite_resets_the_cursor_too() {
    // The case `len` cannot see. `body` produces fixed-width lines, so a
    // rewrite with different ids and different token counts of the same digit
    // width lands on exactly the same byte length — and the stat gate would
    // wave it through as an extend. What catches it is the anchor: the bytes
    // immediately before the cursor offset are no longer what was folded.
    let t = Transcript::new("sess-samelen", &body("p", 12));
    let cursors = TranscriptCursors::default();

    let (first, _) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert_eq!(first.tokens.input_tokens, 120);
    let was = t.len();

    let rewritten: String =
        (0..12).map(|i| line(&format!("q{i}"), "claude-opus-4-8", 20, 6)).collect();
    t.replace(&rewritten);
    t.bump_mtime();
    assert_eq!(t.len(), was, "the fixture must actually be the same length to test this");

    let (after, w) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(w.reset, "a same-length rewrite must still throw the cursor away");
    assert_eq!(
        after.tokens.input_tokens, 240,
        "and the totals are the rewritten file's alone — not the old ones, and not \
         both files summed"
    );
    assert_eq!(after.tokens, parse_claude_transcript(&t.text()).tokens);
}

// ---------------------------------------------------------------------------
// Partial-line safety
// ---------------------------------------------------------------------------

#[test]
fn a_half_written_line_is_not_consumed_until_its_newline_arrives() {
    // A JSONL writer appends the record and its newline as separate bytes, so
    // a 1 Hz poll lands between them routinely. The record below is COMPLETE,
    // valid JSON with no trailing newline — the worst case, because it parses:
    // a reader that consumed it would advance the offset past bytes the writer
    // has not finished with, and the real record that follows would then be
    // folded from its middle.
    let t = Transcript::new("sess-partial", &body("h", 2));
    let whole = line("h-tail", "claude-opus-4-8", 1000, 7);
    let (record, newline) = whole.split_at(whole.len() - 1);
    assert_eq!(newline, "\n", "the fixture splits exactly the terminator off");
    t.append(record);

    let cursors = TranscriptCursors::default();
    let (before, _) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert_eq!(
        before.tokens.input_tokens, 20,
        "only the two newline-terminated lines are folded; the unterminated tail waits"
    );

    t.append(newline);

    let (after, w) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(!w.reset, "the newline arriving is an ordinary append");
    assert_eq!(
        after.tokens.input_tokens,
        20 + 1000,
        "once terminated it is folded — exactly once, not zero times and not twice"
    );
    assert_eq!(after.tokens, parse_claude_transcript(&t.text()).tokens);
}
