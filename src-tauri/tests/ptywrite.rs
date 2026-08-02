//! Integration tests for the PTY *input* path (#719) — the mirror of
//! `ptyout.rs`'s coverage of the output path (#712).
//!
//! Must be an integration test, not a unit test (CLAUDE.md constraint #4 — the
//! Windows test exe needs build.rs's comctl32-v6 manifest). These drive the
//! SHIPPED functions on a real `PtyManager` backed by a real ConPTY pair and a
//! real (trivial, immediately-exiting) child — `register_gated_fake_for_test`,
//! whose only departure from production is that the pane's writer parks inside
//! `write` until the test releases it. That is exactly the condition #719 is
//! about: an agent that has stopped draining its stdin, so `write_all` does not
//! return. No Tauri `AppHandle` (unavailable headless) and no real agent CLI
//! (CLAUDE.md constraint 3) is involved.
//!
//! What is proven here is the property the fix is for and no unit test can
//! reach: while one pane's stdin is wedged, *everything else still moves*.
//! Before #719 the blocking `write_all` ran under the global `ptys` map lock,
//! so a wedged pane held the one lock every pty command, the attention scan and
//! pane teardown all take — turning "one agent is busy" into whole-app IPC
//! latency. Reverting the fix (taking the map lock across the write again) reds
//! the first three tests below by timeout, which is the shape the defect has.

use loomux_lib::pty::{PtyManager, PtyWriteGate};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Long enough that a loaded CI runner never trips it, short enough that a real
/// regression fails the job rather than hanging it. Every use is a "did this
/// make progress at all" question, not a latency measurement — the fix moves
/// the wait from unbounded to nothing, so there is no near-miss to tune around.
const GRACE: Duration = Duration::from_secs(10);

/// Run `f` on its own thread; report whether it finished within `t`.
///
/// A `false` here means the call is still blocked, which is the pre-#719
/// defect. The thread is left parked deliberately: it is holding a lock we
/// cannot take back, and the test harness exits the process when the run ends.
fn completes_within<T: Send + 'static>(t: Duration, f: impl FnOnce() -> T + Send + 'static) -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let v = f();
        let _ = tx.send(v);
    });
    rx.recv_timeout(t).is_ok()
}

/// Wedge `id`'s stdin: start a frontend write that parks inside the writer, and
/// return once it is provably in there. The `JoinHandle` is returned so the
/// test can release the gate and confirm the write then completes.
fn wedge(pm: &Arc<PtyManager>, id: u32) -> (Arc<PtyWriteGate>, JoinHandle<Result<(), String>>) {
    let (_captured, gate) = pm.register_gated_fake_for_test(id);
    let writer_pm = pm.clone();
    let handle = std::thread::spawn(move || writer_pm.write_from_frontend(id, "wedged", true));
    assert!(
        gate.wait_for_writes(1, GRACE),
        "setup: the write never reached the pane's writer"
    );
    (gate, handle)
}

// ---------- the fix: a wedged pane holds nothing but itself ----------

#[test]
fn a_wedged_pane_does_not_hold_the_global_ptys_lock() {
    let pm = Arc::new(PtyManager::default());
    let (gate, writer) = wedge(&pm, 1);

    // Pane 1's stdin is now blocked mid-`write_all`. `live_ids` takes the
    // global `ptys` map lock — the lock the attention scan (#40), `get_output`,
    // pane teardown and every other pty command take. It must still answer.
    let q = pm.clone();
    assert!(
        completes_within(GRACE, move || q.live_ids()),
        "the global ptys lock is held across a blocking write — one wedged \
         pane stalls every pty command (#719)"
    );

    gate.open();
    assert!(writer.join().expect("writer thread").is_ok());
}

#[test]
fn a_wedged_pane_does_not_block_another_panes_write() {
    let pm = Arc::new(PtyManager::default());
    // Register pane 2 BEFORE wedging pane 1: registration itself takes the map
    // lock, so doing it after would make the pre-#719 code hang here in setup
    // rather than fail on the assertion that names the defect.
    let other = pm.register_fake_for_test(2, b"");
    let (gate, writer) = wedge(&pm, 1);

    // The user-visible harm in #719, stated directly: pane 2's agent is fine,
    // so typing into pane 2 must land while pane 1 is wedged.
    let w = pm.clone();
    assert!(
        completes_within(GRACE, move || w.write_bytes(2, b"still typing")),
        "a wedged pane blocks writes to an unrelated pane (#719)"
    );
    assert_eq!(&*other.lock().unwrap(), b"still typing");

    gate.open();
    assert!(writer.join().expect("writer thread").is_ok());
}

