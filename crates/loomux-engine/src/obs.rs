//! Crash observability (issue #53).
//!
//! Three cheap, dependency-free facilities so the *next* hard crash leaves
//! something to read:
//!
//! 1. A **panic hook** that appends a crash log (message + location + thread +
//!    backtrace) to `<data>/orrerix/logs/crash-<ts>.log`. It wraps — and still
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
//! None of that is desktop-specific, which is why the file lives here (#888
//! slice A3 batch 7). What *is* desktop-specific — the `StartupNotice` state
//! cell and the `take_startup_notice` command the webview drains it through —
//! stayed in `src-tauri/src/obs.rs`, which re-exports everything below so every
//! `crate::obs::…` call site over there still resolves.
//!
//! Rotation mirrors the orchestration audit log (`rotate_audit_if_needed`): one
//! kept generation, size-triggered, single-write `O_APPEND` lines. Breadcrumb
//! writes stay lock-free — unlike the audit log they carry no rotation/append
//! ordering contract, and a line that races a rollover lands in the rotated
//! generation rather than being lost. A crash that
//! aborts the process without unwinding (stack overflow, an FFI access
//! violation, `abort()`) never runs the hook — see `doc/design/crash-observability.md`.

use crate::brand;
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

/// Guards the rejection warning below to once per process: `data_root_from`
/// is called from every `logs_dir()`/`state_dir()`/`default_root()` call —
/// unbounded over the process lifetime, including once per breadcrumb write
/// — not once at startup.
static WARNED_BAD_DATA_DIR: std::sync::Once = std::sync::Once::new();

/// The `data_root()` decision, taking the env var reading as a parameter so
/// it's testable without mutating real process env (`std::env::set_var` is
/// both `unsafe` and cross-thread-global — not worth it for a one-branch
/// decision when the branch itself can just be a pure function).
///
/// An empty or relative override is rejected rather than used as-is: every
/// consumer of this root treats persistence as best-effort (never a hard
/// failure), so a bad value wouldn't error — it would silently redirect
/// orchestration state, logs, and tabs into whatever the process's current
/// working directory happens to be (often a git repo). Falling back to the
/// platform data dir keeps that failure mode from being silent: the
/// rejection prints once to stderr (not routed through `breadcrumb()`, which
/// itself calls back into this function via `logs_dir()` — that would
/// recurse). The durable, always-checkable record of what root a run actually
/// used is the `data_root=` startup breadcrumb (`src-tauri/src/lib.rs`), not
/// this warning.
///
/// An override — under *either* env name (#1153) — is used **exactly as
/// given**: no rename, no migration, no probing of a sibling. An operator who
/// names a root has named the root, and an E2E run's isolated profile must
/// stay isolated. The `loomux`→`orrerix` rename applies only to the *platform
/// default* below, via [`resolve_default_root`].
fn data_root_from(env_override: Option<std::ffi::OsString>) -> PathBuf {
    match override_root(env_override) {
        Some(path) => path,
        None => resolve_default_root(),
    }
}

/// The **one** definition of "this run is using an explicitly-named root", and
/// the one place a bad override is rejected and reported.
///
/// Extracted because two guards depend on the same answer and must never
/// disagree about it: `data_root_from` decides which root to *use*, and
/// `init_data_root` decides whether to *migrate* — and a migration guard that
/// read the variable by its own slightly different rule would be a bypass
/// exactly the width of the difference. `None` means "no usable override; the
/// platform default applies", for both callers alike.
///
/// Rejecting rather than warning-and-using is the pre-existing #394 behaviour:
/// every consumer treats persistence as best-effort, so an empty or relative
/// value would not error — it would silently redirect orchestration state, logs
/// and tabs into the process's current working directory (often a git repo).
fn override_root(env_override: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let dir = env_override?;
    let path = PathBuf::from(&dir);
    if !dir.is_empty() && path.is_absolute() {
        return Some(path);
    }
    WARNED_BAD_DATA_DIR.call_once(|| {
        eprintln!(
            "orrerix: {}={dir:?} is empty or not an absolute path — ignoring it \
             and using the platform data dir instead",
            brand::env_names("DATA_DIR"),
        );
    });
    None
}

/// The platform data dir itself — the parent both the current and the legacy
/// roots sit in. Split out so the migration decision and the roots it acts on
/// are derived from one expression rather than two that could drift.
fn platform_data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir)
}

