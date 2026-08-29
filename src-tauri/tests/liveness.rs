//! Rust-level LIVENESS tests for the responsiveness-root-cause epic (#1600).
//!
//! Must be an integration test, not a unit test (CLAUDE.md constraint 4 — the
//! Windows test exe needs build.rs's comctl32-v6 manifest). Everything here
//! drives SHIPPED functions against a real `PtyManager` backed by a real ConPTY
//! pair and a real (trivial, immediately-exiting) child, with no Tauri
//! `AppHandle` (unavailable headless) and no real agent CLI (constraint 3).
//!
//! **Why this file exists at all is #1600 §2.2.** This repo's performance
//! guards are source scans: they assert a SHAPE, and every one of the four
//! hangs (#1564, #1592, #1595, beta6) shipped past a growing wall of them
//! because the shape was correct and the app still stopped. `perf_dispatch.rs`'s
//! newest guard would PASS on beta6. The question a scan cannot ask is the one
//! the user actually cares about — *does this still make progress while
//! something else is stuck?* — so every test here is that question, asked with a
//! deterministic stuck thing and a bounded wait, never a sleep.
//!
//! # The harness contract, for the phases that append here
//!
//! `tests/liveness.rs` is created by the Phase 2.3 worker (#1607) and APPENDED
//! to by the Phase 1, 2.1 and 3a workers (#1600 plan comment 4/4 b). Keep
//! `GRACE`/`SETTLE`/`completes_within`/`test_registry` shared rather than
//! re-cutting them per slice; they are the same idiom as `tests/ptywrite.rs` and
//! `tests/perf_leaflocks.rs`, deliberately.
//!
//! **One rule an appender has to know: the blocking pool in this binary is a
//! shared fixture.** `pool()` installs a tokio runtime with
//! `max_blocking_threads(2)` as the process-wide `tauri::async_runtime`, and
//! `tauri::async_runtime::set` PANICS if the runtime is already initialized — so
//! every test that needs an executor or the blocking pool must reach it through
//! `pool()`, never through `tauri::async_runtime::block_on`/`spawn_blocking` on
//! a runtime it initialized itself. A test that SATURATES that pool holds
//! `POOL_SERIAL` for the duration, because two of them at once would each be
//! measuring the other.
//!
//! # Rows not in this file yet, and why (so their absence is a decision)
//!
//! The plan's table has L0-L6. This file lands L3a, L3b and L4 — the rows Phase
//! 2.3 gates — and **not** these two, both for the same mechanical reason: an
//! `#[ignore]`d test still has to COMPILE, and these name APIs that do not exist
//! on this branch.
//!
//! - **L0** (negative control: `group_summary` does not return while `agents`
//!   is held) needs `OrchRegistry::hold_lock_for_test`, Phase 0 interface item
//!   3 (#1601, PR #1605). There is no headless substitute: the shipped paths
//!   that hold `agents` across a pty read (`orchestrator_activity`) need an
//!   `AppHandle`, and `output_totals_from`/`compact_signals_from` deliberately
//!   snapshot-then-release (#743 S7). What L0 documents about the POOL half of
//!   the class is covered here meanwhile — see L3a's control, which is the
//!   beta6 mechanism itself.
//! - **L3b's second clause** ("Phase 0's pool-depth counter reads 0 throughout")
//!   needs #1601's counted `spawn_blocking` door. L3b's FIRST clause does not,
//!   and lands live below.
//!
//! Both are for the post-#1601 lift pass, which also un-`#[ignore]`s L4.

use loomux_lib::orchestration::{Guardrails, OrchRegistry};
use loomux_lib::pty::{PtyManager, WriteReceiver};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

/// Long enough that a loaded CI runner never trips it, short enough that a real
/// regression fails the job rather than hanging it. Every use is a "did this
/// make progress at all" question, not a latency measurement — the fixes this
/// epic ships move the wait from unbounded to nothing, so there is no near-miss
/// to tune around. Same value and same reason as `ptywrite.rs`'s.
const GRACE: Duration = Duration::from_secs(10);

