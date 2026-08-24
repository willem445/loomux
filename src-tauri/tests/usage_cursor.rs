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

// ---------------------------------------------------------------------------
// The blind spot, and the timer that bounds it (#1361 review B1)
// ---------------------------------------------------------------------------

/// Edit one byte of an ALREADY-CONSUMED line, far enough back that the anchor
/// window cannot see it, and keep appending normally.
///
/// This is the shape no stat arm and no anchor detects: the file grew, the
/// mtime moved forward, the creation time is unchanged, and the last
/// `ANCHOR_BYTES` of the consumed region are untouched. The two tests below
/// pin BOTH halves of the honest claim — that the guards miss it, and that the
/// revalidation timer is what bounds it — because a design note that says so
/// and a suite that pins only the happy half is the mismatch this repo keeps
/// catching.
fn edit_an_early_consumed_line(t: &Transcript) -> u64 {
    let text = t.text();
    // `body()` writes `"input_tokens":10` on every line; rewrite the FIRST
    // occurrence to 90. Same length, so nothing shifts, and the first line is
    // thousands of bytes before the end of the consumed region.
    let at = text.find("\"input_tokens\":10").expect("fixture shape");
    let mut bytes = text.into_bytes();
    let needle = b"\"input_tokens\":10";
    bytes[at + needle.len() - 2] = b'9'; // 10 -> 90
    let edited = String::from_utf8(bytes).expect("still utf-8");
    fs::write(&t.path, &edited).expect("rewrite in place");
    80 // how much the input-token total moves once the edit is actually read
}

#[test]
fn an_edit_below_the_anchor_window_is_not_detected_by_any_guard() {
    // A long revalidation interval, so this test is purely about the guards.
    let t = Transcript::new("sess-blind", &body("b", 40));
    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));

    let (first, _) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert_eq!(first.tokens.input_tokens, 400, "positive control: 40 lines folded");

    let delta = edit_an_early_consumed_line(&t);
    t.bump_mtime();
    t.append(&line("b-new", "claude-opus-4-8", 1, 1));

    let (after, w) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(!w.reset, "no guard fires: len grew, mtime moved forward, anchor intact");

    let full = parse_claude_transcript(&t.text());
    assert_eq!(full.tokens.input_tokens, 400 + delta + 1, "the file really did change");
    assert_eq!(
        after.tokens.input_tokens,
        400 + 1,
        "#1361 B1, pinned rather than papered over: the cursor folds the append on \
         and never re-reads the edited bytes, so it disagrees with a full re-parse \
         by exactly the edit. This is the documented residual — if this assertion \
         ever starts failing because a guard caught it, the docs on ANCHOR_BYTES \
         and stat_verdict are now understating what the code does and must be \
         re-widened"
    );
}

#[test]
fn the_revalidation_timer_bounds_that_blind_spot() {
    // The same fixture and the same edit, with the timer expired instead.
    let t = Transcript::new("sess-reval", &body("r", 40));
    let cursors = TranscriptCursors::with_revalidate_after(Duration::ZERO);

    let (first, _) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert_eq!(first.tokens.input_tokens, 400);

    let delta = edit_an_early_consumed_line(&t);
    t.bump_mtime();
    t.append(&line("r-new", "claude-opus-4-8", 1, 1));

    let (after, w) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(w.revalidated, "the age check, not a guard, is what discarded the cursor");
    assert!(w.reset, "and a revalidation IS a reset — `revalidated` only says why");
    assert_eq!(
        after.tokens,
        parse_claude_transcript(&t.text()).tokens,
        "so the totals come back to the full re-parse answer — this is the whole \
         reason the timer exists"
    );
    assert_eq!(after.tokens.input_tokens, 400 + delta + 1, "and by the edit's own size");
}

#[test]
fn an_unexpired_cursor_is_not_revalidated() {
    // Non-vacuity for the two tests above: `revalidated` must not simply be
    // true on every tick, or the bound they pin would be measuring nothing.
    let t = Transcript::new("sess-noreval", &body("n", 4));
    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));

    cursors.session_usage_measured(t.root(), &t.session).expect("first");
    t.append(&line("n-new", "claude-opus-4-8", 1, 1));

    let (_, w) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(!w.revalidated, "well inside the interval, the cursor keeps folding");
    assert!(!w.reset);
}

// ---------------------------------------------------------------------------
// The reset arms the first round left unpinned (#1361 review N1, N2)
// ---------------------------------------------------------------------------

#[test]
fn an_mtime_that_moved_backwards_resets_the_cursor() {
    // A file restored over from a copy, a sync or a checkout. Content and
    // length are identical, so neither `len` nor the anchor can object — this
    // arm is the only thing that fires, which is what makes it worth pinning.
    let t = Transcript::new("sess-backwards", &body("m", 6));
    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));

    let (first, _) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert_eq!(first.tokens.input_tokens, 60, "positive control: 6 lines folded");

    let was = fs::metadata(&t.path).expect("stat").modified().expect("mtime");
    let f = fs::OpenOptions::new().write(true).open(&t.path).expect("open");
    f.set_times(fs::FileTimes::new().set_modified(was - Duration::from_secs(60)))
        .expect("set mtime backwards");
    drop(f);

    let (after, w) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(w.reset, "an mtime that moved backwards must throw the cursor away");
    assert!(!w.revalidated, "and it is a GUARD firing, not the timer");
    assert_eq!(after.tokens, first.tokens, "the content did not change, so the answer does not");
}

