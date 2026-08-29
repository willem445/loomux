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
//! **And the second rule, about the other shared fixture: the pool-depth
//! COUNTER.** `selfwatch::pool_in_flight()` is process-global, so a test that
//! reads it is reading every other test's hand-offs too. Any test that reaches
//! `blocking::spawn_counted` — directly, or through a command that delegates —
//! must hold `POOL_SERIAL`, and so must any test that ASSERTS on the counter.
//! L3b does both. Nothing in this binary counts today (L3a saturates with
//! `tauri::async_runtime::spawn_blocking` directly, which is uncounted, and L0
//! calls `group_summary` on the caller's thread), which is exactly why the rule
//! is written down now: the first appender to add a counted hand-off would
//! otherwise make L3b's counter assertion fire with "the pty input path entered
//! the counted blocking pool" — a false accusation, on the row whose job is to
//! be believed.
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
//! **L1 is appended by Phase 1 (#1608).** It obeys the pool rule above by
//! not engaging with it at all: every call it makes runs on the caller's
//! thread or on a thread it spawns itself, it reaches no counted hand-off,
//! and it asserts nothing about `pool_in_flight()`, so it neither needs nor
//! holds `POOL_SERIAL`. That is a property of what L1 tests — a published
//! read is a pointer clone, which is the whole point — rather than a
//! convention it follows, so it is stated here where a later appender will
//! read it.
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
//! **Two axes this file's version would have added, recorded as KNOWN gaps
//! rather than forgotten ones.** Neither is a reason to ship a second scan.
//!
//! 1. **"once", not just "one file".** L4 asserted the marker appears in
//!    exactly one file, exactly ONCE. `selfwatch.rs` asserts only that at least
//!    one permitted call exists in `blocking.rs` — there is no cap. A second
//!    `spawn_blocking(` added *inside* `blocking.rs` but outside
//!    `spawn_counted` would bypass the counter and stay green. Closing it is a
//!    count assertion in `selfwatch.rs`, which belongs to whoever owns that
//!    file rather than to this slice.
//! 2. **The `crates/` root.** L4 walked it; `selfwatch.rs` walks `src-tauri/src`
//!    only. Vacuous today: the pool is `tauri::async_runtime`'s, and
//!    `loomux-engine` is Tauri-free by construction
//!    (`doc/design/engine-extraction.md`) — neither crate's manifest carries
//!    tauri or tokio, so the call is unreachable there. If a crate under
//!    `crates/` ever links Tauri, that scan's root list is the thing to widen.

use loomux_engine::lockwatch::tracked_lock_names;
use serde_json::json;
use serde_json::Value;
use loomux_lib::orchestration::views::{group_view_payload, strip_view_payload, VIEW_STALE_AFTER_MS};
use loomux_lib::orchestration::{GroupId, Guardrails, OrchRegistry, Role};
use loomux_lib::pty::{PtyManager, WriteReceiver};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

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
    // Held because this row ASSERTS on the process-global pool counter, not
    // because it saturates anything — see the counter rule in the module
    // header. The baseline check below guards the read at the start; the lock
    // is what stops a concurrent counted hand-off landing between that read and
    // the assertion at the end, which would accuse the input path of entering
    // the pool when it never did.
    let _serial = POOL_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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

// ============================================================
// L1 (Phase 1, #1608) — the published reads answer while the registry does not
// ============================================================

/// Longer than any assertion below needs, so a hold never expires mid-probe and
/// turns a real failure into a flake that reads as a pass.
const HOLD_MS: u64 = 30_000;

/// A hold that is meant to END during the test, for the one assertion whose
/// subject is RECOVERY rather than liveness-under-hold.
const SHORT_HOLD_MS: u64 = 1_500;

/// Whether the published snapshot has no groups in it — the state in which both
/// payload builders take an early exit rather than assembling anything.
fn views_snapshot_is_empty(reg: &Arc<OrchRegistry>) -> bool {
    reg.views.load().value.groups.is_empty()
}

/// Split ONE snapshot of the tracked-lock names into the ones Phase 0's seam
/// can hold and the ones it refuses.
///
/// `hold_lock_for_test` knows four names and returns `false` for the rest — a
/// deliberate choice documented at that seam ("a representative handful rather
/// than all 82"). So this test iterates EVERY tracked name, holds the ones it
/// can, and reports the rest as a stated residual rather than silently
/// covering four and reading as covering all of them. Widening the seam widens
/// this test with no edit here.
fn classify(reg: &Arc<OrchRegistry>, names: &[String]) -> (Vec<String>, Vec<String>) {
    let mut holdable = Vec::new();
    let mut refused = Vec::new();
    for name in names {
        // A 1 ms probe: this only asks whether the seam KNOWS the name. The
        // real holds are taken per-lock in the test below.
        if reg.hold_lock_for_test(name, 1) {
            holdable.push(name.clone());
        } else {
            refused.push(name.clone());
        }
    }
    (holdable, refused)
}

