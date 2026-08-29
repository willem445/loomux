//! The webview's half of the liveness heartbeat (#1601, plan §3 Phase 0.4).
//!
//! The backend half is [`loomux_engine::selfwatch`]: the watchdog stamps once a
//! second and reports its own scheduling lag. This module is the one line of
//! Tauri surface that lets the *other* thread stamp too, so the two can be
//! compared — which is the whole feature. beta5 and beta6 present identically
//! (the window is up and the app does not work) and are opposite failures; one
//! stalled the webview thread and one left it perfectly healthy while starving
//! everything behind it. Telling them apart cost a release cycle.
//!
//! Kept in its own module rather than folded into `obs.rs`: `obs` is the crash
//! trail's desktop seam, and its one command is about a notice from the
//! PREVIOUS run. This is about the current one, and the argument below is
//! specific enough to want its own page.

use loomux_engine::{lockwatch, selfwatch};

/// Record that the webview thread is alive.
///
/// **Sync, deliberately, and it is `performance.md` §4 X7 rather than a
/// `cheap` row.** INV-1's default is that a command delegates its body to the
/// blocking pool — and this command must NOT, because the pool is one of the
/// two things it is measuring. Once the pool is exhausted (the plan's §1.2
/// mechanism: the state in which `write_pty` can no longer be scheduled and
/// every pane stops accepting input), a delegated `liveness_stamp` would never
/// run either. The webview would then look stuck at exactly the moment it is
/// the only healthy half left, and the instrument would report the opposite of
/// the truth on the one occasion anybody reads it.
///
/// So it is sync, and the `cheap` bar is met on both halves of the #1595 rule
/// this repo now applies to that class: the body is in-memory only (six relaxed
/// atomic stores and a monotonic clock read — no allocation, no IO, no
/// formatting, no `Mutex` at all, so there is nothing whose ACQUISITION could
/// park the webview thread), and it takes no lock any background thread can
/// hold, which is what the `cheap` classification actually turns on. It is
/// filed as `exception` rather than `cheap` because the SYNCHRONY is a
/// requirement here rather than an absence of reason to convert: a future
/// sweep that mechanically converts the `cheap` rows would silently break it,
/// and an argued row is what stops that.
///
/// **Reentrancy.** None to argue: there is no shared state beyond the atomics
/// themselves, every write is unconditional, and two stamps racing leave the
/// later one's values — which is the correct answer, since a stamp is a claim
/// about *now*.
///
/// The caller passes what only it can measure: how late its own timer was, how
/// late a frame was serviced (`None` when none was, which a hidden window is
/// entitled to), and whether the window was hidden. See `src/liveness.ts`.
#[tauri::command]
pub fn liveness_stamp(timer_lag_ms: u64, frame_lag_ms: Option<u64>, hidden: bool) {
    selfwatch::webview_stamp(lockwatch::mono_ms(), timer_lag_ms, frame_lag_ms, hidden);
}
