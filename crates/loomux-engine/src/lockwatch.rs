//! Instrumented mutexes (#1601, plan §3 Phase 0.1/0.2) — what was holding a
//! lock when the app stopped answering — and, since #1609 (Phase 2.1), a
//! bound on how long a waiter pays for it; and since #1610 (Phase 3a), a
//! runtime check that the order they are taken in is the order that was
//! declared.
//!
//! # Why this exists
//!
//! Three betas in a row shipped a responsiveness fix derived from *reading*
//! code, and each relocated the symptom instead of removing it
//! (`doc/plans/responsiveness-root-cause.md` §1). The reason a reading was the
//! best evidence available is that plan's §2.3: a hang produces no artifact at
//! all. `obs` writes a crash log when the process dies, and a process wedged on
//! a mutex does not die — so the total evidence for the last three incidents
//! was a human saying "it froze".
//!
//! This module is the missing artifact. Phase 0 fixed nothing, bounded
//! nothing and changed no behaviour; it made the state a hang leaves behind
//! *reportable*, so the next remedy would be chosen against evidence instead
//! of against a story that fits.
//!
//! #1609 is that next remedy, and it lives here too because it is the same
//! object seen from the other side: [`TrackedMutex::lock_within`] and
//! [`Busy`] turn one waiter's unbounded park into a typed answer that NAMES
//! the holder the instrument above already knows about. The policy half —
//! the thread-local budget, [`crate::budget::MutationScope`] and the six
//! budget constants — is [`crate::budget`], and the contract both halves
//! publish is `doc/design/lock-liveness.md`.
//!
//! # The shape
//!
//! [`TrackedMutex`] is `parking_lot::Mutex` plus a small block of atomics,
//! and its [`lock_safe`](TrackedMutex::lock_safe) has the SAME signature as
//! [`crate::obs::LockExt::lock_safe`] — so the call sites in
//! `orchestration/mod.rs` are untouched and the migration was a type swap on
//! the registry's fields. (Phase 0 shipped it over std's `Mutex`; #1609
//! swapped the inner primitive for the one that has a timed acquire. The
//! one observable difference is poisoning, which parking_lot does not have
//! at all — see [`TrackedMutex`]'s own note for why that is the same
//! outcome `lock_safe`'s `into_inner` recovery already produced.)
//!
//! Two things are reported, and they are complementary rather than redundant:
//!
//! - **A hold that ENDED** past the threshold, with its exact total. The guard's
//!   `Drop` stamps it; [`drain_completed_holds`] composes and writes it. This
//!   cannot miss a completed hold, however briefly the watchdog happened to be
//!   looking elsewhere.
//! - **A hold STILL in flight** ([`LockWatch::tick`]). That is the case a drop
//!   can never report, and it is the beta6 case: a hold that never ends has no
//!   drop to run.
//!
//! Together they cover every hold past the threshold — one of the two always
//! fires, and neither depends on the other being wired.
//!
//! Both are *composed* on the watchdog thread ([`crate::selfwatch`]), and that
//! split is the point rather than an implementation detail: see
//! [`TrackedGuard::drop`].
//!
//! # What acquiring and releasing cost
//!
//! Both paths are a fixed number of atomic operations on this lock's own cache
//! line, one monotonic clock read, and — since #1610 — one push or pop on a
//! thread-local fixed array. There is **no allocation, no formatting, no global
//! lock and no syscall on either** — the global registry is touched at
//! CONSTRUCTION only, the held-lock stack is per-thread and never shared, and
//! every byte of every report is produced on the watchdog thread, with no
//! tracked lock held. See [`TrackedMutex::lock_safe`] and
//! [`TrackedGuard::drop`].
//!
//! A lock-ORDER finding (#1610) writes no file from the acquiring thread
//! either: it stamps one slot in atomics and the watchdog composes it, which
//! is the same split, for the same reason. See [`stamp_order_report`]. The
//! exceptions are the PANICS — the debug panic on an inversion, and (#1702) a
//! re-entrant acquisition's refusal in any build — because a panic runs `obs`'s
//! hook, and that writes a crash log while the outer lock is still held. Both
//! are threads that are leaving rather than continuing; see
//! [`TrackedMutex::refuse_reentrant`], which states the cost.
//!
//! It was not always so, and the reasoning that got it wrong is worth keeping.
//! The release path used to write its own report, excused by "a hold that has
//! already lasted seconds is not a hot path". That is true of the HOLDER and
//! false of everyone else: a hold long enough to report is precisely the one
//! with waiters queued behind it, so the file write landed on every waiter's
//! latency path — the path this whole plan is about (#1605 review B1).
//!
//! # Reading a hold coherently
//!
//! The holder publishes several fields that must be read as one record, and the
//! watchdog reads them from another thread without taking the mutex — taking it
//! would make the observer wait on exactly the thing it is observing, which is
//! the one behaviour a hang instrument may never have. So the fields are
//! published behind a generation counter (even = free, odd = held) in the
//! seqlock shape: the holder writes the fields, then publishes the generation
//! with a `Release` store; a reader takes the generation with `Acquire`, reads
//! the fields, and takes the generation again, discarding the sample if it
//! moved. The mutex itself serialises the writers, so there is exactly one
//! writer per generation and no CAS loop is needed.

use crate::budget;
use crate::obs::LockExt;
// The inner primitive (#1609). Aliased rather than imported under its own
// name so the std `Mutex` below — this module's OWN registry and report
// ring, which are not tracked and must not be — still reads as std's at a
// glance.
use parking_lot::{Mutex as InnerMutex, MutexGuard as InnerGuard};
use std::panic::Location;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// How long a hold must last before it is reported, by default.
///
/// Five seconds, from the plan's §3 Phase 0.2. It is far above anything this
/// app does deliberately — the longest *intended* hold in the registry is a
/// small-file read-modify-write — and far below the minutes a pool-exhausting
/// hold runs for, so it separates "a slow tick" from "the defect" without
/// needing to be tuned first.
pub const DEFAULT_HOLD_WARN_MS: u64 = 5_000;

/// Most recent hold reports kept in memory for a diagnostic dump. Small: this
/// is a tail, not a log — the durable record is the breadcrumb.
pub const REPORT_RING: usize = 64;

/// Prune the lock registry's dead entries when it passes this length. Far above
/// the app's real population (the registry's own ~85 plus one per live pane and
/// group), so a shipped build never reaches it — see [`TrackedMutex::new`] for
/// the process this is actually for.
const REGISTRY_PRUNE_AT: usize = 4096;

/// The process's monotonic zero. `Instant` is not storable in an atomic, so
/// every stamp here is milliseconds since this baseline.
static BASELINE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Milliseconds since process start, monotonic. One `QueryPerformanceCounter`
/// (or `clock_gettime`) — tens of nanoseconds, and immune to a wall-clock step,
/// which matters because every duration here is a *hold*, not a date.
pub fn mono_ms() -> u64 {
    BASELINE.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Mints the per-lock ids. Monotonic and process-local — no getrandom
/// (CLAUDE.md constraint 2), the same std-only shape `OrchRegistry`'s own
/// counters already use.
static NEXT_LOCK_ID: AtomicU64 = AtomicU64::new(1);

/// Mints the per-thread ids below.
static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// This thread's id, assigned on its first tracked acquisition.
    ///
    /// A counter of our own rather than `ThreadId::as_u64` (unstable) or the
    /// thread's NAME. A name would read better in a breadcrumb, but the MCP
    /// server spawns one thread per request (`orchestration/mcp.rs`), so a
    /// per-thread `String` retained anywhere would be unbounded over a session
    /// — and the diagnosis this module exists for is carried by the call SITE,
    /// with the id only distinguishing one holder from another.
    ///
    /// `Cell<u64>` has no destructor, so this costs a thread-local read and
    /// nothing at thread teardown.
    static THREAD_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn this_thread() -> u64 {
    THREAD_ID.with(|c| {
        let v = c.get();
        if v != 0 {
            return v;
        }
        let fresh = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
        c.set(fresh);
        fresh
    })
}

/// The observable state of one tracked lock. Lives behind an `Arc` so the
/// registry can hold a `Weak` and a dropped lock leaves no entry behind.
struct LockState {
    id: u64,
    name: &'static str,
    /// Even = free, odd = held. Bumped once on acquire and once on release, so
    /// each hold owns exactly one odd value and a reader can tell two
    /// consecutive holds apart. See the module's "Reading a hold coherently".
    generation: AtomicU64,
    holder_thread: AtomicU64,
    /// The `#[track_caller]` site of the acquisition, as a pointer to the
    /// compiler-emitted `Location`. Null until the first acquisition.
    holder_site: AtomicPtr<Location<'static>>,
    acquired_ms: AtomicU64,
    /// Threads currently blocked in `lock_safe` on this lock. Incremented
    /// BEFORE the blocking call, so a waiter is counted while it waits rather
    /// than once it has stopped waiting.
    waiters: AtomicU32,

    // ---- the completed-hold slot (#1605 review B1) ----
    //
    // A hold that ENDS past the threshold is news, and composing that news is
    // allocation, formatting, a process-global lock and a file write. None of
    // that may happen on the release path, because `Drop::drop` runs BEFORE the
    // struct's fields drop and `TrackedGuard.guard` is a field — so anything
    // done there is done with the reported mutex still locked, and every waiter
    // queued behind it pays for it. A hold that has already lasted seconds is
    // exactly the one with waiters queued behind it, so "not a hot path" was
    // the wrong exculpation: it is not the HOLDER's latency path, it is every
    // WAITER's, and that is the path this whole plan is about.
    //
    // So the release path only STAMPS, in atomics, and [`drain_completed_holds`]
    // — called from the watchdog thread, holding no tracked lock — does the
    // composing. This is what the plan's §3 Phase 0.2 says anyway: the watchdog
    // reports, "off every hot path".
    done_pending: AtomicBool,
    done_ms: AtomicU64,
    done_thread: AtomicU64,
    done_site: AtomicPtr<Location<'static>>,
    done_waiters: AtomicU32,

    /// The hold generation this lock last wrote a `lock-busy` breadcrumb
    /// for (#1609). `u64::MAX` = never; `0` = a busy whose holder could not
    /// be sampled. See [`TrackedMutex::busy`] for why the two sentinels
    /// differ.
    busy_reported_gen: AtomicU64,
    /// How many `lock-busy` / `lock-busy-in-mutation` breadcrumbs this lock
    /// has actually written. The edge-trigger above is a claim about a COUNT
    /// ("once per hold, not once per waiter"), and a claim about a count
    /// needs a count to check it — parsing the breadcrumb file would make
    /// every test that asserts it depend on the log-dir override and on
    /// nothing else in the process writing at the same time.
    busy_breadcrumbs: AtomicU64,

    /// This lock's position in the declared acquisition order (#1610), or
    /// [`UNRANKED`] for one built with [`TrackedMutex::new`]. Written once at
    /// construction and never again, so every read of it is `Relaxed` and no
    /// ordering question arises.
    rank: u32,
    /// Whether this lock's first nesting under another has already been
    /// reported. Only meaningful while `rank == UNRANKED`; see
    /// [`note_unranked_nesting`].
    unranked_reported: AtomicBool,

    // ---- the deferred lock-order report slot (#1610) ----
    //
    // Same shape, and the same reason, as the completed-hold slot above: a
    // lock-order finding is MADE on the acquiring thread with the outer lock
    // still held, and composing one is allocation, formatting, a
    // process-global lock and a file write. See `stamp_order_report`.
    /// `REPORT_NONE` when empty; otherwise what `drain_lock_order_reports`
    /// should compose.
    report_kind: AtomicU8,
    /// The acquiring site, and the site of the hold it collided with.
    report_site: AtomicPtr<Location<'static>>,
    report_outer_site: AtomicPtr<Location<'static>>,
    /// The collided hold's rank, or `UNRANKED`.
    report_outer_rank: AtomicU32,
}

impl LockState {
    fn new(name: &'static str, rank: u32) -> Self {
        Self {
            id: NEXT_LOCK_ID.fetch_add(1, Ordering::Relaxed),
            name,
            rank,
            unranked_reported: AtomicBool::new(false),
            report_kind: AtomicU8::new(REPORT_NONE),
            report_site: AtomicPtr::new(std::ptr::null_mut()),
            report_outer_site: AtomicPtr::new(std::ptr::null_mut()),
            report_outer_rank: AtomicU32::new(UNRANKED),
            generation: AtomicU64::new(0),
            holder_thread: AtomicU64::new(0),
            holder_site: AtomicPtr::new(std::ptr::null_mut()),
            acquired_ms: AtomicU64::new(0),
            waiters: AtomicU32::new(0),
            done_pending: AtomicBool::new(false),
            done_ms: AtomicU64::new(0),
            done_thread: AtomicU64::new(0),
            done_site: AtomicPtr::new(std::ptr::null_mut()),
            done_waiters: AtomicU32::new(0),
            busy_reported_gen: AtomicU64::new(u64::MAX),
            busy_breadcrumbs: AtomicU64::new(0),
        }
    }

    /// Take the pending completed-hold report, if there is one.
    ///
    /// The `swap` is what makes this exactly-once across any number of
    /// concurrent drainers, and the `Acquire` pairs with the `Release` store in
    /// [`TrackedGuard::drop`] that published the four fields below it.
    fn take_completed(&self) -> Option<HoldReport> {
        if !self.done_pending.swap(false, Ordering::Acquire) {
            return None;
        }
        let (file, line) = site_of(self.done_site.load(Ordering::Relaxed));
        Some(HoldReport {
            lock: self.name,
            site_file: file,
            site_line: line,
            holder_thread: self.done_thread.load(Ordering::Relaxed),
            held_ms: self.done_ms.load(Ordering::Relaxed),
            waiters: self.done_waiters.load(Ordering::Relaxed),
            still_held: false,
        })
    }

    /// A coherent read of this lock's hold, or `None` if it is free (or if the
    /// hold changed underneath the read).
    ///
    /// Never blocks and never touches the mutex itself — an instrument that
    /// waited on the lock it is reporting would be the first casualty of the
    /// hang it exists to describe.
    fn sample(&self, now_ms: u64) -> Option<LockSnapshot> {
        let g1 = self.generation.load(Ordering::Acquire);
        if g1 % 2 == 0 {
            return None; // free
        }
        let thread = self.holder_thread.load(Ordering::Relaxed);
        let site = self.holder_site.load(Ordering::Relaxed);
        let since = self.acquired_ms.load(Ordering::Relaxed);
        let waiters = self.waiters.load(Ordering::Relaxed);
        if self.generation.load(Ordering::Acquire) != g1 {
            return None; // the hold ended (or was replaced) mid-read
        }
        let (file, line) = site_of(site);
        Some(LockSnapshot {
            id: self.id,
            name: self.name,
            generation: g1,
            holder_thread: thread,
            site_file: file,
            site_line: line,
            held_ms: now_ms.saturating_sub(since),
            waiters,
        })
    }
}

/// Resolve a stored `Location` pointer back to `(file, line)`.
fn site_of(p: *mut Location<'static>) -> (&'static str, u32) {
    if p.is_null() {
        return ("<unknown>", 0);
    }
    // SAFETY: the only value ever stored in `holder_site` is a pointer taken
    // from `Location::caller()`, which returns `&'static Location<'static>` — a
    // reference to a constant the compiler emits into the binary's read-only
    // data. It is valid for the whole program, is never written through, and
    // `Location<'static>` is `Sync` (three `Copy` fields), so reading it from
    // the watchdog thread is sound. Null is the "never acquired" sentinel and
    // is filtered above.
    let loc: &'static Location<'static> = unsafe { &*(p as *const Location<'static>) };
    (loc.file(), loc.line())
}

/// Every live tracked lock. Touched at CONSTRUCTION and by the watchdog's
/// snapshot — never on an acquire or a release.
static REGISTRY: Mutex<Vec<Weak<LockState>>> = Mutex::new(Vec::new());

/// The threshold the guard's `Drop` reports past. A global rather than a field
/// so a test (and, later, a diagnostic setting) can move it without touching
/// the construction sites.
static HOLD_WARN_MS: AtomicU64 = AtomicU64::new(DEFAULT_HOLD_WARN_MS);

/// The current report threshold, in milliseconds.
pub fn hold_warn_ms() -> u64 {
    HOLD_WARN_MS.load(Ordering::Relaxed)
}

/// Set the report threshold. Process-wide.
pub fn set_hold_warn_ms(ms: u64) {
    HOLD_WARN_MS.store(ms, Ordering::Relaxed);
}

/// The tail of recent hold reports, for a diagnostic dump and for tests.
static REPORTS: Mutex<Vec<HoldReport>> = Mutex::new(Vec::new());

/// One over-threshold hold, as reported by either the guard's `Drop` or the
/// watchdog. `still_held` is the difference that matters to a reader: a
/// released hold is a slow path, an unreleased one is a hang.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldReport {
    pub lock: &'static str,
    pub site_file: &'static str,
    pub site_line: u32,
    pub holder_thread: u64,
    pub held_ms: u64,
    pub waiters: u32,
    /// `true` when the hold was still in flight when this was reported.
    pub still_held: bool,
}

impl HoldReport {
    /// The breadcrumb `event` this report is written under.
    pub fn event(&self) -> &'static str {
        if self.still_held {
            "lock-slow"
        } else {
            "lock-freed"
        }
    }

    /// The breadcrumb `detail`: one line of `key=value` fields.
    ///
    /// A breadcrumb line is `stamp event detail` split on spaces, so every
    /// value here is emitted space-free — a path with a space in it would
    /// otherwise turn one record into two fields' worth of confusion.
    pub fn detail(&self) -> String {
        format!(
            "lock={} held_ms={} waiters={} thread={} at={}:{}",
            spaceless(self.lock),
            self.held_ms,
            self.waiters,
            self.holder_thread,
            spaceless(self.site_file),
            self.site_line,
        )
    }
}

fn spaceless(s: &str) -> String {
    s.chars().map(|c| if c == ' ' || (c as u32) < 0x20 { '_' } else { c }).collect()
}

/// Record a report: one breadcrumb, plus the in-memory tail.
///
/// **Only ever called from the watchdog thread**, which holds no tracked lock.
/// That is the whole of why this is allowed to allocate, format, take a global
/// lock and write a file: none of it happens while any reported mutex is held.
/// It used to be called from `TrackedGuard::drop` — see the completed-hold slot
/// on [`LockState`] for why that was wrong, and for the shape that replaced it.
fn record(report: HoldReport) {
    crate::obs::breadcrumb(report.event(), &report.detail());
    let mut ring = REPORTS.lock_safe();
    if ring.len() >= REPORT_RING {
        ring.remove(0);
    }
    ring.push(report);
}

/// The recent hold reports, oldest first.
pub fn recent_reports() -> Vec<HoldReport> {
    REPORTS.lock_safe().clone()
}

/// Drop every recorded report (tests, and a future "start a fresh capture").
pub fn clear_reports() {
    REPORTS.lock_safe().clear();
}

/// Hold the report ring for `ms` on a thread of its own, returning once it is
/// actually held.
///
/// Test seam (#1605 review B1). The property under test — the release path
/// takes no process-global lock — is only observable by making that lock
/// unavailable and watching a release happen anyway. Reading the code proves
/// nothing here: the defect this closes was invisible for exactly as long as
/// everyone read `Drop::drop` as running after the guard's fields dropped.
#[doc(hidden)]
pub fn hold_report_ring_for_test(ms: u64) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let _held = REPORTS.lock_safe();
        let _ = tx.send(());
        std::thread::sleep(std::time::Duration::from_millis(ms));
    });
    let _ = rx.recv();
}

