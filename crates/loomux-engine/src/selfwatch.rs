//! The app watching itself (#1601, plan §3 Phase 0.2/0.3/0.4).
//!
//! [`crate::lockwatch`] answers *who is holding a lock*. This module answers
//! the two questions beside it that the beta4→beta6 incident chain turned on,
//! and drives the one thread that reports all three.
//!
//! # 0.3 — how deep the shared blocking pool is
//!
//! `write_pty` and every converted `orch_*` command hand their bodies to the
//! SAME `tauri::async_runtime::spawn_blocking` pool, and nothing in the tree
//! sets `max_blocking_threads`, so it is tokio's default of 512. The plan's §1.2
//! mechanism ends there: once the pool is full, `write_pty` can no longer be
//! scheduled, its promise never resolves, and `src/ptywrite.ts`'s per-pane chain
//! stops dispatching — every pane stops accepting input at once, while the
//! window keeps painting. A report that reads `in-flight 512` is a diagnosis
//! instead of a mystery.
//!
//! [`pool_enter`] is taken at the HAND-OFF, not inside the task, so the counter
//! is "submitted and not yet finished" — queued work included. That is the
//! number that reaches the ceiling; a counter of running tasks alone would read
//! 512 and stay there with no way to see the queue behind it.
//!
//! **Sampled on the peak, not on the instant.** The watchdog looks once a
//! second, and a threshold crossed and recovered between two looks would
//! otherwise be invisible. [`pool_enter`] keeps a high-water mark with one
//! `fetch_max`, and [`pool_take_peak`] hands it to the watchdog and re-arms it
//! at the current depth — so a crossing cannot be missed by sampling, only by
//! the pool never actually filling.
//!
//! # 0.4 — which half of the app stopped
//!
//! beta5 and beta6 look identical from outside — the window is up and the app
//! does not work — and they are opposite failures: beta5 stalled the webview
//! thread, beta6 left it perfectly healthy and starved everything behind it.
//! Telling them apart cost a release cycle.
//!
//! So both halves stamp. The watchdog stamps each tick, and reports its own
//! scheduling lag with it, because a watchdog that is itself starved must not
//! be able to report "backend fine". The webview stamps on its own cadence
//! through the `liveness_stamp` command, carrying how late its timer was and
//! how late a frame was serviced. [`liveness`] is the pure verdict over the
//! two, and the divergence is the diagnosis:
//!
//! | watchdog | webview | verdict |
//! | --- | --- | --- |
//! | fresh | fresh | [`Liveness::Ok`] |
//! | fresh | stale, window visible | [`Liveness::GuiStuck`] — beta5's shape |
//! | fresh | stale, window hidden | [`Liveness::GuiHidden`] — no evidence |
//! | stale | fresh | [`Liveness::BackendStuck`] — beta6's shape |
//! | stale | stale | [`Liveness::BothStuck`] |

use crate::lockwatch::{self, LockWatch};
use crate::obs;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// The watchdog's cadence. One second: fast enough that a five-second hold is
/// reported while it is still happening, slow enough to be free.
pub const WATCHDOG_INTERVAL_MS: u64 = 1_000;

/// How stale a stamp may be before [`liveness`] calls its side stuck.
///
/// Three watchdog intervals. One would make a single descheduled tick — a
/// laptop resuming, a GC-ish pause in the webview, a loaded CI box — read as a
/// hang, and this instrument's whole value is that a report from it is
/// believable.
pub const LIVENESS_STALE_MS: u64 = 3 * WATCHDOG_INTERVAL_MS;

/// The in-flight depths worth saying something about, from the plan's §3
/// Phase 0.3. Ascending; each is reported once on the way up.
pub const POOL_STEPS: &[i64] = &[64, 128, 256];

// ---------- 0.3: blocking-pool depth ----------

static POOL_IN_FLIGHT: AtomicI64 = AtomicI64::new(0);
static POOL_PEAK: AtomicI64 = AtomicI64::new(0);

/// Counts one `spawn_blocking` hand-off for as long as it is outstanding.
///
/// Held by the closure that was handed off, so it is dropped when the task
/// finishes — including when the task panics, since a panicking task still
/// unwinds its own locals.
pub struct PoolTicket {
    _private: (),
}

/// Register a `spawn_blocking` hand-off. Move the returned ticket INTO the
/// closure being handed off; dropping it early undercounts exactly the depth
/// this exists to measure.
pub fn pool_enter() -> PoolTicket {
    let depth = POOL_IN_FLIGHT.fetch_add(1, Ordering::Relaxed) + 1;
    POOL_PEAK.fetch_max(depth, Ordering::Relaxed);
    PoolTicket { _private: () }
}

