//! #1601 Phase 0 — the app's self-observability, tested as the instrument it is.
//!
//! These live as integration tests (not unit tests) because test executables
//! that link the full lib need the common-controls-v6 manifest embedded via
//! `rustc-link-arg-tests` (CLAUDE.md constraint 4), and because half of what is
//! being asserted is a property of `OrchRegistry`, which lives in `src-tauri`.
//!
//! **What an instrument owes over what a feature owes.** A test that only drove
//! the pure reporting rules over synthesized snapshots would be green against an
//! instrument that records nothing at all — the snapshots would be the test's
//! own invention. So the recording path is exercised for real (a lock is taken,
//! held, and read from another thread) and the reporting rules are ALSO driven
//! purely, where a five-second threshold can be asserted without a test that
//! takes five seconds.

use loomux_lib::lockwatch::{self, HoldReport, LockSnapshot, LockWatch, TrackedMutex};
use loomux_lib::selfwatch::{self, Heartbeat, Liveness, LivenessWatch, PoolWatch};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Serializes the tests that move process-global state — the hold threshold and
/// the report ring. `lock_safe`, never `.lock().unwrap()`: one failing test
/// under the guard would otherwise poison it and every later test on this lock
/// would die of `PoisonError` instead of its own assertion (CLAUDE.md).
static SERIAL: Mutex<()> = Mutex::new(());

use loomux_engine::obs::LockExt;

/// Spin until `cond`, or fail. Bounded so a broken instrument fails as an
/// ASSERTION with a message rather than as a suite timeout with nothing to
/// quote (the #744 rule).
fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ---------- 0.1: what a hold records ----------

#[test]
fn a_hold_is_reported_with_the_call_site_that_actually_took_it() {
    let lock = TrackedMutex::new("specimen", 0u32);

    // `line!()` shares the source line with the `lock_safe()` beside it, so the
    // expectation is derived at the call site rather than written down as a
    // number that rots the moment anything above it moves.
    let (guard_a, line_a) = (lock.lock_safe(), line!());
    let held = lockwatch::held_locks(lockwatch::mono_ms());
    let s = held.iter().find(|s| s.name == "specimen").expect("the held lock must be in the snapshot");
    assert!(
        s.site_file.ends_with("selfwatch.rs"),
        "the site must name this file, got {}",
        s.site_file
    );
    assert_eq!(s.site_line, line_a, "the site must be the line that took the lock");
    drop(guard_a);

    // The discriminating half: a SECOND acquisition from a different line must
    // report that line. An implementation that recorded a constant, or the
    // construction site, or the first acquisition forever, passes the assertion
    // above and fails this one.
    let (guard_b, line_b) = (lock.lock_safe(), line!());
    assert_ne!(line_a, line_b, "the fixture is only a witness while the two sites differ");
    let held = lockwatch::held_locks(lockwatch::mono_ms());
    let s = held.iter().find(|s| s.name == "specimen").expect("still held");
    assert_eq!(s.site_line, line_b, "a second hold must report ITS site, not the first one's");
    drop(guard_b);

    // And a released lock is not in the snapshot at all.
    let held = lockwatch::held_locks(lockwatch::mono_ms());
    assert!(
        !held.iter().any(|s| s.name == "specimen"),
        "a released lock must not read as held"
    );
}

