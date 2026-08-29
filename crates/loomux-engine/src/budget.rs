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

/// A human's one-shot read command (`orch_tasks`, `orch_questions_list`, …).
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

    /// Whether a DURABLE WRITE has happened inside the current budget frame.
    ///
    /// Set by [`note_durable_write`], cleared by [`read_budget`] on the way
    /// out. While it is set, this thread behaves exactly as it does inside a
    /// [`MutationScope`]: a timeout WAITS instead of unwinding. See
    /// [`note_durable_write`] for why that is the rule.
    static SEALED: Cell<bool> = const { Cell::new(false) };

    /// Whether a durable write has happened in the current budget frame AT
    /// ALL — set even when the frame was already exempt from unwinding.
    ///
    /// Deliberately NOT the same flag as `SEALED`, and the difference is the
    /// whole of whether [`torn_writes`] can detect anything. A tear is "this
    /// frame wrote and then unwound"; sealing is what PREVENTS one. Counting
    /// tears off the seal flag makes the count structurally zero in a broken
    /// tree as well as a sound one, because an unsealed frame is exactly the
    /// one that can unwind — which is how the first version of this invariant
    /// came to pass five scratch rounds in a row while enforcing nothing.
    static WROTE: Cell<bool> = const { Cell::new(false) };

    /// Seals and tears on THIS thread, as `(sealed, torn)`.
    ///
    /// The process-global counters beside them are the field-report numbers;
    /// these are what a TEST can assert on. `cargo test` runs a binary's tests
    /// concurrently, so a delta on a global counter is a race against whatever
    /// sibling happens to write next — which is not a hypothetical: the first
    /// version of the seal test failed with `left 2, right 1` against a
    /// mechanism that was working correctly.
    static THREAD_SEALS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
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

