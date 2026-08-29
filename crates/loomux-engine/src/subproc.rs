//! Bounded child-process capture (#656, split out of
//! `OrchRegistry::capture_with_timeout` by #698).
//!
//! Everything the orchestration core needs to run one `gh`/`git` child and get
//! back what it did **under a deadline**: the timeout constants, the bounded
//! wait, the two-pipe drain, and the process-wide ceiling on reader threads
//! abandoned by a capture that gave up.
//!
//! It is here (#888 slice A3 batch 9) because none of it is desktop-specific:
//! `std::process` + `std::thread` and nothing else — no `tauri`, no pty, no
//! pane. A headless daemon runs the same single poll loop the app does, and the
//! property this module exists to protect is that loop's: one child parked on a
//! stalled connection must not stop every `notify_when` notice in the process.
//!
//! Its one outward edge is [`crate::obs::LockExt`] — the poison-tolerant
//! `Mutex::lock` the backlog list is taken through — which batch 7 brought into
//! this crate ahead of it. That ordering was the point of batch 7: nothing that
//! locks can move before `lock_safe` has.

use std::sync::mpsc;
use std::time::Duration;

use crate::obs::LockExt;

/// Bound on ONE `gh` subprocess run by the poller (#656). `gh_capture` was a
/// bare `Command::output()`, which waits forever; since #406 folded both
/// pollers into a single loop, one child parked on a stalled connection stops
/// every `notify_when` notice in the process — and the fleet's whole
/// anti-deadlock discipline ("register the watch, end the turn") rests on
/// those notices arriving.
///
/// Sized well above any healthy call rather than tight: a live `gh` list or
/// `pr checks` lands in ~1s, and a false timeout is not free — `poll_watches`
/// counts one as a failed poll, so three slow-but-live ticks in a row would
/// cancel a watch that was about to resolve. A genuinely stalled connection
/// hangs for minutes or forever, so anything in this band separates the two
/// cases equally well; erring long only costs a slower tick. The bound is
/// per-call, and with the per-tick caps on both halves
/// (`notify::MAX_POLLS_PER_TICK`, `intake::MAX_INTAKE_POLLS_PER_TICK`) one
/// tick is bounded too — generously, but bounded, which is the property that
/// was missing.
pub const GH_CAPTURE_TIMEOUT: Duration = Duration::from_secs(20);
/// How often `capture_with_timeout` re-checks a still-running child. Fine
/// enough to add no perceptible latency to a ~1s call, coarse enough that the
/// wait costs nothing measurable on a loop that wakes every 30s.
const GH_CAPTURE_POLL_STEP: Duration = Duration::from_millis(25);
/// How long the timeout path waits to reap the child it just killed (#656,
/// rev-lead finding 2). Reaping a killed process is normally instantaneous;
/// this exists only so the tail of a bounded wait is itself bounded, for the
/// one case where the kill cannot land immediately (a child in
/// uninterruptible sleep). Short, because waiting longer buys nothing: if it
/// hasn't been reaped by now it is not the sleep that is slow.
const GH_CAPTURE_REAP_TIMEOUT: Duration = Duration::from_secs(2);
/// Ceiling on reader threads left blocked on children `capture_with_timeout`
/// abandoned (#656, rev-lead finding 1). Two per timed-out call, and a tick
/// makes at most `notify::MAX_POLLS_PER_TICK` × 2 + `intake::
/// MAX_INTAKE_POLLS_PER_TICK` × 2 = 24 calls, so this engages well inside a
/// single pathological tick rather than after many — the point is to stop
/// accumulation across ticks, which is where an unbounded leak would actually
/// come from.
pub const GH_CAPTURE_MAX_LEAKED_READERS: usize = 16;

/// Reader threads abandoned by a timed-out `capture_with_timeout`, held so
/// they can be counted rather than forgotten. Process-wide because the leak
/// is: the bound has to survive across ticks, and there is one poll loop.
static GH_CAPTURE_LEAKED_READERS: std::sync::OnceLock<
    crate::lockwatch::TrackedMutex<Vec<std::thread::JoinHandle<Vec<u8>>>>,
> = std::sync::OnceLock::new();

/// [`GH_CAPTURE_LEAKED_READERS`], initialised on first use — a getter for the
/// same reason `orchestration::audit_lock` is one (#1601): registering a lock
/// with the watchdog is not a `const` operation.
fn leaked_readers() -> &'static crate::lockwatch::TrackedMutex<Vec<std::thread::JoinHandle<Vec<u8>>>> {
    GH_CAPTURE_LEAKED_READERS
        .get_or_init(|| crate::lockwatch::TrackedMutex::new("gh_capture_leaked_readers", Vec::new()))
}

/// Drop the handles of readers that have since ended and report how many are
/// still blocked. A reader ends as soon as its pipe closes, which in the
/// ordinary stall is the moment its child is killed — so this normally
/// returns 0 and the ceiling below is never approached.
fn sweep_leaked_readers() -> usize {
    let mut leaked = leaked_readers().lock_safe();
    leaked.retain(|reader| !reader.is_finished());
    leaked.len()
}