#[test]
fn a_waiter_is_counted_while_it_is_still_waiting() {
    let lock = Arc::new(TrackedMutex::new("waitspec", 0u32));
    let guard = lock.lock_safe();

    let go = Arc::new(AtomicBool::new(false));
    let mut waiters = Vec::new();
    for _ in 0..3 {
        let lock = lock.clone();
        let go = go.clone();
        waiters.push(std::thread::spawn(move || {
            let _g = lock.lock_safe();
            go.store(true, Ordering::SeqCst);
        }));
    }

    wait_for("three threads to be blocked on the lock", || lock.waiters() == 3);

    // The count is observable through the SNAPSHOT, not only through the
    // accessor — the snapshot is what the watchdog reads and what a breadcrumb
    // is built from, and the two could disagree.
    let held = lockwatch::held_locks(lockwatch::mono_ms());
    let s = held.iter().find(|s| s.name == "waitspec").expect("held");
    assert_eq!(s.waiters, 3, "the snapshot must carry the live waiter count");
    assert!(!go.load(Ordering::SeqCst), "no waiter can have acquired while we hold it");

    drop(guard);
    for w in waiters {
        w.join().unwrap();
    }
    wait_for("the waiters to drain", || lock.waiters() == 0);
}

#[test]
fn a_hold_past_the_threshold_is_reported_on_release_with_its_total() {
    let _serial = SERIAL.lock_safe();
    let restore = HoldWarn::set(30);
    lockwatch::clear_reports();

    let lock = TrackedMutex::new("dropspec", 0u32);
    let (guard, line) = (lock.lock_safe(), line!());
    std::thread::sleep(Duration::from_millis(80));
    drop(guard);

    // The drain is the watchdog's job in the app; a test binary has no watchdog,
    // so it stands in for one. The split is deliberate — see
    // `the_release_path_takes_no_global_lock_while_the_mutex_is_still_held`.
    lockwatch::record_all(lockwatch::drain_completed_holds());

    let reports = lockwatch::recent_reports();
    let r = reports
        .iter()
        .find(|r| r.lock == "dropspec")
        .expect("a hold past the threshold must be reported when it ends");
    assert!(!r.still_held, "a completed hold is reported as ended");
    assert!(r.held_ms >= 30, "the total must be the real duration, got {}", r.held_ms);
    assert_eq!(r.site_line, line, "and it must name the site that took it");
    assert_eq!(r.event(), "lock-freed");
    assert!(
        r.detail().contains("lock=dropspec") && r.detail().contains("waiters="),
        "the breadcrumb detail must carry the fields a reader needs: {}",
        r.detail()
    );

    // The counterfactual: a hold UNDER the threshold is silent. Without this the
    // assertion above is satisfied by an instrument that reports every unlock.
    //
    // Filtered by NAME rather than asserting an empty ring: the threshold is
    // process-global, so lowering it here can make a test running in parallel
    // report a hold of its own, and an emptiness assertion would then fail for
    // somebody else's reason.
    lockwatch::clear_reports();
    {
        let _quick = lock.lock_safe();
    }
    lockwatch::record_all(lockwatch::drain_completed_holds());
    assert!(
        !lockwatch::recent_reports().iter().any(|r| r.lock == "dropspec"),
        "a hold under the threshold must not be reported"
    );
    drop(restore);
}

/// #1605 review B1. `Drop::drop` runs BEFORE the struct's fields drop, and
/// `TrackedGuard.guard` is a field — so anything the release path does, it does
/// with the reported mutex still locked and every waiter still queued behind it.
/// The release path used to compose and write its report there.
///
/// Reading the code is what missed it, so this does not read the code: it makes
/// the process-global report ring UNAVAILABLE and asks whether a release still
/// completes. It also asks whether the report survives the detour, because a
/// release that got fast by dropping the report on the floor would pass the
/// first half alone.
#[test]
fn the_release_path_takes_no_global_lock_while_the_mutex_is_still_held() {
    let _serial = SERIAL.lock_safe();
    let restore = HoldWarn::set(20);
    lockwatch::clear_reports();

    // Far longer than any release may take, and far longer than SETTLE below.
    const RING_HELD_MS: u64 = 2_000;
    const SETTLE_MS: u128 = 250;
    lockwatch::hold_report_ring_for_test(RING_HELD_MS);

    let lock = TrackedMutex::new("ringspec", 0u32);
    let guard = lock.lock_safe();
    std::thread::sleep(Duration::from_millis(60)); // past the threshold
    let started = Instant::now();
    drop(guard);
    let released = started.elapsed();

    assert!(
        released.as_millis() < SETTLE_MS,
        "the release path waited {released:?} for a lock it must never take — a report composed \
         while the mutex is still held is paid for by every waiter queued behind it, which is the \
         latency path this whole change is about"
    );
    // …and the mutex really is free, not merely released-quickly-in-appearance.
    assert!(
        lock.try_lock_safe().is_some(),
        "the mutex must be free the instant the release path returns"
    );

    // The stamp survived: once the ring is available the report is still there,
    // with its real duration. A release that got fast by losing the report is
    // not the fix.
    let recovered = lockwatch::drain_completed_holds();
    let r = recovered
        .iter()
        .find(|r| r.lock == "ringspec")
        .expect("the completed hold must still be pending for the watchdog to pick up");
    assert!(r.held_ms >= 60, "with its real total, got {}", r.held_ms);
    assert!(!r.still_held);
    drop(restore);
}

