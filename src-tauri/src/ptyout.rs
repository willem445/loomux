//! Coalescing of a pane's PTY output before it crosses the IPC boundary
//! (#712).
//!
//! WHY THIS EXISTS. Every `pty-output` event costs the app one **main-thread
//! script evaluation**, and that cost is per EVENT, not per byte. Tauri's
//! event transport is not a data channel: `Emitter::emit` ends in
//! `Webview::emit_js` -> `Webview::eval(emit_js_script(..))` (tauri 2.11.5,
//! `src/webview/mod.rs` + `src/event/mod.rs`), which builds a fresh JS source
//! string with the whole payload inlined as a literal and hands it to
//! `eval_script`. Off the main thread that goes through
//! `send_user_message` -> `proxy.send_event(..)` (tauri-runtime-wry 2.11.4),
//! i.e. one event-loop wakeup on the GUI thread, which then calls
//! `evaluate_script` and the webview compiles and runs that one-shot script.
//! `tauri::ipc::Channel` is no cheaper for messages this size — it evals too,
//! below its direct-execute threshold — so the transport is not the lever.
//! The only lever is HOW MANY messages we send.
//!
//! Before this module the reader thread emitted once per `read()` return.
//! `read()` returns whatever ConPTY has buffered right now — a TUI agent
//! redrawing a status line produces a long stream of small chunks — so the
//! event rate was set by the child's write pattern, unbounded, and multiplied
//! by the number of live panes. With five or six busy agent panes that is a
//! steady flood of one-shot script compilations on the single GUI thread, and
//! that thread is also the one that services keyboard input and paints. It
//! shows up exactly as reported in #712: the whole app goes sluggish (typing
//! AND scrolling) while total CPU sits at 15-30% — one saturated thread on a
//! many-core box, not a saturated machine.
//!
//! WHAT IT DOES. Bytes are unchanged and stay strictly ordered; only the
//! number of events is bounded. A leading-edge throttle: a chunk that arrives
//! when the pane has been quiet for at least `min_interval_ms` is emitted
//! immediately, so interactive echo keeps its pre-#712 latency exactly, and
//! only a pane already streaming inside that window has its chunks merged
//! into the next emit. One frame (16 ms) is the window because xterm.js
//! repaints at most once per animation frame — delivering more often than
//! that buys nothing a human can see, it only buys script compilations.
//!
//! WHAT IT DELIBERATELY DOES NOT TOUCH. The rolling output ring
//! (`PtyManager`'s `OutputBuf`) is still teed on the reader thread the
//! instant bytes arrive, ahead of any coalescing. Orchestration reads panes
//! through that ring (attention scan, question detection, `get_output`), so
//! none of its timing moves.

/// Minimum spacing between `pty-output` events for ONE pane, in milliseconds.
///
/// One 60 Hz frame: xterm.js schedules its repaint on `requestAnimationFrame`,
/// so two events inside the same frame paint once anyway — the second one buys
/// a script compilation and nothing else. It is also the whole added latency
/// budget, and only for a pane that is ALREADY streaming: the leading-edge
/// rule (see `OutputCoalescer::take_due`) means the first chunk after a quiet
/// pane emits with no delay at all.
pub const PTY_EMIT_MIN_INTERVAL_MS: u64 = 16;

/// Hard cap on how many bytes one `pty-output` event carries. A batch that
/// reaches it is due immediately regardless of the clock, so a pane dumping
/// output faster than the window can drain (`cat` of a huge file) still makes
/// progress in bounded pieces instead of growing one unbounded script string.
/// 64 KiB of raw bytes is ~87 KB of base64 in the emitted script — large
/// enough that the per-event overhead is thoroughly amortized, small enough
/// that no single event is itself a stall.
pub const PTY_EMIT_MAX_BATCH: usize = 64 * 1024;

/// Accumulates one pane's output chunks and decides when the batch is due.
///
/// Pure and clock-injected (`now_ms` is passed in, never read): the policy is
/// the part worth pinning, and a test that had to sleep for real windows
/// could only ever pin it loosely. `pty_output_pump` is the thin loop that
/// supplies a real monotonic clock and a real channel.
pub struct OutputCoalescer {
    min_interval_ms: u64,
    max_batch: usize,
    buf: Vec<u8>,
    /// When the last batch was emitted. `None` until the first emit, which is
    /// what makes a freshly-opened pane's very first output immediate.
    last_emit_ms: Option<u64>,
}