#[test]
fn l1_a_published_read_returns_while_every_holdable_registry_lock_is_held() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    // Vacuity guard: the count `tracked_lock_names()` ACTUALLY returns. Phase 0
    // measured 82 (the plan said 85); this is a floor, not a pin, because the
    // number moves whenever a registry field is added and a test that has to be
    // edited for that is a test people edit without reading.
    // ONE read of the process-global registry, reused everywhere below.
    //
    // Reading it twice and comparing the two reads is a race, not a check:
    // `tracked_lock_names()` lists every LIVE tracked lock in the PROCESS, and
    // cargo runs this file's tests on separate threads in one process, so the
    // other test's registry construction lands between two reads. That is
    // exactly how this test first failed (87 vs 86).
    let names: Vec<String> = {
        let mut n: Vec<String> = tracked_lock_names().into_iter().map(str::to_string).collect();
        n.sort();
        n.dedup();
        n
    };
    let total = names.len();
    assert!(
        total >= 80,
        "tracked_lock_names() returned only {total} names — the lock registry stopped \
         registering, so iterating it proves nothing"
    );

    let (holdable, refused) = classify(&reg, &names);
    assert!(
        holdable.len() >= 4,
        "Phase 0's hold seam accepted only {} of {total} tracked locks ({holdable:?}) — it knows \
         four by name, so fewer than that means the seam broke, not that the registry shrank",
        holdable.len()
    );
    // NOT a partition assertion: `classify` walks `names` once, so
    // `holdable + refused == total` holds by construction and could never
    // fail. What CAN fail is the seam accepting a name this snapshot never
    // listed — which would mean the two are reading different registries.
    // `holdable` is BUILT from `names`, so asserting membership in `names`
    // would be a second tautology in place of the first. What CAN fail: the
    // four names Phase 0 documents its seam as knowing must all be holdable.
    // A renamed registry field, or a seam that stopped matching, reddens here
    // instead of quietly shrinking this test's coverage to nothing while its
    // own message still reports a count.
    for expected in ["agents", "groups", "mq_state_lock", "tasks_lock"] {
        assert!(
            holdable.iter().any(|h| h == expected),
            "`{expected}` is documented as holdable by hold_lock_for_test but was refused. \
             Holdable: {holdable:?}. Either the registry field was renamed or the seam broke; \
             either way this test now covers fewer locks than it reports."
        );
    }
    assert!(
        !refused.is_empty(),
        "every one of the {total} tracked locks was holdable, so the residual this test \
         reports is empty — the seam is documented as knowing four names, so this means it \
         stopped refusing rather than that it grew"
    );

    // Publish first (#1625 review N1). Without this the loop probes an EMPTY
    // snapshot: `group_view_payload` takes its `return Value::Null` early exit
    // and `strip_view_payload` its no-groups branch, so the class property is
    // exercised but the POPULATED path never is. One pass makes the strip probe
    // walk a real map and the group probe assemble a real payload, under a hold.
    reg.views.note_view_lease(&g.id);
    reg.views.publish_pass_at(&reg, Instant::now());
    assert!(
        !views_snapshot_is_empty(&reg),
        "setup: the snapshot must be populated, or the probes below take their empty-map \
         early exits and this loop proves less than it reports"
    );

    // THE PROPERTY. For each lock the seam can hold: both published reads must
    // return. Before #1608 each of these was ten (and two) registry
    // acquisitions per tick, so this is exactly L0's shape with the fix in.
    for name in &holdable {
        assert!(
            reg.hold_lock_for_test(name, HOLD_MS),
            "setup: the `{name}` hold must be real, or the assertions below prove nothing"
        );

        // NEGATIVE CONTROL, per lock, and the reason this test is not a
        // tautology: a REGISTRY read must NOT return while `agents` is held.
        // If the seam ever stops actually holding, every `completes_within`
        // below starts passing for the wrong reason and reads exactly like
        // coverage. Only `agents` is probed this way — it is the lock
        // `group_summary` takes first — so the control is asserted against a
        // read whose parking is known, not against every lock's shape.
        //
        // (L0 above states the same class as a test of its own — it landed with
        // #1607. This is the in-test control, so L1 is non-vacuous standing
        // alone, independently of L0.)
        if name == "agents" {
            let probe = reg.clone();
            let gid = g.id.clone();
            assert!(
                !completes_within(SETTLE, move || probe.group_summary(&gid)),
                "SETUP FAILURE, not a pass: `group_summary` returned while `agents` was held, \
                 so the hold is not holding and every assertion in this test is vacuous"
            );
        }

        let probe = reg.clone();
        let gid = g.id.clone();
        assert!(
            completes_within(GRACE, move || {
                group_view_payload(&probe.views.load(), &gid, Instant::now())
            }),
            "orch_group_view's body did not return while `{name}` was held. A polled read must \
             take NO registry lock: that is what stops one long hold parking a blocking-pool \
             thread per poller per tick until write_pty cannot be scheduled (#1600 §1.2)"
        );

        let probe = reg.clone();
        assert!(
            completes_within(GRACE, move || {
                strip_view_payload(&probe.views.load(), Instant::now())
            }),
            "orch_strip_view's body did not return while `{name}` was held (same property, the \
             other poll site)"
        );
    }

    // The residual, stated rather than implied — and printed, so a widening of
    // the seam is visible in the log rather than needing an edit here.
    println!(
        "L1 covered {}/{total} tracked locks: {holdable:?}. NOT holdable by Phase 0's seam \
         (stated residual, not a silent gap): {} names.",
        holdable.len(),
        refused.len()
    );
}