/// #1605 review N2, repointed by #1609. The claim it was written for —
/// "same POISON-TOLERANT semantics as `obs::LockExt::lock_safe`" — is gone
/// with the std inner primitive: `TrackedMutex` is `parking_lot::Mutex`
/// now, and parking_lot has no poison state to be tolerant of.
///
/// The assertion is NOT relaxed to fit that, because the property a caller
/// actually depended on survives the swap intact and is what is pinned here:
/// **a holder that dies does not take the lock with it, and the next
/// acquirer sees what it wrote** (#53's "at worst slightly stale, never
/// memory-unsafe"). Under std that was `into_inner` recovering a poisoned
/// guard; under parking_lot the guard simply unlocks as the unwind passes
/// through it. Same outcome, one fewer concept.
///
/// `obs`'s own `lock_safe_recovers_a_poisoned_mutex` stays exactly as it was:
/// it covers `LockExt` over the std mutexes that remain elsewhere (`pty.rs`,
/// and `lockwatch`'s own registry and report ring), which DO still poison.
#[test]
fn a_panicking_holder_releases_the_tracked_lock() {
    let lock = Arc::new(TrackedMutex::new("deadholderspec", 41u32));

    let poisoner = {
        let lock = lock.clone();
        std::thread::spawn(move || {
            let mut g = lock.lock_safe();
            *g = 42;
            panic!("dying with the lock held, on purpose");
        })
    };
    assert!(poisoner.join().is_err(), "the fixture is only a witness if that thread panicked");

    // Blocking acquire: the lock is free and the write the dying thread made
    // is still there — "at worst slightly stale, never memory-unsafe" (#53).
    assert_eq!(*lock.lock_safe(), 42, "a dead holder must not swallow its own write");

    // Non-blocking acquire: the same answer. `None` from `try_lock_safe` means
    // one thing only — someone else has it — and a lock whose holder is gone is
    // not that.
    let g = lock
        .try_lock_safe()
        .expect("a lock whose holder died is FREE; refusing it would report the wrong fact");
    assert_eq!(*g, 42);
    drop(g);

    // The negative control, so the assertion above is not satisfied by a
    // try-lock that simply always succeeds.
    let held = lock.lock_safe();
    assert!(lock.try_lock_safe().is_none(), "a genuinely held lock must still be refused");
    drop(held);
}

/// Restores the process-global hold threshold on the way out, so one test's
/// override cannot leak into another's (CLAUDE.md's `Drop`-guard rule).
struct HoldWarn(u64);
impl HoldWarn {
    fn set(ms: u64) -> Self {
        let prev = lockwatch::hold_warn_ms();
        lockwatch::set_hold_warn_ms(ms);
        Self(prev)
    }
}
impl Drop for HoldWarn {
    fn drop(&mut self) {
        lockwatch::set_hold_warn_ms(self.0);
    }
}

// ---------- 0.2: the watchdog's reporting rule ----------