// ---------------------------------------------------------------------------
// Bounded acquisition (#1609, plan §3 Phase 2.1)
// ---------------------------------------------------------------------------

/// A fixed backoff hint, in milliseconds, handed to a caller that got [`Busy`].
///
/// Deliberately a constant rather than a prediction. Nothing here knows when
/// the holder will release — that is the whole point of the failure this bounds
/// — and the obvious derivation is worse than useless: a number scaled DOWN
/// from how long the lock has already been held says "try again sooner" exactly
/// when the evidence says the opposite. Five seconds is short enough that a
/// transient contention spike costs one retry and long enough that an agent
/// retrying a wedged registry is not a busy-wait.
pub const BUSY_RETRY_AFTER_MS: u64 = 5_000;

/// Who was holding a lock when someone else's budget ran out.
///
/// `Option`al on [`Busy`] because it is sampled with the seqlock read that
/// never blocks (see the module's "Reading a hold coherently"): if the hold
/// ended or was replaced mid-read the honest answer is "not sampled", not a
/// torn one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HolderInfo {
    /// The `#[track_caller]` site the holder acquired from.
    pub site_file: &'static str,
    pub site_line: u32,
    /// How long the holder had held it when the waiter gave up.
    pub held_ms: u64,
    /// The holder's tracked thread id (see `THREAD_ID`).
    pub thread: u64,
}

/// Why an acquisition was refused (#1610).
///
/// The plan's 3a sketch spells the re-entrant answer `Busy::Reentrant`, which
/// reads as an enum VARIANT. It is a `kind` on the struct instead, and the
/// reason is that the two answers carry the same evidence: both name the lock,
/// the holder's site and how long that hold has run — for a re-entrant
/// acquisition the "holder" is this thread's own earlier frame, which is
/// precisely the field a reader needs. Splitting the struct in two would have
/// duplicated all four fields to distinguish them, and rewritten every consumer
/// (`mcp.rs`'s `rpc_busy` / `busy_tool_text`, `views.rs`'s fallbacks,
/// `budget.rs`'s unwind payload) to match on a shape whose halves are the same.
/// The deviation is recorded in `doc/design/lock-liveness.md` §3, and the rank
/// mechanism it belongs to in `doc/design/lock-order.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusyKind {
    /// The budget ran out while someone ELSE held the lock. Phase 2.1's
    /// ordinary case: contention, and a retry can succeed.
    Timeout,
    /// This THREAD already holds the lock. A defect rather than contention —
    /// the inner primitive is not re-entrant, so the acquisition that was
    /// refused here would have self-deadlocked permanently. #1600 §1.2 names
    /// this case as the one invisible to an inversion search, because one lock
    /// is no cycle.
    Reentrant,
}

/// A tracked-lock acquisition that was refused — because it ran out of budget,
/// or (#1610) because this thread already holds the lock.
///
/// This is the typed value the whole of Phase 2.1 converts a hang into: a
/// polled view keeps its previous value and marks itself partial, an MCP call
/// answers a retryable error, a cadenced tick skips. Every field is here so the
/// answer can NAME the cause rather than say "busy" — the epic's §2.3 is that
/// four incidents produced no evidence at all, and an unexplained "busy" is the
/// same defect one layer up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Busy {
    /// Why this acquisition was refused.
    pub kind: BusyKind,
    /// The lock's name, as given to [`TrackedMutex::new`].
    pub lock: &'static str,
    /// How long this waiter actually waited before giving up. Not the budget:
    /// a nested acquisition inherits whatever the frame had LEFT, so the two
    /// differ and the measured one is the one a reader can act on.
    pub waited: Duration,
    /// The holder, when it could be sampled without waiting.
    pub holder: Option<HolderInfo>,
    /// Threads still blocked on this lock, NOT counting the one this `Busy`
    /// answers — that waiter has already stopped waiting.
    pub waiters: usize,
}

impl Busy {
    /// The backoff hint, in milliseconds. See [`BUSY_RETRY_AFTER_MS`].
    ///
    /// The same constant for both [`BusyKind`]s, and that is a judgement rather
    /// than an oversight. A re-entrant refusal is a defect that a retry of the
    /// *same* call cannot clear — but the caller this reaches is an MCP request
    /// or a cadenced tick, and its retry is a fresh request on a fresh thread
    /// with an empty held-lock stack, which is the only sense in which any
    /// `Busy` was ever "retryable". Nothing here knows enough to say otherwise,
    /// and a `retry_after_ms` of 0 would say "hammer it".
    pub fn retry_after_ms(&self) -> u64 {
        BUSY_RETRY_AFTER_MS
    }

    /// Whether this refusal is a re-entrant self-acquisition (#1610) rather
    /// than contention.
    pub fn is_reentrant(&self) -> bool {
        matches!(self.kind, BusyKind::Reentrant)
    }

    /// The breadcrumb `detail`: one line of `key=value` fields.
    ///
    /// Space-free values throughout, for the same reason
    /// [`HoldReport::detail`] is: a breadcrumb line is `stamp event detail`
    /// split on spaces.
    pub fn detail(&self) -> String {
        // The `kind` leads, so a grep for `lock-busy` in a field report can be
        // split into contention and defect without parsing the rest.
        let kind = match self.kind {
            BusyKind::Timeout => "timeout",
            BusyKind::Reentrant => "reentrant",
        };
        match &self.holder {
            Some(h) => format!(
                "kind={} lock={} waited_ms={} waiters={} held_ms={} thread={} at={}:{}",
                kind,
                spaceless(self.lock),
                self.waited.as_millis(),
                self.waiters,
                h.held_ms,
                h.thread,
                spaceless(h.site_file),
                h.site_line,
            ),
            None => format!(
                "kind={} lock={} waited_ms={} waiters={} holder=unsampled",
                kind,
                spaceless(self.lock),
                self.waited.as_millis(),
                self.waiters,
            ),
        }
    }
}

impl std::fmt::Display for Busy {
    /// One line, and it is a PUBLIC CONTRACT: this text is what reaches an
    /// agent's context inside `loomux busy: <this>. Nothing was executed; retry
    /// in ~N s.` and what a human reads in the group view's partial badge. See
    /// `doc/design/lock-liveness.md` §3.
    ///
    /// The plan's sketch prefixed it with `registry busy: `. Dropped, because
    /// every caller that renders this already says "busy" in its own first
    /// three words and "loomux busy: registry busy: `agents` …" is a sentence
    /// nobody would write on purpose. The lock's name still leads.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` ", self.lock)?;
        // A re-entrant refusal renders as its OWN sentence rather than as a
        // timeout with odd numbers in it (#1610). The timeout wording ends
        // "waited 0.0 s", which for this case would be true and completely
        // misleading — nothing waited, and nothing is going to free up: the
        // caller is standing on the lock itself. The reader of this line is an
        // agent deciding whether to retry, so the line has to say which of the
        // two situations it is in.
        if self.is_reentrant() {
            return match &self.holder {
                Some(h) => write!(
                    f,
                    "is already held by this same thread, taken {} ago at {}:{} — a re-entrant \
                     acquisition would deadlock, so it was refused",
                    secs(Duration::from_millis(h.held_ms)),
                    h.site_file,
                    h.site_line,
                ),
                None => write!(
                    f,
                    "is already held by this same thread — a re-entrant acquisition would \
                     deadlock, so it was refused"
                ),
            };
        }
        match &self.holder {
            Some(h) => write!(
                f,
                "held {} by {}:{} (thread {})",
                secs(Duration::from_millis(h.held_ms)),
                h.site_file,
                h.site_line,
                h.thread
            )?,
            // Not an error and not a shrug: the holder released or was replaced
            // between the timeout and the sample, which is itself informative.
            None => write!(f, "held by a caller that released before it could be sampled")?,
        }
        write!(f, ", {} waiters; waited {}", self.waiters, secs(self.waited))
    }
}

impl std::error::Error for Busy {}

/// `42.1 s` — one decimal, because these are seconds-scale by construction (a
/// sub-second wait never produces a `Busy`; the tightest budget is 1 s).
fn secs(d: Duration) -> String {
    format!("{:.1} s", d.as_secs_f64())
}

// ---------------------------------------------------------------------------
// Lock ranks (#1610, plan §3 Phase 3a)
// ---------------------------------------------------------------------------
//
// The epic's §3 opens on the thing this closes: 17 mutexes on one struct with
// **no declared lock order**, and `resolve_token`'s own comment saying why —
// "locking them together would pin a lock order no other call site promises to
// respect". Thirteen doc comments in `orchestration/mod.rs` DO state an order.
// A comment cannot fail a build, which is §2.2's whole finding about this
// repo's guard culture, so those thirteen claims are moved here as ranks and
// checked at run time.
//
// The check is per-THREAD and needs no second thread to fire: a cycle between
// two locks deadlocks only when two threads race, but the ordering FACT that
// permits it is visible on one thread the first time it takes them the wrong
// way round. That is why this finds inversions a soak test cannot — it does not
// need the race to happen, only the order.