#[test]
fn l1_stale_flips_on_the_clock_while_a_lock_is_held_and_clears_on_the_next_publish() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    // One good publish, then the registry wedges.
    //
    // Stamped in the PAST, and that is what makes the release assertion at
    // the end of this test able to fail (#1625 review B3ii). With
    // `Instant::now()` the un-republished world was ALSO under the 5 s
    // threshold at `recovered`, so `!stale_at(recovered)` held whether or
    // not the republish did anything — the two candidate outcomes did not
    // diverge, and the only way it could ever have moved was a setup pass
    // slower than 5 s, i.e. a flake. Sixty seconds back makes the
    // un-republished world unambiguously stale.
    let published_at = Instant::now()
        .checked_sub(Duration::from_secs(60))
        .expect("the test host has more than 60s of uptime");
    reg.views.note_view_lease_at(&g.id, published_at);
    reg.views.publish_pass_at(&reg, published_at);
    // SHORT, unlike the holds above: this test's last step needs the hold to
    // END so the publisher can recover. Long enough that every assertion
    // before it runs with the lock genuinely held (they are all injected-clock
    // reads, so they take microseconds), short enough to elapse well inside
    // `GRACE`.
    assert!(
        reg.hold_lock_for_test("groups", SHORT_HOLD_MS),
        "setup: the `groups` hold must be real"
    );

    let stale_at = |now: Instant| -> bool {
        let payload = group_view_payload(&reg.views.load(), &g.id, now);
        payload
            .get("meta")
            .and_then(|m| m.get("stale"))
            .and_then(Value::as_bool)
            .expect("meta.stale is a bool")
    };

    // The read still answers with the wedge in place — that is L1 — and what it
    // answers is HONEST about its age. Injected clock rather than a sleep: the
    // property is the threshold, not the wall time.
    assert!(!stale_at(published_at), "a payload just published is not stale");
    assert!(
        stale_at(published_at + Duration::from_millis(VIEW_STALE_AFTER_MS + 1)),
        "past the threshold, a snapshot the publisher can no longer refresh must report stale — \
         a frozen panel that looks live is the disclosure gap #1604 review N3 deferred here"
    );
    assert!(
        stale_at(published_at + Duration::from_secs(3600)),
        "and waiting longer never clears it: the badge is released by EVIDENCE (the next \
         successful store), never by elapsed time"
    );

    // RELEASE ON EVIDENCE — and the publisher is the thread that pays for it.
    //
    // An earlier version of this asserted the republish must NOT park behind
    // `groups`. That was false, and this test caught it: `publish_group_at`
    // calls `compute_group` calls `group_summary`, which takes `agents` and
    // then `groups`. It parks BY DESIGN — being the one thread that waits is
    // the whole shape of Phase 1, and the READS staying live while it waits is
    // what the test above asserts.
    //
    // So what this step asserts is the recovery: the publisher parks, the hold
    // ends, the publish completes on its own, and the badge clears on that
    // evidence rather than on a timer. `SHORT_HOLD_MS` is what makes it
    // observable inside `GRACE` — the hold really does elapse during the call.
    let recovered = Instant::now();

    // The control BRACKETS the republish at one instant, which is the only
    // placement that discriminates: stale at `recovered` before it, not stale
    // at `recovered` after it. Reading it afterwards — as this first did —
    // measures the snapshot the republish has already re-stamped, so it
    // asserts the pre-republish world's staleness while looking at the
    // post-republish one.
    //
    // It can only be true because `published_at` is 60 s back: with
    // `Instant::now()` both worlds sit under the 5 s threshold and neither
    // this control nor the assertion it guards could ever fail.
    assert!(
        stale_at(recovered),
        "control: the snapshot must be stale at `recovered` BEFORE the republish, or the \
         assertion after it is a reading of one outcome rather than a choice between two"
    );

    assert!(
        completes_within(GRACE, {
            let reg = reg.clone();
            let gid = g.id.clone();
            move || reg.views.publish_group_at(&reg, &gid, recovered)
        }),
        "the publisher must RECOVER once the hold ends: it parks behind `groups` by design, \
         and a publish that never completes afterwards would mean the badge can never clear"
    );
    assert!(!stale_at(recovered), "one successful publish is the evidence that clears the badge");
}

// ---------- L2d: the budget mechanism, through a REAL registry read ----------
//
// `crates/loomux-engine/src/budget.rs` unit-tests `read_budget` and
// `MutationScope` against a `TrackedMutex` built for the purpose. That proves
// the mechanism; it does not prove the mechanism survives contact with
// `orchestration/mod.rs`, and the difference is the whole risk of Phase 2.1:
// the unwind travels through real registry code, which owns guards, `Drop`
// impls and (in principle) a `catch_unwind` of its own that could swallow a
// typed payload it has never heard of.
//
// So these two rows drive SHIPPED registry functions. `group_summary` is the
// one L0 already uses as its negative control, which is what makes the pair
// legible: L0 says this read does not return while `agents` is held, and L2d
// says the same read under a budget does.