fn snap(id: u64, generation: u64, held_ms: u64, waiters: u32) -> LockSnapshot {
    LockSnapshot {
        id,
        name: "spec",
        generation,
        holder_thread: 7,
        site_file: "src/spec.rs",
        site_line: 42,
        held_ms,
        waiters,
    }
}

#[test]
fn a_still_held_lock_is_reported_once_per_hold_not_once_per_tick() {
    let mut watch = LockWatch::new();

    assert!(watch.tick(&[snap(1, 1, 4_000, 0)], 5_000).is_empty(), "under the threshold: silent");
    let first = watch.tick(&[snap(1, 1, 6_000, 3)], 5_000);
    assert_eq!(first.len(), 1, "crossing the threshold reports once");
    assert!(first[0].still_held, "a watchdog report is about a hold in flight");
    assert_eq!(first[0].waiters, 3);

    assert!(
        watch.tick(&[snap(1, 1, 300_000, 87)], 5_000).is_empty(),
        "the SAME hold, five minutes later, must not report again — a hang would \
         otherwise write one breadcrumb per second for as long as it lasts"
    );

    // A different hold of the same lock is a different event, and the generation
    // is what says so.
    let second = watch.tick(&[snap(1, 3, 9_000, 1)], 5_000);
    assert_eq!(second.len(), 1, "a NEW hold past the threshold is news again");

    // And the suppression state is released when the hold is over, so `warned`
    // is bounded by concurrent slow holds rather than by slow holds ever seen.
    assert!(watch.tick(&[], 5_000).is_empty());
    assert_eq!(watch.tracked(), 0, "nothing held: nothing to suppress");
}

#[test]
fn the_watchdog_reports_every_slow_lock_in_one_tick_not_just_the_first() {
    let mut watch = LockWatch::new();
    let reports = watch.tick(&[snap(1, 1, 6_000, 0), snap(2, 1, 4_000, 0), snap(3, 1, 7_000, 9)], 5_000);
    assert_eq!(reports.len(), 2, "both over-threshold holds, and only those");
    assert_eq!(reports[1].waiters, 9);
}

// ---------- 0.3: blocking-pool depth ----------

#[test]
fn the_pool_counter_moves_with_real_hand_offs_and_remembers_its_peak() {
    let before = selfwatch::pool_in_flight();
    let tickets: Vec<_> = (0..5).map(|_| selfwatch::pool_enter()).collect();
    assert_eq!(
        selfwatch::pool_in_flight(),
        before + 5,
        "an outstanding hand-off is counted while it is outstanding"
    );
    drop(tickets);
    assert_eq!(selfwatch::pool_in_flight(), before, "and released when it finishes");

    // The peak survives the drop, which is what makes a 1 Hz watchdog unable to
    // miss a crossing that happened between two of its looks.
    let peak = selfwatch::pool_take_peak();
    assert!(peak >= before + 5, "the peak must remember the burst, got {peak}");
    assert_eq!(
        selfwatch::pool_take_peak(),
        selfwatch::pool_in_flight(),
        "and re-arm at the current depth once read, so the next tick reports THIS window"
    );
}

#[test]
fn each_pool_threshold_is_reported_on_the_way_up_and_re_armed_on_the_way_down() {
    let mut watch = PoolWatch::new();
    assert_eq!(watch.tick(63), None, "below the first step: silent");
    assert_eq!(watch.tick(64).map(|r| r.step), Some(64), "the first crossing is news");
    assert_eq!(watch.tick(100), None, "still in the same band: not news again");
    assert_eq!(watch.tick(130).map(|r| r.step), Some(128), "a deeper band is news");
    assert_eq!(watch.tick(512).map(|r| r.step), Some(256), "and the deepest one it knows");

    // Release on EVIDENCE (performance.md P4): the band is re-armed by a depth
    // that actually came back down, never by elapsed time.
    assert_eq!(watch.tick(10), None, "a fall is not news");
    assert_eq!(watch.armed_at(), None, "but it does re-arm the ladder");
    assert_eq!(watch.tick(300).map(|r| r.step), Some(256), "so a second exhaustion reports again");
}