impl OutputCoalescer {
    pub fn new(min_interval_ms: u64, max_batch: usize) -> Self {
        Self { min_interval_ms, max_batch, buf: Vec::new(), last_emit_ms: None }
    }

    /// Append one freshly-read chunk to the pending batch.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Bytes currently held back (0 when nothing is pending).
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Whether the pending batch is due at `now_ms` — i.e. `take_due` would
    /// hand it over. Due when the pane has been quiet for at least one
    /// window (including the never-emitted case: a pane's first bytes never
    /// wait), or when the batch has reached `max_batch`.
    fn due(&self, now_ms: u64) -> bool {
        if self.buf.is_empty() {
            return false;
        }
        if self.buf.len() >= self.max_batch {
            return true;
        }
        match self.last_emit_ms {
            None => true,
            Some(t) => now_ms.saturating_sub(t) >= self.min_interval_ms,
        }
    }

    /// Take the pending batch if it is due at `now_ms`, stamping the emit
    /// clock. `None` means "keep waiting" — see `wait_ms`.
    ///
    /// Never hands back more than `max_batch`: a chunk can push the buffer
    /// past the cap in one go, and the cap is a promise about the SIZE OF AN
    /// EVENT, not a threshold to notice afterwards. Whatever is left over is
    /// still over the cap, so it is due again at this same instant — callers
    /// drain in a loop (see `pty_output_pump`).
    pub fn take_due(&mut self, now_ms: u64) -> Option<Vec<u8>> {
        if !self.due(now_ms) {
            return None;
        }
        self.last_emit_ms = Some(now_ms);
        Some(self.take_capped())
    }

    fn take_capped(&mut self) -> Vec<u8> {
        if self.buf.len() <= self.max_batch {
            std::mem::take(&mut self.buf)
        } else {
            self.buf.drain(..self.max_batch).collect()
        }
    }

    /// How long to wait before the pending batch becomes due: `Some(0)`
    /// exactly when `take_due` would emit right now, `None` when nothing is
    /// pending at all. The pump uses it as a `recv_timeout` bound, so it must
    /// never under-report (a short wait only costs one extra loop) and never
    /// return `Some` for an empty buffer (that would spin).
    pub fn wait_ms(&self, now_ms: u64) -> Option<u64> {
        if self.buf.is_empty() {
            return None;
        }
        if self.due(now_ms) {
            return Some(0);
        }
        // Not due => last_emit_ms is Some and the window has not elapsed.
        let elapsed = now_ms.saturating_sub(self.last_emit_ms.unwrap_or(now_ms));
        Some(self.min_interval_ms.saturating_sub(elapsed))
    }

    /// Take the pending bytes regardless of the clock, still capped at
    /// `max_batch` per call. This is the EOF / shutdown path: a pane whose
    /// child exits mid-window must still deliver its last bytes (an exit
    /// message, a final prompt), not drop them. Call it until it returns
    /// `None` — a residue larger than the cap needs more than one call.
    pub fn take_all(&mut self) -> Option<Vec<u8>> {
        if self.buf.is_empty() {
            return None;
        }
        Some(self.take_capped())
    }
}

/// Drain `rx` into `emit`, coalescing per `PTY_EMIT_MIN_INTERVAL_MS` /
/// `PTY_EMIT_MAX_BATCH`. Runs on its own thread (one per pane, alongside the
/// reader and waiter threads) and returns when the sender is dropped, after
/// flushing any residue.
///
/// The sink is a parameter rather than an inlined `app.emit` so this — the
/// actual shipped loop, with its real channel and real monotonic clock — is
/// what an integration test drives; a headless test has no Tauri `AppHandle`
/// (see `PtyManager::register_fake_for_test`'s note on why).
pub fn pty_output_pump<F: FnMut(Vec<u8>)>(rx: std::sync::mpsc::Receiver<Vec<u8>>, emit: F) {
    pty_output_pump_with(rx, PTY_EMIT_MIN_INTERVAL_MS, PTY_EMIT_MAX_BATCH, emit)
}