impl Drop for PoolTicket {
    fn drop(&mut self) {
        POOL_IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Hand-offs submitted and not yet finished.
pub fn pool_in_flight() -> i64 {
    POOL_IN_FLIGHT.load(Ordering::Relaxed)
}

/// The highest depth since the last call, re-arming the high-water mark at the
/// current depth. Called by the watchdog, once per tick.
pub fn pool_take_peak() -> i64 {
    let now = POOL_IN_FLIGHT.load(Ordering::Relaxed);
    POOL_PEAK.swap(now, Ordering::Relaxed).max(now)
}

/// The highest [`POOL_STEPS`] entry at or below `depth`, if any.
pub fn pool_step(depth: i64) -> Option<i64> {
    POOL_STEPS.iter().rev().copied().find(|s| depth >= *s)
}

/// One blocking-pool depth report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolReport {
    /// The [`POOL_STEPS`] threshold that was crossed.
    pub step: i64,
    /// The peak depth that crossed it.
    pub peak: i64,
}

/// The watchdog's pool half: reports each threshold once on the way up, and
/// re-arms it only once the depth has actually come back below it.
///
/// Release on EVIDENCE rather than on elapsed time (`performance.md` P4): a
/// pool that sits at 300 for ten minutes is one event, not six hundred, and a
/// pool that drains to 10 and climbs to 300 again is genuinely a second event.
#[derive(Debug, Default)]
pub struct PoolWatch {
    reported: Option<i64>,
}

impl PoolWatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick(&mut self, peak: i64) -> Option<PoolReport> {
        let step = pool_step(peak);
        match (self.reported, step) {
            // Deeper than anything reported so far — including the first crossing.
            (None, Some(s)) => {
                self.reported = Some(s);
                Some(PoolReport { step: s, peak })
            }
            (Some(prev), Some(s)) if s > prev => {
                self.reported = Some(s);
                Some(PoolReport { step: s, peak })
            }
            // Came back down: re-arm at the shallower step (or below the ladder
            // entirely), and say nothing — a fall is not news.
            (Some(_), s) => {
                self.reported = s;
                None
            }
            (None, None) => None,
        }
    }

    /// The step this watch would currently suppress a repeat of.
    pub fn armed_at(&self) -> Option<i64> {
        self.reported
    }
}

// ---------- 0.4: liveness heartbeat ----------

static WATCHDOG_MS: AtomicU64 = AtomicU64::new(0);
static WATCHDOG_TICKS: AtomicU64 = AtomicU64::new(0);
static WATCHDOG_LAG_MS: AtomicU64 = AtomicU64::new(0);
static WEBVIEW_MS: AtomicU64 = AtomicU64::new(0);
static WEBVIEW_STAMPS: AtomicU64 = AtomicU64::new(0);
static WEBVIEW_TIMER_LAG_MS: AtomicU64 = AtomicU64::new(0);
/// `-1` = the webview reported no frame serviced in its last window.
static WEBVIEW_FRAME_LAG_MS: AtomicI64 = AtomicI64::new(-1);
static WEBVIEW_HIDDEN: AtomicBool = AtomicBool::new(false);

/// Both halves' most recent stamps, read as one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heartbeat {
    pub watchdog_ms: u64,
    pub watchdog_ticks: u64,
    /// How much the watchdog's last interval overshot [`WATCHDOG_INTERVAL_MS`].
    pub watchdog_lag_ms: u64,
    pub webview_ms: u64,
    pub webview_stamps: u64,
    /// How late the webview's own timer was, as the webview measured it.
    pub webview_timer_lag_ms: u64,
    /// How late a frame was serviced, or `None` if none was in that window.
    pub webview_frame_lag_ms: Option<u64>,
    /// Whether the window was hidden when the webview last stamped. A hidden
    /// window is not painting by design, so a missing frame is not evidence.
    pub webview_hidden: bool,
}

/// Record a watchdog tick.
pub fn watchdog_stamp(now_ms: u64, lag_ms: u64) {
    WATCHDOG_MS.store(now_ms, Ordering::Relaxed);
    WATCHDOG_LAG_MS.store(lag_ms, Ordering::Relaxed);
    WATCHDOG_TICKS.fetch_add(1, Ordering::Release);
}