/// What to do with the two platform-default roots, `<data>/orrerix` and the
/// pre-#1153 `<data>/loomux`.
///
/// **This enum is the migration policy, and it is deliberately the whole of
/// it.** See `doc/design/rebrand-filesystem.md`. The shipped policy is
/// move-on-first-launch; reverting to read-the-old-one-forever is **one arm of
/// [`plan_default_root`]** — `(false, true) => RootPlan::UseLegacy` instead of
/// `Migrate` — plus a doc edit, not a re-architecture.
///
/// That revert works only because [`root_action`] gives `UseLegacy` its **own**
/// dispatch arm. It did not, in this PR's first cut: `UseLegacy` was folded in
/// with `Migrate` on the argument that `plan_default_root` "cannot return it",
/// which made the documented revert **inert** — the edited arm still reached
/// `migrate_default_root` and still renamed the user's data. A comment whose
/// premise the documented next edit voids is not a guard, so the premise is
/// gone and every variant is dispatched on its own (rev-lead round 1, B1).
/// `the_documented_revert_really_stops_the_migration` is the pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootPlan {
    /// `<data>/orrerix` already exists. Use it, and never touch `<data>/loomux`
    /// again — including when both exist, which is what a *reverted* build or a
    /// concurrently-running old instance leaves behind.
    UseNew,
    /// Only `<data>/loomux` exists: an existing install seeing the new name for
    /// the first time. Move it, then use the new name.
    Migrate,
    /// The move was refused by the OS (see [`migrate_default_root`]). Keep
    /// using the old root for this run — starting a blank profile because a
    /// rename failed would look exactly like "all my groups are gone".
    UseLegacy,
    /// Neither exists: a first-ever launch. There is nothing to migrate and
    /// nothing to fall back to.
    Fresh,
}

/// The migration decision, pure: the fs probes are the caller's, so every arm
/// is reachable in a test without a disk.
///
/// **This is the function a policy revert edits** — see [`RootPlan`].
pub fn plan_default_root(new_exists: bool, legacy_exists: bool) -> RootPlan {
    match (new_exists, legacy_exists) {
        (true, _) => RootPlan::UseNew,
        (false, true) => RootPlan::Migrate,
        (false, false) => RootPlan::Fresh,
    }
}

/// What a plan tells the caller to actually *do* — the dispatch, lifted out of
/// the filesystem code so all four variants are decided in one pure place and
/// pinned without a disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootAction {
    /// Use `<data>/orrerix` as it stands.
    UseNew,
    /// Use `<data>/loomux` as it stands. **No move, no rename** — this is both
    /// the refused-move fallback and, after a policy revert, the steady state.
    UseLegacy,
    /// Move `<data>/loomux` to `<data>/orrerix`, then use the new one. A refused
    /// move degrades to `UseLegacy` — see [`migrate_default_root`].
    MoveThenUseNew,
}

/// The dispatch. **Exactly one variant may move anything**, and that fact is
/// what makes the documented revert real: flipping `plan_default_root`'s
/// `(false, true)` arm to `UseLegacy` lands here on an arm that renames nothing.
pub fn root_action(plan: RootPlan) -> RootAction {
    // SCRATCH MUTATION (#1153 red evidence, round F): `UseLegacy` folded back in
    // with `Migrate` — exactly the shape review found, in which the documented
    // one-arm revert is inert because the edited arm still reaches
    // `migrate_default_root`. Nothing else is touched.
    match plan {
        RootPlan::UseNew | RootPlan::Fresh => RootAction::UseNew,
        RootPlan::UseLegacy | RootPlan::Migrate => RootAction::MoveThenUseNew,
    }
}