/// The budget these rows measure against. The publisher's, because
/// `group_summary` is a section of the published payload.
const L2D_BUDGET: Duration = loomux_engine::budget::POLL_LOCK_BUDGET;

#[test]
fn l2d_a_read_budget_around_a_real_registry_read_answers_busy_instead_of_parking() {
    let (reg, _dir) = test_registry();
    let group = reg.create_group("C:/tmp/repo", rails()).expect("create a group");

    // The discriminating half. Without it, `is_err()` below is satisfied just as
    // well by a `read_budget` that never succeeds at all — and by a GroupId this
    // registry has never heard of.
    let ok = loomux_engine::budget::read_budget(L2D_BUDGET, || reg.group_summary(&group.id));
    assert!(
        ok.is_ok(),
        "setup: an uncontended registry read must COMPLETE under a budget. If this fails, \
         every Busy below is about the budget mechanism rather than about the lock"
    );

    // `agents` is what `group_summary` takes, and the hold outlives the budget
    // by 3x so the answer cannot come from the hold expiring.
    assert!(
        reg.hold_lock_for_test("agents", 4_000),
        "setup: hold_lock_for_test refused the lock name 'agents'"
    );

    let started = std::time::Instant::now();
    let busy = loomux_engine::budget::read_budget(L2D_BUDGET, || reg.group_summary(&group.id))
        .err()
        .expect(
            "a registry read under a budget must answer Busy. Parking here is the beta6 defect \
             this phase exists to remove; an Ok means the unwind never fired",
        );
    let waited = started.elapsed();

    assert_eq!(
        busy.lock, "agents",
        "the Busy must name the lock that actually blocked, or the breadcrumb it writes sends \
         the next diagnosis somewhere else"
    );
    // It answered on the BUDGET, not on the hold expiring. Without this the row
    // passes against a mechanism that simply waited the hold out.
    assert!(
        waited < Duration::from_millis(3_000),
        "it answered after {waited:?}, which is the hold expiring rather than the budget firing"
    );
    // The unwind left no wreckage: the lock the read was ABOUT is not tracked as
    // held by the abandoned frame, and the registry still answers afterwards.
    assert!(
        completes_within(GRACE, {
            let (r, g) = (reg.clone(), group.id.clone());
            move || r.group_summary(&g)
        }),
        "after the hold ended, the same read must work again — an unwind that left a guard \
         behind would have wedged `agents` permanently, which is worse than the bug"
    );
}

#[test]
fn l2d_a_timeout_inside_a_mutation_scope_waits_rather_than_unwinding() {
    // The safety lever, on a real read path. A mutating frame must never be
    // abandoned partway between two maps, so inside a `MutationScope` the same
    // timeout WAITS — and this row is what makes `doc/design/lock-liveness.md`
    // §4's argument checkable rather than merely stated.
    let (reg, _dir) = test_registry();
    let group = reg.create_group("C:/tmp/repo", rails()).expect("create a group");

    assert!(
        reg.hold_lock_for_test("agents", SHORT_HOLD_MS),
        "setup: hold_lock_for_test refused the lock name 'agents'"
    );

    let started = std::time::Instant::now();
    let out = loomux_engine::budget::read_budget(L2D_BUDGET, || {
        let _scope = loomux_engine::budget::MutationScope::enter();
        reg.group_summary(&group.id)
    });
    let waited = started.elapsed();

    assert!(
        out.is_ok(),
        "inside a MutationScope a budget timeout must WAIT for the lock, not unwind to the \
         frame — a mutation abandoned between two maps is corruption, which is the one \
         outcome this trade refuses"
    );
    // And it really did wait past the budget rather than winning a race: without
    // this the assertion above passes against a lock that was never held.
    assert!(
        waited >= L2D_BUDGET,
        "it returned in {waited:?}, inside the {L2D_BUDGET:?} budget — the hold was not in its \
         way, so this row proves nothing about what a timeout does"
    );
}

// ---------- L2a-L2c: the MCP surface and the cadenced loops ----------

use loomux_engine::budget as bg;
use loomux_lib::orchestration::mcp;

/// Restores the mutating-tool deadline however the scope ends — the
/// `HoldWarn` idiom from `tests/selfwatch.rs`, for the same reason: a test that
/// fails while it is moved leaves every later test measuring against a number
/// nobody set.
struct MutateDeadline(Duration);
impl MutateDeadline {
    fn set(d: Duration) -> Self {
        Self(bg::set_mutate_deadline_for_test(d))
    }
}
impl Drop for MutateDeadline {
    fn drop(&mut self) {
        bg::set_mutate_deadline_for_test(self.0);
    }
}

/// A registry with an agent whose token the MCP surface will resolve, plus that
/// token. `set_self_arc` because `dispatch_bounded` spawns the mutating helper
/// on the registry's own `Arc`.
fn mcp_fixture() -> (Arc<OrchRegistry>, String, GroupId, tempfile::TempDir) {
    let (reg, dir) = test_registry();
    // `dispatch_bounded` runs a mutating tool on the registry's own `Arc`; a
    // registry that never had `set_self_arc` called has none, and L2b would then
    // be measuring a path the app does not take.
    reg.set_self_arc();
    let group = reg.create_group("C:/tmp/repo", rails()).expect("create a group");
    let agent = reg
        .spawn_agent(&group.id, Role::Orchestrator, "orch", "", false, None)
        .expect("a fake agent to carry a resolvable token");
    (reg, agent.token, group.id, dir)
}

