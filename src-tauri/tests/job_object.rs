//! Windows Job Object kill-on-close integration test (issue #78).
//!
//! Proves the core guarantee W1 adds: enrolling a pane's spawned child in a
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` job means closing the job handle reaps
//! the *whole* descendant tree — not just the direct child. Pre-#78, killing an
//! ancestor on Windows left descendants (orphaned wrapper shells with live
//! agents, a squatting vite) running.
//!
//! The test opens a real ConPTY the same way `spawn_pty` does, runs a shell
//! that spawns a long-lived grandchild, enrolls the shell via the exact code
//! path production uses (`loomux_lib::pty::assign_kill_on_close_job`), then
//! drops only the job handle and asserts — via `Get-Process` — that the
//! grandchild is gone. A second test covers the fail-soft path.
//!
//! Windows-only: the feature is Windows-only (Unix relies on process-group
//! teardown). The whole file compiles to nothing elsewhere.
#![cfg(windows)]

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::time::{Duration, Instant};

/// The PowerShell to drive the panes with: prefer pwsh 7, fall back to
/// Windows PowerShell (mirrors the lib's `default_shell`). Both ship on the
/// GitHub `windows-latest` runners.
fn powershell() -> &'static str {
    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
            .unwrap_or(false)
    };
    if on_path("pwsh.exe") {
        "pwsh.exe"
    } else {
        "powershell.exe"
    }
}

/// Is a process with this PID currently alive? Uses `Get-Process`, as the task
/// specifies, so the assertion reflects a real OS process-table query.
fn pid_alive(pid: u32) -> bool {
    let script = format!(
        "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ 'ALIVE' }} else {{ 'DEAD' }}"
    );
    let out = std::process::Command::new(powershell())
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("run Get-Process");
    String::from_utf8_lossy(&out.stdout).contains("ALIVE")
}

/// Poll `cond` until it holds or `timeout` elapses. Returns whether it held.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cond()
}

#[test]
fn kill_on_close_job_reaps_the_whole_descendant_tree() {
    let sh = powershell();
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    let pidfile_str = pidfile.to_string_lossy().replace('\'', "''");

    // The shell child: wait briefly so the test can enroll it in the job
    // BEFORE it forks (only descendants born after assignment inherit the
    // job), then spawn a long-lived grandchild via a direct CreateProcess
    // (UseShellExecute=$false), record its PID, and idle. `Start-Process`/
    // ShellExecute could break away from the job; a direct .NET Process.Start
    // does not.
    let script = format!(
        "Start-Sleep -Milliseconds 800; \
         $psi = New-Object System.Diagnostics.ProcessStartInfo; \
         $psi.FileName = '{sh}'; \
         $psi.Arguments = '-NoProfile -NonInteractive -Command Start-Sleep 300'; \
         $psi.UseShellExecute = $false; \
         $p = [System.Diagnostics.Process]::Start($psi); \
         Set-Content -LiteralPath '{pidfile_str}' -Value $p.Id; \
         Start-Sleep 300"
    );

    // Open a real ConPTY exactly like spawn_pty.
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(sh);
    cmd.args(["-NoLogo", "-NoProfile", "-Command", &script]);
    let child = pair.slave.spawn_command(cmd).expect("spawn shell");
    drop(pair.slave);

    let shell_pid = child.process_id().expect("shell pid");

    // Enroll via the production code path. This is the unit under test.
    let job = loomux_lib::pty::assign_kill_on_close_job(shell_pid)
        .expect("job creation + assignment must succeed on Windows");

    // Wait for the grandchild to be born and its PID recorded.
    assert!(
        wait_until(Duration::from_secs(15), || pidfile.is_file()
            && !std::fs::read_to_string(&pidfile)
                .unwrap_or_default()
                .trim()
                .is_empty()),
        "grandchild never reported its PID"
    );
    let grandchild_pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .expect("grandchild pid");

    // Sanity: the whole chain is live before we tear it down.
    assert!(pid_alive(shell_pid), "shell should be alive pre-kill");
    assert!(
        pid_alive(grandchild_pid),
        "grandchild should be alive pre-kill"
    );

    // The load-bearing act: close ONLY the job handle. We deliberately keep the
    // ConPTY master + child alive so a passing test can only be explained by
    // KILL_ON_JOB_CLOSE reaping the tree — not by ConPTY teardown.
    drop(job);

    assert!(
        wait_until(Duration::from_secs(10), || !pid_alive(grandchild_pid)),
        "grandchild survived job close — kill-on-close did not reap the tree"
    );
    assert!(
        wait_until(Duration::from_secs(10), || !pid_alive(shell_pid)),
        "shell survived job close"
    );

    // Keep the pty pair alive until here so nothing else could have killed the
    // tree; then let it drop.
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
