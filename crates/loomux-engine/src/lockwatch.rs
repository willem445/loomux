//! Instrumented mutexes (#1601, plan §3 Phase 0.1/0.2) — what was holding a
//! lock when the app stopped answering.
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
//! This module is the missing artifact. It does not fix anything, bound
//! anything, or change any behaviour; it makes the state a hang leaves behind
//! *reportable*, so the next remedy is chosen against evidence instead of
//! against a story that fits.
//!
//! # The shape
//!
//! [`TrackedMutex`] is `std::sync::Mutex` plus a small block of atomics, and
//! its [`lock_safe`](TrackedMutex::lock_safe) has the SAME signature and the
//! same poison-tolerant semantics as [`crate::obs::LockExt::lock_safe`] — so
//! the call sites in `orchestration/mod.rs` are untouched and the migration is
//! a type swap on the registry's fields.
//!
//! Two reporters, and they are complementary rather than redundant:
//!
//! - **The guard's `Drop`** reports a hold that exceeded the threshold, with
//!   the exact total. It cannot miss a *completed* hold, however briefly the
//!   watchdog happened to be looking elsewhere.
//! - **The watchdog** ([`LockWatch::tick`], driven by [`crate::selfwatch`])
//!   reports a hold that is STILL in flight. That is the case `Drop` can never
//!   report, and it is the beta6 case: a hold that never ends has no drop to
//!   run.
//!
//! Together they cover every hold past the threshold — one of the two always
//! fires, and neither depends on the other being wired.
//!
//! # What an acquisition costs
//!
//! Everything on the acquire path is a fixed number of atomic operations on
//! this lock's own cache line plus one monotonic clock read. There is **no
//! allocation, no formatting, no global lock and no syscall** on the acquire or
//! the release path — the global registry is touched at CONSTRUCTION only, and
//! the reporting path runs solely when a hold has already lasted seconds, which
//! by definition is not a hot path. See [`TrackedMutex::lock_safe`].
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

use crate::obs::LockExt;
use std::panic::Location;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

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
        }
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
/// Only ever called for a hold that has ALREADY lasted seconds, so the file
/// write it performs is not on any path a latency claim depends on.
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

/// A `Mutex` that says who is holding it, since when, from where, and how many
/// threads are behind them.
///
/// A drop-in for `std::sync::Mutex` at the call sites that use
/// [`crate::obs::LockExt::lock_safe`]: the inherent `lock_safe` below shadows
/// the trait method, so a field's type changing from `Mutex<T>` to
/// `TrackedMutex<T>` changes nothing at the places that lock it.
pub struct TrackedMutex<T> {
    inner: Mutex<T>,
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
        Self { inner: Mutex::new(value), state }
    }

    /// Poison-tolerant lock, tracked. Same contract as
    /// [`crate::obs::LockExt::lock_safe`]: a mutex poisoned by some other
    /// thread's panic is recovered rather than propagated (#53).
    ///
    /// **The cost, since this is the app's hottest shared path.** Per
    /// acquisition, on top of the `Mutex::lock` that was already there: two
    /// relaxed read-modify-writes on the waiter count, three relaxed stores,
    /// one release read-modify-write on the generation, and one monotonic clock
    /// read. Per release: one release read-modify-write, three relaxed loads
    /// and a comparison. No allocation, no formatting, no global lock, no
    /// syscall, and nothing that can block. The clock read is the only item
    /// above a few nanoseconds — tens of nanoseconds on every platform this
    /// ships to — which is why it is a *monotonic* read and not a `SystemTime`,
    /// and why the cheaper alternative (stamping holds against the watchdog's
    /// 1 Hz tick, one atomic load) was rejected: it would floor every reported
    /// duration at a second, on the one instrument whose whole job is to say
    /// how long something took.
    #[track_caller]
    pub fn lock_safe(&self) -> TrackedGuard<'_, T> {
        let site = Location::caller();
        let st = &self.state;
        // Registered BEFORE blocking: a waiter that only counts once it has
        // stopped waiting is invisible for exactly the interval it matters.
        st.waiters.fetch_add(1, Ordering::Relaxed);
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        st.waiters.fetch_sub(1, Ordering::Relaxed);

        let acquired_ms = mono_ms();
        st.holder_thread.store(this_thread(), Ordering::Relaxed);
        st.holder_site.store(site as *const _ as *mut _, Ordering::Relaxed);
        st.acquired_ms.store(acquired_ms, Ordering::Relaxed);
        // Publishes the three stores above; even -> odd marks the lock held.
        st.generation.fetch_add(1, Ordering::Release);

        TrackedGuard { guard, state: st, acquired_ms }
    }

    /// Take the lock if it is free RIGHT NOW, never blocking.
    ///
    /// Poison-tolerant like [`lock_safe`](Self::lock_safe) — a mutex some other
    /// thread panicked under is recovered rather than reported as unavailable,
    /// because "the data may be slightly stale" is not the same fact as "the
    /// lock is busy" and a caller choosing between the two deserves the right
    /// one. `None` means genuinely held by someone else.
    ///
    /// A successful acquisition is recorded exactly as a blocking one is, so a
    /// hold taken this way is as visible to the watchdog as any other. A
    /// FAILED one touches the waiter count not at all: nothing waited.
    #[track_caller]
    pub fn try_lock_safe(&self) -> Option<TrackedGuard<'_, T>> {
        let site = Location::caller();
        let guard = match self.inner.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        let st = &self.state;
        let acquired_ms = mono_ms();
        st.holder_thread.store(this_thread(), Ordering::Relaxed);
        st.holder_site.store(site as *const _ as *mut _, Ordering::Relaxed);
        st.acquired_ms.store(acquired_ms, Ordering::Relaxed);
        st.generation.fetch_add(1, Ordering::Release);
        Some(TrackedGuard { guard, state: st, acquired_ms })
    }

    /// This lock's name, as given to [`TrackedMutex::new`].
    pub fn name(&self) -> &'static str {
        self.state.name
    }

    /// Threads currently blocked waiting for this lock.
    pub fn waiters(&self) -> u32 {
        self.state.waiters.load(Ordering::Relaxed)
    }
}

/// A held [`TrackedMutex`]. Derefs to the guarded value exactly like a
/// `MutexGuard`, and clears the hold record when it drops.
pub struct TrackedGuard<'a, T> {
    guard: MutexGuard<'a, T>,
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
    fn drop(&mut self) {
        let st = self.state;
        let held_ms = mono_ms().saturating_sub(self.acquired_ms);
        let waiters = st.waiters.load(Ordering::Relaxed);
        let thread = st.holder_thread.load(Ordering::Relaxed);
        let (file, line) = site_of(st.holder_site.load(Ordering::Relaxed));
        // odd -> even: the lock is free. Published before the report so a
        // watchdog tick racing this drop sees the release rather than a hold it
        // would double-report.
        st.generation.fetch_add(1, Ordering::Release);
        // NEUTERED (scratch, #1601): unreachable, so a released hold is
        // never reported however long it lasted.
        if held_ms == u64::MAX && held_ms >= HOLD_WARN_MS.load(Ordering::Relaxed) {
            record(HoldReport {
                lock: st.name,
                site_file: file,
                site_line: line,
                holder_thread: thread,
                held_ms,
                waiters,
                still_held: false,
            });
        }
    }
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