/// Wall-clock ms, for the tick functions that take a `now`. They all skip in
/// L2c, so the value only has to be plausible.
fn now_ms_local() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[test]
fn l2a_mcp_answers_within_its_budgets_while_a_registry_lock_is_held() {
    let (reg, token, _group, _dir) = mcp_fixture();

    // Baseline: both halves answer normally. Without this, every assertion
    // below is satisfied by an MCP surface that is broken in some other way.
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let warm = mcp::handle_for_test(&reg, &ping, Some(&token)).expect("a request with an id");
    assert!(warm.get("result").is_some(), "setup: ping must answer normally: {warm}");

    // Resolved NOW, before anything is held. `resolve_token` is an unbudgeted
    // direct call here, so resolving it under the `groups` hold below would park
    // this thread for the whole hold — and the read half would then run against
    // an `agents` hold that had already expired, which is how the first version
    // of this row passed for the wrong reason.
    let caller = reg.resolve_token(&token).expect("the token resolves");

    assert!(reg.hold_lock_for_test("groups", 20_000), "setup: cannot hold `groups`");

    // ---- the auth half: this is #1606's measured hole ----
    //
    // `ping` takes no registry lock of its own, and never reached its arm: every
    // request resolves its token first, and `resolve_token` takes `groups`.
    let started = std::time::Instant::now();
    let busy = mcp::handle_for_test(&reg, &ping, Some(&token)).expect("a request with an id");
    let waited = started.elapsed();

    assert!(
        waited < bg::MCP_AUTH_BUDGET + GRACE,
        "the MCP did not answer a ping in {waited:?} while `groups` was held. That is the \
         defect this phase removes, measured: #1606 logged `mcp ok=false in 20004ms`"
    );
    let err = busy.get("error").unwrap_or_else(|| panic!("expected an error envelope: {busy}"));
    assert_eq!(
        err.get("code").and_then(|c| c.as_i64()),
        Some(mcp::MCP_BUSY_CODE),
        "a busy registry must answer the RETRYABLE code, not the permanent auth refusal \
         (-32000) — a caller that cannot tell them apart either retries forever or gives up \
         forever: {busy}"
    );
    let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or_default();
    assert!(msg.starts_with("loomux busy:"), "message is a public contract: {msg}");
    assert!(msg.contains("groups"), "the answer must NAME the lock that blocked: {msg}");
    // The `data` shape is the half a client machine-reads.
    let data = err.get("data").unwrap_or_else(|| panic!("no data block: {busy}"));
    assert_eq!(data.get("retryable").and_then(|r| r.as_bool()), Some(true));
    assert!(
        data.get("retry_after_ms").and_then(|r| r.as_u64()).is_some_and(|m| m > 0),
        "retry_after_ms must be a positive hint: {data}"
    );

    // ---- the read-tool half ----
    //
    // `list_agents` takes `agents`. Driven through `dispatch`, which is the seam
    // that owns the read budget — and note that what it covers is the WHOLE arm,
    // not just `call_tool`: `note_agent_ack` takes `agents` too and runs first,
    // so a budget around `call_tool` alone would be a bound with an unbounded
    // wait in front of it. This row is what found that.
    assert!(reg.hold_lock_for_test("agents", 20_000), "setup: cannot hold `agents`");
    let call = json!({ "name": "list_agents", "arguments": {} });
    let started = std::time::Instant::now();
    let out = mcp::dispatch(&reg, &caller, "tools/call", &call).expect("a tool RESULT, not a protocol error");
    let waited = started.elapsed();

    assert!(
        waited < bg::MCP_READ_BUDGET + GRACE,
        "a read tool did not answer in {waited:?} with `agents` held"
    );
    assert_eq!(
        out.get("isError").and_then(|e| e.as_bool()),
        Some(true),
        "a busy read is an EXECUTION failure, so it is an isError result rather than a \
         protocol error — that is the shape that reaches the model's context: {out}"
    );
    let text = out["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.starts_with("loomux busy:"), "read-tool text is a public contract: {text}");
    assert!(text.contains("agents"), "the text must name the lock that blocked: {text}");
    assert!(
        text.contains("Nothing was executed"),
        "the caller has to be told it may retry safely: {text}"
    );
}

#[test]
fn l2b_a_slow_mutating_tool_answers_and_still_completes_exactly_once() {
    // The exactly-once property, which is why a mutating tool is NOT unwound.
    // A deadline that abandoned the work would double-execute on the retry the
    // message tells the caller not to make.
    const HOLD_MS: u64 = 3_000;
    let _deadline = MutateDeadline::set(Duration::from_millis(300));
    let (reg, token, group, _dir) = mcp_fixture();

    assert!(reg.hold_lock_for_test("tasks_lock", HOLD_MS), "setup: cannot hold `tasks_lock`");

    let call = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "upsert_task", "arguments": { "title": "l2b-exactly-once" } },
    });
    let started = std::time::Instant::now();
    let out = mcp::handle_for_test(&reg, &call, Some(&token)).expect("a request with an id");
    let waited = started.elapsed();

    assert!(
        waited < bg::mutate_deadline() + GRACE,
        "the handler waited {waited:?} on a mutating tool — the point of the deadline is that \
         the orchestrator's turn is not spent on it"
    );
    let text = out["result"]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("still executing"),
        "the caller must be told the work is IN FLIGHT, not that it failed: {out}"
    );
    assert!(
        text.contains("do NOT re-issue"),
        "the instruction not to retry is the whole reason this is not an unwind: {text}"
    );

    // Now let the hold expire and prove the tool completed EXACTLY once.
    let deadline = std::time::Instant::now() + Duration::from_millis(HOLD_MS) + GRACE;
    loop {
        let n = reg.tasks(&group).iter().filter(|t| t.title == "l2b-exactly-once").count();
        if n == 1 {
            break;
        }
        assert!(n <= 1, "the tool ran {n} times — a deadline that abandons work double-executes");
        assert!(
            std::time::Instant::now() < deadline,
            "the tool never completed after the hold ended; `it WILL complete` is then a false \
             promise made to an agent"
        );
        std::thread::yield_now();
    }
}