/// `pty_output_pump` with the policy constants supplied rather than shipped.
///
/// Exists for one reason: `tests/ptyout.rs` runs the SAME loop twice, once
/// with the shipped window and once with a zero window — which is exactly the
/// pre-#712 policy, one event per `read()` return — and compares the event
/// counts. Without that arm, "coalescing reduced the event count" is a claim
/// about a number with nothing to compare it to; with it, the un-coalesced
/// count is measured on the real code path, on every platform, on every run.
#[doc(hidden)] // pub for the #712 integration test's A/B against the old policy
pub fn pty_output_pump_with<F: FnMut(Vec<u8>)>(
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    min_interval_ms: u64,
    max_batch: usize,
    mut emit: F,
) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    let epoch = Instant::now();
    let mut co = OutputCoalescer::new(min_interval_ms, max_batch);
    // Monotonic, and relative to this pane's own start — never SystemTime,
    // which a clock adjustment could run backwards mid-stream.
    let now_ms = move || epoch.elapsed().as_millis() as u64;

    loop {
        // Block with no timeout while the pane is quiet: an idle pane must
        // cost nothing, and the first chunk after idle is due on arrival.
        match rx.recv() {
            Ok(chunk) => co.push(&chunk),
            Err(_) => break,
        }
        loop {
            let now = now_ms();
            if let Some(batch) = co.take_due(now) {
                emit(batch);
                // A batch that ran past the cap comes out in cap-sized pieces:
                // each `take_due` hands over at most one, and the remainder is
                // still over the cap, so it is due at this same instant.
                while let Some(more) = co.take_due(now) {
                    emit(more);
                }
                break;
            }
            // `wait_ms` is Some(>0) here: the buffer is non-empty (we just
            // pushed) and not due (take_due said so).
            let wait = co.wait_ms(now).unwrap_or(0);
            match rx.recv_timeout(Duration::from_millis(wait)) {
                Ok(chunk) => co.push(&chunk),
                // Window elapsed: the next take_due emits.
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    while let Some(batch) = co.take_all() {
                        emit(batch);
                    }
                    return;
                }
            }
        }
    }
    while let Some(batch) = co.take_all() {
        emit(batch);
    }
}

// ---------- tests ----------
//
// The coalescing POLICY only — pure, clock-injected, no channel and no
// threads, so these stay inline `#[cfg(test)]` unit tests (CLAUDE.md
// constraint 4's integration-only rule is about tests that link the lib's
// Tauri surface). `tests/ptyout.rs` covers `pty_output_pump` itself against a
// real channel and the real clock.

#[cfg(test)]
mod tests {
    use super::*;

    const W: u64 = 16;
    const CAP: usize = 64 * 1024;

    /// Interactive echo must not get slower. A pane that has been quiet emits
    /// the moment a chunk lands — no window, no wait.
    #[test]
    fn first_chunk_of_a_quiet_pane_emits_immediately() {
        let mut co = OutputCoalescer::new(W, CAP);
        co.push(b"x");
        assert_eq!(co.wait_ms(0), Some(0));
        assert_eq!(co.take_due(0).as_deref(), Some(&b"x"[..]));

        // ... and again after any gap of a full window: still no delay.
        co.push(b"y");
        assert_eq!(co.wait_ms(100), Some(0));
        assert_eq!(co.take_due(100).as_deref(), Some(&b"y"[..]));
    }

    /// The defect itself: a stream of small chunks inside one window must
    /// cost ONE event, not one per chunk.
    #[test]
    fn a_burst_inside_one_window_collapses_to_one_event() {
        let mut co = OutputCoalescer::new(W, CAP);
        let mut events = 0;
        // t=0: leading edge, emits at once.
        co.push(b"a");
        if co.take_due(0).is_some() {
            events += 1;
        }
        // 50 more chunks spread across the SAME window (t=1..15).
        for t in 1..=15u64 {
            for _ in 0..4 {
                co.push(b"a");
            }
            if co.take_due(t).is_some() {
                events += 1;
            }
        }
        assert_eq!(events, 1, "chunks inside the window must not each emit");
        // They are held, not dropped — due at the window edge.
        assert_eq!(co.pending(), 60);
        assert_eq!(co.take_due(16).map(|b| b.len()), Some(60));
    }

    /// The contrast that says what the window is actually doing: with no
    /// window at all, the same burst is the pre-#712 policy — one event per
    /// chunk, an event rate set by the child rather than by loomux. Kept as a
    /// permanent statement of the behaviour this module replaced, so a future
    /// reader can see what a zero window would put back.
    #[test]
    fn without_a_window_every_chunk_is_its_own_event() {
        let mut co = OutputCoalescer::new(0, CAP);
        let mut events = 0;
        for t in 0..16u64 {
            co.push(b"a");
            if co.take_due(t).is_some() {
                events += 1;
            }
        }
        assert_eq!(events, 16, "a zero window coalesces nothing");
    }

