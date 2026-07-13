//! Crash observability (issue #53).
//!
//! Three cheap, dependency-free facilities so the *next* hard crash leaves
//! something to read:
//!
//! 1. A **panic hook** that appends a crash log (message + location + thread +
//!    backtrace) to `<data>/loomux/logs/crash-<ts>.log`. It wraps — and still
//!    chains to — the default hook, and is written to never panic itself.
//! 2. A **breadcrumb log** (`breadcrumbs.log`, rotated once at ~2 MB) of
//!    timestamped one-liners for lifecycle events — pane/PTY open/close/resize,
//!    agent spawn/death, MCP request failures, delivery outcomes. It must never
//!    be handed prompt or output *content*: that lives in the audit log already,
//!    and breadcrumbs stay small and privacy-safe (event + ids only).
//! 3. A **running.lock sentinel**, written at startup and removed on a clean
//!    shutdown. Finding it at the next startup means the previous run died
//!    without unwinding to a clean exit; we surface a next-launch notice that
//!    names the newest crash log.
//!
//! Rotation mirrors the orchestration audit log (`rotate_audit_if_needed`): one
//! kept generation, size-triggered, single-write `O_APPEND` lines. Breadcrumb
//! writes stay lock-free — unlike the audit log they carry no rotation/append
//! ordering contract, and a line that races a rollover lands in the rotated
//! generation rather than being lost. A crash that
//! aborts the process without unwinding (stack overflow, an FFI access
//! violation, `abort()`) never runs the hook — see `doc/design/crash-observability.md`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Breadcrumb log rolls to `breadcrumbs.1.log` past this size (one kept
/// generation). Two megabytes is thousands of one-liners — plenty of "what was
/// in flight" history without unbounded growth.
const BREADCRUMB_ROTATE_BYTES: u64 = 2 * 1024 * 1024;

/// Poison-tolerant `Mutex::lock` (issue #53). A poisoned mutex means some
/// *other* thread panicked while holding it; `.lock().unwrap()` would then
/// propagate that panic to every later locker, turning one edge-case panic into
/// a cascade that takes the whole app down. For loomux's registries and PTY
/// tables the guarded data is at worst slightly stale after such a panic (a
/// half-finished map insert), never memory-unsafe — so recovering the guard and
/// proceeding is strictly safer than crashing. Use for shared, long-lived locks
/// on the hot paths; the audit lists which locks were converted.
pub trait LockExt<T> {
    fn lock_safe(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_safe(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------- log directory ----------

/// Test-only override so the fs-touching helpers can be pointed at a temp tree
/// without mutating global env — safe under parallel test execution.
#[cfg(test)]
static LOG_DIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// `<user data dir>/loomux/logs`. Falls back to the temp dir when the platform
/// has no data dir, mirroring `OrchRegistry::default_root` so crash logs and
/// orchestration state live under the same `loomux/` root.
pub fn logs_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = LOG_DIR_OVERRIDE.lock().unwrap().clone() {
        return dir;
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("loomux")
        .join("logs")
}

// ---------- timestamps ----------

/// Format unix-millis as `YYYYMMDD-HHMMSS` (UTC): filename-safe and lexically
/// sortable, so "newest crash log" is a plain string max. Pure — computed via
/// Howard Hinnant's days-from-civil algorithm so no date crate is pulled in
/// (and nothing that would drag in getrandom; see Cargo.toml).
fn stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// Civil (year, month, day) from a count of days since the Unix epoch.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // day-of-era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year [0, 365]
    let mp = (5 * doy + 2) / 153; // month-portion [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------- breadcrumbs ----------

/// Roll `current` over to `rotated` once it exceeds `cap`. Lock-free like the
/// audit-log rotation: a lost race just leaves one thread's rename failing
/// harmlessly.
fn rotate_if_needed(current: &Path, rotated: &Path, cap: u64) {
    if fs::metadata(current).map(|m| m.len()).unwrap_or(0) > cap {
        let _ = fs::rename(current, rotated); // replaces the old generation
    }
}

/// Append one timestamped breadcrumb to `<logs>/breadcrumbs.log`. Best-effort
/// and cheap (one `O_APPEND` write, atomic per line); never logs prompt/output
/// content. `event` is a short kind, `detail` a few ids/flags — no free text
/// from panes.
pub fn breadcrumb(event: &str, detail: &str) {
    breadcrumb_in(&logs_dir(), event, detail);
}

fn breadcrumb_in(dir: &Path, event: &str, detail: &str) {
    let _ = fs::create_dir_all(dir);
    rotate_if_needed(
        &dir.join("breadcrumbs.log"),
        &dir.join("breadcrumbs.1.log"),
        BREADCRUMB_ROTATE_BYTES,
    );
    // Build the whole line first and emit it with ONE write: `O_APPEND` is atomic
    // per write syscall, and a `writeln!` with several arguments emits one write
    // per fragment — which is precisely how the audit log ended up with records
    // spliced into each other (#240). Breadcrumbs are written from every pane
    // thread, so the same race lives here.
    let line = format!("{} {} {}\n", stamp(now_ms()), event, detail);
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("breadcrumbs.log"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

// ---------- panic hook ----------

/// Install the crash-logging panic hook. Wraps the existing hook (so dev-build
/// console output is unchanged) and, before chaining to it, writes a crash log
/// and a `panic` breadcrumb. Every step is best-effort; the hook never panics.
///
/// Works for panics on *any* thread — the background PTY reader/waiter threads,
/// the MCP request threads, delivery threads, and the watchers all route
/// through the process-wide hook, not just the main thread.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Never let the hook itself unwind and mask the real panic.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write_crash_log(info)));
        default(info);
    }));
}

fn write_crash_log(info: &std::panic::PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let tname = thread.name().unwrap_or("<unnamed>").to_string();
    let payload = info.payload();
    let msg = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string());
    let loc = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    // force_capture ignores RUST_BACKTRACE, so a crash log always carries a
    // backtrace. Frame *symbols* depend on the release profile keeping a
    // symbol table (see the design note); the addresses are always useful.
    let bt = std::backtrace::Backtrace::force_capture().to_string();
    record_crash(&logs_dir(), &tname, &msg, &loc, &bt);
    breadcrumb(
        "panic",
        &format!("thread={tname} at {}", loc.replace(' ', "_")),
    );
}