/// Whether an unwind is FORBIDDEN on this thread right now — because it is
/// inside a [`MutationScope`], or because a durable write has already landed
/// in this budget frame ([`note_durable_write`]).
///
/// The acquisition path asks this, not `in_mutation` alone: the two have the
/// same consequence and differ only in how they were entered, one declared by
/// a caller and one observed at the write.
pub(crate) fn unwind_forbidden() -> bool {
    in_mutation() || SEALED.with(|s| s.get())
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
    // The seal belongs to the FRAME, not to the thread: an outer frame that
    // already wrote durably stays sealed after this one returns, and this one
    // starts unsealed so its own bound is live until it writes something.
    let sealed_before = SEALED.with(|x| x.replace(false));
    let wrote_before = WROTE.with(|x| x.replace(false));

    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    let _sealed_here = SEALED.with(|x| x.replace(sealed_before));
    let wrote_here = WROTE.with(|x| x.replace(wrote_before));
    BUDGET.with(|b| b.set(prev));

    match out {
        Ok(v) => Ok(v),
        Err(payload) => match payload.downcast::<BudgetTimeout>() {
            // Ours.
            Ok(t) if t.frame == frame => {
                // THE TEAR: this frame performed a durable write and is now
                // unwinding out of it. `note_durable_write` seals the frame
                // precisely so this cannot happen, so a non-zero count means
                // the seal was bypassed — a durable write that reached
                // neither seal door, or a mutation the seal was removed from.
                //
                // Measured off WROTE, never off the seal flag: a sealed frame
                // never unwinds, so counting tears off SEALED gives zero in a
                // broken tree as readily as a sound one. Counted rather than
                // asserted so the invariant is a NUMBER a test can check; a
                // panic here would take the app down over bookkeeping.
                if wrote_here {
                    THREAD_SEALS.with(|c| {
                        let (s, t) = c.get();
                        c.set((s, t + 1));
                    });
                    TORN_WRITES.fetch_add(1, Ordering::Relaxed);
                    crate::obs::breadcrumb(
                        "budget-torn-write",
                        &format!("lock={} seen={}", t.busy.lock, TORN_WRITES.load(Ordering::Relaxed)),
                    );
                }
                Err(t.busy)
            }
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
// The durable-write seal — rider R1, as a mechanism rather than an argument
// ---------------------------------------------------------------------------

/// Budget frames that performed a durable write and were therefore sealed.
///
/// The POPULATION control for [`torn_writes`]: zero torn writes means nothing
/// if no read path ever wrote anything, which is exactly what a test over a
/// faked registry would report.
static SEALED_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Budget frames that unwound AFTER a durable write — the tear this exists to
/// make impossible. Structurally zero; see [`read_budget`].
static TORN_WRITES: AtomicU64 = AtomicU64::new(0);

/// How many seals get a breadcrumb before the trail goes quiet.
///
/// Bounded because a read path that writes every poll tick would otherwise
/// produce a breadcrumb every second forever — turning the evidence trail into
/// the noise it exists to cut through, which is the same argument
/// [`crate::lockwatch::TrackedMutex::busy`]'s edge-trigger makes. The counters
/// keep the whole truth regardless; only the narration is capped.
const SEAL_BREADCRUMBS: u64 = 8;

/// A durable write is happening. If it is inside a [`read_budget`] frame, SEAL
/// that frame: from here to the end of it, a timeout waits instead of
/// unwinding.
///
/// # The rule this implements, and why it is this one
///
/// Rider R1 asked for the writes on read paths to be proved or scoped. Round 1
/// of review showed the enumeration answering it was incomplete — four
/// `ToolKind::Read` arms wrote durably and were never looked at, one of them
/// (`check_mail`) *consumingly*. Two structural rules were available:
///
/// 1. every tool arm that writes durably becomes `ToolKind::Mutate`; or
/// 2. every durable write on a read path is followed by mutation semantics, so
///    a timeout waits.
///
/// **(2), implemented here rather than site by site.** (1) alone cannot reach
/// the writes that are not behind a tool at all — the snapshot publisher's
/// `usage.json` merge is on a read path with no tool anywhere near it — and a
/// hand-placed `MutationScope` per site is the "did we remember" review
/// dependency the epic's §2 is about, over seventeen arms plus whatever is
/// added next month. Classification is still corrected where a tool genuinely
/// mutates (see `mcp::tool_kind`); this is the floor underneath it.
///
/// **Why sealing at the WRITE and not at the function's entry.** The tear is
/// never the write itself — an unwind can only fire at a lock acquisition, so a
/// write is torn only by an acquisition that comes AFTER it. Sealing at the
/// write is therefore the minimum window that closes the hazard: everything
/// before the first durable write keeps its bound, and a read path that never
/// writes is never affected at all. Scoping the whole function would give up
/// the bound on paths that mostly do not write.
///
/// **What it costs.** A read path that writes durably becomes unbounded *after*
/// that write. That is the same trade [`MutationScope`] makes, narrowed to the
/// smallest region that needs it, and it fails toward a stall — which Phase 0's
/// watchdog reports with the holder named — rather than toward a torn write.
///
/// The seal is per FRAME: [`read_budget`] clears it on entry and restores what
/// it found on exit, so an inner frame's write does not silently disarm the
/// outer one's bound for the rest of a long request.
pub fn note_durable_write(what: &str) {
    // Cheap and in this order on purpose: the common answer is "no budget
    // installed", which costs one thread-local read.
    if BUDGET.with(|b| b.get()).is_none() {
        return;
    }
    // Recorded FIRST and unconditionally: a frame that wrote is a frame a
    // tear can be measured against, whether or not it was already exempt.
    WROTE.with(|w| w.set(true));
    if SEALED.with(|s| s.replace(true)) || in_mutation() {
        return; // already sealed, or a declared mutation: nothing to report.
    }
    THREAD_SEALS.with(|c| {
        let (s, t) = c.get();
        c.set((s + 1, t));
    });
    let n = SEALED_FRAMES.fetch_add(1, Ordering::Relaxed);
    if n < SEAL_BREADCRUMBS {
        crate::obs::breadcrumb(
            "budget-sealed",
            &format!("what={} seen={}", what.replace(' ', "_"), n + 1),
        );
    }
}

/// How many budget frames have been sealed by a durable write. The population
/// control: a `torn_writes() == 0` assertion is vacuous unless this moved.
pub fn sealed_frames() -> u64 {
    SEALED_FRAMES.load(Ordering::Relaxed)
}

/// How many budget frames unwound after a durable write. **The invariant: this
/// is zero.**
///
/// **What this is, exactly, and what it is not.** It is a RUNTIME TRIPWIRE,
/// not the counterfactual for the seal — and the difference is worth stating
/// because the first version of this comment claimed the stronger thing.
///
/// A tear needs `WROTE` set, and only [`note_durable_write`] sets it — which
/// also seals. So in a tree where the doors are wired, a counted tear is
/// unreachable by construction: the only mutation that produces one is
/// disarming the seal while leaving the recording, and that reddens this
/// module's own seal tests first (demonstrated: #1609 review round 2, scratch
/// round 6, which reddened
/// `a_durable_write_seals_its_budget_frame_so_a_later_timeout_waits` and
/// `the_seal_belongs_to_the_frame_and_not_to_the_thread`). Those tests ARE the
/// seal's counterfactual.
///
/// What this counter adds is the case no test can stage: the seal bypassed in
/// the FIELD — a door that records but stops sealing, or a frame reaching an
/// unwind with a write behind it for a reason nobody predicted. It fires once
/// per occurrence with a breadcrumb, and `tests/liveness.rs` asserts it stayed
/// at zero across a sweep of every read entry point, which is a regression
/// guard over the real tool surface rather than a proof.
///
/// The residual it cannot see, stated because it is the real one: a durable
/// write through a door that calls neither seal nor record sets no flag at
/// all. `doc/design/lock-liveness.md` §4.3 lists the doors that do.
pub fn torn_writes() -> u64 {
    TORN_WRITES.load(Ordering::Relaxed)
}

/// Whether this thread's current budget frame has been sealed. For tests.
#[doc(hidden)]
pub fn sealed_for_test() -> bool {
    SEALED.with(|s| s.get())
}

/// `(seals, tears)` on THIS thread. The parallelism-safe form of
/// [`sealed_frames`] / [`torn_writes`] — see `THREAD_SEALS`.
pub fn thread_seal_counts() -> (u64, u64) {
    THREAD_SEALS.with(|c| c.get())
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
    fn a_durable_write_seals_its_budget_frame_so_a_later_timeout_waits() {
        // Review round 1, B1/B2. The tear is never the write itself — an unwind
        // can only fire at a lock acquisition, so a write is torn by an
        // acquisition that comes AFTER it. `check_mail` was the live instance:
        // it atomically replaces `mailbox.json` with every message marked read,
        // and THEN takes `app` and `AUDIT_LOCK`. An unwind at either left the
        // human's mail consumed on disk while the caller was told "Nothing was
        // executed".
        //
        // The seal closes it structurally: the first durable write in a frame
        // disarms unwinding for the rest of that frame.
        let lock = Arc::new(TrackedMutex::new("sealspec", 7u32));
        hold_for(lock.clone(), Duration::from_millis(600));

        let sealed_before = sealed_frames();
        let torn_before = torn_writes();
        let started = Instant::now();

        // The seal is read INSIDE the frame, because that is where it lives:
        // `read_budget` restores what it found on the way out, so a check after
        // the call would be asking the wrong question. It is also the only
        // parallelism-safe form — `sealed_frames()` is a process-global counter
        // and `cargo test` runs these concurrently, so an exact delta on it
        // races whatever sibling test happens to write next (measured: this
        // assertion first failed with left 2, right 1, against a mechanism that
        // was working).
        let out = read_budget(Duration::from_millis(50), || {
            note_durable_write("sealspec.json");
            let sealed_here = sealed_for_test();
            // The acquisition AFTER the write — the only shape that can tear it.
            (*lock.lock_safe(), sealed_here)
        });
        let waited = started.elapsed();

        let (value, sealed_here) =
            out.expect("a frame that has already written durably must WAIT, not unwind");
        assert_eq!(value, 7);
        assert!(
            sealed_here,
            "the write must SEAL the frame — that is the mechanism, not a report"
        );
        // And it really did wait past the budget: without this the assertion
        // above passes against a lock that was never held.
        assert!(
            waited >= Duration::from_millis(300),
            "it returned in {waited:?}, so the hold was not in its way and this proves nothing"
        );
        // The global counter only has to MOVE: an exact delta would be a race
        // against every other test in this binary, and the per-frame assertion
        // above is the precise one.
        assert!(
            sealed_frames() > sealed_before,
            "the seal counter did not move, so nothing recorded the write"
        );
        assert_eq!(
            torn_writes(),
            torn_before,
            "a sealed frame must never unwind; torn_writes is the invariant rider R1 asks for"
        );
    }

    #[test]
    fn the_seal_belongs_to_the_frame_and_not_to_the_thread() {
        // Without this, one inner write would disarm every later read on the
        // same thread — an MCP request thread serves one request, but the
        // publisher thread runs pass after pass forever, and a single corrupt
        // `usage.json` rename would silently unbound it for the process's life.
        assert!(!sealed_for_test(), "a test thread starts unsealed");

        let inner = read_budget(Duration::from_millis(200), || {
            read_budget(Duration::from_millis(100), || {
                note_durable_write("inner.json");
                sealed_for_test()
            })
        });
        assert_eq!(
            inner.expect("outer").expect("inner"),
            true,
            "the inner frame must be sealed by its own write"
        );
        assert!(!sealed_for_test(), "the seal must not outlive the frames that set it");

        // A write with no budget installed seals nothing: there is no frame to
        // seal and nothing that could unwind.
        note_durable_write("no-budget.json");
        assert!(!sealed_for_test());
    }
}