    /// Sustained streaming is bounded by the window, not by the child's write
    /// pattern: 500 chunks over 1 s can cost at most ~1000/16 events.
    #[test]
    fn sustained_streaming_is_bounded_by_the_window() {
        let mut co = OutputCoalescer::new(W, CAP);
        let mut events = 0;
        // A chunk every 2 ms for one second.
        for i in 0..500u64 {
            co.push(&[b'z'; 8]);
            if co.take_due(i * 2).is_some() {
                events += 1;
            }
        }
        assert!(
            events <= 1000 / W + 2,
            "500 chunks over 1s produced {events} events; window allows ~{}",
            1000 / W + 2
        );
        assert!(events >= 2, "the stream must still flow, got {events} events");
    }

    /// Coalescing may reorder nothing and lose nothing: the concatenation of
    /// every emitted batch is byte-identical to the concatenation of pushes.
    #[test]
    fn bytes_survive_in_order() {
        let mut co = OutputCoalescer::new(W, CAP);
        let mut pushed: Vec<u8> = Vec::new();
        let mut emitted: Vec<u8> = Vec::new();
        for i in 0..200u64 {
            let chunk = format!("[{i}]").into_bytes();
            pushed.extend_from_slice(&chunk);
            co.push(&chunk);
            // Irregular arrival: sometimes inside a window, sometimes past it.
            if let Some(b) = co.take_due(i * 3) {
                emitted.extend_from_slice(&b);
            }
        }
        if let Some(b) = co.take_all() {
            emitted.extend_from_slice(&b);
        }
        assert_eq!(emitted, pushed);
    }

    /// A pane dumping faster than the window can drain still emits in bounded
    /// pieces — the cap overrides the clock, and no single batch exceeds it.
    #[test]
    fn the_batch_cap_overrides_the_window() {
        let mut co = OutputCoalescer::new(W, 1024);
        assert!(co.take_due(0).is_none(), "nothing pending yet");
        co.push(&[b'q'; 64]);
        // Consume the leading edge so the window is genuinely in force.
        assert!(co.take_due(0).is_some());
        // One chunk lands four caps' worth in a single push, well inside the
        // window: it must come out at once, in cap-sized pieces.
        co.push(&[b'q'; 4096]);
        assert_eq!(co.wait_ms(1), Some(0), "over the cap is due at once");
        let mut sizes = Vec::new();
        while let Some(b) = co.take_due(1) {
            sizes.push(b.len());
        }
        assert_eq!(sizes, vec![1024, 1024, 1024, 1024]);
        assert_eq!(co.pending(), 0);
    }

    /// The EOF flush is capped the same way, and drains completely — a child
    /// that dies holding megabytes must neither lose them nor deliver them as
    /// one unbounded event.
    #[test]
    fn take_all_drains_in_capped_pieces() {
        let mut co = OutputCoalescer::new(W, 1024);
        co.push(&[b'q'; 2500]);
        let mut sizes = Vec::new();
        while let Some(b) = co.take_all() {
            sizes.push(b.len());
        }
        assert_eq!(sizes, vec![1024, 1024, 452]);
    }

    /// A child that exits mid-window must not take its last bytes with it.
    #[test]
    fn eof_flushes_the_residue() {
        let mut co = OutputCoalescer::new(W, CAP);
        co.push(b"first");
        assert!(co.take_due(0).is_some());
        co.push(b"goodbye");
        assert!(co.take_due(1).is_none(), "still inside the window");
        assert_eq!(co.take_all().as_deref(), Some(&b"goodbye"[..]));
        assert_eq!(co.take_all(), None, "nothing left to flush twice");
    }

    /// `wait_ms` must never invite a spin: no pending bytes means no wait at
    /// all, and a held batch reports the real remaining window.
    #[test]
    fn wait_ms_reports_the_remaining_window() {
        let mut co = OutputCoalescer::new(W, CAP);
        assert_eq!(co.wait_ms(0), None, "empty buffer must not schedule a wake");
        co.push(b"a");
        assert!(co.take_due(10).is_some());
        co.push(b"b");
        assert_eq!(co.wait_ms(10), Some(16));
        assert_eq!(co.wait_ms(20), Some(6));
        assert_eq!(co.wait_ms(26), Some(0));
    }
}
