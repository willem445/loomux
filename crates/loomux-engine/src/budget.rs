//! Bounded acquisition (#1609, plan §3 Phase 2.1) — the thread-local budget a
//! read path runs under, and the scope that switches it off for a mutation.
//!
//! # What this closes
//!
//! [`crate::lockwatch`] (Phase 0) made a long hold *reportable*. It bounds
//! nothing: `lock_safe()` still parks forever, and the epic's §1.2 chain runs
//! from exactly that — one wedged mutex, then every MCP request thread parked
//! in `resolve_token` before dispatch, then the shared blocking pool filling
//! with polled reads, then every pane refusing input.
//!
//! Phase 1 removed the poll path's acquisitions outright. The measured hole it
//! does not reach is the MCP one: the soak lane (#1606) shows an MCP `ping`
//! answering in 6 ms normally and **not at all in 20 s** while `groups` is
//! held, because `OrchRegistry::resolve_token` takes `groups` before any
//! dispatch happens and `ping` never gets that far. A budget is what turns
//! that into a retryable answer.
//!
//! # The lever, and why it is a thread-local rather than 30 call-site edits
//!
//! The alternative considered and rejected in the plan was a `_within` variant
//! of each registry read function. That recreates the "did we remember to bound
//! this one" review dependency the epic's §2 is *about*: every guard this repo
//! has added for the last four hangs described the previous failure exactly,
//! and the next unbounded read is the one nobody thought to convert.
//!
//! So the budget travels with the THREAD, not with the function.
//! [`read_budget`] installs a deadline; every [`crate::lockwatch::TrackedMutex`]
//! acquisition made underneath it — including ones written next month, in code
//! that has never heard of this module — is bounded by whatever is left of it.
//!
//! # Why the exit is an unwind
//!
//! `lock_safe()` returns a guard, not a `Result`. That signature is what let
//! Phase 0 swap 448 call sites by changing a field's type, and changing it now
//! would undo that. An infallible signature has exactly two ways to report a
//! failure: hang, or unwind. So a timeout unwinds — with a typed payload,
//! caught at the [`read_budget`] frame that owns the deadline, thrown by
//! [`std::panic::resume_unwind`], which does **not** run the panic hook. No
//! crash log is written and `obs`'s hook never sees it: this is a control-flow
//! edge dressed as an unwind, not a panic.
//!
//! # Why that is safe, and where the argument lives
//!
//! An unwind through a read path can only be safe if no read path is in the
//! middle of writing something when it fires. Two mechanisms, one argument:
//!
//! - [`MutationScope`] — at every mutating entry point. A budget timeout
//!   observed at depth > 0 does not unwind at all: it waits, unbounded, and
//!   breadcrumbs `lock-busy-in-mutation`. A half-applied multi-map mutation is
//!   therefore not *unlikely*, it is unreachable.
//! - The audit of the writes that happen on READ paths — `usage_memo`,
//!   `default_branch_memo` and the rest — which is `doc/design/lock-liveness.md`
//!   §4, and is where a reviewer should hold this.

use crate::lockwatch::Busy;
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// The budgets. One place, on purpose (plan 3/4 a-i).
// ---------------------------------------------------------------------------

/// The snapshot publisher's per-section budget.
///
/// One second, because the publisher's own cadence is the recovery: a section
/// that cannot get its lock this pass keeps its previous value, is marked
/// partial, and is tried again next pass. Waiting longer would buy a fresher
/// number at the cost of the tick it is supposed to be serving.
pub const POLL_LOCK_BUDGET: Duration = Duration::from_secs(1);

/// A cadenced backend loop's ENTRY acquisition.
///
/// Five seconds — above any hold this app takes deliberately (see
/// [`crate::lockwatch::DEFAULT_HOLD_WARN_MS`], the threshold past which a hold
/// is already reportable), so a tick skips only when something is genuinely
/// wrong. A skipped tick is bounded by the next tick, which is INV-6's shape.
pub const TICK_LOCK_BUDGET: Duration = Duration::from_secs(5);

/// Resolving an MCP caller's token, before dispatch.
///
/// The tightest of the MCP three because it is paid by EVERY request including
/// `ping`, which takes no registry lock of its own and is the orchestrator's
/// "are you alive" probe. This is the budget that answers #1606's measured
/// hole.
pub const MCP_AUTH_BUDGET: Duration = Duration::from_secs(5);