/// Whether a capture may spawn a child, given how many abandoned readers are
/// still blocked. Pure, so the ceiling policy is testable without arranging a
/// real leak — the `due_watches`/`due_intake_polls` idiom applied to the one
/// other unbounded resource this poller can grow.
pub fn gh_capture_admitted(live_readers: usize) -> bool {
    live_readers < GH_CAPTURE_MAX_LEAKED_READERS
}

/// Bounded `Child::wait` (#656): poll until the child reports exit or
/// `timeout` elapses. `Ok(None)` means it was still running at the deadline —
/// the caller decides whether that is a kill or a give-up. Every wait on a
/// child in this file goes through here, so there is no arm left that can
/// block without a deadline.
///
/// `pub` for the same reason as `capture_with_timeout`: pinning "still
/// running at the deadline" needs a real child, not a mock.
pub fn wait_bounded(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(GH_CAPTURE_POLL_STEP);
    }
}

/// Spawn a child under a deadline and return **what it did**, not a verdict on
/// it: its exit status, its stdout and its stderr (#656; split out of
/// `OrchRegistry::capture_with_timeout` by #698).
///
/// Both callers need the same delicate machinery — null stdin, concurrent
/// drains of both pipes, a bounded wait, a kill with its own bounded reap, and
/// the process-wide abandoned-reader ceiling — and they differ only in what
/// they do with the result. `capture_with_timeout` collapses a non-zero exit
/// into `Err`, which is right for a `gh` read whose only question is "did it
/// work". The merge queue cannot: `git ls-remote --exit-code` answers "does
/// this ref exist" as `0` vs `2`, so for it a non-zero exit is **data**
/// ([`crate::mqdriver::CmdOut`]). Writing that a second time would be a second
/// implementation of the one primitive here that must not have two —
/// every arm of it exists because of a specific way an unbounded wait bites.
///
/// # Why the merge queue is in scope for the bound at all
///
/// Its `git`/`gh` calls run inside the same single poll loop (#406/#652), so a
/// `git fetch` parked on a stalled connection would stop every `notify_when`
/// notice in the fleet from firing — precisely the failure #656 closed, and the
/// one the fleet's "register the watch, end the turn" discipline rests on.
///
/// See `OrchRegistry::capture_with_timeout`'s comment for the argument behind
/// each step; it is not repeated here.
pub fn capture_raw_with_timeout(
    cmd: std::process::Command,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String, String), String> {
    capture_raw_inner(cmd, timeout, wait_bounded)
}

/// The same capture with its **main wait forced to fail** — the one arm of
/// `capture_raw_inner` that a test cannot otherwise reach (#699).
///
/// Reaching it for real needs `Child::try_wait` itself to error, which no
/// supported platform does on demand, and the arm is precisely where the
/// reader accounting used to be dropped. So the wait is injected rather than
/// mocked away: the production entry point above hands in `wait_bounded` and
/// this one hands in a closure that errors, and every other step — the spawn,
/// the two readers, the abandon accounting, the post-kill reap — is the same
/// code in both. Same `#[doc(hidden)] pub` idiom as
/// `seed_leaked_readers_for_test`: a seam, not a second implementation.
#[doc(hidden)] // pub for integration tests: force the wait-error early return
pub fn capture_raw_with_failing_wait_for_test(
    cmd: std::process::Command,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String, String), String> {
    capture_raw_inner(cmd, timeout, |_child, _timeout| Err("forced wait failure (test seam)".to_string()))
}

/// Give up on a child mid-capture: kill it, reap it on its own deadline, and
/// park **both** readers where the ceiling can see them — the one accounting
/// step every abandonment arm goes through (#699). Returns `reason` so a call
/// site is a single `return Err(abandon_child_and_readers(…))` and cannot
/// account for one reader, or neither, by omission.
///
/// The reap goes through `wait_bounded` for the reason `GH_CAPTURE_REAP_TIMEOUT`
/// exists: a bound whose last act is an unbounded `wait()` is not a bound. On
/// the wait-error arm that reap will normally fail immediately too (whatever
/// broke `try_wait` is still broken), which costs nothing — it is an `Err`,
/// which is already bounded, and the kill above is what the readers need.
fn abandon_child_and_readers(
    child: &mut std::process::Child,
    out_reader: std::thread::JoinHandle<Vec<u8>>,
    err_reader: std::thread::JoinHandle<Vec<u8>>,
    reason: String,
) -> String {
    let _ = child.kill();
    let _ = wait_bounded(child, GH_CAPTURE_REAP_TIMEOUT);
    let mut leaked = leaked_readers().lock_safe();
    leaked.push(out_reader);
    leaked.push(err_reader);
    reason
}

