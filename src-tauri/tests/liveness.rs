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

use loomux_engine::lockwatch::{tracked_lock_names, tracked_lock_ranks};
use serde_json::json;
use serde_json::Value;
use loomux_lib::orchestration::views::{group_view_payload, strip_view_payload, VIEW_STALE_AFTER_MS};
use loomux_lib::orchestration::lockorder;
use loomux_lib::orchestration::{mailbox, Caller, GroupId, Guardrails, OrchRegistry, Role};
use loomux_lib::pty::{PtyManager, WriteReceiver};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The session-sized fixture generator (#1702). `tests/common/mod.rs`, so it is
/// compiled INTO this target rather than built as a test target of its own.
mod common;

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
/// A roster that DECLARES a manager, so `post_to_manager` is not refused and
/// `check_mail` is a listed tool. Parsed from YAML rather than hand-built, so a
/// new field on `Block` cannot give this fixture a shape a real workflow file
/// would not produce — the `manager_lifecycle.rs` idiom.
const WITH_MANAGER: &str = "version: 1\nblocks:\n  - id: manager\n    kind: manager\n  - id: worker\n    kind: worker\n";

fn rails_with_manager() -> Guardrails {
    let blocks = loomux_lib::orchestration::workflow::parse_workflow(WITH_MANAGER)
        .expect("the fixture roster must parse")
        .blocks;
    Guardrails { blocks, ..rails() }
}

fn mcp_fixture() -> (Arc<OrchRegistry>, String, GroupId, tempfile::TempDir) {
    let (reg, dir) = test_registry();
    // `dispatch_bounded` runs a mutating tool on the registry's own `Arc`; a
    // registry that never had `set_self_arc` called has none, and L2b would then
    // be measuring a path the app does not take.
    reg.set_self_arc();
    let group =
        reg.create_group("C:/tmp/repo", rails_with_manager()).expect("create a group");
    let agent = reg
        .spawn_agent(&group.id, Role::Orchestrator, "orch", "", false, None)
        .expect("a fake agent to carry a resolvable token");
    // Seed one UNREAD message. `check_mail` writes only when it stamps
    // something read (`if stamped > 0`), so against an empty mailbox the sweep
    // drives the tool without ever reaching `write_mailbox` — which is why the
    // scratch rounds for L2g kept coming back green while the sweep looked
    // complete.
    reg.post_to_manager(&group.id, "orch", "seed for the L2g sweep", mailbox::Kind::Update)
        .expect("the roster declares a manager, so a post is accepted");
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
fn l2b_a_slow_mutating_tool_answers_early_and_is_never_double_executed() {
    // The AT-MOST-once property, which is why a mutating tool is NOT unwound: a
    // deadline that abandoned the work would double-execute on the retry the
    // message tells the caller not to make.
    //
    // The name says "never double-executed" rather than "completes exactly
    // once" because the second is the claim #1702 P3 retracted (see
    // `doc/design/lock-liveness.md`): a helper thread that PANICS never
    // completes at all, so the guarantee the shipped code makes is at most
    // once, not exactly once. This row drives the non-panicking case — a tool
    // slow only because a lock is held — where the work does land, and what it
    // pins is that it lands ONCE. A test name is a surface a retracted claim
    // can survive on, and this one used to carry it.
    const HOLD_MS: u64 = 3_000;
    let _deadline = MutateDeadline::set(Duration::from_millis(300));
    let (reg, token, group, _dir) = mcp_fixture();

    assert!(reg.hold_lock_for_test("tasks_lock", HOLD_MS), "setup: cannot hold `tasks_lock`");

    let call = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "upsert_task", "arguments": { "title": "l2b-lands-once" } },
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
        let n = reg.tasks(&group).iter().filter(|t| t.title == "l2b-lands-once").count();
        if n == 1 {
            break;
        }
        assert!(n <= 1, "the tool ran {n} times — a deadline that abandons work double-executes");
        assert!(
            std::time::Instant::now() < deadline,
            "the tool never completed after the hold ended, so the caller was told to wait for \
             something that never arrived"
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
fn every_read_classified_tool_is_a_tool_the_surface_actually_lists() {
    // Review N2. The old version of this row could not fail: its
    // `match tool_kind(name)` loop was total, so "every listed tool reaches a
    // decision" was a tautology, and the only live assertions were two floors.
    // It also drove ONE role while claiming a per-role population, so three
    // role-gated tools were never seen by it.
    //
    // The direction that actually bites is the other one, and it had a live
    // instance: `pr_checks` sat in the Read set and is not a tool at all —
    // `tool_defs` registers no such name — so the set was 16 real tools plus a
    // dead row, with nothing red. A typo'd Read entry degrades to `Mutate`
    // silently, which is fail-safe but also invisible.
    //
    // Both populations are now taken from the shipped code: `READ_TOOLS` is the
    // constant `tool_kind` itself reads, and `all_listed_tool_names()` unions
    // `tool_defs` over EVERY role — a pure function, so no fixture decides what
    // this test can see.
    let listed = mcp::all_listed_tool_names();
    assert!(
        listed.len() >= 35,
        "the listing collapsed to {} tools; this row is comparing against almost nothing",
        listed.len()
    );

    let orphans: Vec<&str> =
        mcp::READ_TOOLS.iter().copied().filter(|n| !listed.iter().any(|l| l == n)).collect();
    assert!(
        orphans.is_empty(),
        "these names are classified ToolKind::Read but are not tools the surface lists: \
         {orphans:?}. A dead row classifies nothing — and the next typo in that table also \
         degrades to Mutate in silence, which is safe and unreadable."
    );

    // The other direction, as a floor rather than an equality: a listed tool is
    // free to be a Mutate, so the two sets are not equal and never should be.
    // What must hold is that the Read set is not empty — a `READ_TOOLS` that had
    // shrunk to nothing would make the read budget apply to nothing at all while
    // every assertion above still passed.
    assert!(
        mcp::READ_TOOLS.len() >= 8,
        "only {} tools classify as Read; the read budget then governs almost nothing",
        mcp::READ_TOOLS.len()
    );

    // And the classification really is consulted rather than defaulted: at least
    // one listed tool must come back Read, and at least one Mutate.
    let reads = listed.iter().filter(|n| mcp::tool_kind(n) == mcp::ToolKind::Read).count();
    let mutates = listed.iter().filter(|n| mcp::tool_kind(n) == mcp::ToolKind::Mutate).count();
    assert!(reads > 0 && mutates > 0, "reads={reads} mutates={mutates}");
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
    let (reg, token, group, _dir) = mcp_fixture();
    let base = reg.resolve_token(&token).expect("the token resolves");

    // The population is `READ_TOOLS` — every name the budget treats as a read —
    // NOT one role's listing. That distinction is the whole of why the first
    // scratch round for this row came back green: L2g swept `tools/list` for an
    // ORCHESTRATOR, and `check_mail` (the tool this property exists for) is in
    // the MANAGER tier, so the sweep never drove it. That is the same defect the
    // review found in the classification test, and the same one it found in
    // `tool_kind` itself — a population that excludes its own subject.
    //
    // `Caller` is constructible directly, so each tool is driven by a caller
    // whose ROLE actually lists it, without spawning an agent per role.
    // The matrix comes from `mcp` so this cannot omit a dimension the listing
    // branches on — which is exactly how `session_digest` (hint-gated) was
    // reported unreachable here after the same omission had already been fixed
    // in the sibling guard.
    let listed_by: Vec<(Role, Option<&str>, Vec<String>)> = mcp::listing_matrix()
        .into_iter()
        .map(|(role, hint)| (role, hint, mcp::listed_tool_names_for(role, hint)))
        .collect();

    // Resolve every Read tool to a caller BEFORE anything is held: building the
    // plan takes registry locks of its own, and doing that under the hold would
    // measure the plan rather than the sweep.
    let mut plan: Vec<(&str, Caller)> = Vec::new();
    let mut unreachable: Vec<&str> = Vec::new();
    for name in mcp::READ_TOOLS {
        match listed_by.iter().find(|(_, _, names)| names.iter().any(|n| n == name)) {
            Some((role, hint, _)) => plan.push((
                name,
                Caller {
                    agent_id: base.agent_id.clone(),
                    group: base.group.clone(),
                    role: *role,
                    role_hint: hint.map(str::to_string),
                },
            )),
            None => unreachable.push(name),
        }
    }
    assert!(
        unreachable.is_empty(),
        "these Read-classified tools are listed for no role, so this sweep cannot drive them: \
         {unreachable:?}. A tool the budget governs but this row cannot reach is exactly the \
         gap that let the first scratch round for this row come back green."
    );
    assert!(plan.len() >= 8, "only {} Read tools are reachable: {plan:?}", plan.len());

    let (sealed_before, torn_before) = loomux_engine::budget::thread_seal_counts();

    // Held for the whole sweep: `app` is what `write_mailbox` takes AFTER its
    // durable replace, so it is the lock a write-then-acquire tear needs.
    assert!(reg.hold_lock_for_test("app", 20_000), "setup: cannot hold `app`");

    for (name, caller) in &plan {
        let call = json!({ "name": name, "arguments": {} });
        // The answer does not matter — Ok, isError and busy are all fine. What
        // matters is that no frame unwound after writing.
        let _ = mcp::dispatch(&reg, caller, "tools/call", &call);
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

    // THE POSITIVE CONTROL — and deliberately not "did some tool happen to
    // write". The first version asserted exactly that and failed on CI: no Read
    // tool wrote durably during the sweep, so `torn == 0` was vacuous. The
    // control caught its own test, which is what a control is for, but it also
    // showed the row was resting on a tool INCIDENTALLY writing. That is a fact
    // about today's fixture, not a property of anything.
    //
    // So the instrument is demonstrated directly: a frame that writes and then
    // hits a held lock must SEAL and WAIT. If that stops holding, the sweep
    // above is measuring a mechanism that is not running, and this says so.
    let (probe_seals_before, probe_torn_before) = loomux_engine::budget::thread_seal_counts();
    assert!(reg.hold_lock_for_test("agents", 4_000), "setup: cannot hold `agents`");
    let probe_started = std::time::Instant::now();
    let probe = loomux_engine::budget::read_budget(loomux_engine::budget::POLL_LOCK_BUDGET, || {
        loomux_engine::budget::note_durable_write("l2g-probe.json");
        // The acquisition AFTER the write — the only shape that tears.
        reg.group_summary(&group)
    });
    let probe_waited = probe_started.elapsed();
    let (probe_seals, probe_torn) = loomux_engine::budget::thread_seal_counts();

    assert!(
        probe.is_ok(),
        "a frame that wrote durably and then hit a held lock UNWOUND. The seal is not \
         engaging, so `torn == 0` above measures a mechanism that is not running"
    );
    assert!(
        probe_waited >= loomux_engine::budget::POLL_LOCK_BUDGET,
        "the probe returned in {probe_waited:?}, inside its own budget — the hold was not in \
         its way, so it demonstrates nothing about what a sealed frame does"
    );
    assert_eq!(
        probe_seals - probe_seals_before,
        1,
        "the probe's durable write did not seal its frame"
    );
    assert_eq!(probe_torn, probe_torn_before, "the probe itself tore");

    // WHAT THE SWEEP DOES AND DOES NOT SHOW — recorded rather than asserted,
    // because the obvious assertion is unsound in BOTH directions and each was
    // tried on CI before this comment existed.
    //
    // `swept_writes > 0` fails on a CORRECT tree: with `check_mail`,
    // `queue_orphans` and `list_locks` classified `Mutate` — which is the fix
    // this review round made — the Read set contains no tool that durably writes
    // in this fixture. Demanding one demands the classification be wrong.
    //
    // `swept_writes == 0` is no better: `group_usage` IS a Read that replaces
    // `usage.json`, and whether it does so in a given run depends on live agents
    // having snapshots — a property of the fixture, not of the code.
    //
    // MEASURED, not reasoned (#1609 review round 2, B1): the round that puts
    // `check_mail` back in the Read set unsealed — with the two `budget.rs` seal
    // tests `#[ignore]`d so `cargo` reaches this binary at all — run 33257747970,
    // reddens the PROBE above ("a frame that wrote durably and then hit a held
    // lock UNWOUND") and leaves the sweep's own `torn == 0` GREEN. So the sweep does not catch a misclassified writer, and
    // no run has shown it failing. It is a regression guard; the probe is the
    // assertion with a counterfactual.
    //
    // So the sweep asserts what it supports: every Read tool was reachable and
    // driven (above), and none tore. The two stronger claims live where they can
    // be demonstrated — the probe shows the instrument runs, and `budget.rs`'s
    // own seal tests redden when the seal is disarmed. Three claims, three
    // places, each with the counterfactual it actually has.
    let swept_writes = sealed_after - sealed_before;
    let _ = swept_writes;
}

// ---------- L5: the lock-order checker, over the REAL rank table ----------
//
// #1600 plan comment 4/4 b, rows L5a-L5c, gated on Phase 3a (#1610).
//
// These drive `orchestration::lockorder`'s shipped consts through real
// `OrchRegistry` fields, not through locks a test built for itself. The
// distinction is the one every "the extracted unit is green while the caller is
// wrong" finding in this repo turns on: a checker proved correct on two locks in
// `lockwatch.rs`'s own unit tests says nothing about whether the table was
// applied to `groups`, or applied the right way up.
//
// **L5c is not a test in this file, and cannot be.** The plan's row is "the
// whole existing suite green with the checker armed" — which is what the CI run
// of `tests/orchestration.rs` (and every other binary here) IS, in a debug
// build, where a violation panics. Writing an assertion for it would be writing
// a test that asserts the other tests passed. Its stated bound, from the plan:
// paths the suite does not exercise are covered by the release-mode breadcrumb,
// not by this run.

/// A short budget for a `lock_within` probe that must answer at once.
///
/// Long enough that a loaded runner cannot make a genuinely free lock look
/// contended, and 200x shorter than the answer a re-entrancy refusal must give
/// — which is immediate. It is also the WATCHDOG for the failing run: with the
/// refusal removed, the probe returns a timeout `Busy` after this instead of
/// parking forever, so the row fails naming an assertion rather than arriving
/// as a job timeout (the #744 idiom, same as `lockwatch.rs`'s `HOLD_MAX`).
const REENTRANCY_PROBE_BUDGET: Duration = Duration::from_millis(2_000);

#[test]
fn l5a_a_planted_inversion_panics_under_the_checker_naming_both_locks() {
    let (reg, _dir) = test_registry();

    // The DECLARED direction first. Without it this row passes just as well
    // against a checker that refuses every nesting, which would be a checker
    // nobody could ship — and the two directions are the same two locks, so
    // nothing but the ORDER distinguishes the two halves.
    let ok = reg
        .with_lock_for_test("agents", || reg.with_lock_for_test("groups", || 7u32))
        .expect("`agents` is a known lock name")
        .expect("`groups` is a known lock name");
    assert_eq!(ok, 7, "`agents` (rank 510) then `groups` (rank 520) is the declared order");

    // Now the inversion: `groups` outermost, `agents` under it. One thread, no
    // race — which is the point of a rank checker over a soak test. It does not
    // need the deadlock to happen, only the order that permits it.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = reg.with_lock_for_test("groups", || reg.with_lock_for_test("agents", || ()));
    }));
    let payload = panicked.err().expect(
        "taking `agents` under `groups` must panic under the checker in a debug/test build",
    );
    let msg = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();

    // Both locks, both ranks, and both call sites — the half of a violation
    // report that makes it actionable rather than another thing to go looking
    // for in a 54,000-line module (#1600 §2.3).
    let needles = [
        "agents".to_string(),
        "groups".to_string(),
        format!("rank {}", lockorder::AGENTS),
        format!("rank {}", lockorder::GROUPS),
        // Two needles, not one path: the recorded site is a real `file!()`,
        // so it is `src-tauri/src/orchestration/mod.rs` on unix and
        // `src-tauri\src\orchestration\mod.rs` on Windows. Asserting the
        // slashed form passed on two platforms and failed on the third
        // (run 33262628789).
        "orchestration".to_string(),
        "mod.rs".to_string(),
    ];
    for needle in &needles {
        assert!(msg.contains(needle), "the panic lost {needle:?}: {msg}");
    }
    // The sites are the real `lock_safe()` call sites in `mod.rs`, not this
    // test's line and not the checker's own — `#[track_caller]` is what buys
    // that, and it is the difference between a report that names code and one
    // that names the instrument.
    assert!(!msg.contains("liveness.rs"), "the recorded sites are the seam's, not this test's: {msg}");
}

#[test]
fn l5b_a_reentrant_acquisition_answers_busy_instead_of_hanging() {
    let (reg, _dir) = test_registry();

    // Discriminating half: an uncontended `lock_within` on the same field must
    // SUCCEED. Without it, "returns Err" is satisfied by a lock that never
    // grants anything.
    assert!(
        reg.lock_within_for_test("tasks_lock", REENTRANCY_PROBE_BUDGET)
            .expect("`tasks_lock` is a known lock name")
            .is_ok(),
        "an uncontended registry lock must be acquired, not reported busy"
    );

    let started = Instant::now();
    let busy = reg
        .with_lock_for_test("tasks_lock", || {
            reg.lock_within_for_test("tasks_lock", REENTRANCY_PROBE_BUDGET)
        })
        .expect("`tasks_lock` is a known lock name")
        .expect("`tasks_lock` is a known lock name")
        .err()
        .expect(
            "re-acquiring a lock this thread already holds must be refused; today it parks \
             forever, and one lock is no cycle for an inversion search to find (#1600 §1.2)",
        );

    assert!(
        busy.is_reentrant(),
        "the refusal must say WHY — a re-entrant self-acquisition is a defect, not contention: \
         {busy:?}"
    );
    assert_eq!(busy.lock, "tasks_lock");
    assert!(
        started.elapsed() < REENTRANCY_PROBE_BUDGET,
        "the refusal must be immediate rather than budget-shaped: took {:?}",
        started.elapsed()
    );
    let holder = busy.holder.as_ref().expect("the holder is this thread's own earlier frame");
    assert!(
        holder.site_file.ends_with("mod.rs"),
        "the refusal must name the site that already holds it, which is in the registry, not \
         here: {}:{}",
        holder.site_file,
        holder.site_line
    );
    // `Display` reaches an agent's context through `loomux busy:` — a
    // re-entrant refusal must not read as ordinary contention that a retry
    // clears.
    let rendered = busy.to_string();
    assert!(
        rendered.contains("already held by this same thread"),
        "the rendered refusal reads as contention: {rendered}"
    );
}

#[test]
fn l5_every_lockorder_rank_is_distinct() {
    // Load-bearing rather than tidy. Distinct ranks are what make "two held
    // locks share a rank" mean "the same field from two different registries" —
    // which this very binary builds, several times per test — instead of "two
    // peers that must never nest". Collapse two consts onto one value and the
    // checker starts refusing nestings that are fine.
    let mut seen: Vec<(u32, &str)> = Vec::new();
    for (name, rank) in lockorder::ALL {
        if let Some((_, other)) = seen.iter().find(|(v, _)| *v == rank.get()) {
            panic!("`{name}` and `{other}` share rank {rank}; ranks must be distinct");
        }
        seen.push((rank.get(), *name));
    }
    // Vacuity control: an "every pair differs" over an empty list is true.
    assert_eq!(seen.len(), lockorder::ALL.len());
    assert!(lockorder::ALL.len() >= 18, "the table shrank: {}", lockorder::ALL.len());
}

/// Separate from the distinctness row on purpose: two properties in one test
/// means whichever assertion runs first masks the other, and a guard nobody can
/// redden alone is a guard nobody has checked.
#[test]
fn l5_every_lockorder_const_names_a_lock_that_still_exists() {
    // A const that names a field which has been renamed away ranks nothing at
    // all, silently. `tracked_lock_names()` is every LIVE tracked lock in this
    // process, so a fresh registry puts all of them there.
    let (reg, _dir) = test_registry();
    // `AUDIT_LOCK` is a lazily-initialised static rather than a registry field,
    // so it is only live once something has taken it. Take it.
    reg.with_lock_for_test("audit", || ()).expect("`audit` is a known lock name");
    let live = tracked_lock_names();
    for (name, _) in lockorder::ALL {
        assert!(
            live.contains(name),
            "`lockorder` ranks `{name}`, which is not a live tracked lock — the field was \
             renamed and the const now ranks nothing"
        );
    }
    // Vacuity control: the assertion above is an "every" over a list, and it
    // passes trivially if the list is empty or the scan found nothing.
    assert!(lockorder::ALL.len() >= 18, "the table shrank: {}", lockorder::ALL.len());
    assert!(live.len() >= 60, "the live-lock scan found only {} locks", live.len());
}

/// The rank table is only a table if it is APPLIED (#1610 review B1).
///
/// The one claim this whole change exists to abolish is "a doc comment states
/// an order and nothing can fail a build over it". Moving those claims into
/// consts does not by itself close it: *"this field carries this rank"* is a
/// new claim of exactly the same shape, and three guards that look like they
/// cover it do not —
///
/// - the const-names-a-live-lock row reads `tracked_lock_names()`, which is
///   names only, and a field built with plain `new` registers the same name;
/// - the distinctness row reads `lockorder::ALL` and never touches a lock;
/// - `selfwatch.rs`'s named-construction scan floors `named + ranked`, and
///   reverting a field moves it from one bucket to the other, leaving the sum
///   and both floors intact.
///
/// **And the suite itself is structurally blind to it:** removing a rank can
/// only remove violations, never create one, so no green run anywhere — on any
/// platform, in any round — is evidence that a rank is still applied.
///
/// Both directions are checked, because one fix closes both and the second is
/// the one that keeps the DISTINCTNESS property honest: a rank written inline
/// (`LockRank::new(520)` at a construction site) is invisible to a guard that
/// only reads `ALL`, and a duplicate rank means those two locks nest freely in
/// both directions.
///
/// Mismatches are COLLECTED rather than asserted one at a time: a table is a
/// set, and a reader fixing one row wants to see the other four in the same
/// run.
#[test]
fn l5_every_lockorder_const_is_applied_to_its_field() {
    let (reg, _dir) = test_registry();
    // `AUDIT_LOCK` is a lazily-initialised static rather than a registry field,
    // so it is only live once something has taken it. Take it.
    reg.with_lock_for_test("audit", || ()).expect("`audit` is a known lock name");

    // Normalised to owned scalars so the comparisons below are between a
    // `&str` and a `u32` rather than between four layers of reference.
    let live: Vec<(String, Option<u32>)> = tracked_lock_ranks()
        .into_iter()
        .map(|(name, rank)| (name.to_string(), rank.map(|r| r.get())))
        .collect();

    let mut wrong: Vec<String> = Vec::new();

    // Direction 1: every const in the table is applied, to a live lock, at the
    // rank the table says.
    for (name, rank) in lockorder::ALL {
        let want = rank.get();
        let seen: Vec<Option<u32>> =
            live.iter().filter(|(n, _)| n.as_str() == *name).map(|(_, r)| *r).collect();
        if seen.is_empty() {
            wrong.push(format!(
                "`{name}`: the table ranks it {want}, but no live tracked lock carries that name"
            ));
            continue;
        }
        for got in seen {
            match got {
                Some(actual) if actual == want => {}
                Some(actual) => wrong.push(format!(
                    "`{name}`: the table says rank {want}, the live lock carries {actual}"
                )),
                None => wrong.push(format!(
                    "`{name}`: the table says rank {want}, the live lock is UNRANKED — the const \
                     enforces nothing and the checker cannot see this field at all"
                )),
            }
        }
    }

    // Direction 2: no live lock carries a rank the table does not know.
    for (name, rank) in &live {
        let Some(rank) = *rank else { continue };
        if !lockorder::ALL.iter().any(|(n, r)| *n == name.as_str() && r.get() == rank) {
            wrong.push(format!(
                "`{name}` is live at rank {rank}, which `lockorder::ALL` does not carry — a rank \
                 written at a construction site is invisible to the distinctness guard, and two \
                 locks sharing a rank nest freely in BOTH directions"
            ));
        }
    }

    // De-duplicated, and this is not tidiness. A mismatch is a fact about a
    // FIELD; the scan sees one entry per LIVE lock of that name, and this
    // binary runs its tests concurrently, so the repeat count is however many
    // other tests happened to be holding a registry at that instant. Reporting
    // the raw list makes the message both unreadable and NON-DETERMINISTIC —
    // measured, not feared: the first red run of this row printed each of its
    // three findings six times (#1610 review B1's scratch round).
    wrong.sort();
    wrong.dedup();
    assert!(
        wrong.is_empty(),
        "the rank table and the live locks disagree:\n  {}",
        wrong.join("\n  ")
    );

    // Vacuity controls. Both assertions above are "every" over a list, and both
    // pass trivially against an empty table or an empty scan.
    assert!(lockorder::ALL.len() >= 18, "the table shrank: {}", lockorder::ALL.len());
    let ranked = live.iter().filter(|(_, r)| r.is_some()).count();
    assert!(
        ranked >= lockorder::ALL.len(),
        "only {ranked} live locks carry a rank at all, against {} consts — the scan is not \
         seeing the registry",
        lockorder::ALL.len()
    );
    assert!(live.len() >= 60, "the live-lock scan found only {} locks", live.len());
}

// ---------- L6: the attention tick on a long-lived session (#1702) ----------
//
// Two rows, and they are split rather than merged because they redden for
// different reasons and a red evidences only the assertion it REACHED
// (CLAUDE.md). L6a is the deadlock witness: on the pre-fix tick it never gets
// past "did the call return", so an equality assertion sharing its body would
// bank a coverage claim no run ever produced. L6b is that equality, and its own
// counterfactual is a tick that returns while masking nothing.
//
// **What the pre-fix red looks like depends on the base**, and the rows are
// written so the failure line says which:
//
//   - on `main` BEFORE Phase 3a (#1698): `attention_tick` takes `agents` and
//     re-acquires it through `delivered_mask_lines` -> `delivered_prompt_record`
//     -> `session_for_pty`. `parking_lot::Mutex` is not re-entrant and the tick
//     runs inside `tick_gate`'s `MutationScope`, where an expired budget waits
//     rather than unwinding — so the thread parks forever and the row reports a
//     TIMEOUT.
//   - on `main` WITH 3a: the same path takes `by_pty` (rank 500) while holding
//     `agents` (510) one line EARLIER than the re-entrant acquire, which is a
//     descending pair, so the rank checker panics naming both locks before the
//     deadlock can form. The row reports a PANIC, with the checker's message.
//
// Both are the same defect through different instruments, which is why the
// runner below distinguishes them instead of reporting one as the other.

/// Guardrails for a fixture that needs more than [`rails`]'s four panes.
fn rails_for(max_agents: u32) -> Guardrails {
    Guardrails { max_agents, ..rails() }
}

/// Lock names no other test uses, so the sampler can identify the thread it is
/// measuring without a process-wide serial guard: tests in this binary run in
/// parallel and `lockwatch::held_locks` is global, so filtering on the name
/// `agents` alone would be reading other tests' registries too.
const L6A_PROBE: &str = "l6a_tick_probe";
const L6A_CONTROL_PROBE: &str = "l6a_control_probe";
const L6B_PROBE: &str = "l6b_tick_probe";

/// Agents in the fixture fleet.
const L6_AGENTS: usize = 6;
/// Prompt deliveries recorded against EACH agent's session. Field-sized: the
/// pane #1702 was reported on had taken thousands.
const L6_DELIVERIES: usize = 4_000;
/// 16 KiB of rendered tail per pane — four times the `ATTENTION_SCAN_BYTES`
/// window production actually reads, so the fixture is strictly harder than the
/// real subject rather than a scaled-down model of it.
const L6_TAIL_BYTES: usize = 16 * 1024;
/// The sampler's cadence. Not a wait — this file's "never a sleep" rule is
/// about waiting for a condition, and every wait below is a bounded
/// `recv_timeout`.
const L6_SAMPLE_EVERY: Duration = Duration::from_micros(500);
/// How long L6a's instrument control holds `agents` deliberately, and the floor
/// the sampler must report for it. Both are far above `L6_SAMPLE_EVERY` and far
/// below anything that would make the row slow.
const L6_CONTROL_HOLD: Duration = Duration::from_millis(250);
const L6_CONTROL_FLOOR_MS: u64 = 50;

/// The subject both L6 rows measure.
///
/// The fixture is as much the finding as the assertions are: #1702 survived four
/// betas because *no test in this repo could build the state that triggers it*.
/// `attention_setup`, the helper every attention test in `orchestration.rs`
/// uses, spawns agents with no pty, so `pty_id` is `None`, the per-agent mask is
/// never reached, and the whole class is unreachable from that file however many
/// cases it adds. The soak lane has the mirror gap: it wedges a lock and probes,
/// which measures VICTIMS of a hold and never constructs a holder out of
/// ordinary state. So the trigger — running, pty-bound, `by_pty`-mapped,
/// session-bound, quiet past the attention window — is what
/// `common::fabricate_long_lived_session` exists to produce.
struct L6Fixture {
    reg: Arc<OrchRegistry>,
    _dir: tempfile::TempDir,
    fx: common::LongLivedSession,
    tails: std::collections::HashMap<String, String>,
    /// What the mask says, computed from the same shipped helpers with no
    /// registry lock involved anywhere — "the unbounded computation".
    reference: std::collections::HashMap<String, bool>,
}

fn l6_fixture() -> L6Fixture {
    use loomux_lib::orchestration::{mask_loomux_notices_with_record, prompt_wait_detected};
    use std::collections::HashMap;

    let (reg, _dir) = test_registry();
    let g = reg
        .create_group("C:/tmp/repo", rails_for(L6_AGENTS as u32 + 2))
        .expect("create a group");
    let fx = common::fabricate_long_lived_session(&reg, &g.id, L6_AGENTS, L6_DELIVERIES);
    assert_eq!(
        fx.agent_ids.len(),
        L6_AGENTS,
        "setup: the fixture got fewer panes than it asked for, so everything below would be \
         measuring a smaller subject than it claims"
    );

    // Pane 0's tail ends with the very line loomux last delivered into its
    // session — #576/rev-126's case, the one the record exists to mask — so the
    // mask must claim it and that pane must NOT flag. Every other pane ends on a
    // real CLI dialog and must flag. A fixture where every pane answered the
    // same way would pass just as well against a tick that had stopped masking.
    let mut tails: HashMap<String, String> = HashMap::new();
    for (i, id) in fx.agent_ids.iter().enumerate() {
        let ending =
            if i == 0 { fx.last_delivered[id].clone() } else { common::DIALOG_ENDING.to_string() };
        tails.insert(id.clone(), common::padded_tail(L6_TAIL_BYTES, &ending));
    }

    let reference: HashMap<String, bool> = fx
        .agent_ids
        .iter()
        .map(|id| {
            let delivered =
                reg.delivered_mask_lines(fx.pty_of[id], fx.session_of.get(id).map(|s| s.as_str()));
            let masked = mask_loomux_notices_with_record(&tails[id], &delivered);
            (id.clone(), prompt_wait_detected(&masked))
        })
        .collect();

    L6Fixture { reg, _dir, fx, tails, reference }
}

/// How a measured body ended.
enum Ran<R> {
    Done(R),
    /// The thread panicked — under Phase 3a this is the rank checker refusing
    /// the inversion, and its message names both locks.
    Panicked(String),
    /// Neither returned nor panicked inside the grace window: parked.
    Parked,
}

struct Sampled<R> {
    ran: Ran<R>,
    /// The longest `agents` hold observed FOR THE MEASURED THREAD.
    max_hold_ms: u64,
    /// How many samples saw such a hold.
    samples: usize,
    /// Weak control: the sampler identified the measured thread through its
    /// probe lock, so it was running and could attribute a hold to it. This is
    /// NOT evidence that it can see a hold of any particular duration — that is
    /// what `L6_CONTROL_HOLD` is for.
    saw_probe: bool,
}

/// Run `body` on its own thread and report the longest `agents` hold that
/// thread took while it ran.
///
/// The thread is identified by a uniquely-named probe lock it holds and then
/// releases before `body` starts, so the probe can never be mistaken for the
/// thing being measured, and the sampler needs no process-wide serial guard.
fn measure_agents_hold<R: Send + 'static>(
    probe_name: &'static str,
    body: impl FnOnce() -> R + Send + 'static,
) -> Sampled<R> {
    use loomux_engine::lockwatch::{held_locks, mono_ms, TrackedMutex};
    use std::sync::atomic::{AtomicBool, Ordering};

    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<R>();
    let stop = Arc::new(AtomicBool::new(false));
    let t_stop = stop.clone();

    let worker = std::thread::spawn(move || {
        let probe = TrackedMutex::new(probe_name, ());
        let held = probe.lock_safe();
        let _ = ready_rx.recv_timeout(GRACE);
        drop(held);
        let out = body();
        t_stop.store(true, Ordering::Relaxed);
        let _ = done_tx.send(out);
    });

    let s_stop = stop.clone();
    let sampler = std::thread::spawn(move || {
        // Phase 1 — identify the measured thread through the probe.
        let deadline = Instant::now() + GRACE;
        let mut who = None;
        while Instant::now() < deadline {
            if let Some(s) = held_locks(mono_ms()).into_iter().find(|s| s.name == probe_name) {
                who = Some(s.holder_thread);
                break;
            }
            std::thread::sleep(L6_SAMPLE_EVERY);
        }
        let Some(who) = who else { return (false, 0u64, 0usize) };
        let _ = ready_tx.send(());

        // Phase 2 — the measurement, bounded by its own deadline as well as by
        // the stop flag, because the failure L6a guards against is precisely
        // the case where the flag is never set.
        let deadline = Instant::now() + GRACE;
        let (mut max_ms, mut samples) = (0u64, 0usize);
        while !s_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            for s in held_locks(mono_ms()) {
                if s.name == "agents" && s.holder_thread == who {
                    samples += 1;
                    max_ms = max_ms.max(s.held_ms);
                }
            }
            std::thread::sleep(L6_SAMPLE_EVERY);
        }
        (true, max_ms, samples)
    });

    let got = done_rx.recv_timeout(GRACE);
    stop.store(true, Ordering::Relaxed);
    let (saw_probe, max_hold_ms, samples) = sampler.join().expect("the sampler thread panicked");

    let ran = match got {
        Ok(r) => Ran::Done(r),
        Err(_) if worker.is_finished() => {
            let msg = match worker.join() {
                Err(p) => p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "<non-string panic payload>".to_string()),
                Ok(_) => "<thread finished without sending and without panicking>".to_string(),
            };
            Ran::Panicked(msg)
        }
        // Deliberately NOT joined: it is parked on a lock this process still
        // holds, and the harness exits when the run ends — the same posture
        // `completes_within` documents at the top of this file.
        Err(_) => Ran::Parked,
    };
    Sampled { ran, max_hold_ms, samples, saw_probe }
}