/// Cached because the answer must not change under a running process, and
/// because it costs filesystem probes.
///
/// `data_root()` is called on *every breadcrumb write* — a root that re-derived
/// itself per call would both stat the disk on a hot path and, far worse, be
/// free to flip mid-run the instant the migration landed, splitting one
/// session's logs, orchestration state and `running.lock` across two
/// directories. One resolution per process, taken at the first call.
static DEFAULT_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Resolve the platform-default root **and perform the one-time
/// `<data>/loomux` → `<data>/orrerix` move** if this is the first launch that
/// finds only the old one (#1153 phase 4). Call once, early, from a real app's
/// startup — `src-tauri/src/lib.rs` and `loomux-server`'s `main`.
///
/// The move is deliberately NOT lazy inside `data_root()`. A rename of the
/// user's live profile is not something a getter called on every breadcrumb
/// write should be able to trigger, and — the concrete reason — `data_root()`
/// is reachable from unit and integration tests, from the daemon's config
/// probing, and from any future tool linking the engine. Making the move an
/// explicit startup act means running the test suite on a developer's machine
/// can never rename the profile of the app they have open.
///
/// The contract is enforced rather than asked for: both this and the read-only
/// [`resolve_default_root`] initialize the same `OnceLock`, so whichever ran
/// first decides, exactly once. If something reads the root before startup gets
/// here, no move happens *this launch* and the old root keeps being used —
/// which is the same safe state as a refused rename, and it retries next
/// launch.
pub fn init_data_root() {
    init_data_root_from(brand::env_os("DATA_DIR").value);
}

/// [`init_data_root`] with the environment reading as a parameter. Returns
/// whether it settled (and possibly migrated) the **platform default** — false
/// means an explicit root is in force and this call did nothing at all, which
/// is the branch a test can assert without a disk.
fn init_data_root_from(env_override: Option<std::ffi::OsString>) -> bool {
    // An explicitly-named root is used exactly as given: no rename, no
    // migration, no probing of a sibling. Without this guard, a run pointed at
    // an isolated profile — an E2E run, a second dev instance — would still
    // rename the user's REAL `<data>/loomux` out from under the app they have
    // open, which is precisely the state #394's override exists to avoid
    // touching. The rule is read through `override_root`, the same function
    // `data_root_from` decides with, so "which root is in force" has one answer.
    if override_root(env_override).is_some() {
        return false;
    }
    let _ = DEFAULT_ROOT.get_or_init(|| {
        let (new, legacy) = default_root_pair();
        match root_action(plan_default_root(new.is_dir(), legacy.is_dir())) {
            RootAction::UseNew => new,
            RootAction::UseLegacy => legacy,
            RootAction::MoveThenUseNew => match migrate_default_root(&legacy, &new) {
                RootPlan::UseNew => new,
                _ => legacy,
            },
        }
    });
    true
}

/// The two platform-default roots, current and legacy, derived from one
/// expression so the decision and the paths it acts on cannot drift.
fn default_root_pair() -> (PathBuf, PathBuf) {
    let parent = platform_data_dir();
    (parent.join(brand::NAME), parent.join(brand::LEGACY_NAME))
}

/// The platform-default root, **read-only**: it chooses between the two names
/// and never moves anything. See [`init_data_root`] for who does the moving.
fn resolve_default_root() -> PathBuf {
    DEFAULT_ROOT
        .get_or_init(|| {
            let (new, legacy) = default_root_pair();
            match root_action(plan_default_root(new.is_dir(), legacy.is_dir())) {
                RootAction::UseNew => new,
                // No move has happened, so the data is still under the old name
                // — for a pending `MoveThenUseNew` exactly as for `UseLegacy`.
                RootAction::UseLegacy | RootAction::MoveThenUseNew => legacy,
            }
        })
        .clone()
}