// ---------- 0.4: which half stopped ----------

fn beat(watchdog_ms: u64, watchdog_lag_ms: u64, webview_ms: u64, hidden: bool) -> Heartbeat {
    Heartbeat {
        watchdog_ms,
        watchdog_ticks: 10,
        watchdog_lag_ms,
        webview_ms,
        webview_stamps: 10,
        webview_timer_lag_ms: 0,
        webview_frame_lag_ms: Some(3),
        webview_hidden: hidden,
    }
}

#[test]
fn the_heartbeat_separates_a_stuck_gui_from_a_starved_backend() {
    let stale = 3_000;
    let now = 100_000;

    assert_eq!(selfwatch::liveness(&beat(now, 0, now, false), now, stale), Liveness::Ok);
    assert_eq!(
        selfwatch::liveness(&beat(now, 0, now - 20_000, false), now, stale),
        Liveness::GuiStuck,
        "backend ticking, webview silent, window on screen — beta5's shape"
    );
    assert_eq!(
        selfwatch::liveness(&beat(now, 20_000, now, false), now, stale),
        Liveness::BackendStuck,
        "the watchdog's OWN scheduling lag is what makes this reachable: it has \
         just stamped, so a freshness test on the stamp alone would read Ok from \
         inside a starved backend every time — beta6's shape"
    );
    assert_eq!(
        selfwatch::liveness(&beat(now - 20_000, 0, now, false), now, stale),
        Liveness::BackendStuck,
        "and a stamp that stopped arriving says the same thing"
    );
    assert_eq!(
        selfwatch::liveness(&beat(now - 20_000, 0, now - 20_000, false), now, stale),
        Liveness::BothStuck
    );

    // A hidden window's timers are throttled by the platform, so a stale stamp
    // from one is not evidence — and saying so is different from saying Ok.
    assert_eq!(
        selfwatch::liveness(&beat(now, 0, now - 20_000, true), now, stale),
        Liveness::GuiHidden,
        "minimizing the app must not fire the GUI-stuck alarm"
    );
    assert_eq!(Liveness::GuiHidden.event(), None, "and must not write a breadcrumb");
    assert_eq!(Liveness::GuiStuck.event(), Some("live-gui-stuck"));

    // Nothing to compare is its own answer, not a false alarm about either half.
    let unarmed = Heartbeat { webview_stamps: 0, ..beat(now, 0, 0, false) };
    assert_eq!(selfwatch::liveness(&unarmed, now, stale), Liveness::Unarmed);
}

#[test]
fn a_hang_is_one_breadcrumb_and_not_one_per_second() {
    let mut watch = LivenessWatch::new();
    assert_eq!(watch.tick(Liveness::Ok), None, "health is not news");
    assert_eq!(watch.tick(Liveness::BackendStuck), Some(Liveness::BackendStuck));
    assert_eq!(watch.tick(Liveness::BackendStuck), None, "the same verdict again is not news");
    assert_eq!(watch.tick(Liveness::GuiStuck), Some(Liveness::GuiStuck), "a changed verdict is");
    assert_eq!(watch.tick(Liveness::Ok), None, "recovery is silent...");
    assert_eq!(
        watch.tick(Liveness::GuiStuck),
        Some(Liveness::GuiStuck),
        "...but it re-arms, so a second episode is reported"
    );
}

// ---------- the migration, and the one door ----------

