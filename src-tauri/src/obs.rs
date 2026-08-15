//! Crash observability (issue #53) — the desktop half, and the seam the rest of
//! `src-tauri` still reaches the whole of it through.
//!
//! The facilities themselves — the panic hook and its crash logs, the
//! breadcrumb log and its rotation, the `LOOMUX_DATA_DIR`-aware `data_root()`,
//! the `running.lock` sentinel, and the poison-tolerant [`LockExt`] every
//! long-lived `Mutex` in this crate locks through — now live in
//! [`loomux_engine::obs`] (#888 slice A3 batch 7). None of that is
//! desktop-specific: it is `std` plus `dirs`, and a headless daemon needs
//! exactly the same crash trail a windowed app does.
//!
//! What could not cross is below, and it is the whole of what stayed: the file
//! had already fenced its Tauri surface off behind its own
//! `next-launch notice (Tauri surface)` section marker, and that surface is a
//! state cell plus one `#[tauri::command]`. Moving it would have meant the
//! engine linking Tauri, which is the one thing that crate exists not to do.
//!
//! The `pub use` below is why nothing else in `src-tauri` changed: all ~45
//! `crate::obs::…` call sites across eleven files spell the same paths they
//! always did. It is written out item by item rather than as a glob so that
//! what this crate re-exports is a list somebody chose, not whatever the engine
//! module happens to make public next.

pub use loomux_engine::obs::{
    breadcrumb, check_and_arm, data_root, install_panic_hook, logs_dir, mark_clean_exit, LockExt,
    StartupCheck,
};

use std::sync::Mutex;

// ---------- next-launch notice (Tauri surface) ----------

/// One-shot holder for the next-launch notice. The frontend drains it once at
/// startup via `take_startup_notice` and shows a toast.
#[derive(Default)]
pub struct StartupNotice(pub Mutex<Option<String>>);

/// Return (and clear) the unclean-exit notice, or `null` when the last exit was
/// clean. Poison-tolerant: a poisoned lock still yields the value rather than
/// taking a command thread down.
#[tauri::command]
pub fn take_startup_notice(state: tauri::State<StartupNotice>) -> Option<String> {
    state
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}