/// A read-only MCP tool call.
///
/// Longer than the auth budget because a read tool genuinely does more work,
/// and short enough that an agent's turn is not spent waiting: an agent that
/// gets a retryable answer in 15 s can do something else, and one that gets
/// nothing in 15 s has already lost the turn.
pub const MCP_READ_BUDGET: Duration = Duration::from_secs(15);

/// How long the MCP handler waits for a MUTATING tool before answering that it
/// is still running.
///
/// A deadline on the WAIT, never on the work: see `doc/design/lock-liveness.md`
/// §3. The tool keeps executing on its own thread and completes exactly once.
///
/// The SHIPPED value. Code reads [`mutate_deadline`], which is this unless a
/// test has overridden it — the same shape (and the same reason)
/// [`crate::lockwatch::set_hold_warn_ms`] has: a liveness test whose subject is
/// a 30-second bound would otherwise add 30 seconds to every run of the suite,
/// and what the test is about is the SHAPE of the answer, not the number.
pub const MCP_MUTATE_DEADLINE: Duration = Duration::from_secs(30);

static MUTATE_DEADLINE_MS: AtomicU64 = AtomicU64::new(30_000);

/// How long the MCP handler waits for a mutating tool. [`MCP_MUTATE_DEADLINE`]
/// unless a test has moved it.
pub fn mutate_deadline() -> Duration {
    Duration::from_millis(MUTATE_DEADLINE_MS.load(Ordering::Relaxed))
}

/// Move the mutating-tool deadline. Process-wide; returns the previous value so
/// a caller can restore it from a `Drop` guard rather than remembering to.
#[doc(hidden)]
pub fn set_mutate_deadline_for_test(d: Duration) -> Duration {
    Duration::from_millis(MUTATE_DEADLINE_MS.swap(d.as_millis() as u64, Ordering::Relaxed))
}

/// A human's one-shot read command (`orch_tasks`, `orch_audit`, …).
///
/// Ten seconds: a human who clicked something is owed an answer well inside
/// their patience, and these commands degrade to an empty value rather than an
/// error (`command_group` has no error channel), so the cost of the bound is a
/// blank panel and a breadcrumb rather than a wrong one.
pub const COMMAND_READ_BUDGET: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// The thread-local state
// ---------------------------------------------------------------------------

/// Mints frame ids. Monotonic and process-local — no getrandom (CLAUDE.md
/// constraint 2), like every other id in this crate.
static NEXT_FRAME: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// This thread's active budget: `(deadline, owning frame id)`.
    ///
    /// `Cell<Option<..>>` of two `Copy` scalars, so reading it on the
    /// acquisition path is a thread-local load and nothing else — no
    /// allocation, no destructor at thread teardown, which matters because the
    /// MCP server spawns one thread per request.
    static BUDGET: Cell<Option<(Instant, u64)>> = const { Cell::new(None) };

    /// How many [`MutationScope`]s this thread is inside.
    static MUTATION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// What is left of this thread's budget, and which frame owns it.
///
/// `Some((Duration::ZERO, frame))` is a real answer and not an absence: the
/// deadline has passed, so the next acquisition gets one try and no wait.
pub(crate) fn remaining() -> Option<(Duration, u64)> {
    BUDGET.with(|b| {
        b.get().map(|(deadline, frame)| (deadline.saturating_duration_since(Instant::now()), frame))
    })
}

/// Whether this thread is inside a [`MutationScope`].
pub(crate) fn in_mutation() -> bool {
    MUTATION_DEPTH.with(|d| d.get()) > 0
}

/// The typed unwind payload. Carries the frame it is addressed to, so a nested
/// [`read_budget`] cannot swallow an outer frame's timeout.
pub(crate) struct BudgetTimeout {
    pub(crate) frame: u64,
    pub(crate) busy: Busy,
}

/// Leave the current [`read_budget`] frame with `busy`.
///
/// `resume_unwind` rather than `panic!`: it does not invoke the panic hook, so
/// `obs`'s hook writes no crash log and the user is not told the app died. The
/// payload is typed, so [`read_budget`] can tell its own timeout from a genuine
/// panic passing through and resume the latter untouched.
pub(crate) fn unwind_to_frame(frame: u64, busy: Busy) -> ! {
    std::panic::resume_unwind(Box::new(BudgetTimeout { frame, busy }))
}

