//! The hidden-pty `ssh-add` driver **off Windows**, against a fake `ssh-add`
//! (#2594 item 1).
//!
//! `src-tauri/tests/sshagent.rs` is the conversation suite and it is
//! Windows-only: its fixtures are `.bat` files, because `cmd.exe`'s `set /p` is
//! what gives that platform a program which blocks on a console read. That left
//! the module's *other* platform untested, and one defect lived exactly there.
//!
//! `sshagent.rs` is not platform-gated. On macOS and Linux
//! `pair.slave.spawn_command` yields a `std::process::Child` whose `Drop` does
//! no wait, so a child that is killed and never waited on stays a **zombie** for
//! the life of the process. The driver's own tail used to kill without waiting
//! on exactly one arm — the one a *successful* launch takes, where a transcript
//! line decided the outcome before the child had exited — so orrerix
//! accumulated one zombie per key loaded, forever, on the two platforms nobody
//! was testing.
//!
//! So what this file pins is not the conversation (that is the Windows suite's
//! job, and the driver is the same code) but the **process** the conversation
//! leaves behind. The fixture is a `/bin/sh` script for the same reason the
//! other suite's is a `.bat`: constraint 3 forbids running the real `ssh-add`,
//! and a shell is the portable way to get a program that prints an ask and then
//! blocks on a read.
#![cfg(unix)]

use loomux_lib::sshagent::{drive_ssh_add_with_transcript, SshAddOutcome};
use std::time::Duration;

/// The marker the fixture prints its own pid on, so the test can look for that
/// process afterwards without reaching inside the driver for a handle it
/// deliberately does not expose.
const PID_MARKER: &str = "loomux-fake-pid=";

/// Printed only if the fixture's LAST read ever returns — i.e. only if the
/// child was allowed to run to completion. Its ABSENCE is what says the driver
/// really did take the arm under test.
const EXITING: &str = "loomux-fake-exiting";

/// Announce a pid, answer the first ask, report success in the vendor's own
/// words, and then **stay alive**, blocked on a read nothing will answer.
///
/// The last part is the whole fixture. The driver ends the conversation the
/// moment `Identity added` is classified, so the child is still running when it
/// decides — which is the arm that killed without reaping.
const ADDS_THEN_LINGERS: &str = concat!(
    "echo loomux-fake-pid=$$\n",
    "echo fake-ssh-add-running\n",
    "printf 'Enter passphrase for /keys/id_ed25519: '\n",
    "read p\n",
    "echo \"Identity added: /keys/id_ed25519 (test-key)\"\n",
    "read hold\n",
    "echo loomux-fake-exiting\n",
);

/// Write a fake `ssh-add` and return the argv that runs it.
///
/// `/bin/sh <script>` rather than an executable bit, so nothing here depends on
/// the test runner's umask.
fn fake_ssh_add(dir: &std::path::Path, name: &str, body: &str) -> Vec<String> {
    let script = dir.join(name);
    std::fs::write(&script, body).expect("write the fake ssh-add");
    vec!["/bin/sh".to_string(), script.to_string_lossy().into_owned()]
}

/// Render a transcript for a panic message: escapes are literal, so a control
/// sequence cannot rewrite the log line it is being reported in.
fn show(transcript: &str) -> String {
    format!("{:?}", transcript)
}

/// The pid the fixture announced.
fn announced_pid(transcript: &str) -> u32 {
    let after = transcript
        .split(PID_MARKER)
        .nth(1)
        .unwrap_or_else(|| panic!("the fixture never announced a pid; transcript: {}", show(transcript)));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("unparsable pid {digits:?}: {e}; transcript: {}", show(transcript)))
}

/// Whether the OS still has a process table entry for `pid` — the slot, not
/// what is in it.
///
/// **A zombie counts as present**, which is the entire point: a killed child
/// nobody waited on is still listed, so this separates "reaped" from "merely
/// dead" — the distinction `Child::kill` alone cannot make. `ps` rather than a
/// `libc` call because this crate takes no new dependency for a test, and
/// `-o pid=` prints the pid with no header on both Linux and macOS.
///
/// Used only by the instrument control. Every assertion about the driver goes
/// through [`still_our_shell_child`] instead — see its doc for why a bare pid
/// probe is the wrong question.
fn process_exists(pid: u32) -> bool {
    !ps(pid, "pid=").is_empty()
}

/// Whether `pid` is still **the child this test spawned**: our own direct child,
/// running a shell.
///
/// A bare `ps -p <pid>` answers "is that slot occupied", and the slot is not the
/// question (#2661 review, rev-std 2). Between the driver reaping the child and
/// this probe, the OS may hand that pid to something else, and the test would
/// then report a correctly-reaped child as a leak — a flake that fires on a
/// green tree and points at the one line it is not about.
///
/// Two facts identify the process rather than the number, and both survive the
/// zombie state this test has to detect:
///
/// - `ppid` stays this test process until the child is **reaped**, which is
///   exactly the transition under test. A recycled pid belongs to some other
///   parent.
/// - `comm` is the shell the fixture runs. This test spawns no other shell, so
///   the residual — a recycled pid that is ALSO a fresh shell child of ours —
///   has no way to arise here. The one other process this test starts is `ps`
///   itself, and `ps` is not `sh`.
///
/// `ps_finds_our_own_shell_child_and_loses_it_once_reaped` is the positive
/// control: an assertion that this returns **false** passes just as well when it
/// can never return true.
fn still_our_shell_child(pid: u32) -> bool {
    // [scratch #2594 R9] a probe that can never answer yes — the shape every
    // "assert it is false" test in this file passes against.
    let line = String::new();
    let mut fields = line.split_whitespace();
    let ppid: Option<u32> = fields.next().and_then(|p| p.parse().ok());
    let comm = fields.next().unwrap_or("");
    let _ = ps(pid, "ppid=,comm=");
    // `comm` is the basename on Linux and may carry a leading `-` for a login
    // shell on macOS, so match the tail rather than the whole field.
    ppid == Some(std::process::id()) && comm.ends_with("sh")
}

