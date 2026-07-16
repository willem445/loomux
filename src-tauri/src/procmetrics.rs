//! Per-process system metrics behind the `metrics.system` broker capability
//! (#360 Slice E — see `doc/design/pane-plugins.md`'s capability table and
//! `pluginbroker.rs`'s module doc comment). This module is the "the numbers"
//! half of the sentence Slice C left in its own doc comment: "the check is
//! real, the numbers aren't yet" — the capability gate itself
//! (`pluginbroker::check_request`) is unchanged and untouched by this slice.
//!
//! **Never a `#[tauri::command]`.** The design note is explicit: `sys_processes`
//! -shaped data is exposed "only through the metrics.system broker handler —
//! never as a command a plugin (or any other webview script) could `invoke`
//! directly." So this module has no command of its own; `pluginbroker::dispatch`
//! calls [`subscribe`]/[`unsubscribe`] the same way it already calls
//! `storage_get`/`fs_read` for the other capabilities.
//!
//! **Bounding, per the design note's threat table** ("a plugin reading metrics
//! shouldn't be able to DoS the host"): [`clamp_interval_ms`] refuses a tight
//! polling loop regardless of what a plugin asks for, and [`shape_processes`]
//! caps + sorts the process list so a plugin never receives the raw process
//! table. Both are pure and unit-tested against injected data — never live
//! system state, per this slice's brief (per-process numbers are
//! environment-dependent; the shaping/bounding logic they run through is not).
//!
//! **Curated payload only** (design note's capability table): [`ProcessSample`]
//! carries name/pid/cpu%/rss and nothing else — no cmdline, no exe path, no
//! environment. `sysinfo::Process` exposes all of those; this module simply
//! never reads them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use sysinfo::{ProcessesToUpdate, System};

use crate::obs::LockExt;
use crate::pluginbroker::{self, PluginErrorWire};

/// A plugin can request a faster tick than this; it never gets one — the
/// design note's DoS-bound applies to the *sampling* cadence, not just the
/// payload shape.
pub const MIN_POLL_INTERVAL_MS: u64 = 1000;
/// A plugin idle for longer than this between requests would see a stale
/// dashboard, so the clamp has a ceiling too, not just a floor.
pub const MAX_POLL_INTERVAL_MS: u64 = 10_000;
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 2000;

/// Cap on processes returned per tick. `sysinfo` on a busy dev machine easily
/// enumerates several hundred processes; streaming all of them, every tick,
/// into a sandboxed plugin window is exactly the unthrottled-table-dump the
/// design note's bounding requirement exists to refuse.
pub const MAX_PROCESSES: usize = 32;

/// The curated per-process payload (design note's capability table: "name,
/// pid, cpu%, rss. No cmdline, no paths, no environment").
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProcessSample {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
}

/// One `metrics.tick` payload: system totals plus the bounded, shaped
/// per-process list.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MetricsSnapshot {
    pub cpu_percent: f32,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub processes: Vec<ProcessSample>,
}

/// Sort by CPU% descending (RSS descending as a tiebreak, so an idle-CPU
/// memory hog still surfaces ahead of processes at 0%) and cap the list at
/// [`MAX_PROCESSES`]. Pure and deterministic — the DoS-bound half of this
/// slice's brief, unit-tested against an injected process list rather than
/// live `sysinfo` output.
pub fn shape_processes(mut processes: Vec<ProcessSample>) -> Vec<ProcessSample> {
    processes.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.rss_bytes.cmp(&a.rss_bytes))
    });
    processes.truncate(MAX_PROCESSES);
    processes
}

/// Clamp a plugin-requested poll interval into
/// `[MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS]`; a missing/non-numeric
/// request falls back to [`DEFAULT_POLL_INTERVAL_MS`]. The other bounding
/// half of this slice's brief — a plugin cannot talk the sampler into a
/// tight loop by asking for one.
pub fn clamp_interval_ms(requested: Option<u64>) -> u64 {
    requested
        .unwrap_or(DEFAULT_POLL_INTERVAL_MS)
        .clamp(MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS)
}

/// One live sample. Impure by construction (reads real system state), so it
/// is deliberately kept to a thin call-through onto [`shape_processes`] — the
/// only part of this file the test suite exercises with live data is this
/// function, and only indirectly, via the subscription thread below.
fn sample_snapshot(sys: &mut System) -> MetricsSnapshot {
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let processes = sys
        .processes()
        .iter()
        .map(|(pid, proc_)| ProcessSample {
            pid: pid.as_u32(),
            name: proc_.name().to_string_lossy().into_owned(),
            cpu_percent: proc_.cpu_usage(),
            rss_bytes: proc_.memory(),
        })
        .collect();

    MetricsSnapshot {
        cpu_percent: sys.global_cpu_usage(),
        mem_used_bytes: sys.used_memory(),
        mem_total_bytes: sys.total_memory(),
        processes: shape_processes(processes),
    }
}

// ---------- per-window subscription registry ----------
//
// Keyed by plugin window label (the same key `pluginbroker`'s session/channel
// registries use), one poll thread per subscribed window. A plugin can only
// ever have one live subscription — `subscribe` replaces rather than stacks —
// so re-subscribing (e.g. to change the interval) can't multiply the poll rate.