/// Write one crash log. Split from `write_crash_log` so the file format is
/// testable without a live `PanicHookInfo` (which can't be constructed).
fn record_crash(dir: &Path, thread: &str, msg: &str, loc: &str, backtrace: &str) {
    let _ = fs::create_dir_all(dir);
    let now = now_ms();
    let path = dir.join(format!("crash-{}.log", stamp(now)));
    let body = format!(
        "loomux crash log\n\
         version: {ver}\n\
         time:    {ts} UTC ({now} ms since epoch)\n\
         thread:  {thread}\n\
         panic:   {msg}\n\
         at:      {loc}\n\n\
         backtrace:\n{backtrace}\n",
        ver = env!("CARGO_PKG_VERSION"),
        ts = stamp(now),
    );
    // Append rather than truncate: two threads panicking into the same-second
    // filename both leave a record instead of one clobbering the other.
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(body.as_bytes());
    }
}

// ---------- unclean-exit detection ----------

/// Outcome of the startup check: whether the previous run ended uncleanly and
/// the newest crash log (if any) to point the user at.
pub struct StartupCheck {
    pub unclean: bool,
    pub crash_log: Option<PathBuf>,
}

impl StartupCheck {
    /// The next-launch toast text, or `None` when the previous exit was clean.
    pub fn notice(&self) -> Option<String> {
        if !self.unclean {
            return None;
        }
        Some(match &self.crash_log {
            Some(p) => format!(
                "loomux exited unexpectedly last run — crash log at {}",
                p.display()
            ),
            None => "loomux exited unexpectedly last run — no crash log was written \
                     (a hard abort, not an unwinding panic); see breadcrumbs.log"
                .to_string(),
        })
    }
}

fn running_lock(dir: &Path) -> PathBuf {
    dir.join("running.lock")
}

/// Detect a leftover sentinel (unclean previous exit), locate the newest crash
/// log, then (re)arm the sentinel for this run. Call once at startup, before
/// anything else can crash.
pub fn check_and_arm() -> StartupCheck {
    check_and_arm_in(&logs_dir())
}

fn check_and_arm_in(dir: &Path) -> StartupCheck {
    let _ = fs::create_dir_all(dir);
    let lock = running_lock(dir);
    let unclean = lock.exists();
    // The sentinel's own mtime marks when the previous (crashed) run *started*.
    // Only a crash log written at or after that instant can belong to that run;
    // a hard abort writes no crash log, so without this guard we'd mis-name an
    // older log from an earlier run and point the user at the wrong crash.
    let since = if unclean {
        fs::metadata(&lock).and_then(|m| m.modified()).ok()
    } else {
        None
    };
    let crash_log = if unclean { newest_crash_log_since(dir, since) } else { None };
    let _ = fs::write(&lock, stamp(now_ms()));
    StartupCheck { unclean, crash_log }
}