/// The body of the capture, with the main wait injected so the wait-error arm
/// is reachable from a test (see `capture_raw_with_failing_wait_for_test`).
/// `wait` is `wait_bounded` in production and nothing else.
fn capture_raw_inner(
    mut cmd: std::process::Command,
    timeout: Duration,
    wait: impl Fn(&mut std::process::Child, Duration) -> Result<Option<std::process::ExitStatus>, String>,
) -> Result<(std::process::ExitStatus, String, String), String> {
    use std::io::Read as _;
    use std::process::Stdio;

    let live = sweep_leaked_readers();
    if !gh_capture_admitted(live) {
        return Err(format!("gh capture backlog: {live} readers still blocked on abandoned children"));
    }

    // stdin: `output()` nulls it implicitly, `spawn()` does not — and an
    // inherited stdin is how a `gh` that decides to prompt hangs forever. It
    // matters at least as much for `git`, whose credential helpers prompt on a
    // terminal the backend does not have anyone watching.
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let mut child_out = child.stdout.take().expect("stdout piped just above");
    let mut child_err = child.stderr.take().expect("stderr piped just above");
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_out.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_err.read_to_end(&mut buf);
        buf
    });

    // From here on both readers exist, and every exit accounts for both of
    // them exactly once: the arm below joins them, and every arm that gives up
    // on a live child parks them via `abandon_child_and_readers` (kill, bounded
    // reap, park). Nothing between the two `spawn`s above and this point can
    // return, and the two returns that precede them — the backlog refusal and a
    // failed `Command::spawn` — have no readers to account for. (A panic in the
    // second `std::thread::spawn` would unwind past all of this rather than
    // return through it; out of scope, and the design note records why.)
    //
    // #699: the wait-error arm used to be a bare `?`. Uncounted is worse than
    // parked, not better — the ceiling admits on "how many readers are still
    // blocked", so readers the sweep can never see make the bound understate
    // the process it exists to bound, and the child stayed alive too (dropping
    // a `Child` does not kill it), which is precisely what keeps those readers
    // blocked forever.
    let waited = match wait(&mut child, timeout) {
        Ok(waited) => waited,
        Err(e) => return Err(abandon_child_and_readers(&mut child, out_reader, err_reader, e)),
    };
    let Some(status) = waited else {
        return Err(abandon_child_and_readers(
            &mut child,
            out_reader,
            err_reader,
            format!("timed out after {}s", timeout.as_secs()),
        ));
    };

    // Exited on its own: both pipes are at EOF (or about to be), so these joins
    // are the bounded tail of a wait that already finished.
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok((
        status,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

#[doc(hidden)] // pub for integration tests: observe the process-wide reader backlog
pub fn gh_capture_live_readers() -> usize {
    sweep_leaked_readers()
}

/// The backlog list itself, **unswept**: how many reader handles are parked,
/// whether or not their reader has since ended (#699).
///
/// `gh_capture_live_readers` answers the question the ceiling asks and is
/// therefore the wrong instrument for pinning the accounting: on an ordinary
/// abandonment the kill closes both pipes and the readers end within
/// milliseconds, so a swept count cannot tell "parked, then ended" from "never
/// parked at all" — it reads the same either way, and which one a test sees is
/// a race it loses on some platforms and not others. Counting the list itself
/// makes "both handles were handed to the ceiling" observable on its own terms.
#[doc(hidden)] // pub for integration tests
pub fn gh_capture_parked_readers() -> usize {
    leaked_readers().lock_safe().len()
}

/// Empty the backlog and hand back what was parked, so a test can start from a
/// known-zero baseline (#699).
///
/// The list is process-wide and every capture test contributes to it, so "this
/// call parked exactly two" is not observable as a before/after delta: the
/// confound is not merely what earlier tests left behind, it is that the next
/// capture's own opening sweep *removes* the ones that have since ended, moving
/// the count down underneath the baseline while the two new handles push it up
/// (measured on CI: before 4, after 4, with both readers correctly parked).
///
/// Dropping a `JoinHandle` does not stop its thread — the handles simply stop
/// being tracked, which is the same residue the ceiling already tolerates for
/// a reader that ended, and it is confined to a test process.
#[doc(hidden)] // pub for integration tests
pub fn drain_parked_readers_for_test() -> Vec<std::thread::JoinHandle<Vec<u8>>> {
    std::mem::take(&mut *leaked_readers().lock_safe())
}

/// Park `n` controllable blocked readers in the backlog, as a real abandoned
/// reader would be. Dropping the returned senders releases them, so a test
/// can drive both the refusal and the drain without arranging a grandchild
/// that holds a pipe — which is neither portable nor deterministic.
#[doc(hidden)] // pub for integration tests
pub fn seed_leaked_readers_for_test(n: usize) -> Vec<mpsc::Sender<()>> {
    let mut holds = Vec::new();
    let mut leaked = leaked_readers().lock_safe();
    for _ in 0..n {
        let (tx, rx) = mpsc::channel::<()>();
        holds.push(tx);
        leaked.push(std::thread::spawn(move || {
            let _ = rx.recv(); // ends when the test drops its sender
            Vec::new()
        }));
    }
    holds
}