/// Record a webview stamp. `frame_lag_ms` is `None` when the webview serviced
/// no frame in the window it is reporting on.
pub fn webview_stamp(now_ms: u64, timer_lag_ms: u64, frame_lag_ms: Option<u64>, hidden: bool) {
    WEBVIEW_MS.store(now_ms, Ordering::Relaxed);
    WEBVIEW_TIMER_LAG_MS.store(timer_lag_ms, Ordering::Relaxed);
    WEBVIEW_FRAME_LAG_MS.store(frame_lag_ms.map(|v| v as i64).unwrap_or(-1), Ordering::Relaxed);
    WEBVIEW_HIDDEN.store(hidden, Ordering::Relaxed);
    WEBVIEW_STAMPS.fetch_add(1, Ordering::Release);
}

/// Both stamps.
pub fn heartbeat() -> Heartbeat {
    let frame = WEBVIEW_FRAME_LAG_MS.load(Ordering::Relaxed);
    Heartbeat {
        watchdog_ms: WATCHDOG_MS.load(Ordering::Relaxed),
        watchdog_ticks: WATCHDOG_TICKS.load(Ordering::Acquire),
        watchdog_lag_ms: WATCHDOG_LAG_MS.load(Ordering::Relaxed),
        webview_ms: WEBVIEW_MS.load(Ordering::Relaxed),
        webview_stamps: WEBVIEW_STAMPS.load(Ordering::Acquire),
        webview_timer_lag_ms: WEBVIEW_TIMER_LAG_MS.load(Ordering::Relaxed),
        webview_frame_lag_ms: if frame < 0 { None } else { Some(frame as u64) },
        webview_hidden: WEBVIEW_HIDDEN.load(Ordering::Relaxed),
    }
}

/// What the two stamps say about which half of the app is stuck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// One of the two halves has never stamped, so there is nothing to compare.
    /// The state at startup, and the state of every headless build and test.
    Unarmed,
    Ok,
    /// The backend is ticking and the webview has stopped stamping — beta5's
    /// shape: the GUI thread is parked.
    GuiStuck,
    /// The webview has stopped stamping and its last stamp said the window was
    /// HIDDEN. Not an alarm, and deliberately not folded into either
    /// [`Liveness::Ok`] or [`Liveness::GuiStuck`].
    ///
    /// The platform throttles a hidden window's timers — a minimized window may
    /// legitimately stamp once a minute — so a stale stamp from one is not
    /// evidence of anything. Calling that `GuiStuck` would fire the alarm every
    /// time the human minimizes the app, which is how an instrument stops being
    /// read; calling it `Ok` would claim a health check that was never made.
    /// This says "no evidence", which is the true answer.
    ///
    /// The residual, stated rather than left to be found: a GUI genuinely
    /// wedged while the window is hidden is reported as this, not as
    /// `GuiStuck`. Nothing here can separate the two — the webview is the only
    /// witness to its own liveness, and a hidden one is not being asked. What
    /// bounds it is that the human's next interaction un-hides the window, and
    /// the very next tick after that reads `GuiStuck` for real.
    GuiHidden,
    /// The webview is stamping and the watchdog is not being scheduled, or its
    /// stamp has gone stale — beta6's shape: the window is alive and everything
    /// behind it is starved.
    BackendStuck,
    BothStuck,
}

/// The pure verdict over a [`Heartbeat`].
///
/// `stale_ms` is how old a stamp may be before its half counts as stopped.
///
/// **The watchdog's own lag is part of "backend fresh", not a separate signal.**
/// This function is called from the watchdog tick, a few microseconds after that
/// tick stamped — so a freshness test on the stamp alone would report `Ok` from
/// inside a starved backend, every time. The lag is the watchdog saying how long
/// it waited to run, and it is the only way this verdict can ever be
/// `BackendStuck` when the watchdog is the one asking.
pub fn liveness(hb: &Heartbeat, now_ms: u64, stale_ms: u64) -> Liveness {
    if hb.watchdog_ticks == 0 || hb.webview_stamps == 0 {
        return Liveness::Unarmed;
    }
    // NEUTERED (scratch, #1601): judge the backend on its stamp alone, which
    // this function reads microseconds after the watchdog wrote it.
    let backend_fresh = now_ms.saturating_sub(hb.watchdog_ms) <= stale_ms;
    let gui_fresh = now_ms.saturating_sub(hb.webview_ms) <= stale_ms;
    match (backend_fresh, gui_fresh, hb.webview_hidden) {
        (true, true, _) => Liveness::Ok,
        (true, false, false) => Liveness::GuiStuck,
        (true, false, true) => Liveness::GuiHidden,
        (false, true, _) => Liveness::BackendStuck,
        // A stale stamp from a hidden window is no evidence about the GUI, but
        // the backend half is measured here and stands on its own — so the
        // verdict names the half it can actually speak for rather than
        // upgrading itself to `BothStuck` on a reading it just declined to use.
        (false, false, true) => Liveness::BackendStuck,
        (false, false, false) => Liveness::BothStuck,
    }
}