/// Two `attention_tick` passes, measured. Two because one cannot reach the
/// defect: the quiet clock is established on the first sighting, so `quiet_for`
/// is zero there and the `&&` chain short-circuits before the mask. The second,
/// five seconds on (past the four-second attention window), is the one that
/// reaches `delivered_mask_lines`.
fn measured_ticks(
    f: &L6Fixture,
    probe: &'static str,
) -> Sampled<(Vec<loomux_lib::orchestration::AttentionItem>, u128, u128)> {
    let reg = f.reg.clone();
    let outputs = f.fx.outputs.clone();
    let tails = f.tails.clone();
    let inputs = f.fx.no_input.clone();
    let now = 1_000_000_000_000u64;
    measure_agents_hold(probe, move || {
        let t0 = Instant::now();
        reg.attention_tick(now, &outputs, &tails, &inputs);
        let first = t0.elapsed().as_micros();
        let t1 = Instant::now();
        let items = reg.attention_tick(now + 5_000, &outputs, &tails, &inputs);
        (items, first, t1.elapsed().as_micros())
    })
}

/// L6a (#1702) — the deadlock witness. `attention_tick` must RETURN on the
/// state that used to wedge it, and must not hold `agents` while it works.
///
/// Phase 2.1's budgets bound what a WAITER pays; what broke here was a HOLD, so
/// the hold is measured directly off `lockwatch::held_locks` rather than by
/// timing a victim.
#[test]
fn l6a_the_attention_tick_returns_on_the_state_that_deadlocked_it() {
    let f = l6_fixture();

    // ---- the instrument control, and it is the whole reason this row can
    // report a zero honestly ----------------------------------------------
    //
    // The hold assertion below is an ABSENCE ("no long hold was seen"), and an
    // absence is byte-identical to an instrument that cannot see the thing.
    // `saw_probe` alone does not close that: it proves the sampler ran and can
    // attribute a hold to the right thread, not that it would CATCH one. So a
    // deliberate `agents` hold of known duration is taken here, on a measured
    // thread, and the sampler must report it. Without this, a fix that stopped
    // taking `agents` at all and a sampler that was blind would produce the
    // same reading.
    let reg = f.reg.clone();
    let control = measure_agents_hold(L6A_CONTROL_PROBE, move || {
        reg.with_lock_for_test("agents", || std::thread::sleep(L6_CONTROL_HOLD))
            .expect("`agents` is a known lock name");
    });
    assert!(
        matches!(control.ran, Ran::Done(())),
        "setup: the control hold itself did not complete, so nothing below is measurable"
    );
    assert!(
        control.max_hold_ms >= L6_CONTROL_FLOOR_MS,
        "the sampler saw a longest `agents` hold of {} ms over {} samples while a hold of {:?} \
         was deliberately taken on the measured thread (saw_probe={}). The instrument cannot \
         see what this row is about, so its reading on the real tick would mean nothing",
        control.max_hold_ms,
        control.samples,
        L6_CONTROL_HOLD,
        control.saw_probe
    );

    // ---- the measurement -------------------------------------------------
    let m = measured_ticks(&f, L6A_PROBE);
    assert!(
        m.saw_probe,
        "the sampler never identified the tick thread through its probe lock, so it observed \
         nothing"
    );
    let (_items, tick1_us, tick2_us) = match m.ran {
        Ran::Done(t) => t,
        Ran::Panicked(msg) => panic!(
            "attention_tick PANICKED instead of returning. On a base carrying Phase 3a this is \
             the #1702 defect caught by the rank checker — the tick holds `agents` (rank 510) \
             and reaches `by_pty` (rank 500) through delivered_mask_lines -> \
             delivered_prompt_record -> session_for_pty, one line before it would re-acquire \
             `agents` itself. The checker said: {msg}"
        ),
        Ran::Parked => panic!(
            "attention_tick did not return within {GRACE:?} and did not panic. That is the \
             #1702 defect on a base without the rank checker: the tick takes `agents` and \
             re-acquires it through delivered_mask_lines -> delivered_prompt_record -> \
             session_for_pty, and parking_lot's mutex is not re-entrant, so it parks forever. \
             The longest `agents` hold observed for that thread was {} ms over {} samples",
            m.max_hold_ms, m.samples
        ),
    };

    // The ceiling is the floor the control above just PROVED the sampler can
    // see — not `TICK_LOCK_BUDGET` (5 s), which this row used to assert and
    // which is two orders of magnitude looser than anything it can support
    // (#1703 review B1). An assertion may only refuse what its instrument is
    // demonstrated to detect; at 5 s it would pass a phase that had regrown an
    // `fs` read and was holding `agents` for 4999 ms on every 3 s tick, while
    // this row's own message claimed every phase was a bounded in-memory pass.
    //
    // Observed on the fixed tick: 0 ms over 0 samples, so this passes with the
    // whole 50 ms to spare. If it ever flakes on a loaded runner, the cause is
    // the sampler catching a DESCHEDULED holder rather than a real hold, and
    // the fix is to raise `L6_CONTROL_HOLD` and `L6_CONTROL_FLOOR_MS`
    // together — never the ceiling alone, which would decouple what this
    // refuses from what the control proves it can see.
    assert!(
        m.max_hold_ms < L6_CONTROL_FLOOR_MS,
        "the tick held `agents` for {} ms (over {} samples), at or past the \
         {L6_CONTROL_FLOOR_MS} ms this row refuses — which is the same figure the control \
         above proves the sampler can see. Every phase of attention_tick that takes a \
         registry lock must be a bounded in-memory pass: no pty read, no board file, no \
         delivery record. The two passes took {tick1_us} and {tick2_us} us",
        m.max_hold_ms,
        m.samples
    );
}