/// Perform the one-time move, and leave a signpost behind.
///
/// **One `fs::rename` and nothing else.** A directory rename within one volume
/// is atomic: either the whole profile moved or none of it did, so there is no
/// half-migrated state to recover from and no copy loop to be interrupted
/// halfway through someone's orchestration history. If it fails — the old root
/// is open by a still-running older build (Windows refuses the rename outright,
/// which is the *good* outcome), a permission problem, a cross-device layout —
/// we simply keep using the old root. Nothing is deleted here, ever, on any
/// path.
///
/// The signpost is what makes the move reversible by a human rather than only
/// by us: after a successful rename we recreate the old directory containing a
/// single text file naming the new location and the one command that undoes
/// this. A user who goes looking for `<data>/loomux` — because a script points
/// at it, because they rolled back to an older build, or just because that is
/// where their data used to be — finds an explanation instead of an absence.
/// Its failure is ignored: a signpost we could not write is a worse experience,
/// not a failed migration.
///
/// **A signpost-only directory is not a profile, and is never re-migrated**
/// (rev-lead round 1). The signpost recreates `<data>/loomux`, so a user who
/// later deletes `<data>/orrerix` — a reset, a partial uninstall — leaves the
/// marker directory as the only one standing. Without this check the next
/// launch would read that as "an unmigrated install", rename the marker into
/// place, and write a *fresh* signpost, so `<data>/orrerix/MOVED-TO-ORRERIX.txt`
/// would tell the user their data had moved to the directory it was sitting in.
/// Harmless, but confusing at exactly the moment someone already is.
///
/// The answer there is `UseNew` and **not** `UseLegacy`: there is no profile to
/// fall back to — the marker directory holds no state — so the run starts clean
/// under the current name, and the stale signpost is left on disk untouched
/// like everything else this function declines to move.
fn migrate_default_root(legacy: &Path, new: &Path) -> RootPlan {
    if holds_only_the_signpost(legacy) {
        return RootPlan::UseNew;
    }
    if fs::rename(legacy, new).is_err() {
        eprintln!(
            "orrerix: could not move {} to {} — continuing to use the old location. \
             (Is an older version still running? It will be retried on the next launch.)",
            legacy.display(),
            new.display(),
        );
        return RootPlan::UseLegacy;
    }
    let _ = fs::create_dir_all(legacy);
    let _ = fs::write(
        legacy.join(MOVED_MARKER),
        format!(
            "loomux is now orrerix.\n\n\
             This app's data moved to:\n\n    {}\n\n\
             Nothing was deleted — the whole directory was renamed. To undo it, quit the app, \
             delete this directory, and rename the one above back to \"{}\".\n\n\
             To keep using a location of your own instead, set {}.\n",
            new.display(),
            brand::LEGACY_NAME,
            brand::env_names("DATA_DIR"),
        ),
    );
    RootPlan::UseNew
}

/// Name of the signpost file left in the old data root after a move. A `.txt`
/// so that double-clicking it on Windows opens something readable.
pub const MOVED_MARKER: &str = "MOVED-TO-ORRERIX.txt";

/// True when `dir` exists and contains nothing but [`MOVED_MARKER`] — i.e. it is
/// a signpost we left behind, not a profile.
///
/// Fails **closed toward migrating**: an unreadable directory answers `false`,
/// so an unexpected permission error degrades to the normal move attempt (which
/// then fails safely on its own) rather than silently declining to migrate a
/// real profile.
fn holds_only_the_signpost(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut saw_marker = false;
    for entry in entries.flatten() {
        if entry.file_name() == MOVED_MARKER {
            saw_marker = true;
        } else {
            return false; // anything else at all means there is state here
        }
    }
    saw_marker
}

/// `<user data dir>/orrerix` (or `$ORRERIX_DATA_DIR` / `$LOOMUX_DATA_DIR` if
/// set) — the root every persisted-state singleton (`orchestration/`, `logs/`,
/// `tabs.json`, …) lives under. A dev instance and a production install share
/// this root by default, which means they also share `running.lock` and every
/// other singleton in it (#394); the env override lets a dev/test run point at
/// its own tree instead — e.g. an isolated profile for E2E runs — without
/// touching the platform data dir at all.
///
/// On the platform default, an install that predates #1153 is migrated once
/// from `<user data dir>/loomux`; see `doc/design/rebrand-filesystem.md`.
pub fn data_root() -> PathBuf {
    data_root_from(brand::env_os("DATA_DIR").value)
}

/// `<data root>/logs`. Mirrors `OrchRegistry::default_root` so crash logs and
/// orchestration state live under the same root.
pub fn logs_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = LOG_DIR_OVERRIDE.lock().unwrap().clone() {
        return dir;
    }
    data_root().join("logs")
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
    // Build the whole line first and emit it with ONE `write_all`: `O_APPEND` is
    // atomic per write syscall, and a `writeln!` with several arguments emits one
    // write per fragment — which is precisely how the audit log ended up with
    // records spliced into each other (#240). Breadcrumbs are written from every
    // pane thread, so the same race lives here. (`write_all` loops on a short
    // write, so this is one syscall *in practice* — regular files don't
    // short-write at these sizes — rather than by contract; see `append_audit`.)
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
///
/// `app_version` is the version stamped into every crash log, and it is a
/// PARAMETER rather than an `env!("CARGO_PKG_VERSION")` down in `record_crash`
/// because of where this code now lives (#888 slice A3 batch 7). That macro
/// names the crate a file is *compiled in*, so once `obs` moved into the engine
/// it would have resolved to this crate's permanent `0.0.0` placeholder — see
/// the manifest — and every crash log would have quietly stopped naming the
/// loomux release that crashed, which is the field
/// `doc/design/crash-observability.md` promises a human reading one. Nothing
/// about that fails to compile, so the identity is injected at the single
/// startup entry point instead: `src-tauri/src/lib.rs` passes its own
/// `env!("CARGO_PKG_VERSION")`, where the macro means what it says.
pub fn install_panic_hook(app_version: &'static str) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Never let the hook itself unwind and mask the real panic.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write_crash_log(app_version, info)
        }));
        default(info);
    }));
}