/// This file's ONE registry construction (#464), and the reason it is a helper
/// rather than a `let` inside the test.
///
/// The four `*_agents_dir_override`/`*_hooks_dir_override` fields are in-memory
/// state on the instance, so a registry built without them falls through to the
/// REAL `~/.claude/agents` and `~/.copilot/agents` and a spawn against it writes
/// a generated agent file into the developer's own profile — which is how the
/// suite once left 1,111 stray files there. `orchestration.rs`'s
/// `no_registry_construction_bypasses_the_test_agent_dir_overrides` scans every
/// file under `tests/` for raw constructions and permits exactly one per
/// sanctioned helper, so this file is listed there with a count of 1.
///
/// The test below only holds a lock and spawns nothing, so the leak could not
/// fire from here today. That is not the standard: the guard is default-deny
/// because "this particular test does not spawn" is a fact about today's body,
/// and the next edit to it is exactly what the guard is for.
fn test_registry() -> (Arc<loomux_lib::orchestration::OrchRegistry>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = loomux_lib::orchestration::OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45999);
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (Arc::new(reg), dir)
}

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<repo>/src-tauri` for this package.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// The struct body of `OrchRegistry`, as text.
fn registry_struct_body() -> Vec<String> {
    let src = read("src-tauri/src/orchestration/mod.rs");
    let mut out = Vec::new();
    let mut inside = false;
    for line in src.lines() {
        if !inside {
            if line.starts_with("pub struct OrchRegistry {") {
                inside = true;
            }
            continue;
        }
        if line == "}" {
            break; // the struct's own closing brace, at column 0
        }
        out.push(line.to_string());
    }
    assert!(!out.is_empty(), "the scan did not find OrchRegistry's body at all");
    out
}

#[test]
fn every_lock_on_the_registry_is_a_tracked_one() {
    // Default-deny on a name-independent axis: the field's declared TYPE. A
    // guard that decided from a field's name would be stepped over by a rename
    // and would enforce nothing (CLAUDE.md's source-scanning-guard rule).
    let body = registry_struct_body();
    let mut tracked = 0usize;
    let mut plain = Vec::new();
    for line in &body {
        let t = line.trim_start();
        let Some((name, ty)) = t.split_once(": ") else { continue };
        if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            continue;
        }
        // Any `Mutex<` that is not a `TrackedMutex<`, wherever it sits in the
        // type. Anchoring on the type's PREFIX would have missed
        // `std::sync::Mutex<..>` and `parking_lot::Mutex<..>` — the two
        // spellings a reader reaching for a plain mutex is most likely to
        // write, since the bare name is no longer imported in that file.
        let mutexes = ty.matches("Mutex<").count();
        let tracked_here = ty.matches("TrackedMutex<").count();
        if mutexes > tracked_here {
            plain.push(name.to_string());
        } else if tracked_here > 0 {
            tracked += 1;
        }
    }
    assert!(
        plain.is_empty(),
        "these registry fields are still plain `Mutex`, so a hold on one is invisible to the \
         watchdog: {plain:?}. Use `TrackedMutex::new(\"<field name>\", ..)` — the name is what a \
         breadcrumb reads, so it is required rather than derived"
    );
    // The vacuity control. Every assertion above is satisfied by a scan that
    // matched nothing at all, and the interesting failure mode of a textual
    // scan is that it stopped matching.
    assert!(
        tracked >= 80,
        "the scan found only {tracked} tracked registry locks; it has stopped reading the struct"
    );

    // THE RESIDUAL, stated where the scan lives rather than left to be found.
    //
    // This reads each field's DECLARED TYPE, one line per field. Three shapes
    // are therefore invisible to it, and the third is not hypothetical:
    //
    //  1. a field whose type is written across two lines;
    //  2. a type ALIAS (`type Guarded<T> = Mutex<T>;`) or a macro-produced field;
    //  3. **a field whose type is a struct that OWNS mutexes.** The declared
    //     type is a plain name, and the locks are a level down.
    //
    // Class 3 had two live instances when this scan was written, and the note
    // here used to claim the count above was the whole population — which was
    // false, and false in the direction that matters (#1605 review N1). They
    // are named rather than counted, because a number cannot be checked:
    //
    //  - `usage_cursors: TranscriptCursors` (`usage.rs`) owns two, and they
    //    were the pair most worth finding: on `orch_group_usage`, one of the
    //    ten polled reads, with `fs::metadata` and `fold_appended` INSIDE the
    //    per-transcript guard. Both are `TrackedMutex` now.
    //  - `roots: Arc<RootRegistry>` (`rootreg.rs`) owns an `RwLock`, which this
    //    scan would not match even at the top level. Left alone: its critical
    //    sections are a `BTreeSet` insert and a prefix comparison, with no IO
    //    and no poll path, so a hold report on it would never say anything.
    //
    // What the compiler does NOT do here, which is why the scan exists at all:
    // a plain `Mutex` has no inherent `lock_safe`, so a field that evades this
    // scan falls through to the `LockExt` trait and keeps compiling. The
    // failure is silent, not loud.
}