/// The sentinel a lock built with [`TrackedMutex::new`] carries.
///
/// `u32::MAX` rather than `0`, so `LockRank::new(0)` stays a legal outermost
/// rank. Nothing ever COMPARES this value: every comparison below filters it
/// out explicitly, because "unranked" is an absence of an opinion rather than a
/// position in the order.
const UNRANKED: u32 = u32::MAX;

/// Where a lock sits in the registry's acquisition order.
///
/// **Smaller is OUTER.** A thread may take rank 500 while holding rank 100;
/// taking rank 100 while holding rank 500 is an inversion — the shape that
/// deadlocks the moment another thread does it the declared way round.
///
/// Sparse integers rather than an enum, on purpose: the table in
/// `orchestration::lockorder` leaves gaps between families so a new lock can be
/// slotted between two existing ones without renumbering. A renumbering touches
/// every const at once, and a diff in which every line changed is one no
/// reviewer can read for ORDER — the single property these values carry.
///
/// **Every ranked FIELD has a distinct rank**, which is load-bearing rather
/// than tidy. It makes "two held locks share a rank" mean "the same field, from
/// two different registries" — something a test process really does build,
/// several times per test — instead of "two peers that must never nest". So
/// re-entrancy is decided by lock IDENTITY (the registry id minted in
/// `LockState`), never by rank equality, and equal ranks nest freely.
/// `orchestration::lockorder::ALL`, and the test over it, are what keep the
/// distinctness true.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct LockRank(u32);

impl LockRank {
    /// A rank. `u32::MAX` is the unranked sentinel and is refused — at compile
    /// time for a `const`, which is how every real one is written.
    pub const fn new(v: u32) -> Self {
        assert!(v != UNRANKED, "u32::MAX is reserved for unranked locks");
        Self(v)
    }

    /// The underlying value, for a table's own uniqueness test.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for LockRank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How deep the order checker follows one thread's nesting.
///
/// Thirty-two, against a deepest DECLARED nesting of three (`queues` ->
/// `recovered_queue` -> `recovered_markers`). Past it the checker stops
/// TRACKING rather than starts refusing: an over-deep hold is invisible to the
/// locks taken under it, which is the same fail-open direction
/// [`report_order_violation`] takes in release. A checker that panicked on its
/// own capacity would be an instrument that crashes the app it is watching.
const HELD_STACK_CAP: usize = 32;

/// One acquisition this thread is currently inside, as the checker sees it.
///
/// `Copy`, with only scalar and pointer fields: the stack below is a fixed
/// array, so a push is a store and a pop is a length decrement — no allocation
/// and no destructor. That is what lets this ride the app's hottest shared path
/// (see [`TrackedMutex::lock_safe`]'s cost note, which counts it).
#[derive(Clone, Copy)]
struct HeldEntry {
    /// Per-thread and monotonic. Identifies THIS acquisition, so a guard
    /// dropped out of order removes its own entry rather than whatever is on
    /// top — Rust permits `drop(outer)` while `inner` is still alive, and a
    /// stack that assumed LIFO would then blame the wrong lock for every check
    /// that followed.
    token: u64,
    /// The lock's registry id. Re-entrancy is decided on this — see
    /// [`LockRank`] for why not on rank equality.
    lock_id: u64,
    name: &'static str,
    /// [`UNRANKED`] for a lock built with [`TrackedMutex::new`].
    rank: u32,
    /// The `#[track_caller]` site this hold was taken at, stored as a pointer
    /// for the same reason `LockState`'s `holder_site` is, and read back
    /// through the same [`site_of`].
    site: *const Location<'static>,
    acquired_ms: u64,
}

const EMPTY_HELD: HeldEntry = HeldEntry {
    token: 0,
    lock_id: 0,
    name: "",
    rank: UNRANKED,
    site: std::ptr::null(),
    acquired_ms: 0,
};

/// This thread's held locks, outermost first.
struct HeldStack {
    entries: [HeldEntry; HELD_STACK_CAP],
    len: usize,
    next_token: u64,
}

impl HeldStack {
    const EMPTY: Self = Self { entries: [EMPTY_HELD; HELD_STACK_CAP], len: 0, next_token: 1 };
}

thread_local! {
    /// The tracked locks this thread is inside right now.
    ///
    /// A fixed array in a `RefCell` rather than a `Vec`: a `Vec` would allocate
    /// on the acquisition path, which is the one thing this module's cost
    /// argument forbids everywhere else. `HeldEntry` has no destructor, so this
    /// also costs nothing at thread teardown — which matters because the MCP
    /// server spawns one thread per request.
    static HELD: std::cell::RefCell<HeldStack> = const { std::cell::RefCell::new(HeldStack::EMPTY) };
}

/// What the checker found when a thread reached for one more lock.
enum Verdict {
    /// Nothing to say.
    Clear,
    /// This exact lock is ALREADY held by this thread. Not a contention
    /// question: the inner primitive is not re-entrant, so the acquisition
    /// self-deadlocks permanently — the case #1600 §1.2 names as invisible to
    /// an inversion search, because one lock is no cycle.
    Reentrant(HeldEntry),
    /// A ranked lock is being taken while a strictly INNER one is held.
    Inversion(HeldEntry),
    /// An unranked lock is nesting under something. Not an error — the table
    /// has no opinion about it yet — but it is exactly the fact the table is
    /// missing, so it is reported once so the table can converge.
    UnrankedUnder(HeldEntry),
}

/// Decide what taking `(lock_id, rank)` would mean on this thread, right now.
///
/// Reads the thread-local stack and nothing else: no atomic on another lock's
/// cache line, no clock, no allocation. Returns [`Verdict::Clear`] if the
/// thread-local is unavailable (teardown) or already borrowed — the checker
/// declines to have an opinion rather than panicking inside somebody's `drop`.
fn inspect_held(lock_id: u64, rank: u32) -> Verdict {
    HELD.try_with(|h| {
        let Ok(stack) = h.try_borrow() else {
            return Verdict::Clear;
        };
        if stack.len == 0 {
            return Verdict::Clear;
        }
        // The innermost RANKED hold — a maximum rather than "the last one
        // pushed", so an unranked lock taken between two ranked ones cannot
        // hide the ranked one underneath it.
        let mut innermost: Option<HeldEntry> = None;
        for e in &stack.entries[..stack.len] {
            if e.lock_id == lock_id {
                return Verdict::Reentrant(*e);
            }
            if e.rank != UNRANKED && innermost.map_or(true, |i| e.rank > i.rank) {
                innermost = Some(*e);
            }
        }
        if rank == UNRANKED {
            return Verdict::UnrankedUnder(stack.entries[stack.len - 1]);
        }
        match innermost {
            // Strictly less: equal ranks are two instances of ONE field (two
            // registries in one test process), not two peers. See [`LockRank`].
            Some(i) if rank < i.rank => Verdict::Inversion(i),
            _ => Verdict::Clear,
        }
    })
    .unwrap_or(Verdict::Clear)
}

/// Record a hold on this thread's stack. `None` = not tracked (over capacity,
/// or the thread-local was unavailable), which the guard remembers so its drop
/// pops nothing.
fn push_held(
    lock_id: u64,
    name: &'static str,
    rank: u32,
    site: &'static Location<'static>,
    acquired_ms: u64,
) -> Option<u64> {
    HELD.try_with(|h| {
        let mut stack = h.try_borrow_mut().ok()?;
        if stack.len >= HELD_STACK_CAP {
            return None;
        }
        let token = stack.next_token;
        stack.next_token = token.wrapping_add(1);
        let len = stack.len;
        stack.entries[len] =
            HeldEntry { token, lock_id, name, rank, site: site as *const _, acquired_ms };
        stack.len = len + 1;
        Some(token)
    })
    .ok()
    .flatten()
}

/// Remove the hold `token` names.
///
/// **Runs inside [`TrackedGuard::drop`], with the reported mutex still held**,
/// so it obeys that body's rules: no allocation, no formatting, no global lock,
/// no syscall, and nothing that can panic (`try_with` and `try_borrow_mut`,
/// both of which decline rather than unwind — a panic here during an unwind
/// would be the double-panic that aborts).
///
/// Searches from the top because the overwhelmingly common case IS the top, and
/// shifts only when a guard was dropped out of order.
fn pop_held(token: u64) {
    let _ = HELD.try_with(|h| {
        let Ok(mut stack) = h.try_borrow_mut() else {
            return;
        };
        for i in (0..stack.len).rev() {
            if stack.entries[i].token == token {
                for j in i..stack.len - 1 {
                    stack.entries[j] = stack.entries[j + 1];
                }
                stack.len -= 1;
                return;
            }
        }
    });
}

/// How many tracked locks this thread is inside.
///
/// The stack's own vacuity control: a checker that never pushed anything
/// reports no violation, which is byte-identical to one that found nothing
/// wrong.
#[doc(hidden)]
pub fn held_lock_depth() -> usize {
    HELD.try_with(|h| h.try_borrow().map(|s| s.len).unwrap_or(0)).unwrap_or(0)
}

/// The breadcrumb an inversion writes. A stable string on purpose: it is what a
/// field report gets grepped for.
const ORDER_EVENT: &str = "lock-order-violation";
/// The breadcrumb a re-entrant acquisition writes.
const REENTRANT_EVENT: &str = "lock-reentrant";
/// The breadcrumb an unranked lock's first nesting writes.
const UNRANKED_EVENT: &str = "lock-rank-unranked";

/// Ordering violations and re-entrant acquisitions seen this process.
static LOCK_ORDER_VIOLATIONS: AtomicU64 = AtomicU64::new(0);

/// Whether an INVERSION panics.
///
/// Debug and test builds: yes — a violation is a defect, and the plan asks for
/// a panic naming both locks and both sites. Release: no, and the fail-open is
/// deliberate rather than timid. An inversion is a *possible* deadlock: it
/// needs a second thread taking the same two locks the other way round, at the
/// same moment. Refusing it would convert that possibility into a certain crash
/// on a path nobody has proven wrong, and the crash trail is what this whole
/// epic is for.
///
/// **It does not decide what a RE-ENTRANT acquisition does** (#1702). That one
/// is refused on every path, in every build, because it is a certain deadlock
/// rather than a possible one — see [`TrackedMutex::refuse_reentrant`]. What
/// this flag still chooses for it is the SHAPE of the refusal: armed, a panic
/// that fails the build; disarmed, an unwind to a `read_budget` frame where one
/// exists.
static LOCK_ORDER_PANICS: AtomicBool = AtomicBool::new(cfg!(debug_assertions));

/// Unranked locks that have nested under another lock for the first time.
static UNRANKED_NESTINGS: AtomicU64 = AtomicU64::new(0);

/// Ordering violations and re-entrant acquisitions this process has seen.
pub fn lock_order_violations() -> u64 {
    LOCK_ORDER_VIOLATIONS.load(Ordering::Relaxed)
}

/// Unranked locks that have nested under another lock for the FIRST time —
/// the rows `orchestration::lockorder` does not yet carry.
pub fn unranked_nestings() -> u64 {
    UNRANKED_NESTINGS.load(Ordering::Relaxed)
}

/// Whether an inversion currently panics. See [`LOCK_ORDER_PANICS`] for what
/// it does and does not decide.
pub fn lock_order_panics() -> bool {
    LOCK_ORDER_PANICS.load(Ordering::Relaxed)
}

/// Switch the panic off (or back on), returning the previous setting.
///
/// Process-wide, so a test that flips it flips it for every test running
/// concurrently in the same binary — take a serial guard around it, the way
/// `obs`'s `SERIAL` does. It exists because the RELEASE behaviour is otherwise
/// unreachable from a test, and a path nobody has executed is a path nobody has
/// checked: an inversion's breadcrumb-and-carry-on, and — since #1702 — a
/// re-entrant acquisition's unwind-or-panic, which is the half that must never
/// park whichever way this flag is set.
#[doc(hidden)]
pub fn set_lock_order_panics(on: bool) -> bool {
    LOCK_ORDER_PANICS.swap(on, Ordering::Relaxed)
}

/// `510`, or `unranked`.
fn rank_text(rank: u32) -> String {
    if rank == UNRANKED {
        "unranked".to_string()
    } else {
        rank.to_string()
    }
}

/// What a deferred lock-order report is about ([`OrderReport::kind`]).
///
/// A `u8` rather than an enum because it lives in an atomic; `0` is the empty
/// slot. Public alongside the field, so a caller reading one can classify it
/// without parsing [`OrderReport::event`].
pub const REPORT_NONE: u8 = 0;
/// A ranked lock taken while a strictly INNER one was held.
pub const REPORT_ORDER: u8 = 1;
/// A lock re-acquired by a thread that already holds it.
pub const REPORT_REENTRANT: u8 = 2;
/// An unranked lock nesting under something, for the first time.
pub const REPORT_UNRANKED: u8 = 3;

/// One lock-order finding, composed on the watchdog thread by
/// [`drain_lock_order_reports`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderReport {
    /// The lock being ACQUIRED when the finding was made.
    pub lock: &'static str,
    pub rank: Option<LockRank>,
    pub site_file: &'static str,
    pub site_line: u32,
    /// The hold it collided with. Only its SITE and rank are kept: the name
    /// would need the other lock's `LockState` to still be alive at drain time,
    /// and a `&'static Location` is valid for the whole program (see
    /// [`site_of`]). The site is also the more useful half — it is the line to
    /// go and read.
    pub outer_rank: Option<LockRank>,
    pub outer_site_file: &'static str,
    pub outer_site_line: u32,
    pub kind: u8,
}

impl OrderReport {
    /// The breadcrumb `event` this report is written under.
    pub fn event(&self) -> &'static str {
        match self.kind {
            REPORT_REENTRANT => REENTRANT_EVENT,
            REPORT_UNRANKED => UNRANKED_EVENT,
            _ => ORDER_EVENT,
        }
    }

    /// The breadcrumb `detail`: one line of `key=value` fields, space-free
    /// throughout for [`HoldReport::detail`]'s reason.
    pub fn detail(&self) -> String {
        format!(
            "lock={} rank={} at={}:{} outer_rank={} outer_at={}:{}",
            spaceless(self.lock),
            rank_text(self.rank.map_or(UNRANKED, LockRank::get)),
            spaceless(self.site_file),
            self.site_line,
            rank_text(self.outer_rank.map_or(UNRANKED, LockRank::get)),
            spaceless(self.outer_site_file),
            self.outer_site_line,
        )
    }
}