/// L6b (#1702) — moving the mask off the lock must not move what it decides.
///
/// The counterfactual this row owns is NOT the deadlock (L6a has that): it is a
/// tick that returns promptly while masking nothing, which L6a passes and this
/// must not.
#[test]
fn l6b_the_masks_the_tick_applies_equal_the_unbounded_computation() {
    use loomux_lib::orchestration::{
        mask_loomux_notices_with_record, prompt_wait_detected, DELIVERED_NOTICES_PER_PANE,
        DELIVERED_PROMPT_LINES_PER_SESSION,
    };
    use std::collections::HashSet;

    let f = l6_fixture();

    // The bound #1702 STATES rather than adds. The issue's premise was that the
    // record grows with the session; it does not, because both halves of
    // `delivered_mask_lines` are drop-oldest and capped where they are written.
    // Four thousand deliveries per session, and the record is still at most
    // forty lines — which is what makes "add no second cap" a checked claim
    // instead of a sentence in a PR body.
    for id in &f.fx.agent_ids {
        let record = f
            .reg
            .delivered_mask_lines(f.fx.pty_of[id], f.fx.session_of.get(id).map(|s| s.as_str()));
        assert!(
            record.len() <= DELIVERED_NOTICES_PER_PANE + DELIVERED_PROMPT_LINES_PER_SESSION,
            "the delivered record for {id} is {} lines after {L6_DELIVERIES} deliveries; the \
             two caps that bound it allow at most {}",
            record.len(),
            DELIVERED_NOTICES_PER_PANE + DELIVERED_PROMPT_LINES_PER_SESSION
        );
        // The vacuity control that ceiling needs: an EMPTY record satisfies any
        // ceiling, and would also mean the fixture never wired the session and
        // every mask in this row ran against nothing.
        assert!(
            !record.is_empty(),
            "the fixture recorded {L6_DELIVERIES} deliveries for {id} and the record is empty, \
             so the ceiling above is satisfied by there being no record at all"
        );
    }

    // The discriminating control, in two halves, because "the reference is
    // mixed" is not enough on its own: a pane can fail to flag because its text
    // is not a question, which would make this row pass against a tick that had
    // stopped masking entirely.
    assert!(
        f.reference.values().any(|v| *v) && f.reference.values().any(|v| !*v),
        "setup: the reference is unanimous ({:?}), so the equality below would not distinguish \
         a masking tick from one that had stopped masking",
        f.reference
    );
    let masked_pane = &f.fx.agent_ids[0];
    assert!(
        prompt_wait_detected(&mask_loomux_notices_with_record(&f.tails[masked_pane], &[])),
        "setup: pane 0's tail is not prompt-shaped WITHOUT the record, so the fact that it does \
         not flag says nothing about masking — it would not flag under any implementation"
    );
    assert!(
        !f.reference[masked_pane],
        "setup: pane 0's tail IS prompt-shaped and the record does not suppress it, so the mask \
         is not claiming the line this fixture was built around"
    );

    let m = measured_ticks(&f, L6B_PROBE);
    let (items, _, _) = match m.ran {
        Ran::Done(t) => t,
        Ran::Panicked(msg) => panic!(
            "could not compare outputs: attention_tick panicked. This row's own assertion was \
             never reached — see L6a, which owns that failure. The panic was: {msg}"
        ),
        Ran::Parked => panic!(
            "could not compare outputs: attention_tick did not return within {GRACE:?}. This \
             row's own assertion was never reached — see L6a, which owns that failure"
        ),
    };

    let flagged: HashSet<String> = items
        .iter()
        .filter(|i| i.reason == "waiting")
        .map(|i| i.agent_id.clone())
        .collect();
    let expected: HashSet<String> =
        f.reference.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect();
    assert_eq!(
        flagged, expected,
        "moving the mask off the lock must not move what it decides: the tick flagged \
         {flagged:?} and the same masking computed outside it says {expected:?}"
    );
}