/// How long a call is given to *be* stuck before the property is probed. It is
/// asserted as a NEGATIVE (`recv_timeout` must time out), so this is not a guess
/// about scheduling: a call that has not returned after 300 ms, when its
/// unblocked cost is microseconds, is stuck at the one place in it that can
/// block. Same idiom and duration as `ptywrite.rs`/`perf_leaflocks.rs`.
const SETTLE: Duration = Duration::from_millis(300);

/// Run `f` on its own thread; report whether it finished within `t`.
///
/// A `false` here means the call is still blocked, which is the defect shape
/// every row in this file is about. The thread is left parked deliberately: it
/// is waiting on something this test still holds, and the harness exits the
/// process when the run ends.
fn completes_within<T: Send + 'static>(t: Duration, f: impl FnOnce() -> T + Send + 'static) -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(t).is_ok()
}

/// #464: every registry construction in this file goes through here, so no
/// spawn can write a generated custom-agent file into the developer's REAL
/// `~/.claude/agents` or `~/.copilot/agents`. Copied from `perf_leaflocks.rs`,
/// which is the reference instance.
///
/// Unused by the rows this file lands with — L3a/L3b/L4 are pty-side and need
/// no registry — and kept because it is half of the harness contract above: L0,
/// L1 and every L2 row is a registry test, and the alternative is each appending
/// worker re-deriving the agent-dir overrides and one of them forgetting one.
#[allow(dead_code)]
fn test_registry() -> (Arc<OrchRegistry>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (Arc::new(reg), dir)
}

/// Guardrails for a registry fixture. See `test_registry`'s note.
#[allow(dead_code)]
fn rails() -> Guardrails {
    Guardrails { max_agents: 4, agent_cli: "claude".into(), ..Guardrails::default() }
}

// ---------- the shared blocking pool fixture ----------

/// How many blocking threads the test runtime has. Two, not 512: the property
/// under test is what happens at the CEILING, and beta6 proved the ceiling is
/// reachable — this just makes reaching it cost two parked tasks instead of the
/// minutes of accumulation §1.2 describes.
const POOL_MAX_BLOCKING: usize = 2;

static POOL: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Serializes the tests that SATURATE the pool. Poison-tolerant on purpose: one
/// failing test must not turn into N `PoisonError`s that make a later mutation
/// round's reds unattributable (CLAUDE.md, `lock_safe`).
static POOL_SERIAL: Mutex<()> = Mutex::new(());

/// The process-wide `tauri::async_runtime` for this test binary, built with a
/// blocking pool of [`POOL_MAX_BLOCKING`].
///
/// `tauri::async_runtime::set` panics if the runtime is already initialized, and
/// it is a `OnceLock` inside tauri, so this is the ONLY place this binary may
/// touch it — see the harness contract in the module doc. Installing it (rather
/// than merely building a private runtime) is what makes the saturation real:
/// `tauri::async_runtime::spawn_blocking`, the call every converted command in
/// the app makes, then lands in THIS pool.
fn pool() -> &'static tokio::runtime::Runtime {
    POOL.get_or_init(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(POOL_MAX_BLOCKING)
            .build()
            .expect("build the small-pool tokio runtime");
        tauri::async_runtime::set(rt.handle().clone());
        rt
    })
}

/// Park every thread in the blocking pool and return the senders whose drop
/// releases them. Returns only once each task has PROVABLY started, so the pool
/// is occupied rather than merely asked to be.
fn saturate_the_pool() -> Vec<mpsc::Sender<()>> {
    let (started_tx, started_rx) = mpsc::channel();
    let mut releases = Vec::new();
    for _ in 0..POOL_MAX_BLOCKING {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        releases.push(release_tx);
        let started = started_tx.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _ = started.send(());
            // Parks until the test drops its sender at the end of the test.
            let _ = release_rx.recv();
        });
    }
    drop(started_tx);
    for i in 0..POOL_MAX_BLOCKING {
        started_rx
            .recv_timeout(GRACE)
            .unwrap_or_else(|_| panic!("setup: blocking task {i} never started — the pool cannot be saturated, so every assertion below would prove nothing"));
    }
    releases
}

/// Wait for a pane writer's completion reply from a plain (non-async) thread.
///
/// `blocking_recv` rather than an `.await` where the async shape is not itself
/// the thing under test: it needs no runtime, so a test using it cannot be
/// measuring the pool it is supposed to be independent of.
fn await_reply(rx: Result<WriteReceiver, String>) -> Result<(), String> {
    let mut rx = rx?;
    rx.blocking_recv().unwrap_or_else(|| Err("pty writer gone".to_string()))
}

