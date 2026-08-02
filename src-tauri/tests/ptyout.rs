//! Integration tests for PTY output coalescing (#712).
//!
//! Must be an integration test, not a unit test (CLAUDE.md constraint #4 — the
//! Windows test exe needs build.rs's comctl32-v6 manifest). These drive the
//! SHIPPED pump — `ptyout::pty_output_pump`, with its real `mpsc` channel and
//! its real monotonic clock, on its own thread, exactly as `spawn_pty` runs it
//! — rather than the pure policy underneath, which `src/ptyout.rs`'s own unit
//! tests already pin against a synthetic clock. What is proven here is the
//! property #712 is about and the policy tests cannot reach: that the number
//! of IPC EVENTS a busy pane costs is bounded by the coalescing window instead
//! of by the child's write pattern.
//!
//! No Tauri runtime and no real agent CLI are involved: the pump takes its
//! emit sink as a parameter, so the sink here is a counting closure.

use loomux_lib::ptyout::{pty_output_pump, PTY_EMIT_MAX_BATCH};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Run the real pump against `chunks`, sending them with `gap` between each,
/// and return every batch it emitted, in order.
fn run_pump(chunks: Vec<Vec<u8>>, gap: Duration) -> Vec<Vec<u8>> {
    let (tx, rx) = channel::<Vec<u8>>();
    let emitted: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = emitted.clone();
    let pump = std::thread::spawn(move || {
        pty_output_pump(rx, move |batch| sink.lock().unwrap().push(batch));
    });
    for c in chunks {
        tx.send(c).expect("pump alive");
        if !gap.is_zero() {
            std::thread::sleep(gap);
        }
    }
    // Dropping the sender is how the reader thread signals EOF in production.
    drop(tx);
    pump.join().expect("pump thread");
    let out = emitted.lock().unwrap().clone();
    out
}

/// The defect: before #712 a pane cost one IPC event per `read()` return, so
/// 1000 small chunks meant 1000 one-shot script compilations on the GUI
/// thread. Coalescing must collapse that by an order of magnitude.
///
/// The bound is deliberately loose (a quarter of the chunks) so it cannot
/// flake on a stalled CI runner: 1000 chunks pushed back-to-back would have to
/// take longer than 250 coalescing windows — four seconds of wall clock for
/// what is a few milliseconds of channel traffic — before it could fail here,
/// while the pre-#712 behaviour (one event per chunk) fails it by 4x.
#[test]
fn a_burst_costs_far_fewer_events_than_chunks() {
    let chunks: Vec<Vec<u8>> = (0..1000).map(|i| format!("line {i}\r\n").into_bytes()).collect();
    let batches = run_pump(chunks, Duration::ZERO);
    assert!(
        batches.len() <= 250,
        "1000 chunks produced {} events; coalescing should be far below one-per-chunk",
        batches.len()
    );
    assert!(!batches.is_empty(), "the stream must still be delivered");
}

/// Coalescing is allowed to change how many events carry the bytes and
/// nothing else. Concatenating every batch must reproduce the byte stream
/// exactly, in order — a terminal cannot survive a reordered or truncated
/// escape sequence.
#[test]
fn every_byte_arrives_exactly_once_and_in_order() {
    let chunks: Vec<Vec<u8>> =
        (0..1000).map(|i| format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8).into_bytes()).collect();
    let expected: Vec<u8> = chunks.iter().flatten().copied().collect();
    let batches = run_pump(chunks, Duration::ZERO);
    let got: Vec<u8> = batches.iter().flatten().copied().collect();
    assert_eq!(got, expected);
}

/// A pane that goes quiet mid-window must still deliver what it had. This is
/// the production EOF path: the reader thread drops its sender when the child
/// closes the pty, and the last bytes (a shell's goodbye, an exit message) are
/// typically well inside one window.
#[test]
fn the_last_bytes_before_eof_are_not_lost() {
    let batches = run_pump(vec![b"first".to_vec(), b"last".to_vec()], Duration::ZERO);
    let got: Vec<u8> = batches.iter().flatten().copied().collect();
    assert_eq!(got, b"firstlast".to_vec());
}

/// Interactive echo must not have become slower. A chunk arriving into a
/// quiet pane is emitted on arrival, so a keystroke echo still crosses in one
/// event with no window added — here, three chunks separated by more than a
/// window each stay three separate, immediate events.
#[test]
fn a_quiet_panes_chunks_are_not_held_back() {
    let chunks = vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()];
    let batches = run_pump(chunks, Duration::from_millis(60));
    assert_eq!(
        batches,
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
        "an idle pane's chunks must cross immediately, not be merged"
    );
}

/// A pane dumping faster than the window drains still emits in bounded
/// pieces: no single event may carry more than the batch cap, so one `cat` of
/// a large file cannot turn into one enormous script string.
#[test]
fn no_single_event_exceeds_the_batch_cap() {
    // 4 MiB in 16 KiB chunks, sent as fast as the channel takes them.
    let chunks: Vec<Vec<u8>> = (0..256).map(|_| vec![b'x'; 16 * 1024]).collect();
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    let batches = run_pump(chunks, Duration::ZERO);
    for b in &batches {
        assert!(
            b.len() <= PTY_EMIT_MAX_BATCH,
            "one event carried {} bytes, over the {PTY_EMIT_MAX_BATCH} cap",
            b.len()
        );
    }
    assert_eq!(batches.iter().map(|b| b.len()).sum::<usize>(), total);
}