/// Stamp a finding into this lock's one deferred-report slot.
///
/// **Runs on the acquiring thread, with the outer lock still held**, so it obeys
/// the same rules [`TrackedGuard::drop`] does: four relaxed stores and one
/// release store, no allocation, no formatting, no global lock, no syscall.
///
/// That is the whole reason this is not a `breadcrumb!` call. #1605 review B1
/// found the release path writing its own report and established why it may
/// not: a report is composed precisely when there are waiters queued behind the
/// hold, so the file write lands on every one of THEIR latency paths, which is
/// the path this epic is about. A violation report is the same shape — worse,
/// because a real inversion on a hot path recurs every time that path runs.
///
/// **Its bound, stated because it is a real one.** One slot per lock, so a
/// second finding on the same lock before a drain overwrites the first.
/// [`lock_order_violations`] counts every one regardless, so nothing is lost
/// numerically; what a burst loses is the second finding's SITES, and the
/// watchdog drains every second. Exactly the bound [`drain_completed_holds`]
/// carries, for exactly the same reason.
fn stamp_order_report(
    st: &LockState,
    kind: u8,
    site: &'static Location<'static>,
    outer: &HeldEntry,
) {
    st.report_site.store(site as *const _ as *mut _, Ordering::Relaxed);
    st.report_outer_site.store(outer.site as *mut _, Ordering::Relaxed);
    st.report_outer_rank.store(outer.rank, Ordering::Relaxed);
    // Publishes the three stores above.
    st.report_kind.store(kind, Ordering::Release);
}

/// Every stamped lock-order finding since the last call, taken exactly once.
///
/// Called from the watchdog thread ([`crate::selfwatch`]) beside
/// [`drain_completed_holds`], and from tests, which have no watchdog. The
/// composing — allocation, `format!`, the breadcrumb write — happens here
/// precisely because none of it may happen where the finding is MADE. See
/// [`stamp_order_report`].
pub fn drain_lock_order_reports() -> Vec<OrderReport> {
    let mut reg = REGISTRY.lock_safe();
    let mut out = Vec::new();
    reg.retain(|weak| match weak.upgrade() {
        Some(state) => {
            let kind = state.report_kind.swap(REPORT_NONE, Ordering::Acquire);
            if kind != REPORT_NONE {
                let (file, line) = site_of(state.report_site.load(Ordering::Relaxed));
                let (ofile, oline) = site_of(state.report_outer_site.load(Ordering::Relaxed));
                let outer_rank = state.report_outer_rank.load(Ordering::Relaxed);
                out.push(OrderReport {
                    lock: state.name,
                    rank: (state.rank != UNRANKED).then(|| LockRank(state.rank)),
                    site_file: file,
                    site_line: line,
                    outer_rank: (outer_rank != UNRANKED).then(|| LockRank(outer_rank)),
                    outer_site_file: ofile,
                    outer_site_line: oline,
                    kind,
                });
            }
            true
        }
        None => false,
    });
    out
}

/// Write a batch of lock-order findings out. Separate from the drain so the
/// decision stays where it is cheap and the IO stays in one place — the same
/// split [`LockWatch::tick`] and [`record_all`] have.
pub fn record_order_reports(reports: Vec<OrderReport>) {
    for r in reports {
        crate::obs::breadcrumb(r.event(), &r.detail());
    }
}

/// The panic a re-entrant acquisition raises, composed once so the debug panic
/// and the shipped-build refusal ([`TrackedMutex::refuse_reentrant`]) are the
/// SAME sentence rather than two that drift (#1702).
///
/// Names both locks and both sites for [`report_order_violation`]'s reason: a
/// report naming only the lock being taken sends the next reader back into a
/// 54,000-line module to guess what was already held.
fn reentrant_panic_message(
    inner_name: &str,
    inner_site: &'static Location<'static>,
    outer_file: &str,
    outer_line: u32,
) -> String {
    format!(
        "{REENTRANT_EVENT}: `{inner_name}` at {}:{} is ALREADY held by this thread, taken \
         at {outer_file}:{outer_line} — this mutex is not re-entrant, so the acquisition \
         would self-deadlock",
        inner_site.file(),
        inner_site.line(),
    )
}

/// Report an INVERSION: panic in debug, stamp for the watchdog in release.
///
/// Inversions only, since #1702. A re-entrant acquisition is refused on every
/// path instead — [`TrackedMutex::check_order`] and `doc/design/lock-order.md`
/// §6 — because the two verdicts are different facts: an inversion is a
/// *possible* deadlock that needs the other thread to show up, and a re-entrant
/// acquire on a non-reentrant mutex is a certain one, on this thread, now.
///
/// **Both locks and both sites, in both surfaces.** A report naming only the
/// lock being taken is the report that sends the next reader back into a
/// 54,000-line module to guess what was already held — which is exactly the
/// state #1600 §2.3 describes. The panic can name the outer lock's NAME as well
/// as its site, because it composes on the spot; the breadcrumb names its site
/// and rank, for [`OrderReport`]'s reason.
///
/// `#[cold]`, and in debug it allocates and panics. That is fine there and
/// nowhere else: a debug build that reaches this is stopping.
#[cold]
fn report_order_violation(
    st: &LockState,
    inner_site: &'static Location<'static>,
    outer: &HeldEntry,
) {
    LOCK_ORDER_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
    if !LOCK_ORDER_PANICS.load(Ordering::Relaxed) {
        stamp_order_report(st, REPORT_ORDER, inner_site, outer);
        return;
    }
    let inner_name = st.name;
    let (outer_file, outer_line) = site_of(outer.site as *mut _);
    panic!(
        "{ORDER_EVENT}: `{inner_name}` (rank {}) at {}:{} acquired while `{}` (rank {}) is \
         held by this thread, taken at {outer_file}:{outer_line} — the declared order is \
         outer rank first; see orchestration::lockorder",
        rank_text(st.rank),
        inner_site.file(),
        inner_site.line(),
        outer.name,
        rank_text(outer.rank),
    );
}

/// Note the first time an UNRANKED lock nests under anything.
///
/// The convergence half of the plan's design: a lock with no rank may nest
/// under anything, so the table can be filled in one family at a time instead
/// of all at once — but every such nesting is an ordering fact the table does
/// not yet carry, and a fact nobody wrote down is the state §3 is about.
///
/// Once per lock, ever, and it does not panic even in debug: an unranked
/// nesting is a gap in the table, not a defect in the code. Like every other
/// finding here it only STAMPS — see [`stamp_order_report`].
#[cold]
fn note_unranked_nesting(st: &LockState, site: &'static Location<'static>, outer: &HeldEntry) {
    if st.unranked_reported.swap(true, Ordering::Relaxed) {
        return;
    }
    UNRANKED_NESTINGS.fetch_add(1, Ordering::Relaxed);
    stamp_order_report(st, REPORT_UNRANKED, site, outer);
}

/// A `Mutex` that says who is holding it, since when, from where, and how many
/// threads are behind them — and, since #1609, one a waiter can put a bound on.
///
/// A drop-in for `std::sync::Mutex` at the call sites that use
/// [`crate::obs::LockExt::lock_safe`]: the inherent `lock_safe` below shadows
/// the trait method, so a field's type changing from `Mutex<T>` to
/// `TrackedMutex<T>` changes nothing at the places that lock it.
///
/// **The inner primitive is `parking_lot::Mutex`, not std's** (#1609). std has
/// no timed acquire at all, and [`lock_within`](Self::lock_within) — the
/// operation the whole of Phase 2.1 is built on — is `try_lock_for`. The one
/// observable consequence is POISONING, and it is a narrowing of a concept
/// rather than a change of behaviour: parking_lot has no poison state, so a
/// holder that panics simply releases the lock and the next acquirer sees
/// whatever it wrote. That is byte-for-byte the outcome `lock_safe`'s
/// `into_inner` recovery produced under std (#53's "at worst slightly stale,
/// never memory-unsafe"), reached by there being nothing to recover from.
/// `obs::LockExt::lock_safe` keeps its own recovery for the std mutexes that
/// remain elsewhere (`pty.rs`, and this module's own report ring).
pub struct TrackedMutex<T> {
    inner: InnerMutex<T>,
    state: Arc<LockState>,
}

impl<T> TrackedMutex<T> {
    /// Wrap `value`, registering the lock under `name`.
    ///
    /// The name is what a human reads in the breadcrumb, so it is the FIELD's
    /// name rather than a type or a call site: the plan's worked example of a
    /// useful report is `mq_state_lock held 340 s by queue_merge_with`, and the
    /// first half of that is only available if someone writes it down here.
    /// Names need not be unique — the per-pane delivery locks all share one —
    /// because the call site and the lock id distinguish them.
    pub fn new(name: &'static str, value: T) -> Self {
        Self::build(name, UNRANKED, value)
    }

    /// Wrap `value`, registering it under `name` at `rank` in the declared
    /// acquisition order (#1610).
    ///
    /// The rank is what turns `orchestration/mod.rs`'s thirteen "Lock order:"
    /// doc claims from prose into something that fails a build. See
    /// [`LockRank`] for the direction (smaller is outer) and
    /// `orchestration::lockorder` for the table itself.
    ///
    /// A lock built with the plain [`new`](Self::new) is UNRANKED: it may nest
    /// under anything and anything may nest under it, so the table can be
    /// filled in one family at a time rather than all at once. Its first
    /// nesting is noted once and reaches the breadcrumb log through the
    /// watchdog as `lock-rank-unranked`, which is how the missing rows announce
    /// themselves instead of waiting to be noticed.
    pub fn new_ranked(name: &'static str, rank: LockRank, value: T) -> Self {
        Self::build(name, rank.get(), value)
    }

    fn build(name: &'static str, rank: u32, value: T) -> Self {
        let state = Arc::new(LockState::new(name, rank));
        let mut reg = REGISTRY.lock_safe();
        // Bounded even with no watchdog running. Every snapshot prunes dead
        // entries, and in the app one runs every second — but a test binary has
        // no watchdog and builds registries in the hundreds, so without this the
        // list would hold one entry per lock the PROCESS ever constructed.
        // INV-8's rule, applied to this module's own retained state: released by
        // a check that needs no memory of when it last ran.
        if reg.len() >= REGISTRY_PRUNE_AT {
            reg.retain(|w| w.strong_count() > 0);
        }
        reg.push(Arc::downgrade(&state));
        drop(reg);
        Self { inner: InnerMutex::new(value), state }
    }