#[test]
fn l2c_every_cadenced_tick_returns_while_the_registry_is_wedged() {
    // Nine loops, each of which used to park a thread for as long as a hold
    // lasted. Driven CONCURRENTLY rather than in sequence: they are independent,
    // the property is "each returns", and nine sequential 5 s skips would put 45
    // seconds into every run of this suite to measure one bound nine times.
    let (reg, _dir) = test_registry();
    let _group = reg.create_group("C:/tmp/repo", rails()).expect("create a group");

    assert!(reg.hold_lock_for_test("agents", 20_000), "setup: cannot hold `agents`");

    let ticks: Vec<(&'static str, Box<dyn Fn(&OrchRegistry) + Send + Sync>)> = vec![
        ("reap_idle_agents", Box::new(|r: &OrchRegistry| { r.reap_idle_agents(now_ms_local()); })),
        ("run_watchdog", Box::new(|r: &OrchRegistry| { r.run_watchdog(now_ms_local()); })),
        ("run_attention", Box::new(|r: &OrchRegistry| r.run_attention(now_ms_local()))),
        ("run_idle_tick", Box::new(|r: &OrchRegistry| { r.run_idle_tick(now_ms_local()); })),
        ("run_compact_nudge", Box::new(|r: &OrchRegistry| { r.run_compact_nudge(now_ms_local()); })),
        ("run_gh_poll_tick", Box::new(|r: &OrchRegistry| { r.run_gh_poll_tick(); })),
        ("run_workflow_gate_reload", Box::new(|r: &OrchRegistry| r.run_workflow_gate_reload())),
        ("run_disk_monitor", Box::new(|r: &OrchRegistry| r.run_disk_monitor())),
        ("flush_due_max_notices", Box::new(|r: &OrchRegistry| r.flush_due_max_notices(now_ms_local()))),
    ];

    let (tx, rx) = mpsc::channel();
    let n = ticks.len();
    for (name, f) in ticks {
        let (r, tx) = (reg.clone(), tx.clone());
        std::thread::spawn(move || {
            f(&r);
            let _ = tx.send(name);
        });
    }
    drop(tx);

    let mut returned = Vec::new();
    let deadline = bg::TICK_LOCK_BUDGET + GRACE;
    while returned.len() < n {
        match rx.recv_timeout(deadline) {
            Ok(name) => returned.push(name),
            Err(_) => break,
        }
    }
    assert_eq!(
        returned.len(),
        n,
        "only {}/{n} cadenced ticks returned within {deadline:?} while `agents` was held. The \
         ones missing are: {:?}. A tick that never returns is one parked thread per cadence \
         for as long as the hold lasts — #1600 §1.2 step 3.",
        returned.len(),
        {
            let mut missing = vec![
                "reap_idle_agents", "run_watchdog", "run_attention", "run_idle_tick",
                "run_compact_nudge", "run_gh_poll_tick", "run_workflow_gate_reload",
                "run_disk_monitor", "flush_due_max_notices",
            ];
            missing.retain(|m| !returned.contains(m));
            missing
        }
    );
}