/// One `ps` read of one pid, with the requested no-header format.
fn ps(pid: u32, format: &str) -> String {
    let out = std::process::Command::new("ps")
        .args(["-o", format, "-p", &pid.to_string()])
        .output()
        .expect("`ps` must be available to run this test at all");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn ps_reports_a_live_process_as_present() {
    // The instrument control, and it is not optional: every assertion below
    // reads a probe returning false as success, and a `ps` that failed, printed
    // a header, or was invoked wrongly would return false for everything —
    // including a process that is definitely there.
    assert!(
        process_exists(std::process::id()),
        "`ps -o pid= -p <self>` must find this very test process, or the instrument is blind"
    );
    // …and it must not simply answer `true`: pid 0 is not a process any `ps`
    // lists on Linux or macOS.
    assert!(!process_exists(0), "`ps` must not claim pid 0 exists");
}

#[test]
fn ps_finds_our_own_shell_child_and_loses_it_once_reaped() {
    // The positive control for [`still_our_shell_child`], which is the probe the
    // two tests below actually assert on. Both assert it is **false**, and a
    // predicate that can never return true satisfies that for every input —
    // including a child sitting right there unreaped, which is the defect #2594
    // item 1 fixed. The control above proves `ps` runs; this one proves the
    // question asked of it can be answered yes.
    //
    // The whole transition is driven on a child this test owns: alive and ours,
    // then killed and waited on, then gone. No agent CLI is involved (CLAUDE.md
    // constraint 3) — it is `/bin/sh` blocking on a read.
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "read x"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("spawn a shell that blocks on a read");
    let pid = child.id();
    assert!(
        still_our_shell_child(pid),
        "a live `/bin/sh` child of this process must be recognised, or every assertion \
         below passes vacuously; `ps -o ppid=,comm= -p {pid}` said {:?}",
        ps(pid, "ppid=,comm=")
    );

    child.kill().expect("kill the control child");
    child.wait().expect("reap the control child");
    assert!(
        !still_our_shell_child(pid),
        "a reaped child must stop being recognised — pid {pid} still reads as ours"
    );
}

#[test]
fn a_success_decided_by_a_transcript_line_still_reaps_the_child() {
    // #2594 item 1. The child is alive when `Identity added` decides the run —
    // it is blocked on a second read — so this is the arm that used to kill and
    // walk away. On Unix that leaves a zombie for the life of the process, one
    // per key the user loads.
    let dir = tempfile::tempdir().unwrap();
    let argv = fake_ssh_add(dir.path(), "adds-then-lingers.sh", ADDS_THEN_LINGERS);

    let (outcome, seen) = drive_ssh_add_with_transcript(&argv, b"hunter2", Duration::from_secs(20));

    assert_eq!(outcome, SshAddOutcome::Added, "transcript: {}", show(&seen));
    // The arm control. If the fixture had been allowed to finish, the child
    // would have exited on its own and the loop's `try_wait` would have reaped
    // it — so this test would pass without the tail ever being exercised.
    assert!(
        !seen.contains(EXITING),
        "the child must still have been RUNNING when the driver decided, or this \
         test is about the wrong arm; transcript: {}",
        show(&seen)
    );
    let pid = announced_pid(&seen);
    assert!(
        !still_our_shell_child(pid),
        "the driver must reap the child it killed — pid {pid} is still an unreaped shell \
         child of this process, which off Windows means a zombie held for the life of the \
         app; transcript: {}",
        show(&seen)
    );
}

/// Prints an ask and then blocks forever: nothing the driver recognises ever
/// arrives, so only the bound can end the run.
const NEVER_ANSWERS: &str = concat!(
    "echo loomux-fake-pid=$$\n",
    "echo fake-ssh-add-running\n",
    "read never\n",
    "echo loomux-fake-exiting\n",
);

#[test]
fn a_timed_out_child_is_reaped_too() {
    // The other way out of the conversation, on the same platform. It already
    // reaped before #2594 — which is what makes it the *discriminator* for the
    // test above rather than a duplicate of it: the two arms differed, and the
    // one that mattered was the success.
    let dir = tempfile::tempdir().unwrap();
    let argv = fake_ssh_add(dir.path(), "never-answers.sh", NEVER_ANSWERS);

    let (outcome, seen) = drive_ssh_add_with_transcript(&argv, b"hunter2", Duration::from_secs(3));

    assert_eq!(outcome, SshAddOutcome::Timeout, "transcript: {}", show(&seen));
    // The vacuity control for this file: a driver whose reader thread never ran
    // returns `Timeout` for every fixture, and would announce no pid at all —
    // which `announced_pid` panics on rather than passing over.
    assert!(
        seen.contains("fake-ssh-add-running"),
        "the driver must actually be reading the child's output; transcript: {}",
        show(&seen)
    );
    let pid = announced_pid(&seen);
    assert!(
        !still_our_shell_child(pid),
        "pid {pid} is still an unreaped shell child of this process after the timeout arm; \
         transcript: {}",
        show(&seen)
    );
}