#[test]
fn the_human_input_signal_is_recorded_before_the_bytes_go_out() {
    let pm = Arc::new(PtyManager::default());
    let (_captured, gate) = pm.register_gated_fake_for_test(3);

    let w = pm.clone();
    let writer = std::thread::spawn(move || w.write_from_frontend(3, "half a thought", true));
    assert!(gate.wait_for_writes(1, GRACE), "the write never reached the writer");

    // The keystroke is in flight and NOT yet out — the exact window a wedged
    // agent stretches to seconds. The question gate, the stranded-text flush
    // and the idle tick all read these two signals to decide whether a human is
    // mid-typing; if they only became true once the bytes landed, a delivery
    // could paste over a line the human has already committed to (#111). So
    // they must already read "yes" here. (Reading them at all also requires the
    // map lock, so before #719 this could not even be asked.)
    let (tx, rx) = mpsc::channel();
    let r = pm.clone();
    std::thread::spawn(move || {
        let _ = tx.send((r.input_pending(3), r.last_user_input_ms(3)));
    });
    let (pending, stamp) = rx
        .recv_timeout(GRACE)
        .expect("input signals unreadable while a write is in flight (#719)");
    assert_eq!(pending, Some(true), "occupancy must be recorded before the write");
    assert!(stamp.unwrap_or(0) > 0, "keystroke recency must be stamped before the write");

    gate.open();
    assert!(writer.join().expect("writer thread").is_ok());
}

// ---------- what must NOT change ----------

#[test]
fn a_write_returns_only_after_its_bytes_are_out() {
    let pm = Arc::new(PtyManager::default());
    let (captured, gate) = pm.register_gated_fake_for_test(4);

    let (tx, rx) = mpsc::channel();
    let w = pm.clone();
    let writer = std::thread::spawn(move || {
        let r = w.write_bytes(4, b"prompt text");
        let _ = tx.send(());
        r
    });
    assert!(gate.wait_for_writes(1, GRACE), "the write never reached the writer");

    // Back pressure is the bounded-memory answer (#719): there is no backend
    // queue, so a caller whose bytes cannot go out is *told* by not returning.
    // That is what keeps the frontend's ordered writer from dumping a whole
    // paste into the backend, and what keeps `write_bytes`'s `Ok` a statement
    // about bytes that actually reached the pane — which `record_delivered_text`
    // (#576) and the echo-verified typing loop both rely on. A refactor to a
    // fire-and-forget queue reds this.
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "the write returned while the pipe was still blocked — bytes were \
         buffered somewhere instead of applying back pressure"
    );
    assert!(captured.lock().unwrap().is_empty(), "nothing should have landed yet");

    gate.open();
    assert!(writer.join().expect("writer thread").is_ok());
    assert!(rx.recv_timeout(GRACE).is_ok(), "the write never completed after release");
    assert_eq!(&*captured.lock().unwrap(), b"prompt text");
}

#[test]
fn a_panes_own_writes_are_delivered_in_order() {
    let pm = PtyManager::default();
    let captured = pm.register_fake_for_test(5, b"");

    // Both input paths into one pane, interleaved: the frontend's keystrokes
    // (`write_pty` -> `write_from_frontend`) and orchestration's own typing
    // (`write_bytes`). Each call completes before the next begins — the
    // frontend's ordered writer (#65) guarantees the same thing across IPC —
    // so the pane's stdin must read back as exact concatenation.
    pm.write_from_frontend(5, "ab", true).expect("write 1");
    pm.write_bytes(5, b"cd").expect("write 2");
    pm.write_from_frontend(5, "ef", true).expect("write 3");
    pm.write_bytes(5, b"\r").expect("write 4");

    assert_eq!(&*captured.lock().unwrap(), b"abcdef\r");
}

#[test]
fn a_write_to_a_pane_that_is_gone_reports_not_found() {
    let pm = PtyManager::default();
    pm.register_fake_for_test(6, b"");

    // Nothing was ever registered under 7.
    assert_eq!(pm.write_from_frontend(7, "x", true), Err("pty not found".into()));
    assert_eq!(pm.write_bytes(7, b"x"), Err("pty not found".into()));

    // And a pane can die between a keystroke and its write — the handle is
    // cloned out of the map, so this must be an error, never a panic.
    pm.kill(6);
    assert_eq!(pm.write_from_frontend(6, "x", true), Err("pty not found".into()));
    assert_eq!(pm.write_cd(6, "C:\\"), Err("pty not found".into()));
}
