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
//! **The symptom when that rule is broken names nothing about this file**, which
//! is why it is written down rather than left to be rediscovered: L3a fails with
//! `panicked at tauri-<ver>/src/async_runtime.rs:<line>` and the message
//! `runtime already initialized`. That is tauri's `set`, refusing because
//! something else in this binary reached the runtime first — not a real failure
//! of the property under test. Observed exactly once, on #1607's red-1 scratch
//! round, where the mutation under test made `enqueue_frontend_write` itself
//! touch the runtime and a sibling test then won the race.
//!
//! # Which of the plan's rows are here, and the one that is deliberately NOT
//!
//! The plan's table has L0-L6. Phase 2.3 gates L0, L3a, L3b and L4. The first
//! three are here and live: L0 and L3b's pool-depth clause were held back while
//! Phase 0 (#1601) was unmerged — an `#[ignore]`d test still has to COMPILE,
//! and both name APIs that did not exist yet — and landed once #1605 did.
//!
//! **L4 is not here, and that is a decision rather than an omission.** It would
//! have asserted that `async_runtime::spawn_blocking(` appears in exactly one
//! file, once. Phase 0 shipped precisely that guard first, as
//! `there_is_exactly_one_door_onto_the_blocking_pool` in
//! `src-tauri/tests/selfwatch.rs`, and shipped it *better*: it carries two
//! vacuity controls to this file's one (it also asserts it can still find the
//! one permitted call), and it matches the bare `spawn_blocking(` rather than
//! the qualified path, so an aliased import cannot walk past it. Two source
//! scans asserting one property is how a mechanism drifts — one gets updated,
//! the other quietly stops meaning anything — so this file defers to that one.
//!
//! The single axis this file's version would have added, recorded so it is a
//! known gap rather than a forgotten one: it walked `crates/` as well as
//! `src-tauri/src`. That is vacuous today, because the pool in question is
//! `tauri::async_runtime`'s and `loomux-engine` is Tauri-free by construction
//! (`doc/design/engine-extraction.md`) — there is nothing there that could
//! call it. If a crate under `crates/` ever links Tauri, that scan's root list
//! is the thing to widen.

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
/// Used by L0, and by every registry row the Phase 1 / 2.1 / 3a workers append:
/// the alternative is each of them re-deriving the agent-dir overrides and one
/// forgetting one, which is the leak #464 exists to stop.
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
fn rails() -> Guardrails {
    Guardrails { max_agents: 4, agent_cli: "claude".into(), ..Guardrails::default() }
}

// ---------- L0: the negative control — what the class actually looks like ----------

#[test]
fn l0_a_registry_read_does_not_return_while_its_lock_is_held() {
    // The control every other row in this file is measured against, and the
    // only one here that is SUPPOSED to fail to make progress. It documents the
    // class #1600 is about — an unbounded `lock_safe` acquisition on a path a
    // human or an agent waits behind — so that "X completed within GRACE"
    // elsewhere means something. Without a demonstration that this harness can
    // observe a stall at all, every positive result in this file is consistent
    // with a harness that cannot see one.
    //
    // `hold_lock_for_test` is #1601 Phase 0's seam (interface item 3) and is
    // what made this row landable: it takes the named registry lock on its own
    // thread at a real `lock_safe` call site and returns only once the hold is
    // real, so there is no race with the thread it just started.
    let (reg, _dir) = test_registry();
    let group = reg.create_group("C:/tmp/repo", rails()).expect("create a group");

    // Baseline first: with nothing held, the read answers. This is the
    // discriminating half — without it the assertion below would pass just as
    // well against a `group_summary` that never returns under any conditions,
    // or a GroupId this registry has never heard of.
    let (r, g) = (reg.clone(), group.id.clone());
    assert!(
        completes_within(GRACE, move || r.group_summary(&g)),
        "setup: group_summary did not answer with NO lock held, so the assertion below would \
         not be about the lock"
    );

    // Now hold `agents` for longer than the probe window and ask again.
    assert!(
        reg.hold_lock_for_test("agents", 4_000),
        "setup: hold_lock_for_test refused the lock name 'agents'"
    );
    let (r, g) = (reg.clone(), group.id.clone());
    assert!(
        !completes_within(SETTLE, move || r.group_summary(&g)),
        "group_summary returned while `agents` was held. That is not a pass — this row is the \
         NEGATIVE control, and it silently stops documenting the class the moment the read \
         either stops taking the lock or the seam stops holding it"
    );
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

/// [`await_reply`] under a deadline: `None` means it never answered, which is
/// the defect shape, and `Some(result)` carries what it answered. The bounded
/// version exists because a test that simply blocks on a reply hangs the CI job
/// instead of failing it.
fn await_reply_within(
    t: Duration,
    rx: Result<WriteReceiver, String>,
) -> Option<Result<(), String>> {
    let (tx, out) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(await_reply(rx));
    });
    out.recv_timeout(t).ok()
}

// ---------- L3a: the input path does not use the shared blocking pool ----------

