//! Direct-CLI-spawn fallback integration test (issue #78, rev-49 note 2).
//!
//! Proves the fail-safe invariant the design rests on: a program that *resolves*
//! to a native `.exe` but then *fails to actually spawn* (corrupt/truncated PE,
//! AV/ACL block, arch mismatch) must not kill the pane — it must retry through
//! the exact shell wrapper the pre-#78 path used. `try_direct_command` only
//! checks `is_file`, not spawnability, so without the retry a bad exe would
//! `Err` out of `spawn_pty` and the agent would die at the #106 bind timeout.
//!
//! Both tests drive the production code path (`loomux_lib::pty::spawn_pane_child`)
//! against a real ConPTY slave, exactly like `spawn_pty` does.
//!
//! Windows-only: the direct-vs-shell distinction and the "native `.exe` that
//! won't launch" failure mode are Windows-specific (off Windows every resolved
//! file is treated as directly executable). Mirrors `job_object.rs`, which is
//! also Windows-gated for the same reason.
#![cfg(windows)]

use portable_pty::{native_pty_system, PtySize};

fn open_slave() -> portable_pty::PtyPair {
    native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty")
}

#[test]
fn resolved_but_unspawnable_native_exe_falls_back_to_the_shell() {
    // A file that resolves as a native image (`.exe`) but is not a valid PE, so
    // `CreateProcess` rejects it (ERROR_BAD_EXE_FORMAT). The direct spawn must
    // fail and the child must instead come up through the shell wrapper.
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("agent.exe");
    std::fs::write(&bad, b"this is not a valid PE image").unwrap();
    let argv = vec![bad.to_string_lossy().into_owned(), "--flag".to_string()];

    let pair = open_slave();
    let (mut child, direct_used) = loomux_lib::pty::spawn_pane_child(
        &*pair.slave,
        Some("cmd.exe /c exit 0"), // the shell fallback the pane would have run
        Some(&argv),
        None,
    )
    .expect("a resolved-but-unspawnable exe must still yield a child via the shell");

    assert!(
        !direct_used,
        "an .exe that fails to spawn must fall back to the shell, not surface as an error"
    );

    // The fallback child is a real, live process (the shell) — not a failure.
    let _ = child.kill();
    drop(pair);
}

#[test]
fn a_genuine_native_exe_spawns_directly() {
    // Positive control: a real native `.exe` (cmd.exe, via %ComSpec%) takes the
    // direct path — `direct_used` is true, no shell wrapper involved.
    let comspec = std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    let argv = vec![comspec, "/c".to_string(), "exit".to_string(), "0".to_string()];

    let pair = open_slave();
    let (mut child, direct_used) =
        loomux_lib::pty::spawn_pane_child(&*pair.slave, None, Some(&argv), None)
            .expect("a native exe must spawn");

    assert!(
        direct_used,
        "a genuine native .exe must be spawned directly, not through the shell"
    );

    let _ = child.kill();
    drop(pair);
}