// ---------- L3a: the input path does not use the shared blocking pool ----------

#[test]
fn l3a_a_keystroke_lands_while_the_blocking_pool_is_saturated() {
    const PANE: u32 = 90301;
    const CONTROL_PANE: u32 = 90302;
    let _serial = POOL_SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let pm = Arc::new(PtyManager::default());
    let captured = pm.register_fake_for_test(PANE, b"");
    let control_captured = pm.register_fake_for_test(CONTROL_PANE, b"");

    let rt = pool();
    let _releases = saturate_the_pool();

    // ---- the control, which is also the base's behaviour verbatim ----
    //
    // Two things at once, and both are load-bearing. It proves the pool really
    // IS saturated — without it "the write completed" passes just as well when
    // the setup silently failed and there was nothing to be isolated from,
    // which is the vacuity trap this repo keeps finding (CLAUDE.md). And it is
    // literally the pre-#1607 write path: `write_pty`'s body was
    // `spawn_blocking(move || state.write_from_frontend(..))`, so this is that
    // body, on this pool, going nowhere. That is beta6's step 4 in three lines.
    let (control_tx, control_rx) = mpsc::channel();
    let control_pm = pm.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = control_tx.send(control_pm.write_from_frontend(CONTROL_PANE, "never lands", true));
    });
    assert!(
        control_rx.recv_timeout(SETTLE).is_err(),
        "setup: a spawn_blocking hand-off completed while {POOL_MAX_BLOCKING} tasks are parked in \
         a {POOL_MAX_BLOCKING}-thread pool. The pool is not saturated (did tauri::async_runtime \
         get initialized before pool()?), so the assertion below would pass without testing \
         anything"
    );
    assert!(
        control_captured.lock().unwrap().is_empty(),
        "the pre-#1607 path wrote bytes while the pool was full — then it was never in the pool"
    );

    // ---- the property ----
    //
    // The same keystroke, through the seam `write_pty` now uses. It must land,
    // because the pane's writer thread is its own and owes the pool nothing.
    // Awaited through `rt.block_on` rather than `blocking_recv` on purpose:
    // this is the one row where the ASYNC shape matters, since `write_pty`
    // really does resolve its future on a runtime whose blocking half is full.
    let write_pm = pm.clone();
    assert!(
        completes_within(GRACE, move || {
            rt.block_on(async move {
                let mut reply = write_pm
                    .enqueue_frontend_write(PANE, "ls\r".to_string(), true)
                    .expect("enqueue onto a registered pane");
                reply.recv().await.expect("the writer thread replied")
            })
        }),
        "a frontend write did not complete while the shared blocking pool was saturated — the \
         input path is still competing with orchestration for pool threads (#1600 §1.2 step 4, \
         Phase 2.3)"
    );
    assert_eq!(
        &*captured.lock().unwrap(),
        b"ls\r",
        "the write resolved without the bytes reaching the pane — a completion reply that does \
         not mean 'the bytes are out' dissolves #65's ordering chain and P6's back pressure"
    );
}

// ---------- L3b: a wedged pane parks its own thread and nothing else's ----------