#[test]
#[ignore = "SCRATCH ROUND ONLY (#1607 red-3b): ignored so this round's red is attributable to exactly one assertion."]
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
    // `Timeout` specifically, not any error. `RecvTimeoutError` has two
    // variants and `Disconnected` — the closure dropped without running, or
    // panicking before its send — satisfies `is_err()` just as well while
    // meaning the OPPOSITE of what this control claims. Distinguishing "queued
    // behind a full pool" from "never happened" is the control's entire job, so
    // it has to name which one it saw.
    assert_eq!(
        control_rx.recv_timeout(SETTLE),
        Err(mpsc::RecvTimeoutError::Timeout),
        "setup: the pre-2.3 spawn_blocking hand-off did not sit QUEUED while {POOL_MAX_BLOCKING} \
         tasks are parked in a {POOL_MAX_BLOCKING}-thread pool. Timeout means the pool is \
         saturated and the assertion below is meaningful; Ok means it is not saturated (did \
         tauri::async_runtime get initialized before pool()? — see the module header); \
         Disconnected means the task never ran at all"
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

    // The plan's second clause for this row (#1601 Phase 0.3's counter): the
    // input path must never ENTER the pool, which is a stronger statement than
    // "the write completed" and is the one that would still be true if the
    // pool happened to be empty. Baselined rather than asserted flat against
    // zero: this is a process-global counter, so pinning it to a literal would
    // make this row a hostage to anything else in the binary that hands off.
    // Today nothing here does — L3a saturates the pool with
    // `tauri::async_runtime::spawn_blocking` directly, which is not counted —
    // and the precondition below says so out loud rather than assuming it.
    let pool_before = loomux_engine::selfwatch::pool_in_flight();
    assert_eq!(
        pool_before, 0,
        "setup: {pool_before} hand-offs were already in flight before this test did anything. \
         Some other test in this binary is using `blocking::spawn_counted`; this row's \
         assertions below need a quiet baseline to mean anything"
    );

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

    // The counter clause. One pane is wedged mid-`write_all` and another has
    // just completed a write, so if the input path touched the pool at all this
    // is the moment it would show: the wedged job would still be occupying a
    // slot. It reads the baseline instead, which is the mechanical statement of
    // "the app's most latency-critical path competes for pool threads with
    // nothing" — the property beta6 turned on and the reason 2.3 exists.
    assert_eq!(
        loomux_engine::selfwatch::pool_in_flight(),
        pool_before,
        "the pty input path entered the counted blocking pool. A wedged pane would then hold a \
         pool slot for as long as its child declines to drain, which is the shared-resource \
         exhaustion #1600 §1.2 is about (#1601 Phase 0.3's counter, #1607 Phase 2.3)"
    );

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

// ---------- the ordering this change ADDS, rather than preserves ----------

#[test]
#[ignore = "SCRATCH ROUND ONLY (#1607 red-3b): ignored so this round's red is attributable to exactly one assertion."]
fn a_cd_and_the_keystrokes_around_it_land_in_arrival_order() {
    // `doc/design/pty-input-path.md`'s ordering table says `write_pty` vs
    // `change_dir` on one pane is "ordered by arrival again (#1607)" — it was
    // ordered before #719, "either order" while both bodies went to a shared
    // pool, and ordered again now that both go to one pane-owned queue.
    //
    // That is the one ordering property this change ADDS rather than preserves,
    // and a claim in a design note with no test under it is exactly the kind of
    // thing that goes quietly false. It is also the only coverage `enqueue_cd`
    // has: `tests/ptywrite.rs` drives `write_cd`'s BODY, which is the half that
    // did not change.
    //
    // Fail-able by construction — the two candidate outputs diverge. Route the
    // `cd` around the writer (write it at enqueue time) and its bytes land
    // FIRST, so the buffer stops starting with `a`. Give it a second queue and
    // `b` can overtake it, so the buffer stops ending with `b`.
    const PANE: u32 = 90321;
    const MARKER: &str = "ord-marker-dir";
    let pm = Arc::new(PtyManager::default());
    let captured = pm.register_fake_for_test(PANE, b"");

    // Enqueue is the arrival point — an in-memory send — so this IS the
    // arrival order, posted from one thread with no interleaving to argue about.
    let a = pm.enqueue_frontend_write(PANE, "a".to_string(), true).expect("enqueue a");
    let cd = pm.enqueue_cd(PANE, MARKER.to_string()).expect("enqueue cd");
    let b = pm.enqueue_frontend_write(PANE, "b".to_string(), true).expect("enqueue b");

    for (what, rx) in [("a", a), ("cd", cd), ("b", b)] {
        assert_eq!(
            await_reply_within(GRACE, Ok(rx)),
            Some(Ok(())),
            "the {what} job never completed — every job posted to a pane's writer must be \
             answered exactly once"
        );
    }

    let out = captured.lock().unwrap().clone();
    let s = String::from_utf8_lossy(&out).into_owned();
    assert!(
        s.starts_with('a'),
        "the cd (or the later keystroke) reached the pane ahead of the keystroke posted before \
         it — cd is not going through the pane's writer queue. Captured: {s:?}"
    );
    assert!(
        s.ends_with('b'),
        "the keystroke posted last did not land last — the pane has more than one path to its \
         stdin. Captured: {s:?}"
    );
    let middle = &s[1..s.len() - 1];
    assert!(
        middle.contains(MARKER),
        "the cd did not land between the two keystrokes at all. Captured: {s:?}"
    );
}