fn write_crash_log(app_version: &str, info: &std::panic::PanicHookInfo<'_>) {
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
    record_crash(&logs_dir(), app_version, &tname, &msg, &loc, &bt);
    breadcrumb(
        "panic",
        &format!("thread={tname} at {}", loc.replace(' ', "_")),
    );
}

/// Write one crash log. Split from `write_crash_log` so the file format is
/// testable without a live `PanicHookInfo` (which can't be constructed) — and,
/// since the version arrives as an argument, so the `version:` line is testable
/// too rather than being whatever crate the test happens to compile in.
fn record_crash(
    dir: &Path,
    app_version: &str,
    thread: &str,
    msg: &str,
    loc: &str,
    backtrace: &str,
) {
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
        ver = app_version,
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

// The next-launch notice's *Tauri surface* — `StartupNotice` and the
// `take_startup_notice` command — is what stayed behind in `src-tauri/src/obs.rs`
// when this file crossed the boundary (#888 slice A3 batch 7). It is the only
// part of the original file that names `tauri::`, the file had already fenced it
// off behind its own section marker, and `StartupCheck::notice` above is the
// whole of what it holds. See that file's header.

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
    fn data_root_honors_env_override() {
        // An absolute path on whatever platform the suite runs on (this repo's
        // CI matrix runs backend tests on ubuntu/macos/windows even though the
        // shipped app is Windows-only) — `temp_dir()` is always absolute.
        let isolated = std::env::temp_dir().join("loomux-isolated-profile-test");
        let overridden = data_root_from(Some(isolated.clone().into_os_string()));
        assert_eq!(overridden, isolated);
    }

    /// The fallback root is whichever of the two names `resolve_default_root`
    /// settled on for this process — asserted as *one of the pair* rather than
    /// as `orrerix`, because a developer machine with a pre-#1153
    /// `<data>/loomux` and a locked-open profile legitimately resolves to the
    /// legacy name (`RootPlan::UseLegacy`), and a test that demanded the new
    /// name would be asserting the migration *succeeded* on the test runner's
    /// own real data dir. The migration policy itself is pinned by
    /// `plan_default_root` and `a_move_leaves_a_signpost_*` below, which need
    /// no real data dir at all.
    #[test]
    fn data_root_falls_back_to_platform_data_dir() {
        let default = data_root_from(None);
        let name = default.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name == brand::NAME || name == brand::LEGACY_NAME,
            "expected one of the two platform-default roots, got {name}"
        );
        assert_ne!(default, std::env::temp_dir().join("loomux-isolated-profile-test"));
    }

    #[test]
    fn data_root_rejects_empty_override() {
        let result = data_root_from(Some(std::ffi::OsString::from("")));
        assert_eq!(result, resolve_default_root(), "should fall back, not resolve to CWD");
    }

    #[test]
    fn data_root_rejects_relative_override() {
        let result = data_root_from(Some(std::ffi::OsString::from(r"relative\path")));
        assert_eq!(result, resolve_default_root(), "should fall back, not resolve to CWD");
    }

    // ---------- the loomux -> orrerix data-root migration (#1153 phase 4) ----------

    /// The migration must never fire for a run pointed at an explicit root.
    /// Without this guard an E2E run — or any second dev instance on an
    /// isolated profile — would rename the user's REAL `<data>/loomux` out from
    /// under the app they have open, which is the one directory #394's override
    /// exists to keep its hands off.
    #[test]
    fn an_explicit_root_is_never_migrated() {
        let explicit = std::env::temp_dir().join("orrerix-explicit-root");
        assert!(
            !init_data_root_from(Some(explicit.into_os_string())),
            "an absolute override must make init a no-op — it must not settle, \
             let alone migrate, the platform default"
        );
    }

    /// …and the two guards agree on WHICH values count as explicit. A value
    /// `data_root_from` rejects must not be one `init_data_root` treats as "an
    /// operator named a root", or the difference between the two rules is a
    /// silent window in which the platform default is used but never migrated.
    #[test]
    fn a_rejected_override_is_not_treated_as_an_explicit_root() {
        for bad in ["", r"relative\path"] {
            assert_eq!(override_root(Some(std::ffi::OsString::from(bad))), None, "{bad:?}");
        }
        let good = std::env::temp_dir().join("orrerix-explicit-root");
        assert_eq!(override_root(Some(good.clone().into_os_string())), Some(good));
        assert_eq!(override_root(None), None);
    }

    /// **The escape hatch, pinned.** The PR shipping this policy without the
    /// human's ratification rests on "reverting is one arm of
    /// `plan_default_root`". That claim is only true if the dispatch below it
    /// honours `UseLegacy` — and in the first cut it did not: `UseLegacy` was
    /// folded in with `Migrate`, so the documented edit still renamed the
    /// user's data (rev-lead round 1, B1).
    ///
    /// This simulates the revert at its own seam: the edited arm's return
    /// value, fed to the real dispatch. If it ever reads `MoveThenUseNew`
    /// again, the policy has become one nobody can undo by the documented
    /// procedure.
    #[test]
    fn the_documented_revert_really_stops_the_migration() {
        let what_the_reverted_arm_returns = RootPlan::UseLegacy;
        assert_eq!(
            root_action(what_the_reverted_arm_returns),
            RootAction::UseLegacy,
            "the one-arm revert must stop the move — if this is MoveThenUseNew the \
             documented escape hatch is inert and the shipped policy cannot be undone"
        );
    }

    /// Every variant dispatched on its own, and **exactly one of them moves**.
    /// The count is the assertion: it is what stops a future arm from being
    /// quietly folded into the migrating branch again.
    #[test]
    fn exactly_one_plan_variant_moves_anything() {
        use RootPlan::*;
        let all = [UseNew, Migrate, UseLegacy, Fresh];
        let movers: Vec<RootPlan> =
            all.iter().copied().filter(|p| root_action(*p) == RootAction::MoveThenUseNew).collect();
        assert_eq!(movers, vec![Migrate], "only Migrate may move; got {movers:?}");
        assert_eq!(root_action(UseNew), RootAction::UseNew);
        assert_eq!(root_action(Fresh), RootAction::UseNew, "a first-ever launch has nothing to move");
        assert_eq!(root_action(UseLegacy), RootAction::UseLegacy);
    }

    #[test]
    fn the_new_root_existing_means_the_old_one_is_never_touched() {
        // Both present is the state a *reverted* build, or an old instance that
        // kept writing during a move, leaves behind. The new one still wins.
        assert_eq!(plan_default_root(true, true), RootPlan::UseNew);
        assert_eq!(plan_default_root(true, false), RootPlan::UseNew);
    }

    #[test]
    fn only_the_old_root_existing_is_the_one_case_that_migrates() {
        assert_eq!(plan_default_root(false, true), RootPlan::Migrate);
    }

    #[test]
    fn a_first_ever_launch_migrates_nothing() {
        assert_eq!(plan_default_root(false, false), RootPlan::Fresh);
    }

    /// The move is a rename, so it must carry the whole profile across — not
    /// just the top-level entries a shallow copy would have managed.
    #[test]
    fn a_move_carries_the_whole_profile_and_leaves_a_signpost() {
        let tmp = std::env::temp_dir().join(format!("orrerix-migr-{}", now_ms()));
        let legacy = tmp.join(brand::LEGACY_NAME);
        let new = tmp.join(brand::NAME);
        fs::create_dir_all(legacy.join("orchestration").join("g-1")).unwrap();
        fs::write(legacy.join("orchestration").join("g-1").join("audit.jsonl"), "x").unwrap();
        fs::write(legacy.join("tabs.json"), "{}").unwrap();

        assert_eq!(migrate_default_root(&legacy, &new), RootPlan::UseNew);

        assert!(new.join("tabs.json").is_file(), "top-level state must move");
        assert_eq!(
            fs::read_to_string(new.join("orchestration").join("g-1").join("audit.jsonl")).unwrap(),
            "x",
            "nested group state must move, byte for byte"
        );
        // Nothing is deleted, and the old location explains itself.
        let marker = fs::read_to_string(legacy.join(MOVED_MARKER)).unwrap();
        assert!(marker.contains(&new.display().to_string()), "signpost must name the new root");
        assert!(
            marker.contains("ORRERIX_DATA_DIR"),
            "signpost must name the escape hatch, got: {marker}"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    /// A signpost-only legacy dir is our own leftover, not a profile, so it is
    /// never migrated — and the run starts clean under the CURRENT name rather
    /// than adopting the marker directory. Without this, deleting
    /// `<data>/orrerix` (a reset, a partial uninstall) would move the stale
    /// marker into place and write a fresh signpost pointing at the directory
    /// it now sits in (rev-lead round 1, non-blocking).
    #[test]
    fn a_signpost_only_legacy_dir_is_never_re_migrated() {
        let tmp = std::env::temp_dir().join(format!("orrerix-signpost-{}", now_ms()));
        let legacy = tmp.join(brand::LEGACY_NAME);
        let new = tmp.join(brand::NAME);
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(MOVED_MARKER), "moved").unwrap();

        assert_eq!(migrate_default_root(&legacy, &new), RootPlan::UseNew);
        assert!(legacy.join(MOVED_MARKER).is_file(), "the stale signpost is left alone");
        assert!(!new.exists(), "and nothing was moved into the new name");

        // …but one real file beside it makes it a profile again, and it moves.
        fs::write(legacy.join("tabs.json"), "{}").unwrap();
        assert_eq!(migrate_default_root(&legacy, &new), RootPlan::UseNew);
        assert!(new.join("tabs.json").is_file(), "a dir with real state must still migrate");
        let _ = fs::remove_dir_all(&tmp);
    }

    /// A refused rename must NOT start a blank profile — the run keeps using
    /// the old root. Provoked by making the destination an existing non-empty
    /// directory, which `rename` refuses on every platform this ships on.
    #[test]
    fn a_refused_move_keeps_using_the_old_root() {
        let tmp = std::env::temp_dir().join(format!("orrerix-migr-block-{}", now_ms()));
        let legacy = tmp.join(brand::LEGACY_NAME);
        let new = tmp.join(brand::NAME);
        fs::create_dir_all(legacy.join("orchestration")).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("occupied"), "in the way").unwrap();

        assert_eq!(migrate_default_root(&legacy, &new), RootPlan::UseLegacy);
        assert!(legacy.join("orchestration").is_dir(), "the old profile must be left intact");
        assert!(
            !legacy.join(MOVED_MARKER).exists(),
            "no signpost may claim a move that did not happen"
        );
        let _ = fs::remove_dir_all(&tmp);
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
        record_crash(
            tmp.path(),
            "9.9.9-test",
            "worker-3",
            "boom",
            "src/pty.rs:42:9",
            "0: frame\n1: frame",
        );
        let files: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| n.starts_with("crash-") && n.ends_with(".log"))
            .collect();
        assert_eq!(files.len(), 1, "exactly one crash log written");
        let body = fs::read_to_string(tmp.path().join(&files[0])).unwrap();
        // The `version:` line must be the version the CALLER supplied — the
        // loomux release, which is the field doc/design/crash-observability.md
        // promises a human reading a crash log. Pinned against a value no crate
        // in this workspace carries on purpose: an `env!("CARGO_PKG_VERSION")`
        // that crept back in would report this crate's `0.0.0` placeholder (or
        // the app's real version, if it crept back into `src-tauri`), and
        // either way this assertion is what notices. Nothing else would — that
        // regression compiles clean and reddens no other test.
        assert!(
            body.contains("version: 9.9.9-test"),
            "crash log must carry the version the host passed in, not the \
             version of whichever crate this code compiles in — got:\n{body}"
        );
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
            install_panic_hook("9.9.9-test");
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
            // The version the host handed `install_panic_hook` survives the
            // whole path — closure capture, `catch_unwind`, `write_crash_log`,
            // `record_crash` — and lands in the file. `records_crash_file_with
            // _context` pins the format; this pins that the hook is what
            // carries the value there.
            assert!(body.contains("version: 9.9.9-test"), "carries the host's app version");
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