    /// Acquire, honouring whatever budget this THREAD is running under.
    ///
    /// **Before any of that**, and whatever the budget says: if this thread
    /// ALREADY holds this lock, nothing is acquired at all. The inner primitive
    /// is not re-entrant, so the acquisition below would park this thread
    /// permanently; #1702 refuses it instead, and the refusal leaves by an
    /// unwind because this signature has nowhere else to put one. See
    /// [`refuse_reentrant`](Self::refuse_reentrant) for the three shapes that
    /// takes and why it is not the fail-open an inversion gets.
    ///
    /// With no [`crate::budget::read_budget`] frame installed — which is every
    /// call site that existed before #1609, and every mutating one after it —
    /// this is otherwise an unbounded acquire and behaves exactly as Phase 0
    /// left it.
    ///
    /// Under a budget, three outcomes:
    ///
    /// 1. acquired within the remaining time: an ordinary guard, tracked
    ///    identically to any other;
    /// 2. ran out, at mutation depth 0: unwinds to the owning `read_budget`
    ///    frame with a [`Busy`], via
    ///    [`crate::budget::unwind_to_frame`] — no panic hook, no crash log;
    /// 3. ran out, inside a [`crate::budget::MutationScope`]: breadcrumbs
    ///    `lock-busy-in-mutation` (edge-triggered, see [`Busy`] below) and then
    ///    waits, unbounded. A slow mutation is a stall; an abandoned one is
    ///    corruption.
    ///
    /// **The cost, since this is the app's hottest shared path.** Per
    /// acquisition, on top of the `Mutex::lock` that was already there: one
    /// thread-local read for the budget, two relaxed read-modify-writes on the
    /// waiter count, three relaxed stores, one release read-modify-write on the
    /// generation, and one monotonic clock read. Per release (the full
    /// accounting is on [`TrackedGuard::drop`]): one monotonic clock read, one
    /// relaxed load and a comparison on the cold path; on the over-threshold
    /// path a further three relaxed loads, four relaxed stores and one release
    /// store, and in both cases one release read-modify-write on the generation.
    ///
    /// **The order checker adds (#1610)**: on acquisition, TWO independent
    /// thread-local accesses — `check_order`'s [`inspect_held`] and
    /// `record_acquired`'s [`push_held`], each a TLS lookup plus a `RefCell`
    /// flag check — around a scan of the held-lock stack, which is zero
    /// comparisons when this thread holds nothing (the overwhelmingly common
    /// case) and at most [`HELD_STACK_CAP`] otherwise, plus one store into a
    /// fixed array. Two rather than one because the check must run BEFORE the
    /// acquisition and the push only after it succeeds; folding them would
    /// mean pushing a hold that may never happen. On release there is ONE
    /// access ([`pop_held`]) and a scan from the top of that array, which
    /// finds its own entry on the first comparison unless a guard was dropped
    /// out of order. The stack is per-thread, so none of it is a shared cache
    /// line and none of it can contend.
    ///
    /// No allocation, no formatting, no global lock, no syscall, and nothing that
    /// can block. The clock read is the only item above a few nanoseconds — tens
    /// of nanoseconds on every platform this ships to — which is why it is a
    /// *monotonic* read and not a `SystemTime`, and why the cheaper alternative
    /// (stamping holds against the watchdog's 1 Hz tick, one atomic load) was
    /// rejected: it would floor every reported duration at a second, on the one
    /// instrument whose whole job is to say how long something took.
    ///
    /// The budget read is a thread-local load on EVERY acquisition, budget or
    /// not. When one is installed the bounded path adds the deadline comparison
    /// in [`crate::budget::remaining`] and its own wait measurement — both on an
    /// acquisition that was already going to block, which is the only place they
    /// can land.
    #[track_caller]
    pub fn lock_safe(&self) -> TrackedGuard<'_, T> {
        let site = Location::caller();
        // BEFORE any acquisition attempt (#1610). A re-entrant acquisition
        // parks forever, so a checker that ran after the acquire would be a
        // checker that never runs on the case it exists for.
        //
        // And it REFUSES rather than reporting-and-carrying-on (#1702). An
        // inversion is still fail-open below; this one is not, because the two
        // are different facts — see `refuse_reentrant`.
        if let Err(busy) = self.check_order(site) {
            self.refuse_reentrant(site, busy);
        }
        let Some((left, frame)) = budget::remaining() else {
            return self.acquire_blocking(site);
        };
        // `unwind_forbidden`, not `in_mutation`: a frame that has already made a
        // durable write is sealed (#1609 review B1/B2), and a sealed frame must
        // wait for exactly the reason a declared mutation must — an acquisition
        // after a write is the only thing that can tear it.
        let must_wait = budget::unwind_forbidden();
        let event = if must_wait { "lock-busy-in-mutation" } else { "lock-busy" };
        match self.acquire_within(site, left, event) {
            Ok(g) => g,
            Err(busy) => {
                if must_wait {
                    // The breadcrumb was already written by `acquire_within`,
                    // edge-triggered — so a mutation that takes twenty locks
                    // under an expired budget reports the HOLD once, not once
                    // per acquisition.
                    self.acquire_blocking(site)
                } else {
                    budget::unwind_to_frame(frame, busy)
                }
            }
        }
    }

    /// Acquire, or give up after `budget` and say who has it.
    ///
    /// The explicit form, for a caller that has somewhere to put an `Err` —
    /// a cadenced tick's entry acquisition skips its tick, and Phase 3's
    /// re-entrancy check will use the same shape. Ignores the thread-local
    /// budget entirely: an explicit bound is a decision, and silently making it
    /// tighter because of an enclosing frame would make this function's own
    /// argument unverifiable at its call site.
    ///
    /// `Duration::ZERO` is a legal budget and means "take it if it is free".
    /// **A re-entrant acquisition is refused here rather than panicking**
    /// (#1610). This entry point has an `Err` to put it in, and the answer it
    /// gives — [`BusyKind::Reentrant`], naming the frame that already holds
    /// the lock — is strictly more useful than a hang and strictly less
    /// destructive than a crash.
    ///
    /// Since #1702 the refusal itself is not this entry point's privilege:
    /// [`lock_safe`](Self::lock_safe) refuses too, and the only difference left
    /// is where the refusal GOES — here it is returned, there it is unwound
    /// (see [`refuse_reentrant`](Self::refuse_reentrant)).
    #[track_caller]
    pub fn lock_within(&self, budget: Duration) -> Result<TrackedGuard<'_, T>, Busy> {
        let site = Location::caller();
        self.check_order(site)?;
        self.acquire_within(site, budget, "lock-busy")
    }

    /// Take the lock if it is free RIGHT NOW, never blocking.
    ///
    /// `None` means genuinely held by someone else — there is no third answer
    /// to distinguish, because the inner primitive does not poison (see
    /// [`TrackedMutex`]).
    ///
    /// A successful acquisition is recorded exactly as a blocking one is, so a
    /// hold taken this way is as visible to the watchdog as any other. A
    /// FAILED one touches the waiter count not at all: nothing waited, and it
    /// writes no breadcrumb — "the lock was busy this instant" is not news, and
    /// this is the one entry point with no budget behind it.
    ///
    /// **The order check runs AFTER the try succeeds** (#1610), not before, and
    /// that is the one place in this module where the check is not on the
    /// blocking side. Nothing here can hang — a `try_lock` that would have
    /// deadlocked simply returns `None` — so the reason to check early is
    /// absent, while the reason not to is real: refusing or panicking on a
    /// speculative acquisition that was never going to be taken would report a
    /// violation that did not happen. A try that SUCCEEDS, on the other hand,
    /// establishes a real nesting, and every nesting is an ordering fact.
    #[track_caller]
    pub fn try_lock_safe(&self) -> Option<TrackedGuard<'_, T>> {
        let site = Location::caller();
        let guard = self.inner.try_lock()?;
        // The discarded `Err` is the re-entrant verdict, and it is unreachable
        // here by construction rather than by policy: `parking_lot::try_lock`
        // on a mutex this thread already holds returns `None`, so the `?` above
        // has already returned. Discarding it rather than unwinding is what
        // keeps this correct if the inner primitive ever changes — a `try` that
        // was GRANTED holds a real guard, and unwinding out of a caller that
        // has one in hand would drop it on a path with no refusal to deliver.
        let _ = self.check_order(site);
        Some(self.record_acquired(guard, site))
    }

    /// The unbounded acquire. Phase 0's path, unchanged.
    fn acquire_blocking(&self, site: &'static Location<'static>) -> TrackedGuard<'_, T> {
        let st = &self.state;
        // Registered BEFORE blocking: a waiter that only counts once it has
        // stopped waiting is invisible for exactly the interval it matters.
        st.waiters.fetch_add(1, Ordering::Relaxed);
        let guard = self.inner.lock();
        st.waiters.fetch_sub(1, Ordering::Relaxed);
        self.record_acquired(guard, site)
    }

    /// The bounded acquire. `event` is the breadcrumb an expiry writes.
    fn acquire_within(
        &self,
        site: &'static Location<'static>,
        budget: Duration,
        event: &'static str,
    ) -> Result<TrackedGuard<'_, T>, Busy> {
        let st = &self.state;
        st.waiters.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let got = self.inner.try_lock_for(budget);
        let waited = started.elapsed();
        st.waiters.fetch_sub(1, Ordering::Relaxed);
        match got {
            Some(guard) => Ok(self.record_acquired(guard, site)),
            None => Err(self.busy(waited, event)),
        }
    }

    /// Stamp a fresh hold. Shared by every acquisition path so a guard obtained
    /// three different ways is one thing to the watchdog.
    ///
    /// `'a` is named rather than elided because the returned [`TrackedGuard`]
    /// borrows from BOTH arguments — the inner guard, and `self.state` for the
    /// tracking — and elision would give those two different lifetimes.
    fn record_acquired<'a>(
        &'a self,
        guard: InnerGuard<'a, T>,
        site: &'static Location<'static>,
    ) -> TrackedGuard<'a, T> {
        let st = &self.state;
        let acquired_ms = mono_ms();
        st.holder_thread.store(this_thread(), Ordering::Relaxed);
        st.holder_site.store(site as *const _ as *mut _, Ordering::Relaxed);
        st.acquired_ms.store(acquired_ms, Ordering::Relaxed);
        // Publishes the three stores above; even -> odd marks the lock held.
        st.generation.fetch_add(1, Ordering::Release);
        // The order checker's own record (#1610), pushed HERE so a guard
        // obtained three different ways is one entry to it as well. `None`
        // means the stack declined to track this hold (over capacity, or a
        // thread-local that is gone), and the guard's drop then pops nothing.
        let held = push_held(st.id, st.name, st.rank, site, acquired_ms);
        TrackedGuard { guard, state: st, acquired_ms, held }
    }

    /// The lock-order check (#1610). Runs before any acquisition that can
    /// block, and after one that cannot ([`try_lock_safe`](Self::try_lock_safe)).
    ///
    /// **A re-entrant verdict is always an `Err`** (#1702). It used to depend
    /// on a `refuse_reentrant` parameter, on the reasoning that `lock_safe`
    /// returns a guard and so "cannot refuse". It can: a refusal it has nowhere
    /// to PUT is still a refusal, delivered as an unwind
    /// ([`refuse_reentrant`](Self::refuse_reentrant)). What the parameter
    /// actually bought was the one behaviour a shipped build may not have —
    /// falling through to `inner.lock()` on a lock this thread already holds,
    /// which is a permanent park rather than a risk of one.
    ///
    /// So the two entry points differ in what they DO with the `Err`, not in
    /// whether they get one: [`lock_within`] hands it to its caller;
    /// [`lock_safe`] unwinds. An inversion is still fail-open — see
    /// [`report_order_violation`] and `doc/design/lock-order.md` §2.1 for why the
    /// two verdicts get different answers.
    ///
    /// [`lock_within`]: Self::lock_within
    /// [`lock_safe`]: Self::lock_safe
    fn check_order(&self, site: &'static Location<'static>) -> Result<(), Busy> {
        let st = &self.state;
        match inspect_held(st.id, st.rank) {
            Verdict::Clear => Ok(()),
            Verdict::Reentrant(outer) => Err(self.reentrant_busy(site, &outer)),
            Verdict::Inversion(outer) => {
                report_order_violation(st, site, &outer);
                Ok(())
            }
            Verdict::UnrankedUnder(outer) => {
                // The load, not the swap, is the steady-state cost: after the
                // one report this lock is ever going to write, every later
                // nested acquisition pays a relaxed load and returns.
                if !st.unranked_reported.load(Ordering::Relaxed) {
                    note_unranked_nesting(st, site, &outer);
                }
                Ok(())
            }
        }
    }

    /// Deliver a re-entrant refusal out of [`lock_safe`](Self::lock_safe),
    /// which has no `Err` to put one in (#1702).
    ///
    /// **Why this one does not fail open the way an inversion does.** The
    /// release fail-open (`doc/design/lock-order.md` §2.1) rests on "refusing
    /// would convert a *possible* hang into a *certain* crash on a path nobody
    /// has proven wrong". That argument is sound for an INVERSION, which needs
    /// a second thread taking the same two locks the other way round before
    /// anything hangs, and it is simply false here: the inner primitive is not
    /// re-entrant, so the acquisition this is standing in front of parks this
    /// thread permanently, every time, with no race required — the certain hang
    /// is the fall-through, not the refusal. #1702 is that path proven wrong in
    /// the field.
    ///
    /// So the acquisition never happens, and the caller is left in one of three
    /// well-defined ways:
    ///
    /// 1. **A build with the panic armed** (debug, `cargo test`, the E2E lane)
    ///    panics naming both locks and both sites, whether or not a budget
    ///    frame is installed. That is deliberately not the plan's literal
    ///    reading — it said unwind wherever a frame exists — because the frame
    ///    may be dozens of frames up in an unrelated module, and answering
    ///    `Busy` there turns the one mechanism that can fail CI on this class
    ///    into a silently-degraded read. A shipped build has no such choice to
    ///    make; a test build does, and the loud half is the point of arming it.
    /// 2. **Under a [`crate::budget::read_budget`] frame**, in a build with the
    ///    panic off: unwind to that frame with the [`BusyKind::Reentrant`]
    ///    `Busy`, which its owner already renders — an MCP `isError` saying
    ///    nothing was executed, a `partial` snapshot section, a command's empty
    ///    degrade. **This unwinds even when [`crate::budget::unwind_forbidden`]
    ///    holds**, which is the one narrowing of `lock-liveness.md` §4.1's
    ///    seal: the seal exists so a sealed frame WAITS rather than tearing a
    ///    durable write, and it is a good trade because waiting ends. Here it
    ///    does not end — the alternative to the tear is not "later", it is
    ///    "never" — and a tear is counted (`budget::torn_writes`) and
    ///    breadcrumbed where a wedge is neither.
    /// 3. **No frame** (a cadenced tick under `tick_gate`'s `MutationScope`, an
    ///    MCP mutate helper thread, a `run_blocking` body): the same panic as
    ///    (1). `obs`'s hook writes a crash log naming both sites, and the
    ///    unwind drops every guard on the way out, so the registry is RELEASED
    ///    — which is the whole point, and the one thing the park could never
    ///    do. Callers are supervised so the panic ends the tick rather than the
    ///    thread; see `obs::TickSupervisor`.
    ///
    /// **The one cost, stated.** The panic hook runs BEFORE the unwind starts,
    /// so that crash log is written while this thread still holds the outer
    /// lock — a file write inside a hold, which is exactly what
    /// [`stamp_order_report`] exists to avoid. It is accepted here and nowhere
    /// else: this is a one-shot defect report on a thread that is already
    /// leaving, the alternative to the write is a hang with no artifact at all
    /// (#1600 §2.3's whole problem), and the waiters whose latency it lands on
    /// are about to be released by the very unwind that follows it.
    #[cold]
    fn refuse_reentrant(&self, site: &'static Location<'static>, busy: Busy) -> ! {
        // Composed before either exit so the two say the same sentence.
        let (outer_file, outer_line) = match &busy.holder {
            Some(h) => (h.site_file, h.site_line),
            // Unreachable: `reentrant_busy` always fills `holder` from the
            // held-lock stack entry it matched. Rendered rather than
            // `unwrap`ped so a refusal can never become a panic ABOUT the
            // refusal, which would name neither site.
            None => ("<unknown>", 0),
        };
        let message = reentrant_panic_message(self.state.name, site, outer_file, outer_line);
        if LOCK_ORDER_PANICS.load(Ordering::Relaxed) {
            panic!("{message}");
        }
        if let Some((_, frame)) = budget::remaining() {
            budget::unwind_to_frame(frame, busy);
        }
        panic!("{message}");
    }

    /// Compose the [`BusyKind::Reentrant`] refusal.
    ///
    /// The "holder" it names is this thread's own earlier frame, taken from the
    /// held-lock stack rather than sampled off the lock — which is both cheaper
    /// and strictly more accurate, since the stack entry cannot have been
    /// replaced under the read.
    ///
    /// Unlike [`busy`](Self::busy), this writes nothing: it stamps the deferred
    /// slot ([`stamp_order_report`]) and lets the watchdog compose it. `busy` is
    /// allowed its breadcrumb because the waiter has already given up and holds
    /// nothing; a re-entrant caller is standing on the very lock it is
    /// reporting on. The one slot per lock also coalesces a cadenced re-entrant
    /// caller to one finding per watchdog tick, which is the edge-trigger
    /// `busy` gets from its generation key.
    fn reentrant_busy(&self, site: &'static Location<'static>, outer: &HeldEntry) -> Busy {
        let st = &self.state;
        let (file, line) = site_of(outer.site as *mut _);
        let busy = Busy {
            kind: BusyKind::Reentrant,
            lock: st.name,
            // Nothing waited. See `Display`, which renders this case as its own
            // sentence rather than as a timeout of zero.
            waited: Duration::ZERO,
            holder: Some(HolderInfo {
                site_file: file,
                site_line: line,
                held_ms: mono_ms().saturating_sub(outer.acquired_ms),
                thread: this_thread(),
            }),
            waiters: st.waiters.load(Ordering::Relaxed) as usize,
        };
        LOCK_ORDER_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
        // STAMP, never write. This runs while THIS thread holds the very lock
        // it is reporting on, so a breadcrumb here would put a file write
        // inside that hold and on every waiter's latency path behind it —
        // #1605 review B1's finding, which applies to a finding about the
        // hold exactly as it applies to a report of one. The slot coalesces
        // per lock per watchdog tick, which also gives the edge-trigger a
        // cadenced re-entrant caller needs.
        stamp_order_report(st, REPORT_REENTRANT, site, outer);
        busy
    }

    /// Compose the [`Busy`], and breadcrumb it ONCE per (lock, hold).
    ///
    /// **Edge-triggered, like `queue_pressure`'s notices.** A wedged registry
    /// has every waiter in the app queued behind one hold; a breadcrumb per
    /// waiter would turn the evidence trail into the noise it exists to cut
    /// through, and would put a file write on the latency path of every one of
    /// them. The key is the hold's own odd generation, so a SECOND hold of the
    /// same lock that also goes busy is a new edge and does report.
    ///
    /// `u64::MAX` is the "never reported" sentinel and `0` is "no holder could
    /// be sampled"; they differ so the first unsampled `Busy` on a lock still
    /// breadcrumbs. Real generations are odd and start at 1, so neither
    /// sentinel can collide with one.
    ///
    /// This runs on the waiter's thread with NO tracked lock held — the waiter
    /// has already given up — so it is allowed to allocate, format and write,
    /// which is the rule [`TrackedGuard::drop`] cannot follow.
    fn busy(&self, waited: Duration, event: &'static str) -> Busy {
        let st = &self.state;
        let snap = st.sample(mono_ms());
        let generation = snap.as_ref().map(|s| s.generation).unwrap_or(0);
        let busy = Busy {
            kind: BusyKind::Timeout,
            lock: st.name,
            waited,
            holder: snap.map(|s| HolderInfo {
                site_file: s.site_file,
                site_line: s.site_line,
                held_ms: s.held_ms,
                thread: s.holder_thread,
            }),
            waiters: st.waiters.load(Ordering::Relaxed) as usize,
        };
        if st.busy_reported_gen.swap(generation, Ordering::Relaxed) != generation {
            st.busy_breadcrumbs.fetch_add(1, Ordering::Relaxed);
            crate::obs::breadcrumb(event, &busy.detail());
        }
        busy
    }

    /// This lock's name, as given to [`TrackedMutex::new`].
    pub fn name(&self) -> &'static str {
        self.state.name
    }

    /// Threads currently blocked waiting for this lock.
    pub fn waiters(&self) -> u32 {
        self.state.waiters.load(Ordering::Relaxed)
    }

    /// How many busy breadcrumbs this lock has written. The edge-trigger in
    /// [`TrackedMutex::busy`], made assertable.
    #[doc(hidden)]
    pub fn busy_breadcrumbs(&self) -> u64 {
        self.state.busy_breadcrumbs.load(Ordering::Relaxed)
    }
}