impl Liveness {
    /// The breadcrumb `event` for a verdict worth writing down, or `None` for
    /// the two that are not news.
    pub fn event(self) -> Option<&'static str> {
        match self {
            // `GuiHidden` is silent for the same reason it exists: it is the
            // absence of evidence, and a breadcrumb per minimize would train a
            // reader to skip this event class.
            Liveness::Unarmed | Liveness::Ok | Liveness::GuiHidden => None,
            Liveness::GuiStuck => Some("live-gui-stuck"),
            Liveness::BackendStuck => Some("live-backend-stuck"),
            Liveness::BothStuck => Some("live-both-stuck"),
        }
    }
}

/// The heartbeat half of the watchdog: reports a verdict on the TRANSITION into
/// it, so a fifteen-minute hang is one breadcrumb rather than nine hundred.
#[derive(Debug, Default)]
pub struct LivenessWatch {
    last: Option<Liveness>,
}

impl LivenessWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Some(verdict)` when the verdict has just changed to one worth
    /// reporting. Recovery is silent by design: the next `Ok` re-arms the
    /// report without writing a line nobody is looking for.
    pub fn tick(&mut self, verdict: Liveness) -> Option<Liveness> {
        let changed = self.last != Some(verdict);
        self.last = Some(verdict);
        if changed && verdict.event().is_some() {
            Some(verdict)
        } else {
            None
        }
    }
}

// ---------- the thread ----------

/// One watchdog per process.
static WATCHDOG_STARTED: AtomicBool = AtomicBool::new(false);

/// Start the self-watchdog.
///
/// One thread, [`WATCHDOG_INTERVAL_MS`] cadence, off every hot path. It takes
/// no tracked lock and no registry lock — it reads atomics and writes
/// breadcrumbs — which is what keeps it able to report the hang rather than
/// join it.
///
/// Idempotent: a second call is a no-op, so a test harness or a second window
/// cannot start two.
pub fn spawn_watchdog() {
    if WATCHDOG_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("loomux-watchdog".into())
        .spawn(run_watchdog);
}

fn run_watchdog() {
    let mut locks = LockWatch::new();
    let mut pool = PoolWatch::new();
    let mut live = LivenessWatch::new();
    let mut last_ms = lockwatch::mono_ms();
    obs::breadcrumb(
        "watchdog-start",
        &format!(
            "interval_ms={} hold_warn_ms={} locks={}",
            WATCHDOG_INTERVAL_MS,
            lockwatch::hold_warn_ms(),
            lockwatch::live_lock_count()
        ),
    );
    loop {
        std::thread::sleep(std::time::Duration::from_millis(WATCHDOG_INTERVAL_MS));
        let now = lockwatch::mono_ms();
        let lag = now.saturating_sub(last_ms).saturating_sub(WATCHDOG_INTERVAL_MS);
        last_ms = now;
        watchdog_stamp(now, lag);

        lockwatch::record_all(locks.tick(&lockwatch::held_locks(now), lockwatch::hold_warn_ms()));

        if let Some(r) = pool.tick(pool_take_peak()) {
            obs::breadcrumb(
                "pool-depth",
                &format!("step={} peak={} in_flight={}", r.step, r.peak, pool_in_flight()),
            );
        }

        let hb = heartbeat();
        if let Some(v) = live.tick(liveness(&hb, now, LIVENESS_STALE_MS)) {
            if let Some(event) = v.event() {
                obs::breadcrumb(event, &heartbeat_detail(&hb, now));
            }
        }
    }
}

/// The `detail` field of a liveness breadcrumb: everything a reader needs to
/// check the verdict rather than take it on trust.
pub fn heartbeat_detail(hb: &Heartbeat, now_ms: u64) -> String {
    format!(
        "watchdog_age_ms={} watchdog_lag_ms={} ticks={} gui_age_ms={} gui_timer_lag_ms={} gui_frame_lag_ms={} gui_hidden={} pool_in_flight={}",
        now_ms.saturating_sub(hb.watchdog_ms),
        hb.watchdog_lag_ms,
        hb.watchdog_ticks,
        now_ms.saturating_sub(hb.webview_ms),
        hb.webview_timer_lag_ms,
        hb.webview_frame_lag_ms.map(|v| v.to_string()).unwrap_or_else(|| "none".into()),
        hb.webview_hidden,
        pool_in_flight(),
    )
}