#[test]
fn every_tool_the_mcp_surface_lists_is_classified_read_or_mutate() {
    // `tool_kind`'s default arm is `Mutate`, which is the safe side — but it is
    // also the arm that would silently swallow the whole surface if the table
    // stopped matching. So the population is taken from what `tools/list`
    // ACTUALLY returns, per role, rather than from a list written here.
    //
    // What this can and cannot say, stated because the default makes it subtle:
    // it proves every listed tool REACHES a decision and that the Read set is
    // non-empty on a role that has reads. It cannot prove a tool is on the right
    // side — that is the reviewable table in `mcp.rs`, and the asymmetry
    // (a mis-classified read merely waits; a mis-classified mutation can be
    // abandoned) is why the default is what it is.
    let (reg, token, _group, _dir) = mcp_fixture();
    let caller = reg.resolve_token(&token).expect("token resolves");
    let listed = mcp::dispatch(&reg, &caller, "tools/list", &json!({})).expect("tools/list");
    let tools = listed["tools"].as_array().expect("an array of tools").clone();

    assert!(
        tools.len() >= 20,
        "tools/list returned {} tools — the scan has stopped reading the surface it is \
         supposed to cover",
        tools.len()
    );

    let mut reads = 0usize;
    for t in &tools {
        let name = t["name"].as_str().expect("every tool has a name");
        match mcp::tool_kind(name) {
            mcp::ToolKind::Read => reads += 1,
            mcp::ToolKind::Mutate => {}
        }
    }
    // The vacuity control. Every assertion above is satisfied by a `tool_kind`
    // whose body is `ToolKind::Mutate` — which is exactly what a table that had
    // stopped matching would degrade to, and it would be invisible without this.
    assert!(
        reads >= 5,
        "only {reads} of {} listed tools classify as Read. The table has stopped matching and \
         every tool is falling through to the default arm — the read budget then applies to \
         nothing at all",
        tools.len()
    );
}

// ---------- L2e: a partial group with nothing to inherit is WITHHELD ----------

#[test]
fn a_first_pass_that_goes_partial_publishes_nothing_rather_than_nulls() {
    // CLAUDE.md's rule (#1671): a snapshot in front of per-item reads inherits
    // what those reads answered, and the miss is SILENT — an absent entry
    // renders as "nothing here" rather than failing, so neither the compiler
    // nor a unit test over a faked registry sees it.
    //
    // The busy fallback keeps a section's PREVIOUS value. On a group's FIRST
    // pass there is no previous value and no previous stamp, so the naive
    // version publishes an entry whose sections are all `Null` and whose
    // `computed_at` is this pass — a panel that reads CURRENT while asserting
    // the group has nothing, with no stale badge to say otherwise.
    //
    // The class this bites is not hypothetical: a restored-but-not-resumed
    // group arrives through a strip lease and has no prior entry by
    // construction (#1625 round 2), so its first pass is exactly the pass that
    // can hit a busy section.

    // The discriminating half, on its own registry: a first pass with nothing
    // held DOES publish the group. Without it, the assertion below passes just
    // as well against a publisher that never publishes anything.
    {
        let (reg, _dir) = test_registry();
        let group = reg.create_group("C:/tmp/repo", rails()).expect("create a group");
        reg.views.publish_pass(&reg);
        assert!(
            reg.views.load().value.groups.contains_key(&group.id),
            "setup: an unheld first pass must publish the group, or the assertion below is \
             about a publisher that does nothing"
        );
    }

    let (reg, _dir) = test_registry();
    let group = reg.create_group("C:/tmp/repo", rails()).expect("create a group");
    assert!(
        reg.views.load().value.groups.is_empty(),
        "setup: this registry has never published, so the group has nothing to inherit"
    );
    assert!(reg.hold_lock_for_test("agents", 20_000), "setup: cannot hold `agents`");

    reg.views.publish_pass(&reg);
    let snapshot = reg.views.load();

    assert!(
        !snapshot.value.groups.contains_key(&group.id),
        "a group whose FIRST pass went partial was published anyway. Its sections are all \
         null and its stamp is fresh, so the strip renders it as a group with nothing in it \
         and no stale badge — the silent \"nothing here\" that absence avoids, since absence \
         already means \"ask again shortly\" to both payload builders"
    );
    // And the payload builder agrees, which is what a reader actually sees.
    assert_eq!(
        loomux_lib::orchestration::views::group_view_payload(&snapshot, &group.id, std::time::Instant::now()),
        serde_json::Value::Null,
        "an absent group must answer Null — the same degrade as a group created since the \
         last pass"
    );
}

// ---------- L2f: a partial group inherits its stalest part's AGE ----------

#[test]
fn a_partial_group_keeps_the_previous_stamp_so_the_badge_can_tell() {
    // This is the rule that makes `viewstale.ts`'s existing pair of labels
    // correct with NO frontend change, and it was the load-bearing choice in
    // this slice — so it needs its own pin rather than riding on the fact that
    // the frontend was not edited.
    //
    // A group that kept a section's previous value keeps that value's AGE too.
    // Stamp it with THIS pass instead and the panel reads current — `stale` is
    // derived from `age_ms`, so a fresh stamp means no badge — while showing a
    // frozen number. That is the silent freeze #1604 review N3 is about, and it
    // is invisible: every field is present and well-formed.
    let (reg, _dir) = test_registry();
    let group = reg.create_group("C:/tmp/repo", rails()).expect("create a group");

    reg.views.publish_pass(&reg);
    let first = reg.views.load();
    let before = first.value.groups.get(&group.id).expect("published").clone();
    assert!(!before.partial, "setup: an unheld pass must not be partial");

    // Something to distinguish the two passes by. `computed_unix_ms` is a
    // wall-clock stamp, so the passes must be separated by at least a
    // millisecond for the assertion below to mean anything.
    std::thread::sleep(Duration::from_millis(5));

    assert!(reg.hold_lock_for_test("agents", 20_000), "setup: cannot hold `agents`");
    reg.views.publish_pass(&reg);
    let after = reg.views.load();
    let now = after.value.groups.get(&group.id).expect("still published — it has a previous value to inherit").clone();

    assert!(now.partial, "a pass that could not read `agents` must mark the group partial");
    assert_eq!(
        now.computed_at, before.computed_at,
        "a partial group must keep its PREVIOUS stamp. With a fresh one, `age_ms` restarts \
         and `stale` never flips, so the panel reads current while showing the value it \
         could not refresh"
    );
    assert_eq!(
        now.computed_unix_ms, before.computed_unix_ms,
        "the wall-clock stamp a human reads must move with `computed_at`, or the badge and \
         the timestamp beside it disagree"
    );

    // And the property as the frontend actually sees it: `partial` reaches the
    // payload, and the age it is paired with is the OLD one.
    let payload =
        loomux_lib::orchestration::views::group_view_payload(&after, &group.id, std::time::Instant::now());
    assert_eq!(payload["meta"]["partial"], serde_json::Value::Bool(true));
    assert_eq!(
        payload["meta"]["published_at_ms"].as_u64(),
        Some(before.computed_unix_ms),
        "the payload must report the stalest part's age, not this pass's"
    );
}