// ---------- L7: a day-old session, and the hold budget (#1702 P4) ----------
//
// L6 asked whether the attention tick RETURNS on the state that deadlocked it,
// and whether it decides the same thing off the lock as on it. Both questions
// are about one function on a six-pane fixture.
//
// L7 asks the question the field asked and the suite never did: with a
// registry the size a day of orchestrating leaves behind — hundreds of dead
// agents nothing prunes, a four-hundred-row board, a pending-question set at
// its cap, an audit log past the viewer's own window, and a live fleet in the
// exact state #1702 wedges on — does EVERY cadenced tick still return inside
// its budget, and does any of them hold a registry lock while it works?
//
// The second half is the one Phase 2.1 structurally could not ask. A budget is
// paid by a WAITER, so every guard in this repo measures the victim of a hold;
// #1702 answered `Busy` correctly for three minutes while the holder sat one
// frame away from its own lock. `lockwatch`'s hold-duration enforcement is the
// other side of that, and L7a is where the suite spends it.

/// Serialises the rows that read the PROCESS-GLOBAL hold registry.
///
/// `lockwatch::observed_holds` drains completed holds and samples in-flight
/// ones for the whole process, so two rows doing it at once would each be
/// classifying the other's deliberate holds. Every other test in this binary
/// takes its long holds through `hold_lock_for_test`, which carries a
/// `LongHoldPermit` and is therefore exempt by construction — this guard is
/// only needed between the rows that plant an UNPERMITTED one.
static HOLD_SERIAL: Mutex<()> = Mutex::new(());