/// A held [`TrackedMutex`]. Derefs to the guarded value exactly like a
/// `MutexGuard`, and clears the hold record when it drops.
pub struct TrackedGuard<'a, T> {
    guard: InnerGuard<'a, T>,
    state: &'a Arc<LockState>,
    acquired_ms: u64,
    /// This hold's entry in the thread's held-lock stack (#1610), or `None` if
    /// the stack declined to track it. Carried on the GUARD rather than looked
    /// up at drop time because a guard may be dropped out of order, and the
    /// token is the only thing that identifies which entry is this one's.
    held: Option<u64>,
}

impl<T> std::ops::Deref for TrackedGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> std::ops::DerefMut for TrackedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Drop for TrackedGuard<'_, T> {
    /// **Nothing here may allocate, format, take a global lock, or make a
    /// syscall** — see the completed-hold slot on [`LockState`] for why. Rust
    /// runs this body BEFORE dropping the struct's fields, and `self.guard` is
    /// a field, so every instruction below executes with the reported mutex
    /// still locked and every waiter still queued behind it.
    ///
    /// What is left is: one clock read, four relaxed loads, four relaxed
    /// stores, one release STORE (`done_pending`, publishing the four stores
    /// above), one release read-modify-write (`generation`), and — since
    /// #1610 — one thread-local borrow plus a scan from the top of a fixed
    /// array to remove this hold's own entry ([`pop_held`]). All of it is on
    /// this lock's own cache line or on this THREAD's own storage, and none of
    /// it can block.
    ///
    /// The store and the read-modify-write are counted apart deliberately: this
    /// body runs with the reported mutex still held, so what it costs is what
    /// every waiter behind it pays, and an RMW is not a store (#1605 review n5,
    /// corrected in #1608).
    ///
    /// **It runs identically during an UNWIND** (#1609, rider R1), which is
    /// what makes [`crate::budget::read_budget`]'s exit safe rather than merely
    /// convenient. Rust drops live locals as an unwind passes through them, so
    /// a guard held when some deeper acquisition times out gets this body: the
    /// generation goes odd -> even, the lock reads FREE to the watchdog, and
    /// `self.guard`'s own drop releases the mutex. Nothing below can panic —
    /// there is no allocation, no indexing and no arithmetic that can overflow
    /// (`saturating_sub`) — so this cannot become the double-panic that turns
    /// an unwind into an abort. `doc/design/lock-liveness.md` §4.2 carries the
    /// argument and `budget::tests::an_unwind_leaves_no_tracked_lock_held` pins
    /// it.
    fn drop(&mut self) {
        let st = self.state;
        // The order checker's pop (#1610). First, so that anything below is
        // reasoning about a thread that is already out of this lock — and
        // cheap enough to belong in this body: a bounds-checked scan from the
        // top of a fixed array, no allocation and nothing that can panic (see
        // `pop_held`).
        if let Some(token) = self.held {
            pop_held(token);
        }
        let held_ms = mono_ms().saturating_sub(self.acquired_ms);
        if held_ms >= HOLD_WARN_MS.load(Ordering::Relaxed) {
            // Stamp only. `drain_completed_holds` composes and writes it, on
            // the watchdog thread, with this lock long since released.
            st.done_ms.store(held_ms, Ordering::Relaxed);
            st.done_thread.store(st.holder_thread.load(Ordering::Relaxed), Ordering::Relaxed);
            st.done_site.store(st.holder_site.load(Ordering::Relaxed), Ordering::Relaxed);
            st.done_waiters.store(st.waiters.load(Ordering::Relaxed), Ordering::Relaxed);
            // Publishes the four stores above.
            st.done_pending.store(true, Ordering::Release);
        }
        // odd -> even: the lock is free.
        st.generation.fetch_add(1, Ordering::Release);
    }
}

/// Every completed over-threshold hold since the last call, taken exactly once.
///
/// Called from the watchdog thread (and from tests, which have no watchdog).
/// This is where the composing happens — allocation, `format!`, the report ring
/// and the breadcrumb write — precisely because none of it may happen where the
/// hold ENDS. See [`TrackedGuard::drop`].
///
/// **Its bound, stated because it is a real one.** Each lock holds ONE pending
/// slot, so a second over-threshold release on the same lock before a drain
/// overwrites the first. At the shipped settings that cannot happen: two holds
/// of one mutex cannot overlap, so each must last at least the threshold (5 s)
/// and the watchdog drains every second. It becomes reachable only if the
/// watchdog is itself starved for longer than the threshold — a state the
/// watchdog reports as [`crate::selfwatch::Liveness::BackendStuck`] on its next
/// tick — or if a caller lowers the threshold below the drain interval, which
/// only tests do (and they drain explicitly).
pub fn drain_completed_holds() -> Vec<HoldReport> {
    let mut reg = REGISTRY.lock_safe();
    let mut out = Vec::new();
    reg.retain(|weak| match weak.upgrade() {
        Some(state) => {
            if let Some(r) = state.take_completed() {
                out.push(r);
            }
            true
        }
        None => false,
    });
    out
}

/// A coherent read of one lock's current hold. Produced by [`held_locks`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockSnapshot {
    pub id: u64,
    pub name: &'static str,
    /// The odd generation this hold owns. `(id, generation)` identifies one
    /// hold for the lifetime of the process, which is what lets the watchdog
    /// report a still-held lock exactly once instead of once per tick.
    pub generation: u64,
    pub holder_thread: u64,
    pub site_file: &'static str,
    pub site_line: u32,
    pub held_ms: u64,
    pub waiters: u32,
}

/// Every lock held at this instant, with how long it has been held.
///
/// Prunes registry entries whose lock has been dropped, so the registry is
/// bounded by LIVE locks rather than by locks this process ever built — which
/// matters because the test suite constructs registries in the hundreds and the
/// per-pane delivery locks come and go with panes.
pub fn held_locks(now_ms: u64) -> Vec<LockSnapshot> {
    let mut reg = REGISTRY.lock_safe();
    let mut out = Vec::new();
    reg.retain(|weak| match weak.upgrade() {
        Some(state) => {
            if let Some(s) = state.sample(now_ms) {
                out.push(s);
            }
            true
        }
        None => false,
    });
    out
}

/// How many tracked locks are live. The watchdog's vacuity control: a scan over
/// an empty registry reports nothing, which is byte-identical to a scan that
/// found nothing wrong.
pub fn live_lock_count() -> usize {
    let mut reg = REGISTRY.lock_safe();
    reg.retain(|w| w.strong_count() > 0);
    reg.len()
}

/// Every live tracked lock's name, sorted, with duplicates kept.
///
/// Duplicates are kept on purpose: the per-pane delivery locks all share one
/// name, so collapsing them would turn "eleven panes have a delivery lock" into
/// "there is a delivery lock" — and a count that silently collapses is the
/// wrong instrument for a resource whose whole failure mode is how many of them
/// there are. Sorted so a caller comparing two readings compares sets rather
/// than construction order.
///
/// This is the surface a diagnostic dump and the later phases' lock-order work
/// read; it does NOT acquire anything, so calling it can never wait.
pub fn tracked_lock_names() -> Vec<&'static str> {
    let mut reg = REGISTRY.lock_safe();
    let mut out = Vec::new();
    reg.retain(|weak| match weak.upgrade() {
        Some(state) => {
            out.push(state.name);
            true
        }
        None => false,
    });
    out.sort_unstable();
    out
}

/// Every live tracked lock's name paired with the rank it was CONSTRUCTED
/// with, sorted by name, duplicates kept (#1610 review B1).
///
/// [`tracked_lock_names`]'s sibling, and the difference is the whole reason it
/// exists. A name is registered identically by [`TrackedMutex::new`] and
/// [`TrackedMutex::new_ranked`], so a guard reading names alone cannot tell a
/// ranked field from one that has quietly been reverted to unranked — and
/// removing a rank can only remove violations, never create one, so no green
/// suite anywhere is evidence about it. The rank is the half that makes "this
/// field carries this rank" a claim a build can fail.
///
/// Like [`tracked_lock_names`] this acquires nothing tracked, so calling it can
/// never wait; and like it, duplicates are kept, because two live locks under
/// one name is a real state (two registries in one test process, or the
/// per-pane delivery locks) and a caller checking a table wants to see all of
/// them rather than whichever one a de-dup happened to keep.
pub fn tracked_lock_ranks() -> Vec<(&'static str, Option<LockRank>)> {
    let mut reg = REGISTRY.lock_safe();
    let mut out = Vec::new();
    reg.retain(|weak| match weak.upgrade() {
        Some(state) => {
            out.push((state.name, (state.rank != UNRANKED).then(|| LockRank(state.rank))));
            true
        }
        None => false,
    });
    out.sort_unstable_by_key(|(name, _)| *name);
    out
}

/// The watchdog's lock half: turns a series of snapshots into at-most-one
/// report per hold.
///
/// Pure — it takes the snapshots and returns the reports, reads no clock and
/// writes no file — so the rule it encodes ("report a hold once it passes the
/// threshold, and only once") is testable without a watchdog thread and without
/// waiting five seconds.
#[derive(Debug, Default)]
pub struct LockWatch {
    /// `(lock id, generation)` of every hold already reported.
    warned: Vec<(u64, u64)>,
}

impl LockWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Report the holds in `held` that have passed `threshold_ms` and have not
    /// been reported before.
    ///
    /// Forgets holds that are over, so `warned` is bounded by the number of
    /// locks concurrently over the threshold rather than by the number of slow
    /// holds this process has ever seen.
    pub fn tick(&mut self, held: &[LockSnapshot], threshold_ms: u64) -> Vec<HoldReport> {
        self.warned
            .retain(|(id, generation)| held.iter().any(|s| s.id == *id && s.generation == *generation));
        let mut out = Vec::new();
        for s in held {
            if s.held_ms < threshold_ms {
                continue;
            }
            if self.warned.contains(&(s.id, s.generation)) {
                continue;
            }
            self.warned.push((s.id, s.generation));
            out.push(HoldReport {
                lock: s.name,
                site_file: s.site_file,
                site_line: s.site_line,
                holder_thread: s.holder_thread,
                held_ms: s.held_ms,
                waiters: s.waiters,
                still_held: true,
            });
        }
        out
    }

    /// How many in-flight holds this watch is currently suppressing repeats
    /// for. The bound above, made assertable.
    pub fn tracked(&self) -> usize {
        self.warned.len()
    }
}

