//! #2365 slice B — the shutdown sweep must record itself independently of the
//! waiter race.
//!
//! `PtyManager::kill_all` drains the backend map and kills every pty, but the
//! per-pty `pty-exit … expected=false` breadcrumb is written by each pty's
//! waiter thread only after `child.wait()` returns — and `kill_all` neither
//! joins those threads nor waits on the children, so on shutdown the process
//! can exit before a slow child finishes dying and live ptys go missing from
//! the final log (#2365: ptys 12 and 14). The fix writes a synchronous
//! `shutdown kill id=N` breadcrumb per drained handle BEFORE `killer.kill()`,
//! on the calling thread. This test drives the REAL `kill_all` through the
//! test-only harness (#420 rev-19 R3) and pins that the sweep's enumeration
//! lands — and that nothing the sweep did not drain gets a line.

use loomux_lib::pty::PtyManager;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

/// Point `obs::breadcrumb` at a fresh, process-unique temp data root.
/// `data_root()` reads `$ORRERIX_DATA_DIR` per call (absolute values only),
/// and this file is its own test binary with a single test, so the override
/// is process-global and uncontended — the developer's real breadcrumbs.log
/// is never touched. Must run before this process's first breadcrumb write.
fn isolate_data_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orrerix-shutdown-crumb-{}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    std::env::set_var("ORRERIX_DATA_DIR", &dir);
    dir
}

fn breadcrumbs_log(root: &Path) -> String {
    let bytes = fs::read(root.join("logs").join("breadcrumbs.log")).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn killall_breadcrumbs_every_drained_pty_synchronously_and_nothing_it_did_not_drain() {
    let root = isolate_data_root();
    let pm = PtyManager::default();

    // Positive control (plan part 4): the sink holds NO `shutdown kill` line
    // before the sweep — the lines asserted below were put there by THIS
    // kill_all, not inherited from the log's past.
    let before = breadcrumbs_log(&root);
    assert!(
        !before.contains("shutdown kill"),
        "breadcrumb sink must hold no `shutdown kill` line before kill_all; got:\n{before}"
    );

    // Two ptys through the real harness (genuine ConPTY pair + killer, per
    // #420 rev-19 R3), plus an id that is never registered.
    let _a = pm.register_fake_for_test(2301, b"");
    let _b = pm.register_fake_for_test(2302, b"");

    pm.kill_all();

    let after = breadcrumbs_log(&root);
    assert!(
        after.contains("shutdown kill id=2301"),
        "kill_all must breadcrumb drained pty 2301 before killing it; got:\n{after}"
    );
    assert!(
        after.contains("shutdown kill id=2302"),
        "kill_all must breadcrumb drained pty 2302 before killing it; got:\n{after}"
    );
    assert_eq!(
        after.matches("shutdown kill id=").count(),
        2,
        "the sweep must leave exactly one line per drained handle; got:\n{after}"
    );
    // Positive control: a pty the sweep never drained gets NO line — the
    // enumeration is the backend map's, not every integer.
    assert!(
        !after.contains("shutdown kill id=2399"),
        "no shutdown-kill line may exist for a pty kill_all never drained; got:\n{after}"
    );
}