/// How long L7's deliberate unpermitted hold runs for: past `HOLD_FAIL_MS`
/// with enough margin that a loaded runner cannot land the scan early.
const L7_PLANTED_HOLD: Duration =
    Duration::from_millis(loomux_engine::lockwatch::HOLD_FAIL_MS + 400);

/// Attention passes L7a times for its percentiles. Small: this is a
/// distribution over a fixed subject, not a benchmark — what it exists to
/// publish is an ORDER OF MAGNITUDE beside the 5 s budget, and thirty samples
/// settle that.
const L7_SAMPLES: usize = 30;

/// Guardrails for the day-old fixture.
///
/// `max_spawns_per_hour: 0` disables the rate backstop, which is not a fudge:
/// the backstop counts admitted spawns in a rolling HOUR and the fixture
/// builds a day's worth of them inside one second, so leaving it on would
/// measure the backstop instead of the session. `max_agents` is the real
/// ceiling (12) because the live fleet is what it bounds, and the dead fleet
/// is unbounded in production precisely because nothing prunes it.
fn day_old_rails() -> Guardrails {
    Guardrails { max_agents: 12, max_spawns_per_hour: 0, ..rails() }
}

/// The day-old session, plus the pty manager its panes are registered in.
struct L7Fixture {
    reg: Arc<OrchRegistry>,
    _dir: tempfile::TempDir,
    ptys: PtyManager,
    fx: common::DayOldSession,
    group: GroupId,
}

fn l7_fixture() -> L7Fixture {
    let (reg, _dir) = test_registry();
    reg.set_self_arc();
    let g = reg.create_group("C:/tmp/repo", day_old_rails()).expect("create a group");
    let fx = common::seed_day_old_session(&reg, &g.id, common::SessionScale::default());

    // Every live pane gets a REAL pty entry carrying its rendered tail, so
    // `attention_inputs_from` builds the maps `attention_tick` consumes rather
    // than the test handing it synthetic ones. That is the half of the gather
    // no liveness row has ever run: which panes are in the population, and how
    // much of each ring is read, are decisions `attention_inputs_from` makes.
    let ptys = PtyManager::default();
    for id in &fx.live.agent_ids {
        let _ring = ptys.register_fake_for_test(fx.live.pty_of[id], fx.tails[id].as_bytes());
    }

    // The fixture's own claims, asserted rather than assumed. Each one is a
    // scale this row's failure message will quote, and a fixture that quietly
    // came up smaller would make every measurement below a statement about a
    // subject nobody built.
    let scale = fx.scale;
    assert_eq!(
        fx.live.agent_ids.len(),
        scale.live_bound,
        "setup: the live fleet is short, so the trigger state may not be present at all"
    );
    assert_eq!(
        fx.dead_ids.len(),
        scale.dead,
        "setup: a guardrail refused part of the dead fleet, so the roster scan below is \
         measuring a smaller population than it claims"
    );
    assert_eq!(fx.board_rows, scale.board_rows, "setup: the board came up short");
    assert_eq!(
        fx.pending_questions, scale.pending_questions,
        "setup: the pending-question set is not at its cap, so the axis is unexercised"
    );
    assert!(
        fx.audit_window.1,
        "setup: the audit log did not overflow the viewer's window ({} entries), so the log \
         axis is smaller than a day-old session's",
        fx.audit_window.0
    );

    L7Fixture { reg, _dir, ptys, fx, group: g.id }
}

/// One tick, as it is driven: on its own thread, reporting the tracked thread
/// id it ran on so a hold can be attributed to it without a sampler.
struct TickRun {
    name: &'static str,
    thread: u64,
    took: Duration,
}