static SUBSCRIPTIONS: Mutex<Option<HashMap<String, Arc<AtomicBool>>>> = Mutex::new(None);

fn with_subscriptions<R>(f: impl FnOnce(&mut HashMap<String, Arc<AtomicBool>>) -> R) -> R {
    let mut guard = SUBSCRIPTIONS.lock_safe();
    f(guard.get_or_insert_with(HashMap::new))
}

fn stop_subscription(label: &str) {
    if let Some(stop) = with_subscriptions(|m| m.remove(label)) {
        stop.store(true, Ordering::Relaxed);
    }
}

/// `metrics.subscribe` handler: starts (or restarts, at a possibly new
/// interval) a background poll loop that pushes `metrics.tick` events to the
/// plugin's already-open broker channel (`pluginbroker::push_event`) until
/// unsubscribed or the window closes. Returns immediately with an ack — the
/// data itself arrives asynchronously over the channel, the same as any other
/// unsolicited `PluginEvent`.
pub fn subscribe(label: &str, params: &Value) -> Result<Value, PluginErrorWire> {
    let requested = params.get("intervalMs").and_then(Value::as_u64);
    let interval = Duration::from_millis(clamp_interval_ms(requested));

    // Stop any prior loop for this window first so two threads never race
    // pushes onto the same channel.
    stop_subscription(label);

    let stop = Arc::new(AtomicBool::new(false));
    with_subscriptions(|m| {
        m.insert(label.to_string(), stop.clone());
    });

    let label = label.to_string();
    thread::spawn(move || {
        let mut sys = System::new();
        // Prime the CPU counters the same way metrics.rs's status-bar sampler
        // does: the first reading needs a prior sample to diff against.
        sys.refresh_cpu_usage();
        thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);

        while !stop.load(Ordering::Relaxed) {
            let snapshot = sample_snapshot(&mut sys);
            if let Ok(payload) = serde_json::to_value(&snapshot) {
                pluginbroker::push_event(&label, "metrics.tick", payload);
            }
            thread::sleep(interval);
        }
    });

    Ok(Value::Null)
}

/// `metrics.unsubscribe` handler: stops this window's poll loop, if any.
/// Idempotent — unsubscribing twice (or without ever subscribing) is not an
/// error.
pub fn unsubscribe(label: &str) -> Result<Value, PluginErrorWire> {
    stop_subscription(label);
    Ok(Value::Null)
}

/// Cleanup hook, called from `lib.rs`'s `on_window_event(Destroyed)` handler
/// alongside `pluginbroker::on_window_destroyed` — a closed plugin window's
/// poll thread stops on its own next tick rather than pushing forever into a
/// dead channel.
pub fn on_window_destroyed(label: &str) {
    stop_subscription(label);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pid: u32, name: &str, cpu: f32, rss: u64) -> ProcessSample {
        ProcessSample {
            pid,
            name: name.to_string(),
            cpu_percent: cpu,
            rss_bytes: rss,
        }
    }

    #[test]
    fn shape_processes_sorts_by_cpu_descending() {
        let input = vec![
            sample(1, "low", 5.0, 100),
            sample(2, "high", 90.0, 100),
            sample(3, "mid", 40.0, 100),
        ];
        let shaped = shape_processes(input);
        assert_eq!(
            shaped.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn shape_processes_breaks_cpu_ties_by_rss_descending() {
        let input = vec![
            sample(1, "a", 0.0, 1_000),
            sample(2, "b", 0.0, 5_000),
            sample(3, "c", 0.0, 2_000),
        ];
        let shaped = shape_processes(input);
        assert_eq!(
            shaped.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn shape_processes_caps_at_max_processes() {
        let input: Vec<ProcessSample> = (0..(MAX_PROCESSES as u32 + 50))
            .map(|pid| sample(pid, "p", pid as f32, 0))
            .collect();
        let shaped = shape_processes(input);
        assert_eq!(shaped.len(), MAX_PROCESSES);
        // The cap keeps the highest-CPU entries, not an arbitrary prefix of
        // the input order.
        assert_eq!(shaped[0].cpu_percent, (MAX_PROCESSES as u32 + 49) as f32);
    }

    #[test]
    fn clamp_interval_ms_defaults_when_unrequested() {
        assert_eq!(clamp_interval_ms(None), DEFAULT_POLL_INTERVAL_MS);
    }

    #[test]
    fn clamp_interval_ms_floors_a_too_fast_request() {
        assert_eq!(clamp_interval_ms(Some(1)), MIN_POLL_INTERVAL_MS);
    }

    #[test]
    fn clamp_interval_ms_ceilings_a_too_slow_request() {
        assert_eq!(clamp_interval_ms(Some(u64::MAX)), MAX_POLL_INTERVAL_MS);
    }

    #[test]
    fn clamp_interval_ms_passes_through_an_in_band_request() {
        assert_eq!(clamp_interval_ms(Some(3000)), 3000);
    }

    #[test]
    fn subscribe_then_unsubscribe_is_idempotent_and_stops_the_loop() {
        let label = "test-metrics-window";
        assert!(subscribe(label, &Value::Null).is_ok());
        assert!(unsubscribe(label).is_ok());
        // Unsubscribing again (already stopped) must not error.
        assert!(unsubscribe(label).is_ok());
    }
}
