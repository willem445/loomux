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

    let reports = lockwatch::recent_reports();
    let r = reports
        .iter()
        .find(|r| r.lock == "dropspec")
        .expect("a hold past the threshold must be reported when it ends");
    assert!(!r.still_held, "a hold reported by the guard's drop has ended");
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
    assert!(
        !lockwatch::recent_reports().iter().any(|r| r.lock == "dropspec"),
        "a hold under the threshold must not be reported"
    );
    drop(restore);
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
    // This reads one line per field, so a field whose type is written across
    // two lines is invisible to it, as is one behind a type ALIAS
    // (`type Guarded<T> = Mutex<T>;`) or produced by a macro. None appears in
    // the struct today — the count above is the whole population — and the
    // compiler is what would catch the consequence anyway: a plain `Mutex` has
    // no inherent `lock_safe`, so a field that evaded this scan would fall
    // through to the `LockExt` trait and keep compiling. That last clause is
    // exactly why the scan is worth having: the failure is SILENT, not loud.
}

#[test]
fn every_registry_lock_is_constructed_with_a_name() {
    let src = read("src-tauri/src/orchestration/mod.rs");
    let unnamed: Vec<&str> = src
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("TrackedMutex::new(") && !l.contains("TrackedMutex::new(\""))
        .collect();
    assert!(
        unnamed.is_empty(),
        "a TrackedMutex built without a literal name: {unnamed:?}. The name is the first half of \
         a readable hold report (`mq_state_lock held 340 s by queue_merge_with`)"
    );
    let named = src.matches("TrackedMutex::new(\"").count();
    assert!(named >= 80, "the scan found only {named} named constructions; it has stopped reading");
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