#[test]
fn every_registry_lock_is_constructed_with_a_name() {
    // **Default-deny over CONSTRUCTORS, not a list of the ones that exist.**
    // This scan used to match the literal `TrackedMutex::new(` and nothing
    // else. #1610 added `new_ranked` and moved seventeen of this file's
    // constructions onto it, and the scan's count fell from 85 to 68 while
    // every one of them still passed a literal name — a guard that had gone
    // blind to a fifth of the struct and could only say so through its own
    // vacuity floor. (Counted on the blobs: `git show <base>:` has 85
    // `TrackedMutex::new("`; head has 68 plus 17 `new_ranked("`.)
    //
    // So it now reads every `TrackedMutex::new` occurrence and classifies what
    // FOLLOWS it. A third constructor added later is refused here by default,
    // rather than quietly falling outside the scan.
    const CTOR: &str = "TrackedMutex::new";
    let src = read("src-tauri/src/orchestration/mod.rs");
    let mut named = 0usize;
    let mut ranked = 0usize;
    let mut unnamed: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for (at, _) in src.match_indices(CTOR) {
        let rest = &src[at + CTOR.len()..];
        let line = || src[at..].lines().next().unwrap_or_default().trim().to_string();
        if rest.starts_with("(\"") {
            named += 1;
        } else if rest.starts_with("_ranked(\"") {
            ranked += 1;
        } else if rest.starts_with('(') || rest.starts_with("_ranked(") {
            // A call, with something other than a string literal for a name.
            unnamed.push(line());
        } else if rest.starts_with('_') && !rest.starts_with("_ranked") {
            // `TrackedMutex::new_something` — a constructor this scan has
            // never heard of. Prose mentions of `new_ranked` land above and
            // are not flagged; a prose mention of a genuinely new one is
            // flagged, which is loud in the right direction.
            unknown.push(line());
        }
        // Anything else (`` `TrackedMutex::new` `` in a doc comment) is a
        // mention rather than a call.
    }
    assert!(
        unnamed.is_empty(),
        "a TrackedMutex built without a literal name: {unnamed:?}. The name is the first half of \
         a readable hold report (`mq_state_lock held 340 s by queue_merge_with`)"
    );
    assert!(
        unknown.is_empty(),
        "a TrackedMutex constructor this scan does not know: {unknown:?}. Teach it here — a \
         constructor outside the classification is a field the scan silently stops covering"
    );
    // The vacuity control, and it is now per-CONSTRUCTOR. A single total is
    // satisfied by one constructor going to zero while the other absorbs it,
    // which is exactly what happened in #1610.
    assert!(
        named + ranked >= 80,
        "the scan found only {named} named + {ranked} ranked constructions; it has stopped reading"
    );
    assert!(named > 0 && ranked > 0, "one constructor matched nothing: {named} named, {ranked} ranked");
}