/// Remove the sentinel to record a clean shutdown. Called from the window
/// Destroyed path; if the process dies before this runs, the next startup sees
/// the sentinel and reports an unclean exit (conservative by design).
pub fn mark_clean_exit() {
    let _ = fs::remove_file(running_lock(&logs_dir()));
}

fn is_crash_log(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".log"))
}

/// Newest `crash-*.log` in `dir` (by filename — stamps sort lexically) whose
/// mtime is at or after `since`. The mtime gate is what keeps a hard abort (no
/// crash log written) from mis-attributing an older log to this crash; pass
/// `since = None` to disable it (best effort when the sentinel mtime is
/// unreadable). `None` when nothing qualifies.
fn newest_crash_log_since(dir: &Path, since: Option<SystemTime>) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_crash_log(p))
        .filter(|p| match since {
            Some(t) => fs::metadata(p)
                .and_then(|m| m.modified())
                .map(|m| m >= t)
                .unwrap_or(false),
            None => true,
        })
        .max()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the few tests that install the global panic hook / use the
    /// log-dir override, so parallel execution can't cross their global state.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn with_log_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        *LOG_DIR_OVERRIDE.lock().unwrap() = Some(dir.to_path_buf());
        let out = f();
        *LOG_DIR_OVERRIDE.lock().unwrap() = None;
        out
    }

    #[test]
    fn stamp_is_sortable_utc() {
        // 2026-07-05T00:00:00Z = 1_783_209_600_000 ms.
        assert_eq!(stamp(1_783_209_600_000), "20260705-000000");
        // One day + 1h2m3s later.
        assert_eq!(stamp(1_783_209_600_000 + 86_400_000 + 3_723_000), "20260706-010203");
        // Epoch.
        assert_eq!(stamp(0), "19700101-000000");
        // Newer stamp must sort after older lexically (drives newest_crash_log).
        assert!(stamp(2_000_000_000_000) > stamp(1_751_673_600_000));
    }

    #[test]
    fn records_crash_file_with_context() {
        let tmp = tempfile::tempdir().unwrap();
        record_crash(tmp.path(), "worker-3", "boom", "src/pty.rs:42:9", "0: frame\n1: frame");
        let files: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| n.starts_with("crash-") && n.ends_with(".log"))
            .collect();
        assert_eq!(files.len(), 1, "exactly one crash log written");
        let body = fs::read_to_string(tmp.path().join(&files[0])).unwrap();
        assert!(body.contains("thread:  worker-3"));
        assert!(body.contains("panic:   boom"));
        assert!(body.contains("src/pty.rs:42:9"));
        assert!(body.contains("0: frame"));
    }

    #[test]
    fn forced_panic_in_background_thread_writes_crash_log() {
        let _serial = SERIAL.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        with_log_dir(tmp.path(), || {
            let prev = std::panic::take_hook();
            install_panic_hook();
            // Panic on a *named background* thread — the acceptance criterion.
            let h = std::thread::Builder::new()
                .name("crash-test-worker".into())
                .spawn(|| panic!("synthetic background crash"))
                .unwrap();
            assert!(h.join().is_err(), "thread must have panicked");
            std::panic::set_hook(prev); // restore before releasing the serial lock

            let dir = fs::read_dir(tmp.path()).unwrap();
            let crash = dir
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("crash-"))
                })
                .expect("a crash log must exist");
            let body = fs::read_to_string(&crash).unwrap();
            assert!(body.contains("crash-test-worker"), "captures the thread name");
            assert!(body.contains("synthetic background crash"), "captures the message");
            // The panic also drops a breadcrumb.
            let crumbs = fs::read_to_string(tmp.path().join("breadcrumbs.log")).unwrap();
            assert!(crumbs.contains("panic"));
        });
    }

    #[test]
    fn unclean_exit_detected_via_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        // First launch: no sentinel yet → clean, and it arms one.
        let first = check_and_arm_in(tmp.path());
        assert!(!first.unclean);
        assert!(running_lock(tmp.path()).exists(), "sentinel armed for this run");
        assert!(first.notice().is_none());

        // Crash (no clean exit): the sentinel survives. Next launch sees it.
        let second = check_and_arm_in(tmp.path());
        assert!(second.unclean, "leftover sentinel means unclean previous exit");
        assert!(second.notice().unwrap().contains("exited unexpectedly"));

        // Clean exit removes it; the following launch is clean again.
        let _serial = SERIAL.lock().unwrap();
        with_log_dir(tmp.path(), || mark_clean_exit());
        assert!(!running_lock(tmp.path()).exists());
        assert!(!check_and_arm_in(tmp.path()).unclean);
    }

    /// Pin a file's mtime so the mtime-gate assertions don't depend on wall
    /// clock. `set_modified` is stable std; no external crate (cf. gitwatch).
    fn set_mtime(path: &Path, t: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn notice_names_only_a_crash_log_from_the_crashed_run() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();

        // Run 1 starts: arm the sentinel. Its mtime is the run's start instant.
        check_and_arm_in(tmp.path());
        let start = fs::metadata(running_lock(tmp.path())).unwrap().modified().unwrap();

        // A stale crash log from an EARLIER run (before this sentinel).
        let old = tmp.path().join("crash-20200101-000000.log");
        fs::write(&old, "old").unwrap();
        set_mtime(&old, start - Duration::from_secs(3600));

        // Case A — hard abort: the run wrote no new crash log. Unclean, but the
        // stale older log must NOT be named (that was the mis-attribution bug).
        let abort = check_and_arm_in(tmp.path());
        assert!(abort.unclean);
        assert!(abort.crash_log.is_none(), "must not name a pre-sentinel log");
        assert!(abort.notice().unwrap().contains("no crash log was written"));

        // Case B — a real panic during this run drops a crash log newer than the
        // sentinel it re-armed above; that one IS named.
        let start2 = fs::metadata(running_lock(tmp.path())).unwrap().modified().unwrap();
        let fresh = tmp.path().join("crash-20260705-120000.log");
        fs::write(&fresh, "boom").unwrap();
        set_mtime(&fresh, start2 + Duration::from_secs(5));

        let crash = check_and_arm_in(tmp.path());
        assert!(crash.unclean);
        assert_eq!(crash.crash_log.as_deref(), Some(fresh.as_path()));
        assert!(crash.notice().unwrap().contains("crash-20260705-120000.log"));
    }

    #[test]
    fn lock_safe_recovers_a_poisoned_mutex() {
        let m = std::sync::Arc::new(Mutex::new(vec![1, 2, 3]));
        let m2 = m.clone();
        // Poison the mutex: mutate then panic while still holding the guard.
        let _ = std::thread::spawn(move || {
            let mut g = m2.lock().unwrap();
            g.push(4);
            panic!("poison the mutex on purpose");
        })
        .join();
        assert!(m.lock().is_err(), "mutex must be poisoned by the panic");

        // The load-bearing fix: lock_safe serves the recovered data instead of
        // propagating the poison as a panic.
        let g = m.lock_safe();
        assert_eq!(&*g, &[1, 2, 3, 4], "recovered guard sees the mutation");
    }

    #[test]
    fn breadcrumb_rotates_at_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let current = tmp.path().join("breadcrumbs.log");
        let rotated = tmp.path().join("breadcrumbs.1.log");

        // Fill past a tiny cap, then one more write must roll the file over.
        fs::write(&current, "x".repeat(64)).unwrap();
        rotate_if_needed(&current, &rotated, 32);
        assert!(rotated.exists(), "over-cap file rolled to generation 1");
        assert!(!current.exists(), "current renamed away");

        // A fresh write recreates the current log; the rotated one is kept.
        breadcrumb_in(tmp.path(), "pty-open", "id=7");
        assert!(current.exists());
        assert!(rotated.exists(), "one generation retained");
        assert!(fs::read_to_string(&current).unwrap().contains("pty-open id=7"));
    }

    #[test]
    fn breadcrumb_writes_event_and_detail_only() {
        let tmp = tempfile::tempdir().unwrap();
        breadcrumb_in(tmp.path(), "delivery", "agent=w-3 outcome=typed");
        let line = fs::read_to_string(tmp.path().join("breadcrumbs.log")).unwrap();
        // stamp <event> <detail>, one line.
        assert!(line.contains(" delivery agent=w-3 outcome=typed"));
        assert_eq!(line.lines().count(), 1);
    }
}