#[test]
fn l3b_a_wedged_pane_does_not_stop_another_panes_frontend_write() {
    const WEDGED: u32 = 90311;
    const HEALTHY: u32 = 90312;
    let pm = Arc::new(PtyManager::default());

    // Register the healthy pane FIRST: registration takes the map lock, so
    // doing it after the wedge would make a regression hang in setup rather
    // than fail on the assertion that names the defect (`ptywrite.rs`'s note).
    let healthy_captured = pm.register_fake_for_test(HEALTHY, b"");
    let (wedged_captured, gate) = pm.register_gated_fake_for_test(WEDGED);

    // Wedge pane 1 THROUGH THE SEAM, so what parks is its own writer thread —
    // the thing 2.3 introduces — and not a thread the test happens to own.
    let mut wedged_reply = pm
        .enqueue_frontend_write(WEDGED, "wedged".to_string(), true)
        .expect("enqueue onto the gated pane");
    assert!(
        gate.wait_for_writes(1, GRACE),
        "setup: pane {WEDGED}'s writer thread never reached the pipe, so it is not wedged"
    );

    // The user-visible harm, stated directly: pane 2's agent is fine, so typing
    // into pane 2 must land.
    let healthy_pm = pm.clone();
    assert!(
        completes_within(GRACE, move || await_reply(healthy_pm.enqueue_frontend_write(
            HEALTHY,
            "still typing".to_string(),
            true
        ))),
        "a wedged pane blocked an unrelated pane's frontend write — the writer threads are not \
         per-pane (#1607)"
    );
    assert_eq!(&*healthy_captured.lock().unwrap(), b"still typing");

    // ...and the wedged pane is still wedged, which is the half that must NOT
    // change. If its write had resolved early, the bytes would be buffered
    // somewhere and `write_pty`'s promise would be lying — the fire-and-forget
    // queue #719 rejected, arriving through the back door.
    assert!(
        wedged_captured.lock().unwrap().is_empty(),
        "the wedged pane's bytes landed while its writer is parked inside `write`"
    );
    assert!(
        wedged_reply.try_recv().is_err(),
        "the wedged pane's write reported completion while its bytes are still in the pipe — \
         back pressure (P6) is gone and #65's chain would dispatch the next chunk"
    );

    // Release it, and confirm the reply really is a statement about bytes: the
    // completion arrives only now, and the pane has them.
    gate.open();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done_tx.send(await_reply(Ok(wedged_reply)));
    });
    let outcome = done_rx
        .recv_timeout(GRACE)
        .expect("the wedged write never completed after the gate opened");
    assert_eq!(outcome, Ok(()));
    assert_eq!(&*wedged_captured.lock().unwrap(), b"wedged");
}

// ---------- L4: one door onto the blocking pool ----------

/// Kept split so the marker never appears as a whole token in this file — the
/// walk reads `src/` only today, and splitting it is what keeps this file
/// unable to be its own specimen if that is ever widened
/// (`perf_dispatch.rs`'s convention).
const RAW_POOL_CALL: &str = concat!("async_runtime::", "spawn_blocking(");

/// Blank every comment, newlines preserved, so a marker in prose is not read as
/// code. Strings are not blanked: no source file in the scanned roots contains
/// this marker inside a string literal, and the one file that does hold it as a
/// literal (`perf_dispatch.rs`'s `DELEGATION`) is under `tests/`, which is not
/// walked. Stated rather than assumed, per the source-scanning-guard convention.
fn code_only(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for i in from..to.min(out.len()) {
            if out[i] != b'\n' {
                out[i] = b' ';
            }
        }
    };
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            let start = i;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            blank(&mut out, start, i);
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            blank(&mut out, start, i);
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking preserves UTF-8 boundaries")
}

fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
#[ignore = "gated on #1601 (Phase 0.3): the six other raw sites move behind blocking::spawn_counted there. Phase 2.3 (#1607) removed pty.rs's two; lift this in the post-#1601 pass"]
fn l4_the_blocking_pool_has_exactly_one_door() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root is src-tauri's parent");
    let mut files = Vec::new();
    rs_files(&root.join("src-tauri/src"), &mut files);
    rs_files(&root.join("crates"), &mut files);
    assert!(
        files.len() > 20,
        "the walk found only {} .rs files — it did not descend, so a clean result would mean \
         nothing (vacuity guard)",
        files.len()
    );

    let mut sites: Vec<(String, usize)> = Vec::new();
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        let n = code_only(&text).matches(RAW_POOL_CALL).count();
        if n > 0 {
            // `/`-separated so the expectation reads the same on all three
            // platforms in the matrix.
            let rel = f.strip_prefix(root).unwrap_or(f).display().to_string().replace('\\', "/");
            sites.push((rel, n));
        }
    }
    sites.sort();

    let expected = vec![("src-tauri/src/blocking.rs".to_string(), 1usize)];
    assert_eq!(
        sites, expected,
        "the blocking pool must have exactly ONE door, so its depth can be counted and its \
         saturation diagnosed (#1600 §2.1, Phase 0.3). Every other module delegates through \
         `blocking::spawn_counted`/`run_blocking`"
    );
}
