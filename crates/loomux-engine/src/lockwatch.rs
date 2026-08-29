//! Instrumented mutexes (#1601, plan §3 Phase 0.1/0.2) — what was holding a
//! lock when the app stopped answering — and, since #1609 (Phase 2.1), a
//! bound on how long a waiter pays for it.
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
//! line plus one monotonic clock read. There is **no allocation, no formatting,
//! no global lock and no syscall on either** — the global registry is touched
//! at CONSTRUCTION only, and every byte of every report is produced on the
//! watchdog thread, with no tracked lock held. See [`TrackedMutex::lock_safe`]
//! and [`TrackedGuard::drop`].
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
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
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
}

impl LockState {
    fn new(name: &'static str) -> Self {
        Self {
            id: NEXT_LOCK_ID.fetch_add(1, Ordering::Relaxed),
            name,
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

/// A tracked-lock acquisition that ran out of budget.
///
/// This is the typed value the whole of Phase 2.1 converts a hang into: a
/// polled view keeps its previous value and marks itself partial, an MCP call
/// answers a retryable error, a cadenced tick skips. Every field is here so the
/// answer can NAME the cause rather than say "busy" — the epic's §2.3 is that
/// four incidents produced no evidence at all, and an unexplained "busy" is the
/// same defect one layer up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Busy {
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
    pub fn retry_after_ms(&self) -> u64 {
        BUSY_RETRY_AFTER_MS
    }

    /// The breadcrumb `detail`: one line of `key=value` fields.
    ///
    /// Space-free values throughout, for the same reason
    /// [`HoldReport::detail`] is: a breadcrumb line is `stamp event detail`
    /// split on spaces.
    pub fn detail(&self) -> String {
        match &self.holder {
            Some(h) => format!(
                "lock={} waited_ms={} waiters={} held_ms={} thread={} at={}:{}",
                spaceless(self.lock),
                self.waited.as_millis(),
                self.waiters,
                h.held_ms,
                h.thread,
                spaceless(h.site_file),
                h.site_line,
            ),
            None => format!(
                "lock={} waited_ms={} waiters={} holder=unsampled",
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
        let state = Arc::new(LockState::new(name));
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
    /// With no [`crate::budget::read_budget`] frame installed — which is every
    /// call site that existed before #1609, and every mutating one after it —
    /// this is an unbounded acquire and behaves exactly as Phase 0 left it.
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
        let Some((left, frame)) = budget::remaining() else {
            return self.acquire_blocking(site);
        };
        let in_mutation = budget::in_mutation();
        let event = if in_mutation { "lock-busy-in-mutation" } else { "lock-busy" };
        match self.acquire_within(site, left, event) {
            Ok(g) => g,
            Err(busy) => {
                if in_mutation {
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
    #[track_caller]
    pub fn lock_within(&self, budget: Duration) -> Result<TrackedGuard<'_, T>, Busy> {
        self.acquire_within(Location::caller(), budget, "lock-busy")
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
    #[track_caller]
    pub fn try_lock_safe(&self) -> Option<TrackedGuard<'_, T>> {
        let site = Location::caller();
        let guard = self.inner.try_lock()?;
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
        TrackedGuard { guard, state: st, acquired_ms }
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
    /// above) and one release read-modify-write (`generation`). All of it is on
    /// this lock's own cache line, and none of it can block.
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

    /// Hold `lock` on its own thread until the returned sender is dropped,
    /// returning only once the hold is REAL.
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
            let _ = release_rx.recv();
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
        let holder = busy.holder.expect("the holder was sampled: it is parked, so nothing moved");
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