// ---------- L2g: rider R1 as an ENFORCED invariant, not an argument ----------

#[test]
fn no_read_tool_can_unwind_after_a_durable_write() {
    // Review round 1 turned R1 from "prove or scope" into this. The enumeration
    // that answered it was incomplete — four `ToolKind::Read` arms wrote
    // durably and were never looked at, one of them (`check_mail`) consumingly —
    // and an enumeration is the wrong instrument anyway: it is a claim about
    // today's call graph, re-verified by nobody.
    //
    // So the property is measured instead. `budget::torn_writes()` counts budget
    // frames that unwound AFTER a durable write, which is the tear itself; the
    // seal (`budget::note_durable_write`) makes it structurally zero. This row
    // drives every tool the surface CLASSIFIES AS A READ, under a budget, with a
    // registry lock held, and asserts that number did not move.
    //
    // Two things make it non-vacuous, and both are needed:
    //
    //  - the population control below (`sealed_frames` must MOVE), so "zero
    //    torn" cannot be "zero writes" — which is exactly what this test would
    //    report against a registry whose read paths never wrote anything;
    //  - the lock it holds is `app`, which `write_mailbox` takes AFTER
    //    atomically replacing `mailbox.json`. That is the shape the tear needs,
    //    and it is the one the review found live.
    let (reg, token, _group, _dir) = mcp_fixture();
    let caller = reg.resolve_token(&token).expect("the token resolves");

    // The Read set, taken from what the surface actually lists rather than a
    // list written here — a tool renamed out of `tools/list` must not silently
    // leave this loop covering one fewer arm.
    let listed = mcp::dispatch(&reg, &caller, "tools/list", &json!({})).expect("tools/list");
    let reads: Vec<String> = listed["tools"]
        .as_array()
        .expect("an array of tools")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .filter(|n| mcp::tool_kind(n) == mcp::ToolKind::Read)
        .collect();
    assert!(
        reads.len() >= 8,
        "only {} listed tools classify as Read — the table has stopped matching and this row \
         is covering almost nothing: {reads:?}",
        reads.len()
    );

    // PER-THREAD counts, not the process-global ones. `cargo test` runs this
    // binary's tests concurrently, so a delta on a global counter is a race:
    // a sibling test's seal would satisfy the population control below while
    // this sweep sealed nothing, which is the exact vacuity it exists to stop.
    // Every Read tool runs INLINE on this thread (only Mutate tools go to the
    // helper thread), so the per-thread counters see the whole sweep.
    let (sealed_before, torn_before) = loomux_engine::budget::thread_seal_counts();

    // Held for the whole sweep: `app` is taken after `write_mailbox`'s durable
    // replace, and by `agent_output_totals` / `attention_inputs` in the app.
    assert!(reg.hold_lock_for_test("app", 20_000), "setup: cannot hold `app`");

    for name in &reads {
        let call = json!({ "name": name, "arguments": {} });
        // The answer does not matter — Ok, isError, busy are all fine. What
        // matters is that no frame unwound after writing.
        let _ = mcp::dispatch(&reg, &caller, "tools/call", &call);
    }

    let (sealed_after, torn_after) = loomux_engine::budget::thread_seal_counts();
    let torn = torn_after - torn_before;
    assert_eq!(
        torn, 0,
        "{torn} read-tool frame(s) unwound AFTER performing a durable write. That is the \
         tear rider R1 is about: the caller is told `Nothing was executed` while a state file \
         has already been replaced. Either the tool mutates and belongs in ToolKind::Mutate, \
         or its write needs to reach `budget::note_durable_write` so the frame seals."
    );

    // The population control. Without it this row passes just as well against a
    // sweep in which no read path wrote anything at all — which is precisely
    // what a faked registry would produce, and precisely the shape of test the
    // review found wanting.
    let sealed = sealed_after - sealed_before;
    assert!(
        sealed > 0,
        "no read tool performed a durable write during this sweep, so `torn == 0` above is \
         vacuous. Either the fixture stopped exercising a writing read (`group_usage` merges \
         `usage.json`) or the seal stopped being reached — both make this row stop meaning \
         anything"
    );
}