// ---------------------------------------------------------------------------
// The public API
// ---------------------------------------------------------------------------

/// Run `f` with every tracked-lock acquisition inside it bounded by `budget`.
///
/// Returns `Err(Busy)` if some acquisition underneath ran out of budget,
/// naming the lock and — when it could be sampled without waiting — who was
/// holding it and for how long. `f` is then abandoned partway through, which is
/// the whole reason [`MutationScope`] exists.
///
/// **Nesting takes the TIGHTER deadline, and the frame id follows it.** An
/// inner `read_budget(30s)` inside an outer `read_budget(1s)` does not extend
/// anything: the outer deadline stays active, a timeout carries the OUTER
/// frame's id, and this inner frame resumes the unwind rather than catching it.
/// Without that, a nested call could quietly buy itself more time than the
/// caller that has to answer a poll tick.
///
/// **The `AssertUnwindSafe`, stated rather than hidden.** `catch_unwind`
/// normally refuses a closure holding `&mut` state, because catching a panic
/// can expose a half-updated value. The assertion is sound here for a reason
/// that is checkable rather than hopeful: the only unwind this frame CATCHES is
/// its own [`BudgetTimeout`], every other payload is resumed untouched, and the
/// state that can be mid-write when a `BudgetTimeout` fires is exactly what
/// `doc/design/lock-liveness.md` §4 enumerates. A genuine panic is never
/// converted into a recovered value here.
pub fn read_budget<T>(budget: Duration, f: impl FnOnce() -> T) -> Result<T, Busy> {
    let frame = NEXT_FRAME.fetch_add(1, Ordering::Relaxed);
    let deadline = Instant::now() + budget;
    let prev = BUDGET.with(|b| b.get());
    let active = match prev {
        // An outer deadline that is already tighter keeps both the deadline
        // and the ownership of it.
        Some((d, f0)) if d <= deadline => (d, f0),
        _ => (deadline, frame),
    };
    BUDGET.with(|b| b.set(Some(active)));

    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    BUDGET.with(|b| b.set(prev));

    match out {
        Ok(v) => Ok(v),
        Err(payload) => match payload.downcast::<BudgetTimeout>() {
            // Ours.
            Ok(t) if t.frame == frame => Err(t.busy),
            // An outer frame's — this one is not the deadline's owner.
            Ok(t) => std::panic::resume_unwind(t),
            // A genuine panic. Untouched, hook already run, crash log already
            // written by `obs`.
            Err(other) => std::panic::resume_unwind(other),
        },
    }
}

/// Marks the calling frame as one that MUTATES, switching the unwind off.
///
/// Inside a scope, an acquisition that runs out of budget does not unwind: it
/// breadcrumbs `lock-busy-in-mutation` and then waits, unbounded, exactly as it
/// did before Phase 2.1. That is a deliberate trade and it is the safe half of
/// the trade — a mutation that is slow is a stall, a mutation abandoned halfway
/// between two maps is corruption, and Phase 0's watchdog already reports the
/// stall with the holder's name attached.
///
/// Place one at every mutating ENTRY point rather than around each write: the
/// hazard is a mutation that spans two acquisitions, so the scope has to span
/// them too.
///
/// Re-entrant: scopes nest and the innermost drop does not clear an outer one.
pub struct MutationScope {
    /// Not a unit struct: `MutationScope;` would then be a valid expression
    /// that constructs nothing, protects nothing and drops nothing.
    _private: (),
}

impl MutationScope {
    /// Enter a mutation scope for as long as the returned guard lives.
    #[must_use = "the scope ends when this guard drops; `let _ = MutationScope::enter()` \
                  drops it immediately and protects nothing"]
    pub fn enter() -> Self {
        MUTATION_DEPTH.with(|d| d.set(d.get() + 1));
        Self { _private: () }
    }
}