/// Write a batch of reports out (breadcrumb + the in-memory tail). Separate
/// from [`LockWatch::tick`] so the decision stays pure and the IO stays in one
/// place.
pub fn record_all(reports: Vec<HoldReport>) {
    for r in reports {
        record(r);
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the BOUNDED half (#1609). The observing half's tests live in
// `src-tauri/tests/selfwatch.rs`, where they were written; these are here
// because they need nothing from `src-tauri` and a test that can run in the
// engine crate should.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod bounded_tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Arc;

    /// A generous timeout for "this should have answered by now" on a loaded
    /// CI runner. Every assertion below is a "did it answer at all" question,
    /// never a latency measurement.
    const GRACE: Duration = Duration::from_secs(10);

    /// How long a `hold` fixture keeps its lock if the test never releases it.
    ///
    /// **This is a watchdog, not a duration the assertions depend on.** Every
    /// bounded-acquisition test here budgets in the tens of milliseconds, so a
    /// PASSING run never reaches this. It exists for the FAILING run: the
    /// natural way to redden any of these rows is to neuter the bound, and a
    /// neutered `lock_within` parks forever — which arrives as a CI job
    /// timeout naming nothing, not as a red naming an assertion. With a
    /// deadline on the holder, the same neuter returns late with the wrong
    /// answer and the assertion says so (the #744 idiom).
    ///
    /// Four seconds is the same value `hold_lock_for_test` uses in
    /// `tests/liveness.rs`, and a 40-80x margin over the budgets below.
    const HOLD_MAX: Duration = Duration::from_secs(4);
    /// Hold `lock` on its own thread until the returned sender is dropped —
    /// or until [`HOLD_MAX`], whichever comes first — returning only once the
    /// hold is REAL.
    ///
    /// The handshake is the point: a test that merely spawns a holder and hopes
    /// is measuring the scheduler. Returns the line the hold was taken on so a
    /// caller can check the recorded call site is the HOLDER's rather than this
    /// helper's — which is what `#[track_caller]` on `lock_safe` buys.
    fn hold<T: Send + 'static>(lock: Arc<TrackedMutex<T>>) -> (mpsc::Sender<()>, u32) {
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let line = std::line!() + 2;
        std::thread::spawn(move || {
            let _g = lock.lock_safe();
            let _ = held_tx.send(());
            // `recv_timeout`, never `recv`: see HOLD_MAX.
            let _ = release_rx.recv_timeout(HOLD_MAX);
        });
        held_rx.recv_timeout(GRACE).expect("setup: the holder thread never acquired the lock");
        (release_tx, line)
    }

    #[test]
    fn lock_within_answers_busy_and_names_the_holder() {
        let lock = Arc::new(TrackedMutex::new("withinspec", 7u32));

        // The discriminating half FIRST. Without it, the assertion below passes
        // just as well against a `lock_within` that never succeeds at all.
        let g = lock
            .lock_within(Duration::from_millis(50))
            .expect("an uncontended lock must be acquired, not reported busy");
        assert_eq!(*g, 7);
        drop(g);
        assert_eq!(
            lock.busy_breadcrumbs(),
            0,
            "an uncontended acquisition wrote a busy breadcrumb; the instrument would then be \
             reporting on every healthy lock in the app"
        );

        let (release, holder_line) = hold(lock.clone());
        let busy = lock
            .lock_within(Duration::from_millis(50))
            .err()
            .expect("a held lock must answer Busy within the budget, not park");

        assert_eq!(busy.lock, "withinspec");
        // `as_ref`: the `Display` assertion below reads the whole `Busy`, and
        // `Option::expect` would move the holder out from under it.
        let holder =
            busy.holder.as_ref().expect("the holder was sampled: it is parked, so nothing moved");
        assert!(
            holder.site_file.ends_with("lockwatch.rs"),
            "the recorded site was {}:{}",
            holder.site_file,
            holder.site_line
        );
        // The call site is the HOLDER's `lock_safe()` line, not `acquire_blocking`'s
        // and not this assertion's. `#[track_caller]` is what makes a breadcrumb
        // name the code that took the lock rather than the instrument.
        assert_eq!(
            holder.site_line, holder_line,
            "the recorded line is not the one the hold was taken on"
        );
        assert!(busy.waited >= Duration::from_millis(40), "waited {:?}", busy.waited);
        assert_eq!(busy.retry_after_ms(), BUSY_RETRY_AFTER_MS);

        // The Display is a public contract (it reaches an agent's context).
        let rendered = busy.to_string();
        for needle in ["`withinspec`", "held ", "lockwatch.rs", "waiters", "waited "] {
            assert!(rendered.contains(needle), "Display lost {needle:?}: {rendered}");
        }
        drop(release);
    }

    #[test]
    fn one_hold_breadcrumbs_once_however_many_waiters_give_up() {
        // The property the plan asks for in as many words: "edge-triggered …
        // never per waiter". A wedged registry has every thread in the app
        // queued behind one hold, so a breadcrumb per waiter turns the evidence
        // trail into the noise it exists to cut through.
        const WAITERS: usize = 6;
        let lock = Arc::new(TrackedMutex::new("edgespec", 0u32));
        let (release, _) = hold(lock.clone());

        let mut threads = Vec::new();
        for _ in 0..WAITERS {
            let l = lock.clone();
            threads.push(std::thread::spawn(move || {
                l.lock_within(Duration::from_millis(60)).err().map(|b| b.waiters)
            }));
        }
        let seen: Vec<Option<usize>> = threads.into_iter().map(|t| t.join().expect("waiter")).collect();
        assert!(
            seen.iter().all(Option::is_some),
            "every waiter must have been refused while the lock was held: {seen:?}"
        );
        assert_eq!(
            lock.busy_breadcrumbs(),
            1,
            "{WAITERS} waiters produced {} breadcrumbs; the edge-trigger is not edge-triggered",
            lock.busy_breadcrumbs()
        );
        drop(release);

        // A SECOND hold that also goes busy is a new edge and must report — an
        // edge-trigger keyed on the lock alone would go silent forever after
        // the first incident, which is the failure mode worth pinning.
        let (release2, _) = hold(lock.clone());
        assert!(lock.lock_within(Duration::from_millis(60)).is_err());
        assert_eq!(
            lock.busy_breadcrumbs(),
            2,
            "a new hold going busy did not report; the key is the lock rather than the hold"
        );
        drop(release2);
    }

    #[test]
    fn a_zero_budget_is_a_try_lock_rather_than_an_error() {
        // `Duration::ZERO` is a legal budget — `read_budget` hands it out the
        // moment a deadline passes, and every acquisition after that point
        // takes this path. It must still SUCCEED on a free lock, or an expired
        // budget would turn a healthy registry into a busy one.
        let lock = TrackedMutex::new("zerospec", 1u32);
        assert!(lock.lock_within(Duration::ZERO).is_ok(), "a free lock must be taken at zero budget");
        let lock = Arc::new(lock);
        let (release, _) = hold(lock.clone());
        assert!(lock.lock_within(Duration::ZERO).is_err(), "a held lock must be refused at once");
        drop(release);
    }
}

// ---------------------------------------------------------------------------
// Lock-rank unit tests (#1610). The registry-level rows — L5a/L5b over the real
// `lockorder` table — are `src-tauri/tests/liveness.rs`; these are the ones that
// need nothing from `src-tauri` and can therefore run here.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod rank_tests {
    use super::*;
    use std::sync::mpsc;

    /// Every test here reads or writes PROCESS-GLOBAL state — the violation
    /// counter, the unranked-report counter, and the panic switch — so they run
    /// one at a time. `lock_safe` rather than `.lock().unwrap()`: one failing
    /// test under this guard would otherwise poison it and report N failures
    /// for one defect (`.orrerix/lessons.md`, and `obs::SERIAL`'s own note).
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Restores the panic switch however the test leaves — including through a
    /// failed assertion, which is an unwind like any other. A global overridden
    /// by a harness and restored on the happy path only is a global that stays
    /// overridden the first time something goes wrong.
    struct PanicSwitch(bool);
    impl Drop for PanicSwitch {
        fn drop(&mut self) {
            set_lock_order_panics(self.0);
        }
    }

    const OUTER: LockRank = LockRank::new(100);
    const INNER: LockRank = LockRank::new(200);

    /// The panic payload as a `String`, or `None` if `f` did not panic.
    fn panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> Option<String> {
        match std::panic::catch_unwind(f) {
            Ok(()) => None,
            Err(payload) => Some(
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "<non-string panic payload>".to_string()),
            ),
        }
    }

    #[test]
    fn the_declared_direction_is_not_a_violation() {
        // The discriminating half, and it goes FIRST. Without it, every
        // assertion below passes just as well against a checker that refuses
        // every nesting — which would be a checker nobody could ship.
        let _serial = SERIAL.lock_safe();
        let outer = TrackedMutex::new_ranked("orderspec_outer", OUTER, 1u32);
        let inner = TrackedMutex::new_ranked("orderspec_inner", INNER, 2u32);
        let before = lock_order_violations();

        let g1 = outer.lock_safe();
        let g2 = inner.lock_safe();
        assert_eq!((*g1, *g2), (1, 2));
        assert_eq!(held_lock_depth(), 2, "both holds must be on this thread's stack");
        drop(g2);
        drop(g1);
        assert_eq!(held_lock_depth(), 0, "the stack must be empty again once both guards drop");
        assert_eq!(
            lock_order_violations(),
            before,
            "taking locks in the DECLARED order reported a violation"
        );
    }

    #[test]
    fn an_inversion_panics_naming_both_locks_and_both_sites() {
        let _serial = SERIAL.lock_safe();
        let outer = TrackedMutex::new_ranked("invspec_outer", OUTER, ());
        let inner = TrackedMutex::new_ranked("invspec_inner", INNER, ());
        let before = lock_order_violations();

        // The inner-ranked lock first, then the outer one under it: the
        // inversion, taken on ONE thread and needing no race to be visible.
        let held_line = std::line!() + 2;
        let msg = panic_message(std::panic::AssertUnwindSafe(|| {
            let _held = inner.lock_safe();
            let _boom = outer.lock_safe();
        }))
        .expect("an inversion must panic in a debug/test build");

        // BOTH locks and BOTH sites. A report naming only the lock being taken
        // is the one that sends the next reader back into a 54k-line module to
        // guess what was already held (#1600 §2.3).
        for needle in ["invspec_outer", "invspec_inner", "lockwatch.rs", "rank 100", "rank 200"] {
            assert!(msg.contains(needle), "the panic lost {needle:?}: {msg}");
        }
        assert!(
            msg.contains(&format!("lockwatch.rs:{held_line}")),
            "the panic must name the line the ALREADY-HELD lock was taken on ({held_line}): {msg}"
        );
        assert_eq!(
            lock_order_violations(),
            before + 1,
            "the violation was not counted"
        );
        assert_eq!(held_lock_depth(), 0, "the unwind must leave no hold on the stack");
    }

    #[test]
    fn a_reentrant_lock_within_is_refused_rather_than_parking() {
        let _serial = SERIAL.lock_safe();
        let lock = TrackedMutex::new_ranked("reentspec", OUTER, ());

        // Discriminating half first: an uncontended `lock_within` must SUCCEED,
        // or the refusal below would be evidence of nothing.
        assert!(lock.lock_within(Duration::from_millis(50)).is_ok());

        let held_line = std::line!() + 1;
        let held = lock.lock_safe();
        let started = Instant::now();
        // A budget 80x the answer this must give. If the refusal is removed,
        // this returns a TIMEOUT `Busy` late instead of parking forever, so the
        // failure names an assertion rather than arriving as a job timeout.
        let busy = lock
            .lock_within(Duration::from_millis(4_000))
            .err()
            .expect("a re-entrant acquisition must be refused, not granted");
        assert!(
            busy.is_reentrant(),
            "a re-entrant acquisition was reported as ordinary contention: {busy:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the refusal must be immediate, not budget-shaped: took {:?}",
            started.elapsed()
        );
        let holder = busy.holder.as_ref().expect("the holder is this thread's own frame");
        assert_eq!(
            holder.site_line, held_line,
            "the refusal must name the line THIS thread already holds it from"
        );
        // `Display` is a public contract: it reaches an agent's context through
        // `loomux busy:`. A re-entrant refusal must not read as contention.
        let rendered = busy.to_string();
        assert!(rendered.contains("already held by this same thread"), "{rendered}");
        assert!(!rendered.contains("waiters"), "the timeout wording leaked in: {rendered}");
        drop(held);
    }

    #[test]
    fn a_reentrant_lock_safe_panics_rather_than_self_deadlocking() {
        let _serial = SERIAL.lock_safe();
        let lock = std::sync::Arc::new(TrackedMutex::new_ranked("reentpanicspec", OUTER, ()));

        // **On its own thread, with a bounded wait.** Not fussiness: this is
        // the one row whose FAILING form is a permanent park — remove the
        // refusal and the second `lock_safe` is the self-deadlock the test is
        // about, which in-thread would arrive as a CI job timeout naming
        // nothing. The thread is left parked deliberately; the run ends and
        // the process exits (`completes_within`'s idiom in `tests/liveness.rs`).
        let (tx, rx) = mpsc::channel();
        let l = lock.clone();
        let held_line = std::line!() + 3;
        std::thread::spawn(move || {
            let msg = panic_message(std::panic::AssertUnwindSafe(|| {
                let _held = l.lock_safe();
                // Without the checker this parks forever, with no cycle for an
                // inversion search to find — #1600 §1.2's invisible case.
                let _boom = l.lock_safe();
                // Only reached if the acquisition was granted, which is the
                // one outcome that is neither a refusal nor a deadlock.
                assert_eq!(held_lock_depth(), 2, "a re-entrant acquisition was GRANTED");
            }));
            let _ = tx.send((msg, held_lock_depth()));
        });

        let (msg, depth) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the re-entrant acquisition PARKED — which is exactly the defect #1610 closes");
        let msg = msg.expect("a re-entrant lock_safe must panic in a debug/test build");
        assert!(msg.contains("reentpanicspec"), "{msg}");
        assert!(msg.contains(&format!("lockwatch.rs:{held_line}")), "{msg}");
        assert_eq!(depth, 0, "the unwind left a phantom hold on that thread's stack");
    }

    #[test]
    fn two_locks_at_the_same_rank_nest_freely() {
        // The test process builds several registries, so two live locks really
        // do share a rank — they are the same FIELD twice, not two peers.
        // Refusing that nesting would fail tests that have nothing wrong with
        // them, which is why re-entrancy is decided on lock identity instead.
        let _serial = SERIAL.lock_safe();
        let a = TrackedMutex::new_ranked("twinspec", OUTER, ());
        let b = TrackedMutex::new_ranked("twinspec", OUTER, ());
        let before = lock_order_violations();
        let g1 = a.lock_safe();
        let g2 = b.lock_safe();
        assert_eq!(held_lock_depth(), 2, "both holds must be on this thread's stack");
        drop(g2);
        drop(g1);
        assert_eq!(lock_order_violations(), before, "equal ranks were treated as a violation");
    }

    #[test]
    fn a_guard_dropped_out_of_order_removes_its_own_entry() {
        // Rust permits `drop(outer)` while `inner` is alive. A stack that
        // assumed LIFO would pop the wrong entry and then judge every later
        // acquisition against a lock that is no longer held.
        //
        // The fixture is built so the two outcomes DIVERGE: with the right
        // entry removed the stack holds rank 300 and taking rank 200 is an
        // inversion; with the wrong one removed it holds rank 100 and the same
        // acquisition is perfectly legal. A non-discriminating fixture here
        // would pass under both implementations.
        let _serial = SERIAL.lock_safe();
        let a = TrackedMutex::new_ranked("ooospec_a", LockRank::new(100), ());
        let b = TrackedMutex::new_ranked("ooospec_b", LockRank::new(300), ());
        let c = TrackedMutex::new_ranked("ooospec_c", LockRank::new(200), ());

        let g_a = a.lock_safe();
        let g_b = b.lock_safe();
        drop(g_a); // out of order: `a` is at the BOTTOM of the stack
        assert_eq!(held_lock_depth(), 1, "one hold is still live");

        let msg = panic_message(std::panic::AssertUnwindSafe(|| {
            let _g = c.lock_safe();
        }))
        .expect(
            "rank 200 under a held rank 300 is an inversion; no panic means the drop removed              the top of the stack instead of its own entry",
        );
        assert!(msg.contains("ooospec_b"), "the surviving hold must be `b`: {msg}");
        assert!(!msg.contains("ooospec_a"), "`a` was released and must not be blamed: {msg}");
        drop(g_b);
        assert_eq!(held_lock_depth(), 0);
    }

    /// The findings this drain produced for `lock`, and only for `lock`.
    ///
    /// Filtered by NAME rather than asserted as a total, because the drain and
    /// the counters beside it are PROCESS-global while `SERIAL` only serializes
    /// this module. The concrete offender, named because a hazard reasoned
    /// about in the abstract gets mis-located: `budget.rs`'s
    /// `an_unwind_leaves_no_tracked_lock_held` takes `unwindheldspec` and then,
    /// while holding it, takes `unwindblockedspec` — one thread, two unranked
    /// locks, freshly constructed each run, outside this module's guard. Every
    /// run of it notes a first nesting. (`nestspec`, three tests down, holds its
    /// lock on a SPAWNED thread and nests nothing; it is not the offender, and
    /// the first version of this note said it was — #1610 review N3.)
    fn drain_for(lock: &str) -> Vec<OrderReport> {
        drain_lock_order_reports().into_iter().filter(|r| r.lock == lock).collect()
    }

    #[test]
    fn an_unranked_lock_nests_under_a_ranked_one_and_is_reported_once() {
        let _serial = SERIAL.lock_safe();
        let ranked = TrackedMutex::new_ranked("unrankedspec_outer", INNER, ());
        let plain = TrackedMutex::new("unrankedspec_plain", ());
        let violations = lock_order_violations();
        let reports = unranked_nestings();
        let held_line = std::line!() + 1;
        let g = ranked.lock_safe();
        drop(plain.lock_safe());
        drop(plain.lock_safe());
        drop(plain.lock_safe());
        drop(g);

        assert_eq!(
            lock_order_violations(),
            violations,
            "an unranked lock may nest under anything; it is not a violation"
        );
        // A FLOOR, not an equality (#1610 review N3). `UNRANKED_NESTINGS` is
        // process-global and `SERIAL` only serializes this module, so
        // `reports + 1` is a race against `budget.rs`'s unwind test landing its
        // own first nesting between the two reads — rare rather than
        // reproducible, which is the worst kind of intermittent to ship.
        //
        // What the floor can say is narrower than "the counter is WIRED", which
        // is what this comment used to claim (#1698 review residual). The very
        // race that forces a floor also lets a SIBLING test's first nesting
        // satisfy it, so a `+1` here is consistent with this lock's nesting
        // having been counted and is not evidence of it: the floor pins only
        // that the process-global counter moved at all across this window.
        //
        // The witness for THIS lock is the per-lock `drain_for` below, which
        // cannot race because no other test touches this lock's name — and it
        // is also what pins "once, not once per acquisition", against the three
        // acquisitions above.
        assert!(
            unranked_nestings() >= reports + 1,
            "an unranked lock nested under a ranked one and the counter did not move"
        );

        // And the finding the watchdog would write says which rank it nested
        // under and where, which is the whole point: a row for the table.
        let found = drain_for("unrankedspec_plain");
        assert_eq!(found.len(), 1, "expected exactly one stamped finding: {found:?}");
        assert_eq!(found[0].rank, None, "the nesting lock is the UNRANKED one");
        assert_eq!(found[0].outer_rank, Some(INNER));
        assert_eq!(
            found[0].outer_site_line, held_line,
            "the finding must name the line the outer hold was taken on"
        );
        assert!(found[0].detail().contains("unrankedspec_plain"), "{}", found[0].detail());
    }

    #[test]
    fn a_release_build_stamps_an_inversion_and_carries_on() {
        // The fail-open half, which is otherwise unreachable from a test — and
        // a fail-open path nobody has executed is a fail-open path nobody has
        // checked. An inversion is a POSSIBLE deadlock: it needs a second
        // thread taking the same two locks the other way round, so turning that
        // possibility into a certain crash is not an improvement.
        //
        // Inversions only, and the name says so since #1702. The re-entrant
        // half used to ride this same fail-open and is now refused instead —
        // `a_reentrant_lock_safe_never_parks_even_with_panics_off` is that
        // half, and the two are separate tests because one assertion pair
        // cannot state two opposite policies.
        let _serial = SERIAL.lock_safe();
        let _restore = PanicSwitch(set_lock_order_panics(false));
        let outer = TrackedMutex::new_ranked("failopenspec_outer", OUTER, 7u32);
        let inner = TrackedMutex::new_ranked("failopenspec_inner", INNER, 9u32);
        let before = lock_order_violations();
        let _ = drain_for("failopenspec_outer"); // start from an empty slot

        let held_line = std::line!() + 1;
        let held = inner.lock_safe();
        let taken_line = std::line!() + 1;
        let taken = outer.lock_safe();
        assert_eq!(*taken, 7, "the inversion must still ACQUIRE — that is what fail-open means");
        assert_eq!(lock_order_violations(), before + 1, "the violation must still be counted");
        drop(taken);
        drop(held);

        // The finding is STAMPED, not written here: composing it is allocation
        // and a file write, and this thread was holding a reported lock. Both
        // locks and both sites survive the deferral, which is the property that
        // makes the release-mode trail worth having at all.
        let found = drain_for("failopenspec_outer");
        assert_eq!(found.len(), 1, "expected exactly one stamped finding: {found:?}");
        assert_eq!(found[0].rank, Some(OUTER));
        assert_eq!(found[0].outer_rank, Some(INNER));
        assert_eq!(found[0].site_line, taken_line, "the acquiring site");
        assert_eq!(found[0].outer_site_line, held_line, "the site of the hold it collided with");
        assert_eq!(found[0].event(), "lock-order-violation");
        assert!(
            drain_for("failopenspec_outer").is_empty(),
            "a drained finding must be taken exactly once"
        );
    }

    #[test]
    fn a_reentrant_lock_safe_never_parks_even_with_panics_off() {
        // The requirement #1702 exists for: **in a SHIPPED build, a re-entrant
        // `lock_safe` may not block forever.** Until this PR it did — the
        // release arm stamped a `lock-reentrant` finding and then fell through
        // to `inner.lock()`, which on a non-reentrant mutex is a permanent
        // park. `set_lock_order_panics(false)` is the only way to reach that
        // arm from a test, and a path nobody has executed is a path nobody has
        // checked.
        //
        // **On its own thread, with a bounded wait**, for
        // `a_reentrant_lock_safe_panics_rather_than_self_deadlocking`'s reason:
        // the FAILING form of this row is a permanent park, which in-thread
        // arrives as a CI job timeout naming nothing. Here the timeout on
        // `recv_timeout` is the assertion, and it is the red this test was
        // written to produce against the base commit.
        let _serial = SERIAL.lock_safe();
        let _restore = PanicSwitch(set_lock_order_panics(false));
        let lock = std::sync::Arc::new(TrackedMutex::new_ranked("reentreleasespec", OUTER, 4u32));
        let before = lock_order_violations();
        let _ = drain_for("reentreleasespec"); // start from an empty slot

        let (tx, rx) = mpsc::channel();
        let l = lock.clone();
        std::thread::spawn(move || {
            // The discriminating half, and it goes first: with the panic switch
            // off an UNCONTENDED acquisition must still be granted, or every
            // assertion below would pass just as well against a build that had
            // stopped acquiring anything at all.
            //
            // `line!()` is read on this side of the spawn, two lines above the
            // acquisition it names, so editing the prose above cannot silently
            // move the offset off its target. It is sent back rather than
            // computed on the main thread for the same reason.
            let held_line = std::line!() + 2;
            let msg = panic_message(std::panic::AssertUnwindSafe(|| {
                let held = l.lock_safe();
                assert_eq!(*held, 4, "an uncontended acquisition must still be granted");
                let _boom = l.lock_safe();
                // Only reached if the acquisition was GRANTED, which on a
                // non-reentrant mutex is the one outcome that is neither a
                // refusal nor a deadlock.
                assert_eq!(held_lock_depth(), 2, "a re-entrant acquisition was GRANTED");
            }));
            let _ = tx.send((msg, held_lock_depth(), held_line));
        });

        let (msg, depth, held_line) = rx.recv_timeout(Duration::from_secs(10)).expect(
            "the re-entrant acquisition PARKED with the panic switch off — the shipped-build \
             behaviour #1702 closes",
        );
        // With no `read_budget` frame on that thread there is nowhere to unwind
        // TO, so the refusal is the same panic the armed build raises — same
        // message, from `reentrant_panic_message`, naming both sites.
        let msg = msg.expect("the refusal must leave by an unwind, not by returning a guard");
        assert!(msg.contains("reentreleasespec"), "{msg}");
        assert!(
            msg.contains(&format!("lockwatch.rs:{held_line}")),
            "the refusal must name the line THIS thread already holds it from ({held_line}): {msg}"
        );
        assert_eq!(depth, 0, "the unwind left a phantom hold on that thread's stack");

        // Counted and reported, exactly as the fail-open arm used to be: what
        // changed is that the acquisition does not happen, not that the
        // evidence went away.
        assert_eq!(
            lock_order_violations(),
            before + 1,
            "a refused re-entrant acquisition must still be counted"
        );
        let found = drain_for("reentreleasespec");
        assert_eq!(found.len(), 1, "expected exactly one stamped finding: {found:?}");
        assert_eq!(found[0].kind, REPORT_REENTRANT, "{found:?}");
        assert_eq!(found[0].event(), "lock-reentrant");
        assert_eq!(
            found[0].outer_site_line, held_line,
            "the finding must name the line the hold it collided with was taken on"
        );
    }

    #[test]
    fn a_reentrant_acquire_unwinds_a_sealed_frame_and_counts_the_tear() {
        // The one narrowing of `lock-liveness.md` §4.1 (#1702). A frame that
        // has made a durable write is SEALED: a budget timeout under it waits
        // instead of unwinding, because a slow mutation is a stall and an
        // abandoned one is corruption. That trade is only good while waiting
        // ENDS. A re-entrant acquisition is the case where it does not — the
        // alternative to the tear is not "later", it is "never" — so this one
        // unwinds through the seal, and the tear is COUNTED where a wedge
        // would have been counted as nothing.
        let _serial = SERIAL.lock_safe();
        let _restore = PanicSwitch(set_lock_order_panics(false));
        let lock = TrackedMutex::new_ranked("reentsealspec", OUTER, 5u32);
        let (_, torn_before) = budget::thread_seal_counts();
        // The process-global counter is the FIELD-REPORT number, and it is
        // checked as a floor rather than a delta because `cargo test` runs this
        // binary's tests concurrently. It is asserted here because after #1702
        // nothing else does: `budget.rs`'s seal test had to move off it (its
        // own assertion is an ABSENCE, which a global cannot state once
        // anything in the process legitimately tears — which is this test).
        let global_torn_before = budget::torn_writes();

        let out: Result<(), Busy> = budget::read_budget(Duration::from_secs(30), || {
            // Discriminating half: inside a sealed frame an ordinary
            // acquisition still succeeds and is NOT unwound. Without it this
            // test would pass against a build that unwound every acquisition
            // made after a durable write.
            let held = lock.lock_safe();
            assert_eq!(*held, 5, "a sealed frame must still be able to take a free lock");
            budget::note_durable_write("reentsealspec-test");
            assert!(budget::sealed_for_test(), "the frame must be sealed before the re-entry");
            let _boom = lock.lock_safe();
            unreachable!("the re-entrant acquisition must not be granted");
        });

        let busy = out.expect_err("the frame must be left with a Busy, not completed");
        assert!(busy.is_reentrant(), "the refusal must be typed as re-entrant: {busy:?}");
        assert_eq!(busy.lock, "reentsealspec");
        assert_eq!(held_lock_depth(), 0, "the unwind left a phantom hold on the stack");
        let (_, torn_after) = budget::thread_seal_counts();
        assert_eq!(
            torn_after,
            torn_before + 1,
            "a tear through the seal must be COUNTED — that is what makes it better than a wedge"
        );
        assert!(
            budget::torn_writes() > global_torn_before,
            "the field-report counter must move too, or a tear is invisible outside a test"
        );
    }

    #[test]
    fn the_held_stack_unwinds_with_the_thread() {
        // `TrackedGuard::drop` runs during an unwind (#1609 rider R1), and the
        // stack pop rides in it — so a `read_budget` timeout, which is an
        // unwind, cannot leave phantom holds behind that make every later
        // acquisition on that thread look like an inversion.
        let _serial = SERIAL.lock_safe();
        let lock = TrackedMutex::new_ranked("unwindstackspec", OUTER, ());
        assert_eq!(held_lock_depth(), 0);
        let msg = panic_message(std::panic::AssertUnwindSafe(|| {
            let _g = lock.lock_safe();
            assert_eq!(held_lock_depth(), 1, "the hold must be on the stack while it is held");
            panic!("deliberate");
        }));
        assert_eq!(msg.as_deref(), Some("deliberate"));
        assert_eq!(held_lock_depth(), 0, "the unwind left a phantom hold on the stack");
    }
}
