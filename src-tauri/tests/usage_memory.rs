//! #1218: reading a Claude transcript must not materialize it.
//!
//! **What killed the app three times.** `claude_session_usage_in` used to
//! append every line of the transcript into one `String` before parsing it.
//! A multi-day agent session's transcript passes 32 MiB; `String`'s backing
//! `Vec<u8>` grows by DOUBLING; and an infallible `Vec` grow whose allocation
//! is refused calls `handle_alloc_error`, which aborts the process on the
//! spot. That abort never enters `std::panicking`, so the panic hook does not
//! run and nothing is written — no crash log, no breadcrumb, which is exactly
//! what the three preserved minidumps show. Both 1.2.0-beta1 dumps are that
//! abort: `Layout { size: 64 MiB, align: 1 }` in one, 128 MiB in the other,
//! on a machine whose system commit charge was pinned at its limit.
//!
//! So the property under test is not "the totals are right" — the old code
//! got those right too, all the way up to the abort. It is **peak live heap
//! must not scale with the file**. That is measurable and nothing else in the
//! suite measures it, so it is pinned here with a counting allocator.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint #4: a
//! unit-test binary linking the full lib misses the comctl32-v6 manifest
//! `build.rs` embeds only for integration-test targets.
//!
//! **This file deliberately holds exactly one `#[test]`.** The allocator
//! counter below is process-global, and `cargo test` runs a binary's tests on
//! parallel threads — a second test in this file would allocate concurrently
//! inside the measured window and make the reading meaningless. Put unrelated
//! usage tests anywhere else.

use loomux_lib::usage::claude_session_usage_in;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Counting allocator
// ---------------------------------------------------------------------------

/// Net live bytes handed out by the allocator, and the high-water mark of the
/// same. Both are process-global by necessity — `#[global_allocator]` has no
/// narrower scope — which is what the one-test-per-file rule above protects.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    let live = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

fn shrank(n: usize) {
    LIVE.fetch_sub(n, Ordering::Relaxed);
}

struct Counting;

// SAFETY: every method forwards to `System` unchanged and only adds relaxed
// counter arithmetic around it, so the allocation contract is whatever
// `System` already guarantees. Counters are updated only on a non-null
// return, so a failed allocation is not counted as live.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        shrank(layout.size());
    }

    // The one that actually matters here: a `Vec` doubling is a realloc, and
    // counting it as alloc+dealloc of the *old* size would miss the growth
    // this whole test is about.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                shrank(layout.size() - new_size);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Bytes of filler per transcript line. Real Claude transcript lines run from
/// a few hundred bytes to tens of kilobytes (the two dumps caught grows of
/// 3,511 and 16,913 bytes); 8 KiB sits in that range.
const LINE_FILLER: usize = 8 * 1024;
/// Enough lines that the file is far larger than the assertion's ceiling —
/// 2048 * ~8 KiB is ~16 MiB against a 2 MiB ceiling, so the old behaviour
/// misses by roughly 8x rather than by a hair.
const LINES: usize = 2048;

/// Peak live heap the whole read is allowed to add. Generous on purpose: the
/// streaming read's real cost is one line plus the parser's message-id dedupe
/// set (ids only — a few hundred KiB at this line count), and the point of
/// the assertion is the ORDER OF MAGNITUDE, not a tight bound that would go
/// red on an allocator's rounding.
const PEAK_CEILING: usize = 2 * 1024 * 1024;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    /// `<root>/<project>/<session>.jsonl` — the layout
    /// `claude_transcript_path` scans for.
    fn write(session: &str) -> (Self, u64) {
        let root = std::env::temp_dir().join(format!(
            "loomux-1218-usage-mem-{}-{}",
            session,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("-Users-someone-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        let path = project.join(format!("{session}.jsonl"));

        // Written incrementally: building the transcript in memory here would
        // be the very thing under test, and would also pollute the baseline.
        let filler = "x".repeat(LINE_FILLER);
        {
            let f = fs::File::create(&path).expect("create transcript");
            let mut w = BufWriter::new(f);
            for i in 0..LINES {
                writeln!(
                    w,
                    concat!(
                        r#"{{"type":"assistant","message":{{"id":"m{i}","model":"claude-sonnet-4-5","#,
                        r#""usage":{{"input_tokens":1,"output_tokens":2,"#,
                        r#""cache_creation_input_tokens":3,"cache_read_input_tokens":4}},"#,
                        r#""filler":"{filler}"}}}}"#
                    ),
                    i = i,
                    filler = filler
                )
                .expect("write line");
            }
            w.flush().expect("flush");
        }
        let size = fs::metadata(&path).expect("stat").len();
        (Fixture { root }, size)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------------------

#[test]
fn reading_a_transcript_does_not_hold_the_whole_file_in_memory() {
    let session = "sess-1218-mem";
    let (fixture, file_size) = Fixture::write(session);

    // The fixture must actually be big enough for the assertion to mean
    // anything — a truncated write would make the ceiling trivially true.
    assert!(
        file_size > 4 * PEAK_CEILING as u64,
        "fixture is {file_size} B, which is not comfortably larger than the \
         {PEAK_CEILING} B ceiling — the memory assertion would pass vacuously"
    );

    // Baseline immediately before the call, so the fixture's own writes and
    // the harness's startup are outside the window.
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);

    let usage = claude_session_usage_in(&fixture.root, session);

    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);

    // Non-vacuity: the read must have parsed the WHOLE file. Without this a
    // reader that returned `None`, or bailed after one line, would sail
    // through the memory assertion below with a peak of nearly zero.
    let usage = usage.expect("transcript should be found and parsed");
    assert_eq!(usage.tokens.input_tokens, LINES as u64, "input tokens");
    assert_eq!(usage.tokens.output_tokens, LINES as u64 * 2, "output tokens");
    assert_eq!(
        usage.tokens.cache_creation_tokens,
        LINES as u64 * 3,
        "cache-creation tokens"
    );
    assert_eq!(
        usage.tokens.cache_read_tokens,
        LINES as u64 * 4,
        "cache-read tokens"
    );

    // The property itself.
    assert!(
        peak <= PEAK_CEILING,
        "reading a {file_size} B transcript added {peak} B of peak live heap, \
         over the {PEAK_CEILING} B ceiling — the reader is materializing the \
         file instead of streaming it (#1218). A peak at or above the file \
         size is the pre-#1218 behaviour that aborted the process via \
         handle_alloc_error when the allocation was refused."
    );
}