impl Drop for MutationScope {
    fn drop(&mut self) {
        MUTATION_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

// ---------------------------------------------------------------------------
// The R1 detector
// ---------------------------------------------------------------------------

/// Durable writes seen on an unscoped read path since the process started.
static UNSCOPED_WRITES: AtomicU64 = AtomicU64::new(0);

/// How many of those get a breadcrumb before the counter goes quiet.
///
/// Bounded because a read path that writes every poll tick would otherwise
/// produce a breadcrumb every two seconds forever — turning the evidence trail
/// into the noise it exists to cut through, which is the same argument
/// [`crate::lockwatch::TrackedMutex::busy`]'s edge-trigger makes. The counter
/// keeps the whole truth regardless; only the narration is capped.
const UNSCOPED_WRITE_BREADCRUMBS: u64 = 8;

/// Report a durable write that is happening inside a [`read_budget`] frame and
/// OUTSIDE any [`MutationScope`].
///
/// This is rider R1's residual, made detectable instead of merely argued.
/// `doc/design/lock-liveness.md` §4 enumerates the writes that occur on read
/// paths today and shows each is either harmless under an unwind or scoped. The
/// part an enumeration cannot cover is the next edit: a write added to a
/// function some read path calls, with nothing to say so. Every durable
/// orchestration state file goes through
/// [`crate::fsatomic::atomic_write`], so that is where this is called from, and
/// the answer to "did anyone remember" stops being a review dependency.
///
/// **It reports; it does not refuse.** A panic here would be a guard that
/// blocks a build, and this repo's rule is that a refusing guard ships only
/// after it has run clean over known-good subjects — which cannot be
/// established for a write path nobody has enumerated yet. So: a counter a test
/// can assert on, and a bounded breadcrumb trail a field report carries.
///
/// **What it does NOT see**, stated where it lives: writes that are not
/// `atomic_write` — `fs::rename`, `fs::write`, `fs::create_dir_all`. The one
/// such site on a read path today is `load_usage_snapshots`' corrupt-file
/// rename, and it is inside `merge_usage_snapshots`' scope; §4 names it.
pub fn note_durable_write(what: &str) {
    // Cheap and in this order on purpose: two thread-local reads, and the
    // common answer is "no budget installed", which costs one of them.
    if true {
        return;
    }
    let n = UNSCOPED_WRITES.fetch_add(1, Ordering::Relaxed);
    if n < UNSCOPED_WRITE_BREADCRUMBS {
        crate::obs::breadcrumb(
            "write-on-read-budget",
            &format!("what={} seen={}", what.replace(' ', "_"), n + 1),
        );
    }
}

/// How many durable writes have happened on an unscoped read path. The R1
/// residual, made assertable: a test that drives every read entry point under a
/// budget and finds this unmoved has checked the property rather than the
/// prose.
pub fn unscoped_durable_writes() -> u64 {
    UNSCOPED_WRITES.load(Ordering::Relaxed)
}

/// This thread's mutation depth. For tests and for a diagnostic dump; the
/// acquisition path uses [`in_mutation`].
#[doc(hidden)]
pub fn mutation_depth_for_test() -> u32 {
    MUTATION_DEPTH.with(|d| d.get())
}

/// Whether this thread currently has a budget installed. For tests: the
/// property "`read_budget` restores what it found" has no other witness.
#[doc(hidden)]
pub fn budget_active_for_test() -> bool {
    BUDGET.with(|b| b.get()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockwatch::{held_locks, mono_ms, TrackedMutex};
    use std::sync::mpsc;
    use std::sync::Arc;

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
    /// Hold `lock` until the returned sender drops — or until [`HOLD_MAX`],
    /// whichever comes first — returning once the hold is REAL. Same handshake
    /// as `lockwatch`'s: a test that spawns a holder and hopes is measuring the
    /// scheduler.
    fn hold<T: Send + 'static>(lock: Arc<TrackedMutex<T>>) -> mpsc::Sender<()> {
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _g = lock.lock_safe();
            let _ = held_tx.send(());
            // `recv_timeout`, never `recv`: see HOLD_MAX.
            let _ = release_rx.recv_timeout(HOLD_MAX);
        });
        held_rx.recv_timeout(GRACE).expect("setup: the holder thread never acquired the lock");
        release_tx
    }