#[test]
fn a_replaced_file_with_identical_content_resets_on_its_creation_time() {
    // A rotation: same path, same bytes, different file. Length matches, the
    // anchor matches, and the mtime is moved forward — so the creation-time arm
    // is the only one that can fire.
    //
    // Whether it CAN fire is a property of the host, and the test measures that
    // rather than assuming it. Two ways it comes back inert: a filesystem with
    // no birth time at all (`created()` is `Err`), and — the one that would
    // otherwise make this flaky on the repo's own platform — NTFS **file
    // tunneling**, which deliberately restores the ORIGINAL creation timestamp
    // when a name is deleted and recreated in the same directory inside a ~15 s
    // window. Sleeping past that window in a unit test is not an option, so the
    // test compares the real before/after creation times and only asserts where
    // the platform actually presents a different one.
    let t = Transcript::new("sess-rotate", &body("c", 6));
    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));

    let (first, _) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert_eq!(first.tokens.input_tokens, 60, "positive control: 6 lines folded");

    let before = fs::metadata(&t.path).expect("stat").created().ok();
    let same = t.text();
    let was_len = t.len();

    // Produce a creation time that is genuinely DISTINGUISHABLE, and only then
    // let the probe below decide.
    //
    // A bare remove-and-recreate can land inside a single clock tick, and the
    // probe then cannot tell "this host coalesces creation times" from "we were
    // simply too quick" — so the test skips, reports ok, and evidences nothing.
    // That is not hypothetical: scratch rounds 13 and 16 removed the SAME arm
    // and this test came back red on one ubuntu run and green on the other. A
    // test that silently degrades to a no-op is worse than no test, because the
    // suite stays green either way.
    let mut after_created = None;
    for _ in 0..10 {
        fs::remove_file(&t.path).expect("remove");
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&t.path, &same).expect("recreate with identical content");
        after_created = fs::metadata(&t.path).expect("stat").created().ok();
        if before.is_none() || after_created != before {
            break;
        }
    }
    assert_eq!(t.len(), was_len, "the replacement must be byte-identical to test this arm");
    t.bump_mtime();

    match (before, after_created) {
        (Some(a), Some(b)) if a != b => {}
        _ => {
            eprintln!(
                "skipped: this host reports no distinct creation time for a recreated \n                 file (no birth time, or NTFS file tunneling), so the creation-time \n                 reset arm is inert here by design — see TranscriptCursor::created"
            );
            return;
        }
    }

    let (after, w) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(
        w.reset,
        "a different file at the same path must throw the cursor away even when its \n         bytes are identical — nothing about the old offset refers into it"
    );
    assert!(!w.revalidated, "and it is a GUARD firing, not the timer");
    assert_eq!(after.tokens, first.tokens, "identical content, so identical totals");
}


#[test]
fn a_transcript_that_moves_project_folder_is_rescanned_and_reset() {
    // The cursor pins its resolved path. When Claude re-encodes a project
    // folder the old path stops being a file, and the tick must rescan the
    // root rather than give up — and must report the cursor it dropped on the
    // way as a reset, which is the `had_cursor` ordering commit `0a79a32a`
    // exists for (#1361 review N2).
    let t = Transcript::new("sess-moved", &body("v", 5));
    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));

    let (first, w1) = cursors.session_usage_measured(t.root(), &t.session).expect("first");
    assert_eq!(first.tokens.input_tokens, 50, "positive control: 5 lines folded");
    assert!(w1.scanned_root, "the first tick resolves the path by scanning");

    let moved = t.root().join("C--tmp-repo-renamed");
    fs::create_dir_all(&moved).expect("create the new project folder");
    fs::rename(&t.path, moved.join(format!("{}.jsonl", t.session))).expect("move it");

    let (after, w2) = cursors.session_usage_measured(t.root(), &t.session).expect("second");
    assert!(w2.scanned_root, "the remembered path stopped being a file, so rescan");
    assert!(
        w2.reset,
        "and the cursor it discarded must be reported as a reset — read off `slot` \
         AFTER the rescan cleared it, this would say `false`"
    );
    assert!(!w2.revalidated, "a vanished path is a guard firing, not the timer");
    assert_eq!(after.tokens, first.tokens, "same transcript, same totals, new path");
}

// ---------------------------------------------------------------------------
// The reader's own degrade (#1361 review N3)
// ---------------------------------------------------------------------------

#[test]
fn a_line_that_is_not_utf8_is_skipped_rather_than_ending_the_parse() {
    // The behaviour change this PR makes to `claude_session_usage_in`'s
    // answer, and the only one that alters what a pre-existing caller gets:
    // the old reader used `.lines().map_while(Result::ok)`, which STOPS at the
    // first undecodable line, so one bad byte truncated a whole session's
    // usage to whatever preceded it.
    let t = Transcript::new("sess-badutf8", &body("u", 2));
    let mut raw = fs::read(&t.path).expect("read");
    raw.extend_from_slice(&[0x7b, 0xff, 0xfe, 0x22, 0x0a]); // `{`, two invalid bytes, `"`, newline
    raw.extend_from_slice(line("u-after", "claude-opus-4-8", 500, 3).as_bytes());
    fs::write(&t.path, &raw).expect("write");

    let cursors = TranscriptCursors::with_revalidate_after(Duration::from_secs(3600));
    let (usage, _) = cursors.session_usage_measured(t.root(), &t.session).expect("read");

    assert_eq!(
        usage.tokens.input_tokens,
        20 + 500,
        "the undecodable line contributes nothing AND does not end the fold — a \
         reader that stopped there would report 20"
    );
}