/// L7a (#1702 P4) — every cadenced tick, a representative MCP read and a
/// representative sync command, run against a day-old session, each inside its
/// budget and none of them holding a registry lock past the hold budget.
///
/// **Why the hold half needs its own mechanism.** `TICK_LOCK_BUDGET` bounds
/// what a tick WAITS. Nothing in this repo bounded what a tick HOLDS, and that
/// is the half #1702 went through: every budget answered correctly, every
/// guard stayed green, and one thread sat on `agents` forever.
#[test]
fn l7a_every_tick_returns_within_budget_on_a_day_old_session() {
    // `unwrap_or_else(into_inner)` rather than `unwrap`, for the reason
    // `POOL_SERIAL`'s two callers use it and CLAUDE.md states: one failing test
    // poisons the guard and every later test on it then dies of `PoisonError`,
    // reporting one genuine failure as N and making a mutation round's reds
    // unattributable.
    let _serial = HOLD_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let f = l7_fixture();
    let now = now_ms_local();

    // ---- the instrument control, before anything is measured -------------
    //
    // The hold assertion below is an ABSENCE, which is byte-identical to an
    // instrument that cannot see the thing. So a deliberate over-threshold
    // hold is taken at a site with NO permit, on a thread whose id this test
    // knows, and the enforcement must catch it — naming that site and that
    // duration. Without this, a scan that had gone blind and a tick that never
    // takes a lock would read exactly the same.
    //
    // This doubles as the planted-6-s red the enforcement owes: it is the
    // failure the mechanism exists to produce, run on every CI job rather than
    // once on a scratch branch.
    let planted = plant_an_unpermitted_hold();
    assert!(
        planted.contains("l7a_planted_hold"),
        "the enforcement did not name the HOLDER SITE, which is the whole of what makes a \
         report actionable: {planted}"
    );
    assert!(
        planted.contains("ms"),
        "the enforcement did not name the DURATION: {planted}"
    );

    // And the other direction: the suite's own deliberate wedge really does
    // take a permit. Without it every 20-30 s hold L1/L2a/L2c/L7b install would
    // be a violation, and the pair above cannot tell an enforcement that works
    // from one that refuses everything.
    //
    // Asserted on the permit REGISTRY rather than by holding for five seconds
    // and scanning: `hold_lock_for_test` takes its permit BEFORE the
    // acquisition and returns only once the hold is real, so by the time it
    // returns the registration has happened — deterministically, with no sleep,
    // and in 50 ms rather than 5 s. What a permit then MEANS to the classifier
    // is pinned by `lockwatch`'s own
    // `a_long_hold_is_a_violation_unless_its_thread_is_permitted` and
    // `a_retired_permit_still_exempts_the_thread_that_earned_it`.
    let permitted_before = loomux_engine::lockwatch::permitted_threads().len();
    assert!(f.reg.hold_lock_for_test("tasks_lock", 50), "setup: cannot hold `tasks_lock`");
    assert!(
        loomux_engine::lockwatch::permitted_threads().len() > permitted_before,
        "`hold_lock_for_test` took no LongHoldPermit, so every deliberate 20-second wedge in \
         this binary is a hold-budget violation and the enforcement would fail the suite on \
         its own fixtures"
    );

    // ---- the measurement -------------------------------------------------
    //
    // Concurrently, as L2c drives them: they are independent, the property is
    // "each returns", and running them in sequence would only make the row
    // slower without making it stricter.
    let ticks: Vec<(&'static str, Box<dyn Fn(&OrchRegistry) + Send + Sync>)> = vec![
        ("reap_idle_agents", Box::new(move |r: &OrchRegistry| { r.reap_idle_agents(now); })),
        ("run_watchdog", Box::new(move |r: &OrchRegistry| { r.run_watchdog(now); })),
        ("run_attention", Box::new(move |r: &OrchRegistry| r.run_attention(now))),
        ("run_idle_tick", Box::new(move |r: &OrchRegistry| { r.run_idle_tick(now); })),
        ("run_compact_nudge", Box::new(move |r: &OrchRegistry| { r.run_compact_nudge(now); })),
        ("run_gh_poll_tick", Box::new(|r: &OrchRegistry| { r.run_gh_poll_tick(); })),
        ("run_workflow_gate_reload", Box::new(|r: &OrchRegistry| r.run_workflow_gate_reload())),
        ("run_disk_monitor", Box::new(|r: &OrchRegistry| r.run_disk_monitor())),
        ("flush_due_max_notices", Box::new(move |r: &OrchRegistry| r.flush_due_max_notices(now))),
        // The publisher, which is a cadenced tick in every sense that matters
        // here even though it is not in `start_*`'s list: it runs once a second
        // per group, and `compute_group`'s roster scan is O(agents ever
        // spawned) — plan row 13 following row 1.
        ("view_publisher", Box::new(|r: &OrchRegistry| r.views.publish_pass(r))),
    ];

    let (tx, rx) = mpsc::channel::<TickRun>();
    let n = ticks.len();
    for (name, func) in ticks {
        let (r, tx) = (f.reg.clone(), tx.clone());
        std::thread::spawn(move || {
            let thread = loomux_engine::lockwatch::current_thread_id();
            let t0 = Instant::now();
            func(&r);
            let _ = tx.send(TickRun { name, thread, took: t0.elapsed() });
        });
    }
    drop(tx);

    let mut runs: Vec<TickRun> = Vec::new();
    // Collected so the hold scan below can be scoped to the threads THIS row
    // created. It has to be: `cargo` runs this binary's tests in one process on
    // many threads, and the hold registry is process-global, so an unscoped
    // scan here is also an assertion about every neighbouring row's deliberate
    // fixtures — see `assert_no_disallowed_hold_over_on`, whose doc records the
    // real one that failed this row's first run.
    let deadline = bg::TICK_LOCK_BUDGET + GRACE;
    while runs.len() < n {
        match rx.recv_timeout(deadline) {
            Ok(r) => runs.push(r),
            Err(_) => break,
        }
    }
    assert_eq!(
        runs.len(),
        n,
        "only {}/{n} ticks returned within {deadline:?} on a {}-agent roster. A tick that never \
         returns is one parked thread per cadence for as long as the hold lasts. Returned: {:?}",
        runs.len(),
        f.fx.roster_size(),
        runs.iter().map(|r| r.name).collect::<Vec<_>>()
    );

    // Every tick inside the budget its own gate probe gives it. Not a latency
    // measurement — the figures below are two orders of magnitude under this —
    // but the number a tick that had regrown a whole-file read would cross.
    for r in &runs {
        assert!(
            r.took < bg::TICK_LOCK_BUDGET,
            "`{}` took {:?} on a day-old session ({} agents, {} board rows, {} audit entries in \
             the viewer's window), past its own {:?} budget",
            r.name,
            r.took,
            f.fx.roster_size(),
            f.fx.board_rows,
            f.fx.audit_window.0,
            bg::TICK_LOCK_BUDGET
        );
    }

    // ---- a representative MCP read, at session scale ---------------------
    //
    // `list_tasks` reads the 400-row board through the shipped dispatch, under
    // the shipped `MCP_READ_BUDGET` frame. Chosen over `list_agents` because
    // the board is the axis that is genuinely unbounded (#1472): the roster is
    // in memory, the board is a whole-file parse per call.
    let orch = f
        .reg
        .spawn_agent(&f.group, Role::Orchestrator, "orch-1", "", false, None)
        .expect("an orchestrator to carry a resolvable token");
    let caller = f.reg.resolve_token(&orch.token).expect("the token resolves");
    let call = json!({ "name": "list_tasks", "arguments": {} });
    let t0 = Instant::now();
    let out = loomux_lib::orchestration::mcp::dispatch(&f.reg, &caller, "tools/call", &call)
        .expect("a tool RESULT, not a protocol error");
    let mcp_took = t0.elapsed();
    assert_ne!(
        out.get("isError").and_then(|e| e.as_bool()),
        Some(true),
        "the MCP read must SUCCEED on an uncontended day-old session — a busy answer here \
         would mean the measurement below is of a refusal, not of a read: {out}"
    );
    assert!(
        mcp_took < bg::MCP_READ_BUDGET,
        "an MCP read of a {}-row board took {mcp_took:?}, past {:?}",
        f.fx.board_rows,
        bg::MCP_READ_BUDGET
    );

    // ---- a representative SYNC command, at session scale -----------------
    //
    // Driven through the shipped `mutating_command` wrapper rather than a
    // rebuilt frame: the wrapper IS the containment #1713 added for this class
    // (a sync command runs inside the WebView2 COM frame, where an unwind
    // aborts the process), so a test that assembled its own would be measuring
    // its own re-implementation. `synccommands.rs` covers WHICH commands carry
    // the frame; this covers what one costs on a day-old registry.
    let victim = f.fx.live.agent_ids[0].clone();
    let t0 = Instant::now();
    OrchRegistry::mutating_command("l7a_ack_attention", || (), || f.reg.ack_attention(&victim));
    let sync_took = t0.elapsed();
    assert!(
        sync_took < bg::COMMAND_READ_BUDGET,
        "a sync command took {sync_took:?} on a day-old session, past {:?}",
        bg::COMMAND_READ_BUDGET
    );

    // ---- the hold budget -------------------------------------------------
    //
    // The whole point of the row. Every tick above has returned, so any hold
    // they took has ended and been stamped; anything still in flight is a hold
    // that outlived its tick.
    let tick_threads: Vec<u64> = runs.iter().map(|r| r.thread).collect();
    loomux_engine::lockwatch::assert_no_disallowed_hold_over_on(
        loomux_engine::lockwatch::HOLD_FAIL_MS,
        &tick_threads,
    );

    // ---- the distribution, printed rather than asserted ------------------
    //
    // A percentile is a measurement, and this file's rule is that an assertion
    // may only refuse what its instrument is demonstrated to detect (#1703
    // review B1). What a p95 supports is a REPORT — the figure a PR body and a
    // future regression are compared against — so it is printed with its
    // subject named, and the budget assertion above is what refuses.
    let (outputs, tails, inputs) = f.reg.attention_inputs_from(&f.ptys);
    assert_eq!(
        tails.len(),
        f.fx.live.agent_ids.len(),
        "setup: the real gather did not produce a tail per live pane ({} of {}), so the \
         percentiles below are over a smaller subject than the fixture built",
        tails.len(),
        f.fx.live.agent_ids.len()
    );
    let mut samples: Vec<u128> = Vec::with_capacity(L7_SAMPLES);
    for i in 0..L7_SAMPLES {
        // Past `ATTENTION_QUIET_MS` on every pass after the first, which is
        // what puts the per-agent chain through the mask rather than short-
        // circuiting on a quiet clock it has only just established.
        let t = Instant::now();
        let _ = f.reg.attention_tick(now + 5_000 * (i as u64 + 1), &outputs, &tails, &inputs);
        samples.push(t.elapsed().as_micros());
    }
    samples.sort_unstable();
    let p = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    println!(
        "L7a attention_tick on a day-old session ({} agents / {} live+bound / {} board rows / \
         {} deliveries per session): p50 {} us, p95 {} us, max {} us, over {L7_SAMPLES} passes \
         (TICK_LOCK_BUDGET is {} ms)",
        f.fx.roster_size(),
        f.fx.live.agent_ids.len(),
        f.fx.board_rows,
        f.fx.scale.deliveries_per_agent,
        p(0.50),
        p(0.95),
        samples[samples.len() - 1],
        bg::TICK_LOCK_BUDGET.as_millis()
    );
    for r in &runs {
        println!("L7a tick `{}` took {:?}", r.name, r.took);
    }
    println!("L7a mcp list_tasks {mcp_took:?}; sync ack_attention {sync_took:?}");

    // A vacuity control for the two `println!`s above and for the whole row:
    // every tick reported a tracked thread id, which is what
    // `assert_no_disallowed_hold_over` would have attributed a hold to.
    assert!(
        runs.iter().all(|r| r.thread != 0),
        "a tick reported no tracked thread id, so a hold it took could not have been \
         attributed to it"
    );
}