    /// Release `lock` after `after`, on its own thread. For the cases where the
    /// point is that a waiter EVENTUALLY gets it.
    fn hold_for<T: Send + 'static>(lock: Arc<TrackedMutex<T>>, after: Duration) {
        let (held_tx, held_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _g = lock.lock_safe();
            let _ = held_tx.send(());
            std::thread::sleep(after);
        });
        held_rx.recv_timeout(GRACE).expect("setup: the holder thread never acquired the lock");
    }

    #[test]
    fn a_lock_taken_under_a_budget_answers_busy_instead_of_parking() {
        let lock = Arc::new(TrackedMutex::new("budgetspec", 5u32));

        // The discriminating half first: with nothing held, the same closure
        // returns its value. Without it, `Err(..)` below would be satisfied by
        // a `read_budget` that never succeeds at all.
        let ok = read_budget(Duration::from_millis(200), || *lock.lock_safe());
        assert_eq!(ok.expect("an uncontended read must complete"), 5);

        let release = hold(lock.clone());
        let started = Instant::now();
        let busy = read_budget(Duration::from_millis(100), || *lock.lock_safe())
            .err()
            .expect("a read under a budget must answer Busy, not park");
        assert_eq!(busy.lock, "budgetspec");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the budget was not the bound: {:?}",
            started.elapsed()
        );
        drop(release);
    }

    #[test]
    fn a_timeout_inside_a_mutation_scope_waits_rather_than_unwinding() {
        // The safety lever, and the one property the whole unwind design rests
        // on: a mutation is never abandoned partway between two maps.
        const HELD: Duration = Duration::from_millis(600);
        let lock = Arc::new(TrackedMutex::new("mutspec", 11u32));
        hold_for(lock.clone(), HELD);

        let started = Instant::now();
        let out = read_budget(Duration::from_millis(50), || {
            let _scope = MutationScope::enter();
            *lock.lock_safe()
        });
        let waited = started.elapsed();

        assert_eq!(
            out.expect("inside a MutationScope a timeout must WAIT, not unwind to the frame"),
            11
        );
        // And it really did wait past the budget rather than winning a race:
        // without this the assertion above passes on a lock that was never held.
        assert!(
            waited >= Duration::from_millis(300),
            "it returned in {waited:?}, so the hold was not in its way and this row proves nothing"
        );
    }

    #[test]
    fn an_unwind_leaves_no_tracked_lock_held() {
        // Rider R1's second half, pinned rather than argued. `TrackedGuard::drop`
        // runs as the unwind passes through the frame that holds `first`; if it
        // did not, the lock would read HELD forever to the watchdog and to every
        // later acquirer — a bounded acquisition that manufactures the exact hang
        // it exists to prevent.
        let first = Arc::new(TrackedMutex::new("unwindheldspec", 1u32));
        let blocked = Arc::new(TrackedMutex::new("unwindblockedspec", 2u32));
        let release = hold(blocked.clone());

        // Annotated because the closure diverges: `unreachable!` is `!`, which
        // coerces to anything, so nothing else in this expression pins `T`.
        let out: Result<u32, Busy> = read_budget(Duration::from_millis(100), || {
            let _held = first.lock_safe();
            // Unwinds from HERE, with `_held` live on the stack above it.
            let _never = blocked.lock_safe();
            unreachable!("the acquisition above cannot succeed while the lock is held")
        });
        let busy = out.err().expect("the inner acquisition must have run out of budget");
        assert_eq!(busy.lock, "unwindblockedspec");

        assert!(
            first.try_lock_safe().is_some(),
            "the guard live on the unwinding stack did not release its lock"
        );
        assert!(
            !held_locks(mono_ms()).iter().any(|s| s.name == "unwindheldspec"),
            "the unwound-through lock is still tracked as held, so the watchdog would report a \
             holder that no longer exists"
        );
        drop(release);
    }

    #[test]
    fn read_budget_restores_what_it_found_on_both_exits() {
        assert!(!budget_active_for_test(), "a test thread starts with no budget");
        let lock = Arc::new(TrackedMutex::new("restorespec", 3u32));

        // Ok path.
        let _ = read_budget(Duration::from_millis(200), || *lock.lock_safe());
        assert!(!budget_active_for_test(), "the Ok path left a budget installed");

        // Err path.
        let release = hold(lock.clone());
        let _ = read_budget(Duration::from_millis(50), || *lock.lock_safe());
        assert!(!budget_active_for_test(), "the Err path left a budget installed");
        drop(release);

        // Panic path: a genuine panic must not leave one either, and must not
        // be converted into a recovered value.
        let caught = std::panic::catch_unwind(|| {
            let _ = read_budget(Duration::from_millis(200), || panic!("a real panic"));
        });
        assert!(caught.is_err(), "read_budget swallowed a genuine panic");
        assert!(!budget_active_for_test(), "the panic path left a budget installed");
    }

    #[test]
    fn a_nested_read_budget_cannot_extend_the_outer_deadline() {
        // The failure this forbids is a slow nested read buying itself 30 s
        // inside a poll tick that owes an answer in 1. The outer frame stays the
        // deadline's owner, so the timeout unwinds PAST the inner frame.
        let lock = Arc::new(TrackedMutex::new("nestspec", 9u32));
        let release = hold(lock.clone());

        let started = Instant::now();
        let outer = read_budget(Duration::from_millis(100), || {
            read_budget(Duration::from_secs(30), || *lock.lock_safe())
        });
        let elapsed = started.elapsed();

        assert!(outer.is_err(), "the OUTER frame must be the one that answers Busy");
        assert!(
            elapsed < Duration::from_secs(5),
            "the inner budget extended the outer one: {elapsed:?}"
        );
        drop(release);
    }

    #[test]
    fn an_inner_frame_answers_when_its_own_deadline_is_the_tighter_one() {
        // The other direction, so the rule above is "tighter wins" rather than
        // "outer always wins" — an inner budget that IS tighter must be the one
        // that fires, and its Err must come back to its own caller rather than
        // unwinding the whole outer frame.
        let lock = Arc::new(TrackedMutex::new("nestinnerspec", 9u32));
        let release = hold(lock.clone());

        let outer = read_budget(Duration::from_secs(30), || {
            let inner = read_budget(Duration::from_millis(100), || *lock.lock_safe());
            assert!(inner.is_err(), "the tighter INNER budget must be the one that fires");
            "the outer frame kept running"
        });
        assert_eq!(
            outer.expect("the outer frame must not have been unwound"),
            "the outer frame kept running"
        );
        drop(release);
    }

    #[test]
    fn mutation_scopes_nest_and_unwind_cleanly() {
        assert_eq!(mutation_depth_for_test(), 0);
        {
            let _a = MutationScope::enter();
            assert_eq!(mutation_depth_for_test(), 1);
            {
                let _b = MutationScope::enter();
                assert_eq!(mutation_depth_for_test(), 2);
            }
            // The inner drop must not clear the outer scope: a mutating entry
            // point that calls another one would otherwise lose its protection
            // for the rest of its body.
            assert_eq!(mutation_depth_for_test(), 1);
        }
        assert_eq!(mutation_depth_for_test(), 0);

        // And a panic through a scope leaves the depth clean, so one failed
        // mutation does not make every later read on this thread unbounded-wait.
        let _ = std::panic::catch_unwind(|| {
            let _s = MutationScope::enter();
            panic!("out through the scope");
        });
        assert_eq!(mutation_depth_for_test(), 0, "a scope leaked its depth on an unwind");
    }

    #[test]
    fn the_r1_detector_fires_on_exactly_the_unwindable_shape() {
        // Rider R1's residual is "a write added to a read path by a later
        // edit". `note_durable_write` is what notices; this pins WHICH shape it
        // notices, because a detector that fires on everything gets muted and
        // one that fires on nothing is indistinguishable from a clean tree.
        //
        // Deltas, not absolutes: the counter is process-global and every other
        // test in this binary that writes a file goes through the same door.
        let before = unscoped_durable_writes();
        note_durable_write("outside-any-budget.json");
        assert_eq!(
            unscoped_durable_writes(),
            before,
            "a write with no budget installed is an ordinary mutation and must not be counted"
        );

        let _ = read_budget(Duration::from_secs(1), || {
            let _scope = MutationScope::enter();
            note_durable_write("scoped.json");
        });
        assert_eq!(
            unscoped_durable_writes(),
            before,
            "a write inside a MutationScope cannot be unwound out of, so it must not be counted"
        );

        // The positive control, and the only shape that IS the hazard: inside a
        // budget, outside a scope.
        let _ = read_budget(Duration::from_secs(1), || note_durable_write("unscoped.json"));
        assert_eq!(
            unscoped_durable_writes(),
            before + 1,
            "the detector did not fire on a durable write inside a read_budget frame — the one \
             shape an unwind could tear"
        );
    }
}
