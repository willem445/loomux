//! Windows Job Object kill-on-close integration test (issue #78).
//!
//! Proves the core guarantee W1 adds: enrolling a pane's spawned child in a
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` job means closing the job handle reaps
//! the *whole* descendant tree — not just the direct child. Pre-#78, killing an
//! ancestor on Windows left descendants (orphaned wrapper shells with live
//! agents, a squatting vite) running.
//!
//! The test opens a real ConPTY the same way `spawn_pty` does, enrolls the
//! pane's shell via the exact code path production uses
//! (`loomux_lib::pty::assign_kill_on_close_job`), and only THEN types a command
//! into the pane that starts a wrapper shell around a long-lived process.
//! Dropping only the job handle must reap both of them and the shell.
//! See doc/design/job-object-teardown.md.
//!
//! ## Why nothing in here starts a PowerShell (#1345)
//!
//! The first cut drove the pane with PowerShell, had that shell spawn a *second*
//! PowerShell as the grandchild, and waited up to 15 s for the grandchild's PID
//! to land in a temp file. Every item that window had to cover was a process
//! start: a cold `pwsh` booting far enough to execute a `-Command` script, an
//! 800 ms sleep so the test could win the enrollment race, and then a second
//! `pwsh` image load. Afterwards the test answered "is this PID alive?" by
//! starting yet another PowerShell per answer (`Get-Process`) — four of them on
//! a healthy run, and one more per 100 ms for as long as either teardown wait
//! went unsatisfied.
//!
//! On `windows-latest` it failed four times in one day, always as "grandchild
//! never reported its PID", always green on re-run, and once on a tree whose
//! exact contents had already passed the same job on `main`. The window was the
//! symptom rather than the defect: every process start inside it was an artifact
//! of how the test was written, not anything #78 guarantees. So they are gone,
//! rather than the window being widened over them:
//!
//! 1. **The tree is native.** `cmd.exe` and `ping.exe` are small, always-resident
//!    System32 binaries; PowerShell is a CLR host that has to boot before it can
//!    run a script, which is what made the old critical path long enough to lose.
//!    Nothing about the guarantee under test cares which program the descendants
//!    run — only that they are descendants born after enrollment.
//! 2. **The ordering is causal, not timed.** The old shell script slept 800 ms so
//!    the test could win the race to enroll it before it forked. Here the
//!    descendants cannot exist until the command that creates them is typed into
//!    the pane, and it is typed after `assign_kill_on_close_job` has returned —
//!    so "born after enrollment" holds by construction and there is no race to
//!    lose. Typing into a pane's shell is also what orrerix does in production.
//! 3. **Liveness and teardown are handle waits.** A `SYNCHRONIZE` handle on a
//!    process is signaled the instant the kernel terminates it, so the teardown
//!    assertion is a real wait (`WaitForSingleObject`) that returns the moment it
//!    happens — no polling, no per-poll process start, and no PID-reuse hazard,
//!    since a handle names one process for as long as it is held. Discovery
//!    still reads the real OS process table (`sysinfo`, which sits on
//!    `NtQuerySystemInformation` — the same table `Get-Process` reports on);
//!    what it no longer does is boot a shell to ask.
//!
//! The budgets below are therefore slack, not schedule: a passing run leaves
//! [`SHELL_SPEAKS`], [`TREE_APPEARS`] and [`TREE_DIES`] almost entirely unspent,
//! because each one ends when the thing it is waiting for happens.
//!
//! Windows-only: the feature is Windows-only (Unix relies on process-group
//! teardown). The whole file compiles to nothing elsewhere.
#![cfg(windows)]

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

/// Budget for "the pane's shell has started and said something".
///
/// This and the two below are all slack rather than schedule: each ends when its
/// event happens, and each is a wait on a local read or a kernel handle rather
/// than on a process start, so a large budget costs a passing run nothing and
/// costs a broken one only the time to report. Measured on the CI leg this test
/// flaked on, everything before teardown — ConPTY open, shell start, the typed
/// command, and discovery — costs ~3 s in total, so the two 60 s budgets carry
/// roughly a 20x margin between them (#1345).
const SHELL_SPEAKS: Duration = Duration::from_secs(60);
/// Budget for "the two System32 processes I just typed have appeared in the
/// process table". The poll under it is a local process-table read, not a
/// process start — which is the whole difference from the 15 s window this
/// replaced.
const TREE_APPEARS: Duration = Duration::from_secs(60);
/// Budget for "the kernel has torn the job down". `KILL_ON_JOB_CLOSE` terminates
/// every member as the last handle closes, and this is a wait on the member
/// handles themselves — a passing run returns as they are signaled, so only a
/// genuine survival can spend it.
const TREE_DIES: Duration = Duration::from_secs(30);

/// The command typed into the pane to build the tree. `ping` to loopback with a
/// 300-count is a ~300 s sleep out of System32: cmd.exe has no sleep builtin, and
/// the native alternatives are each disqualified by their console handling —
/// `timeout` refuses to run at all when its input is redirected, and
/// `pause`/`choice` block on a keypress this test is never going to send. `/d`
/// suppresses the `AutoRun` registry command, so a configured machine cannot
/// inject an extra process into the tree enumerated below.
const SPAWN_TREE_CMD: &[u8] = b"cmd.exe /d /c ping -n 300 127.0.0.1 > nul\r";
/// Image name (lowercased) and depth below the enrolled pane shell of the two
/// processes `SPAWN_TREE_CMD` builds. The depths are the point: depth 2 is what
/// makes this test's name true — a *grandchild*, not just the direct child,
/// which is the whole of what #78 added.
const WRAPPER: (&str, u32) = ("cmd.exe", 1);
const GRANDCHILD: (&str, u32) = ("ping.exe", 2);

/// An OS handle to a process, held so the process can be *waited on* rather than
/// polled for. It also pins identity: a PID may be recycled the moment its
/// process dies, but a handle keeps naming the one process it was opened for.
struct ProcHandle(HANDLE);

impl ProcHandle {
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: a plain Win32 call; the handle is closed in Drop.
        unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            )
            .ok()
            .map(ProcHandle)
        }
    }

    /// Still running? A process handle is signaled on exit, so a zero-timeout
    /// wait reads live/dead without blocking: `WAIT_TIMEOUT` means "not signaled
    /// yet", i.e. alive.
    fn alive(&self) -> bool {
        // SAFETY: `self.0` is live until Drop.
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_TIMEOUT }
    }

    /// Block until this process exits or `deadline` passes; returns whether it
    /// exited.
    fn wait_exit(&self, deadline: Instant) -> bool {
        let ms = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        // SAFETY: `self.0` is live until Drop.
        unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for ProcHandle {
    fn drop(&mut self) {
        // SAFETY: opened by `Self::open`, dropped exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// One live descendant: PID, lowercased image name, depth below the root.
type Descendant = (u32, String, u32);

/// Every live descendant of `root_pid`, walked over the OS process table.
///
/// `born_at_or_after` is a floor on start time, and it is load-bearing rather
/// than tidiness: Windows never reparents, so a process whose parent died long
/// ago still names that parent's PID — and PIDs recycle. A stranger recorded
/// against our shell's number before we ever ran would otherwise be adopted into
/// the tree and then waited on for a death that is never coming, turning this
/// test into a hang. Nothing that predates the shell can be a descendant of it,
/// so the floor drops exactly those. (The shell's own PID cannot be recycled out
/// from under us: the test holds its child handle for the whole run.)
fn live_descendants(sys: &System, root_pid: u32, born_at_or_after: u64) -> Vec<Descendant> {
    let mut found: Vec<Descendant> = Vec::new();
    let mut frontier: Vec<(u32, u32)> = vec![(root_pid, 0)];
    while let Some((parent, depth)) = frontier.pop() {
        // Bound the walk. A recycled PID could in principle close a cycle, and a
        // test that hangs is a worse failure than one that reports wrong. The size
        // cap abandons the walk; the depth cap only stops descending, so the rest
        // of the frontier is still drained.
        if found.len() >= 64 {
            break;
        }
        if depth >= 8 {
            continue;
        }
        for (pid, proc_) in sys.processes() {
            let pid = pid.as_u32();
            if proc_.parent().map(|p| p.as_u32()) != Some(parent) {
                continue;
            }
            if proc_.start_time() < born_at_or_after {
                continue;
            }
            if found.iter().any(|(seen, _, _)| *seen == pid) {
                continue;
            }
            let name = proc_.name().to_string_lossy().to_ascii_lowercase();
            found.push((pid, name, depth + 1));
            frontier.push((pid, depth + 1));
        }
    }
    found
}

/// Does `tree` contain a process with this image name at this depth?
fn has(tree: &[Descendant], want: (&str, u32)) -> bool {
    tree.iter().any(|(_, name, depth)| name == want.0 && *depth == want.1)
}

/// Poll `cond` until it holds or `timeout` elapses. Returns whether it held.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

/// Re-read the process table. `nothing()` still yields pid, parent pid, image
/// name and start time — every field this test reads — while skipping the
/// per-process handle opens that memory/cpu/exe would cost.
fn refresh(sys: &mut System) {
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
}

#[test]
fn kill_on_close_job_reaps_the_whole_descendant_tree() {
    // Open a real ConPTY exactly like spawn_pty, running an interactive cmd.exe:
    // a pane's shell, which is the thing #78 is about.
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/d"]);
    let child = pair.slave.spawn_command(cmd).expect("spawn shell");
    drop(pair.slave);

    let shell_pid = child.process_id().expect("shell pid");

    // Enroll via the production code path. This is the unit under test, and it
    // runs before a single byte is typed — which is what makes "the descendants
    // are born after enrollment" a fact about causality rather than a race the
    // old 800 ms sleep had to win (#1345).
    let job = loomux_lib::pty::assign_kill_on_close_job(shell_pid)
        .expect("job creation + assignment must succeed on Windows");

    // Wait for the shell to speak — its banner and prompt — before typing at it.
    // Console input written into a ConPTY is buffered, so this is belt and
    // braces; it also means a shell that never started is reported as exactly
    // that, instead of as a missing descendant a minute later.
    let spoke = Arc::new(AtomicBool::new(false));
    {
        let spoke = spoke.clone();
        let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                spoke.store(true, Ordering::SeqCst);
            }
        });
    }
    assert!(
        wait_until(SHELL_SPEAKS, || spoke.load(Ordering::SeqCst)),
        "the pane's shell never produced any output — it never started"
    );

    let mut writer = pair.master.take_writer().expect("take pty writer");
    writer
        .write_all(SPAWN_TREE_CMD)
        .expect("type the spawn command into the pane");
    writer.flush().expect("flush the pane's input");

    // Wait for the tree we just typed to exist. The floor for "is this really
    // ours" is the shell's own start time; see `live_descendants`.
    let mut sys = System::new();
    refresh(&mut sys);
    let shell_born = sys
        .process(Pid::from_u32(shell_pid))
        .expect("the pane shell must be in the process table")
        .start_time();

    let mut tree: Vec<Descendant> = Vec::new();
    let _ = wait_until(TREE_APPEARS, || {
        refresh(&mut sys);
        tree = live_descendants(&sys, shell_pid, shell_born);
        has(&tree, WRAPPER) && has(&tree, GRANDCHILD)
    });

    // Positive control, and the assertion that makes this test's name true: what
    // hangs off the enrolled shell is a TREE — a wrapper shell one level down and
    // a long-lived process two levels down. Without it, the teardown assertions
    // below would pass just as happily over an empty set.
    assert!(
        has(&tree, WRAPPER),
        "the wrapper shell never appeared as a direct child of the pane shell; tree = {tree:?}"
    );
    assert!(
        has(&tree, GRANDCHILD),
        "the long-lived grandchild never appeared under the wrapper shell; tree = {tree:?}"
    );

    // Take a handle on every member BEFORE the job closes, so the teardown
    // assertion is a wait on those exact processes rather than a question about a
    // number that may since have been handed to someone else.
    let members: Vec<(u32, String, ProcHandle)> = tree
        .iter()
        .map(|(pid, name, _)| {
            let handle = ProcHandle::open(*pid)
                .unwrap_or_else(|| panic!("could not open descendant {name} ({pid})"));
            (*pid, name.clone(), handle)
        })
        .collect();
    let shell = ProcHandle::open(shell_pid).expect("could not open the pane shell");

    // Sanity: the whole chain is live before we tear it down.
    assert!(shell.alive(), "the pane shell died before the job closed");
    for (pid, name, handle) in &members {
        assert!(
            handle.alive(),
            "descendant {name} ({pid}) died before the job closed"
        );
    }

    // The load-bearing act: close ONLY the job handle. We deliberately keep the
    // ConPTY master + child alive so a passing test can only be explained by
    // KILL_ON_JOB_CLOSE reaping the tree — not by ConPTY teardown.
    drop(job);

    let deadline = Instant::now() + TREE_DIES;
    for (pid, name, handle) in &members {
        assert!(
            handle.wait_exit(deadline),
            "descendant {name} ({pid}) survived job close — kill-on-close did not reap the tree"
        );
    }
    assert!(shell.wait_exit(deadline), "the pane shell survived job close");

    // Keep the pty pair alive until here so nothing else could have killed the
    // tree; then let it drop.
    drop(writer);
    drop(child);
    drop(pair.master);
}

#[test]
fn assign_job_is_fail_soft_on_a_bad_pid() {
    // A PID that does not exist: OpenProcess fails, so job assignment returns
    // None rather than panicking or leaking. This is the fail-soft contract —
    // spawn_pty breadcrumbs and continues with pre-#78 behavior.
    let bogus = 0xFFFF_FFF0u32;
    assert!(
        loomux_lib::pty::assign_kill_on_close_job(bogus).is_none(),
        "assignment to a nonexistent PID must fail soft (None)"
    );
}