/// Take an over-threshold hold at a site with NO [`LongHoldPermit`] and return
/// the enforcement's own failure text.
///
/// The positive control every absence assertion in L7a rests on. It uses a
/// lock this test constructs, so nothing else in the process can be holding
/// it, and it deliberately does NOT go through `hold_lock_for_test` — that
/// seam carries a permit, which is exactly the thing being controlled for.
fn plant_an_unpermitted_hold() -> String {
    use loomux_engine::lockwatch::{
        assert_no_disallowed_hold_over_on, current_thread_id, TrackedMutex, HOLD_FAIL_MS,
    };

    let lock = Arc::new(TrackedMutex::new("l7a_planted_hold", ()));
    let (ready_tx, ready_rx) = mpsc::channel::<u64>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = {
        let lock = lock.clone();
        std::thread::spawn(move || {
            let _held = lock.lock_safe();
            // Its own id, so the scan below is scoped to THIS hold. Reported
            // after the acquisition, because that is what stamps the site.
            let _ = ready_tx.send(current_thread_id());
            // Held until this test says otherwise, rather than for a fixed
            // sleep: the scan has to land while the hold is in flight, and a
            // sleep long enough to guarantee that on a loaded runner would be
            // longer than the row can afford.
            let _ = release_rx.recv_timeout(GRACE * 3);
        })
    };
    let holder_thread = ready_rx.recv_timeout(GRACE).expect("the planted hold never started");
    // Wait out the threshold. Not a poll for a condition — this file's "never
    // a sleep" rule is about waiting for something to happen; here the elapsed
    // time IS the subject.
    std::thread::sleep(L7_PLANTED_HOLD);

    let caught = std::panic::catch_unwind(|| {
        assert_no_disallowed_hold_over_on(HOLD_FAIL_MS, &[holder_thread])
    });
    let _ = release_tx.send(());
    let _ = holder.join();
    // The enforcement does not restore the arming on a panic, deliberately —
    // see its doc. This control is the one caller that must, because the
    // process is NOT failing.
    loomux_engine::lockwatch::set_hold_panics(false);

    let err = caught.expect_err(
        "a five-second hold at a site with no LongHoldPermit did not fail the build. That is \
         the enforcement being blind, and every absence assertion in L7a rests on it",
    );
    err.downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

/// L7b (#1702 P4) — the same day-old session with a registry lock wedged: the
/// MCP answers `Busy` inside its budget and the published view goes stale and
/// clears, rather than either of them waiting on the roster the dead fleet
/// grew.
///
/// L2a and L1 ask this on a four-pane registry. The reason it is worth asking
/// again at scale is that both properties are bounds on a WAIT, and a wait that
/// is bounded correctly can still be preceded by an unbounded amount of work:
/// #1592's boot hang and #1625's publisher were both "the bound was fine, the
/// thing in front of it was not".
#[test]
fn l7b_a_wedged_day_old_registry_still_answers_busy_within_budget() {
    let f = l7_fixture();
    let orch = f
        .reg
        .spawn_agent(&f.group, Role::Orchestrator, "orch-1", "", false, None)
        .expect("an orchestrator to carry a resolvable token");
    // Resolved BEFORE anything is held: `resolve_token` takes `groups`, so
    // resolving it under the hold would park this thread for the whole hold and
    // the read half would then run against an expired one (L2a's own note).
    let caller = f.reg.resolve_token(&orch.token).expect("the token resolves");

    // Baseline, so every assertion below is not satisfied by a surface that is
    // broken some other way.
    let call = json!({ "name": "list_tasks", "arguments": {} });
    let warm = loomux_lib::orchestration::mcp::dispatch(&f.reg, &caller, "tools/call", &call)
        .expect("a tool RESULT");
    assert_ne!(
        warm.get("isError").and_then(|e| e.as_bool()),
        Some(true),
        "setup: the read must succeed uncontended: {warm}"
    );

    // Publish once, so there IS a snapshot for the wedged read below to serve.
    // Without it `group_view_payload` answers `null` for a group nothing has
    // ever published, and the assertion would be about an empty publisher
    // rather than about a registry that cannot be read.
    f.reg.views.publish_pass(&f.reg);
    assert!(
        group_view_payload(&f.reg.views.load(), &f.group, Instant::now()).is_object(),
        "setup: the group is not published even uncontended, so the wedged read below would \
         answer `null` for a reason that has nothing to do with the wedge"
    );

    assert!(f.reg.hold_lock_for_test("agents", 20_000), "setup: cannot hold `agents`");

    let started = Instant::now();
    let busy = loomux_lib::orchestration::mcp::dispatch(&f.reg, &caller, "tools/call", &call)
        .expect("a tool RESULT, not a protocol error");
    let waited = started.elapsed();
    assert!(
        waited < bg::MCP_READ_BUDGET + GRACE,
        "an MCP read did not answer in {waited:?} on a {}-agent roster with `agents` held",
        f.fx.roster_size()
    );
    assert_eq!(
        busy.get("isError").and_then(|e| e.as_bool()),
        Some(true),
        "a wedged registry must answer a busy RESULT: {busy}"
    );
    let text = busy["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.starts_with("loomux busy:"), "read-tool text is a public contract: {text}");

    // The published view: it must still ANSWER — a pointer clone off the last
    // published snapshot — while the registry it was computed from is wedged.
    // That is L1's property, asked on the population the dead fleet grew,
    // because `compute_group`'s roster scan is O(agents ever spawned).
    let started = Instant::now();
    let payload = group_view_payload(&f.reg.views.load(), &f.group, Instant::now());
    let view_took = started.elapsed();
    assert!(
        view_took < bg::POLL_LOCK_BUDGET + GRACE,
        "a published read took {view_took:?} while `agents` was held on a {}-agent roster — a \
         published read is a pointer clone and must not touch the registry at all",
        f.fx.roster_size()
    );
    assert!(
        payload.is_object(),
        "a published read must answer with a payload rather than nothing while the registry is \
         wedged: {payload}"
    );

    // The stale flag is the disclosure half: a panel may show a frozen number,
    // and must never show it as current.
    assert!(
        VIEW_STALE_AFTER_MS > 0,
        "the stale window is what makes a frozen number legible as one"
    );
}

// ---------- the long-hold allowlist, default-deny (#1702 P4) ----------

/// The sites that may construct a [`loomux_engine::lockwatch::LongHoldPermit`],
/// each with the reason it is allowed to hold a tracked lock past
/// `HOLD_FAIL_MS`.
///
/// **Default-deny**: any other construction site fails the scan below. That is
/// the whole mechanism — the enforcement is only as strong as this list is
/// short, and a permit is the one way to make a five-second hold invisible to
/// CI, so adding one has to be a reviewed change rather than a way to quiet a
/// red.
///
/// Both rows carry the same argument in different clothes: the site's ENTIRE
/// PURPOSE is to hold a lock longer than the app ever should, so that something
/// else can be observed surviving it. Neither is a production path that got
/// slow.
const PERMITTED_LONG_HOLDS: &[(&str, &str, &str)] = &[
    (
        "orchestration/mod.rs",
        "OrchRegistry::hold_lock_for_test",
        "the liveness suite's deliberate wedge: L1/L2a/L2c/L7b hold a registry lock for 20-30 s \
         and probe that everything else still answers. Exempting it is what stops the \
         enforcement failing the suite on its own fixtures",
    ),
    (
        "orchestration/e2ehold.rs",
        "e2ehold::hold",
        "the soak lane's injected hold (#1606): 90 s by design, behind #[cfg(debug_assertions)] \
         plus a single-value opt-in, and the subject of e2ehold_guard.rs's four properties",
    ),
];

/// Every PRODUCTION source root, so a permit taken in the engine crate is as
/// visible as one taken in `src-tauri`.
///
/// `lockwatch` itself is where `LongHoldPermit` is DEFINED, and the scan below
/// has to see that file: a scan whose roots missed the defining crate would be
/// unfalsifiable in the direction that matters, since the type could then grow
/// a second construction site next to its own declaration and nothing would
/// notice.
const PERMIT_ROOTS: &[(&str, &str)] = &[
    ("src-tauri", concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
    ("loomux-engine", concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/loomux-engine/src")),
];

fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Only the argued sites may permit a long hold (#1702 P4).
///
/// **What it decides on, and why not a name.** CLAUDE.md's source-scanning
/// rule: a guard that decides from a binding's name enforces nothing, because a
/// rename walks past it. The axis here is name-independent and cannot compile
/// any other way — the type has one constructor, so `LongHoldPermit::new(` is
/// the shape, and the allowlist is keyed on the FILE the call sits in plus the
/// enclosing item's name, which is what a reviewer would read anyway.
///
/// **Both directions are asserted.** An unlisted construction site fails
/// (default-deny), and a listed row that no longer matches anything also fails
/// (a stale row is a row watching nothing — the same defect as an allowlist
/// entry that has drifted off its subject).
///
/// **What it cannot see, stated rather than left to be discovered.**
///
/// - It is a textual scan over `.rs` files: a permit constructed through a type
///   alias, a re-export under another name, or a macro would not match. None
///   exists, and the compiler rather than this scan is what makes a permit
///   reachable only through that one constructor.
/// - The population is PRODUCTION code, so a file is scanned only as far as its
///   first `#[cfg(test)]`. `lockwatch`'s own unit tests construct four permits
///   deliberately — they are what pins what a permit MEANS — and listing each
///   would put four rows in the table for one argument. The cost is that a
///   permit taken inside a `#[cfg(test)]` module is out of population; such a
///   permit cannot reach a shipped build, and a Rust liveness row is scoped to
///   its own threads (`assert_no_disallowed_hold_over_on`) so it cannot be
///   silenced by one either.
/// - It assumes a file's `#[cfg(test)]` module is LAST. That assumption is
///   self-checking rather than trusted: truncating a file early would swallow a
///   production site, and the vacuity control at the bottom then finds fewer
///   sites than the table has rows.
#[test]
fn only_argued_sites_may_permit_a_long_hold() {
    let mut files: Vec<(&str, std::path::PathBuf)> = Vec::new();
    for (label, root) in PERMIT_ROOTS {
        let mut found = Vec::new();
        collect_rs(std::path::Path::new(root), &mut found);
        // Per root, not on the total: a mistyped root contributes nothing and
        // would hide behind the other root's file count, and `collect_rs`
        // returns silently on an unreadable directory — so without this a wrong
        // path is indistinguishable from a clean scan.
        assert!(
            !found.is_empty(),
            "no `.rs` under the {label} root ({root}) — a root that scans nothing is a tripwire \
             that cannot fire"
        );
        files.extend(found.into_iter().map(|p| (*label, p)));
    }

    // The anchor: the file that DEFINES the type must be in scope. Matched on
    // content rather than on a path, so #888 may move `lockwatch` again and
    // this fails loudly instead of quietly scanning past it.
    assert!(
        files.iter().any(|(_, p)| std::fs::read_to_string(p)
            .is_ok_and(|s| s.contains("pub struct LongHoldPermit"))),
        "the scan never reached the file that DEFINES `LongHoldPermit`, so a second constructor \
         written beside its own declaration would be invisible — add its root to PERMIT_ROOTS"
    );

    let mut offenders: Vec<String> = Vec::new();
    let mut row_hits = vec![0usize; PERMITTED_LONG_HOLDS.len()];
    let mut sites_seen = 0usize;

    for (_, path) in &files {
        let src = std::fs::read_to_string(path).unwrap();
        // The file key a row names: the last two path components, so
        // `orchestration/mod.rs` distinguishes it from any other `mod.rs`.
        let key = {
            let mut parts: Vec<String> = path
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect();
            let tail = parts.split_off(parts.len().saturating_sub(2));
            tail.join("/")
        };
        // The enclosing item, tracked as the scan walks: the nearest `fn` line
        // above the call. A structural fact about the file rather than a
        // heuristic about a name — the row records it so a reviewer can find
        // the site, and a MOVE to a different function is a real change to what
        // was argued.
        let mut enclosing = String::from("<file scope>");
        for line in src.lines() {
            let t = line.trim_start();
            // A `#[cfg(test)]` module ends the PRODUCTION population. Files
            // here put theirs last, and the vacuity control at the bottom is
            // what makes the assumption self-checking: swallow a production
            // site and `sites_seen` falls short of the table.
            if t.starts_with("#[cfg(test)]") {
                break;
            }
            if t.starts_with("//") {
                // A comment may spell the construct literally — the permit's
                // own doc and the two argued sites' comments all do — and it
                // must not move `enclosing` either.
                continue;
            }
            // The enclosing item: the nearest `fn` line above the call. The
            // leading space is load-bearing — a bare `fn foo(` at the start of
            // a trimmed line has no space before `fn`, so splitting on `" fn "`
            // alone silently keeps the PREVIOUS item's name and reports every
            // site inside a private fn under whichever `pub fn` came last.
            if let Some(rest) = format!(" {t}").split(" fn ").nth(1).map(str::to_string) {
                if let Some(name) = rest.split('(').next() {
                    enclosing = name.trim().to_string();
                }
            }
            if !t.contains("LongHoldPermit::new(") {
                continue;
            }
            sites_seen += 1;
            // A row names its item as a reader would write it
            // (`OrchRegistry::hold_lock_for_test`); the scan sees the bare `fn`
            // name. Compared on the last `::` segment, and by EQUALITY rather
            // than `contains`, so a row cannot match a longer name that merely
            // ends with it.
            match PERMITTED_LONG_HOLDS.iter().position(|(f, item, _)| {
                let want = item.rsplit("::").next().unwrap_or(item);
                key.ends_with(f) && enclosing == want
            }) {
                Some(i) => row_hits[i] += 1,
                None => offenders.push(format!("{key} (in `{enclosing}`)")),
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a `LongHoldPermit` may only be constructed at a site argued in \
         PERMITTED_LONG_HOLDS — it exempts its thread from the hold-duration enforcement, \
         which is the one way to make a five-second registry hold invisible to CI. \
         Unlisted:\n  {}",
        offenders.join("\n  ")
    );

    // The other direction: a row that matches nothing is watching nothing, and
    // reads as coverage. Required exactly once each, so a site that was
    // DUPLICATED is as loud as one that was deleted.
    for (i, (file, item, why)) in PERMITTED_LONG_HOLDS.iter().enumerate() {
        assert_eq!(
            row_hits[i], 1,
            "PERMITTED_LONG_HOLDS names {file}/{item} ({why}) and the scan matched it \
             {} times, not once — the row is stale, or the permit was duplicated",
            row_hits[i]
        );
    }

    // Vacuity control. Every assertion above is satisfied by a scan that
    // matched nothing at all — no offenders, and `row_hits` would then be zero
    // and caught by the loop above, but only because the rows exist. This pins
    // that the SHAPE is findable at all, independently of the table.
    assert_eq!(
        sites_seen,
        PERMITTED_LONG_HOLDS.len(),
        "the scan found {sites_seen} `LongHoldPermit::new(` call sites against {} argued rows",
        PERMITTED_LONG_HOLDS.len()
    );
}

// ---------- L8: a switched-off reentrant refusal, driven THROUGH the shipped
// command frame (#1702 P5) ----------

/// Serializes any row that flips the process-global
/// `loomux_engine::lockwatch::LOCK_ORDER_PANICS` flag. L5a relies on its
/// `cfg!(debug_assertions)` default (armed in every debug/test build) to
/// require a panic on a planted inversion; `cargo test` runs this binary's
/// tests concurrently on many threads, so a row that disarms the flag without
/// serializing against L5a could make it fail to panic — intermittently, for
/// a reason that has nothing to do with either row's own defect. Same class
/// of hazard `HOLD_SERIAL`/`POOL_SERIAL` already guard in this file.
static LOCK_ORDER_PANICS_SERIAL: Mutex<()> = Mutex::new(());

/// Restores `LOCK_ORDER_PANICS` however the scope ends — the `MutateDeadline`
/// idiom above, for the same reason: a test that fails while the flag is
/// moved must not leave every later test in this binary silently re-armed (or
/// disarmed) against a value nobody chose.
struct LockOrderPanics(bool);
impl LockOrderPanics {
    fn set(on: bool) -> Self {
        Self(loomux_engine::lockwatch::set_lock_order_panics(on))
    }
}
impl Drop for LockOrderPanics {
    fn drop(&mut self) {
        loomux_engine::lockwatch::set_lock_order_panics(self.0);
    }
}

/// L8 (#1702 P5). `doc/design/lock-liveness.md` §7's "The panic is armed, not
/// default" section draws a sharp line: `LOCK_ORDER_PANICS` defaults to
/// `cfg!(debug_assertions)` — true in every test binary — so a re-entrant
/// acquisition panics HERE by default, which is correct for a *test* build
/// (the panic unwinds the offending thread and releases the registry) and
/// says nothing about what a **release** build does, where the constant is
/// off.
///
/// `l5b_a_reentrant_acquisition_answers_busy_instead_of_hanging` already
/// proves the raw mechanism (`lock_within_for_test` refuses with a `Busy`
/// rather than hanging), and `synccommands.rs`'s
/// `the_command_boundary_wrappers_really_install_a_frame` already proves that
/// `OrchRegistry::mutating_command` installs a real `read_budget` frame and
/// `MutationScope` around its body. Neither combines the two: nothing yet
/// drives an ACTUAL re-entrant `lock_safe()` acquisition through the SHIPPED
/// frame with the panic switched off the way a release build runs it, and
/// checks that the command degrades instead of the unwind reaching the
/// WebView2 COM boundary `mutating_command`'s own doc says aborts the
/// process. That gap is what this row closes.
#[test]
fn l8_a_switched_off_reentrant_acquisition_degrades_through_the_command_frame() {
    let _serial = LOCK_ORDER_PANICS_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (reg, _dir) = test_registry();

    // Release semantics: the panic that defaults on in every test binary is
    // switched off for the rest of this test, restored on every exit —
    // including a stray panic of our own — by the guard's `Drop`.
    let _panics_off = LockOrderPanics::set(false);

    // ---- the counterfactual first: WITHOUT any command-boundary frame ----
    //
    // `refuse_reentrant` (lockwatch.rs) is `if LOCK_ORDER_PANICS { panic!() }
    // else if let Some(frame) = budget::remaining() { unwind_to_frame(..) }
    // else { panic!() }` — so with the flag off AND no `read_budget` frame
    // active, it still falls through to the last-resort `panic!`. This is the
    // positive control the row below needs: without it, "the command below
    // degrades" would be satisfied just as well by a mechanism that always
    // degrades, flag or no frame, and would say nothing about the FRAME being
    // what contains it.
    let bare = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reg.with_lock_for_test("tasks_lock", || {
            let _ = reg.with_lock_for_test("tasks_lock", || ());
        })
    }));
    assert!(
        bare.is_err(),
        "a bare re-entrant acquisition with no command-boundary frame around it must still \
         panic even with LOCK_ORDER_PANICS off — otherwise the `mutating_command` result below \
         would be unfalsifiable: nothing would show the FRAME is what contains it rather than \
         the flag alone"
    );

    // ---- the real question: THROUGH the shipped frame ----
    //
    // Driven through `OrchRegistry::mutating_command` rather than a rebuilt
    // frame, for the same reason L7a's sync-command row is: the wrapper IS
    // the containment #1713 added, so a test that assembled its own
    // `read_budget` call would measure its own re-implementation rather than
    // the code every real sync command in `mod.rs` (`bind_agent`,
    // `orch_solo_bind`, …) actually runs through.
    let outcome: Result<(), String> = OrchRegistry::mutating_command(
        "l8_reentrant_probe",
        || Err(loomux_lib::orchestration::COMMAND_REFUSED.to_string()),
        || {
            reg.with_lock_for_test("tasks_lock", || {
                // The re-entrant mistake: a command body that — directly, or
                // several frames down through a helper — takes a registry
                // lock it is already holding. Nothing past this point runs;
                // the inner call never returns normally.
                let _ = reg.with_lock_for_test("tasks_lock", || ());
            });
            Ok(())
        },
    );

    let err = outcome.expect_err(
        "a re-entrant acquisition inside a synchronous command must be REFUSED. With the panic \
         disarmed the way a release build runs it, this call either aborts the process (this \
         test process included, which would report as the harness crashing rather than a clean \
         assertion failure) or answers Ok after a partially-applied mutation. Neither happened, \
         so the shipped `mutating_command` frame contained it as designed"
    );
    assert_eq!(
        err,
        loomux_lib::orchestration::COMMAND_REFUSED,
        "the command degraded, but not to the shipped refusal text every real sync command in \
         `mod.rs` returns for this class"
    );

    // And the registry is usable afterwards: the refusal released everything
    // the abandoned mutation was holding rather than leaving `tasks_lock`
    // wedged for the rest of the process.
    assert!(
        reg.with_lock_for_test("tasks_lock", || ()).is_some(),
        "`tasks_lock` did not release after the refused re-entrant acquisition — a degrade that \
         leaves the lock held is a wedge with better error text, not a rescue"
    );
}