#[test]
fn there_is_exactly_one_door_onto_the_blocking_pool() {
    // #1601 Phase 0.3. A pool-depth reading is a diagnosis only if it is
    // COMPLETE — `in-flight 480 plus however many sites nobody wrapped` is not
    // one — so completeness is pinned here rather than left to whoever
    // remembers to wrap the next hand-off.
    let mut offenders = Vec::new();
    let mut files = 0usize;
    let mut seen_marker = false;
    let mut stack = vec![repo_root().join("src-tauri").join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src-tauri/src must be readable") {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            files += 1;
            let text = std::fs::read_to_string(&path).expect("readable file");
            let rel = path.strip_prefix(repo_root()).unwrap().to_string_lossy().replace('\\', "/");
            for (i, line) in text.lines().enumerate() {
                // The CALL, not a mention: a doc comment naming the runtime
                // function is prose, and this scan is about hand-offs.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if line.contains("spawn_blocking(") {
                    if rel == "src-tauri/src/blocking.rs" {
                        seen_marker = true;
                    } else {
                        offenders.push(format!("{rel}:{}", i + 1));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these hand-offs reach the blocking pool without being counted: {offenders:?}. Call \
         `blocking::spawn_counted` instead — it returns exactly what awaiting a spawn_blocking \
         handle returns, so it is a substitution and not a policy (#1601 Phase 0.3)"
    );
    // Two vacuity controls, because an empty offender list is byte-identical to
    // a scan that read nothing: the walk saw the tree, and it can still find the
    // one call it is written to permit.
    assert!(files > 20, "the walk found only {files} .rs files; it is not reading src-tauri/src");
    assert!(seen_marker, "the walk never found the one permitted call in blocking.rs");
}

#[test]
fn a_real_registry_lock_shows_up_held_with_its_name() {
    // The end-to-end shape: a REAL registry lock, held for a real interval by a
    // real background thread, read by the watchdog's own snapshot. Everything
    // above this point tests one layer; this is the only test that says the
    // layers are connected to each other and to `OrchRegistry`.
    let (reg, _dir) = test_registry();

    assert!(
        !reg.hold_lock_for_test("no_such_lock", 10),
        "an unknown lock name must be refused rather than silently doing nothing"
    );
    assert!(reg.hold_lock_for_test("mq_state_lock", 400), "the hold must be established");

    let held = lockwatch::held_locks(lockwatch::mono_ms());
    let s = held
        .iter()
        .find(|s| s.name == "mq_state_lock")
        .expect("a held registry lock must be visible to the watchdog's snapshot");
    assert!(
        s.site_file.contains("orchestration"),
        "and must name the acquisition site inside the orchestration module, got {}",
        s.site_file
    );

    // The name list is the surface a diagnostic dump reads, and the later
    // phases' lock-order work keys on.
    let names = lockwatch::tracked_lock_names();
    for expected in ["groups", "agents", "mq_state_lock", "tasks_lock", "queues", "queue_draining"] {
        assert!(names.contains(&expected), "{expected} is not a tracked lock: {names:?}");
    }

    wait_for("the injected hold to end", || {
        !lockwatch::held_locks(lockwatch::mono_ms()).iter().any(|s| s.name == "mq_state_lock")
    });
}

#[test]
fn a_watchdog_report_is_one_line_a_human_can_read() {
    let r = HoldReport {
        lock: "mq_state_lock",
        site_file: "src/orchestration/mod.rs",
        site_line: 35_967,
        holder_thread: 12,
        held_ms: 340_123,
        waiters: 87,
        still_held: true,
        permitted: false,
    };
    assert_eq!(r.event(), "lock-slow");
    assert_eq!(
        r.detail(),
        "lock=mq_state_lock held_ms=340123 waiters=87 thread=12 at=src/orchestration/mod.rs:35967"
    );
    // A breadcrumb is `stamp event detail` split on spaces, so a value carrying
    // one would turn one record into a reader's guess about where a field ends.
    let spacey = HoldReport { site_file: "C:/Program Files/x.rs", ..r.clone() };
    assert!(
        spacey.detail().ends_with("at=C:/Program_Files/x.rs:35967"),
        "a path with a space must not split the detail field: {}",
        spacey.detail()
    );
}
