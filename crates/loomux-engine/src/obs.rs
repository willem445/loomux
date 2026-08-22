//! Crash observability (issue #53).
//!
//! Three cheap, dependency-free facilities so the *next* hard crash leaves
//! something to read:
//!
//! 1. A **panic hook** that appends a crash log (message + location + thread +
//!    backtrace) to `<data>/orrerix/logs/crash-<ts>.log`. It wraps — and still
//!    chains to — the default hook, and is written to never panic itself.
//!    It writes in **two phases** (#1219): the minimal record and the `panic`
//!    breadcrumb are composed without `core::fmt` and flushed *before* any
//!    backtrace work, so a death during symbolication — which aborts the
//!    process outright, with no unwind and no second hook call — still leaves
//!    the record it exists to leave. A thread-local latch diverts a re-entrant
//!    run to a single emergency line. See `write_crash_log_in`, `HookGuard`,
//!    and `doc/design/crash-observability.md`.
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
// `GlobalAlloc`/`Layout`/`System` are here for `CrashReportingAlloc` (#1219):
// the crash-reporting `#[global_allocator]` wrapper. `System` is std's own
// allocator — no new dependency, and nothing that could reach `getrandom`
// (CLAUDE.md constraint 2).
use std::alloc::{GlobalAlloc, Layout, System};
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
    match plan {
        RootPlan::UseNew | RootPlan::Fresh => RootAction::UseNew,
        RootPlan::UseLegacy => RootAction::UseLegacy,
        RootPlan::Migrate => RootAction::MoveThenUseNew,
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
    if let Some(dir) = LOG_DIR_OVERRIDE.lock_safe().clone() {
        return dir;
    }
    data_root().join("logs")
}

// ---------- timestamps ----------

/// Format unix-millis as `YYYYMMDD-HHMMSS` (UTC): filename-safe and lexically
/// sortable, so "newest crash log" is a plain string max. Pure — computed via
/// Howard Hinnant's days-from-civil algorithm so no date crate is pulled in
/// (and nothing that would drag in getrandom; see Cargo.toml).
///
/// A thin wrapper over [`push_stamp`], which is the one implementation of the
/// format: the crash path needs the same stamp without `core::fmt` (#1219),
/// and two implementations of a filename format is two chances to drift.
fn stamp(ms: u64) -> String {
    let mut v = Vec::with_capacity(16);
    push_stamp(&mut v, ms);
    // Unreachable by construction — `push_stamp` emits ASCII digits and one
    // `'-'`. Written as a fallback rather than an `unwrap` because callers
    // include a panic hook, where a second panic is the exact failure mode
    // this change exists to prevent.
    String::from_utf8(v).unwrap_or_default()
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

// ---------- allocation-light, `core::fmt`-free composition (#1219) ----------
//
// Everything the panic hook's FIRST phase emits — the crash filename, the
// minimal crash record, and the `panic` breadcrumb — is composed with these
// rather than with `format!`/`write!`. The distinction is narrow and
// deliberate: a `Vec` growth is a `malloc`, and an allocation failure aborts
// the process however you spell it, whereas `core::fmt` is a deep generic
// call tree that runs arbitrary `Display` impls and does its own capacity
// arithmetic. A panic raised down there, while a panic is already in flight,
// is a panic-while-panicking: std's `panic_count` returns `MustAbort::
// PanicInHook` and aborts on the spot, so the hook is never re-entered, no
// unwind starts, and `catch_unwind` never gets control. #1218 is that shape
// twice in two days — a `capacity overflow` under backtrace demangling, and
// zero bytes on disk. So the record that has to survive is built out of byte
// slices and hand-rolled integers.
//
// FMT-FREE REGION BEGIN (#1219) — `the_first_phase_composes_without_the_
// formatting_machinery` scans between this marker and its END for the tokens
// that would put `core::fmt` back on the path.

/// Where the composers below put their bytes. Two implementations, and the
/// difference between them is the entire reason this is a trait rather than a
/// `Vec<u8>` parameter:
///
/// - the **panic** hook may allocate (a panic is not an out-of-memory, and a
///   panic message or a source path has no useful upper bound), so it composes
///   into a `Vec<u8>`;
/// - the **allocation-failure** handler may not allocate *at all* — it runs
///   inside `GlobalAlloc::alloc` on the return path of a request that just
///   failed — so it composes into a [`FixedBuf`] on the stack.
///
/// One set of composers, one record format, two memory disciplines.
trait Sink {
    fn put(&mut self, bytes: &[u8]);
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// A stack-resident byte buffer that **truncates rather than growing**. The
/// allocation-failure path's sink: `N` is chosen to hold the whole record it
/// composes, and overflow is recorded rather than panicked on, because the one
/// thing this path may never do is fail loudly.
struct FixedBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
    overflow: bool,
}

impl<const N: usize> FixedBuf<N> {
    const fn new() -> Self {
        FixedBuf { buf: [0u8; N], len: 0, overflow: false }
    }

    fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl<const N: usize> Sink for FixedBuf<N> {
    fn put(&mut self, bytes: &[u8]) {
        let take = bytes.len().min(N - self.len);
        self.buf[self.len..self.len + take].copy_from_slice(&bytes[..take]);
        self.len += take;
        if take < bytes.len() {
            self.overflow = true;
        }
    }
}

/// Zero padding for [`push_dec`], as a slice rather than a push loop. Twenty
/// is `u64::MAX`'s digit count, so no caller can ask for more.
const ZEROS: &[u8; 20] = b"00000000000000000000";

/// Append `n` as decimal ASCII, left-padded with `'0'` to at least `width`.
/// `width` of 1 means "no padding" (the value's own digits).
fn push_dec<S: Sink>(out: &mut S, mut n: u64, width: usize) {
    let mut buf = [0u8; 20]; // u64::MAX is 20 digits
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let digits = buf.len() - i;
    if digits < width {
        out.put(&ZEROS[..(width - digits).min(ZEROS.len())]);
    }
    out.put(&buf[i..]);
}

/// Append `YYYYMMDD-HHMMSS` (UTC) for unix-millis `ms`. The single
/// implementation of the stamp format; [`stamp`] is its `String` wrapper.
///
/// `ms` is unsigned, so the day count is never negative and the year is never
/// before 1970 — the `max(0)` is a total-function guard, not a reachable case,
/// and a year past 9999 simply widens the field rather than truncating it.
fn push_stamp<S: Sink>(out: &mut S, ms: u64) {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    push_dec(out, y.max(0) as u64, 4);
    push_dec(out, m, 2);
    push_dec(out, d, 2);
    out.put(b"-");
    push_dec(out, sod / 3600, 2);
    push_dec(out, (sod % 3600) / 60, 2);
    push_dec(out, sod % 60, 2);
}

/// Append `s` with spaces and control bytes turned into `'_'`, so a breadcrumb
/// stays the single `stamp event detail` line its readers split on. Applied to
/// the thread name as well as the panic location: a `thread::Builder::name`
/// carrying a space would otherwise split the detail field in two, and a
/// payload with a newline in it would split the record into two lines.
fn push_spaceless<S: Sink>(out: &mut S, s: &str) {
    for &b in s.as_bytes() {
        out.put(&[if b == b' ' || b < 0x20 { b'_' } else { b }]);
    }
}

/// Append `s` with **control bytes only** turned into `'_'`, so it cannot break
/// out of the line it is on.
///
/// The weaker sibling of [`push_spaceless`], and the two are separate because
/// their contracts are: a *breadcrumb* is three space-separated fields, so a
/// space inside one of them is a format break; a *crash log* line has no such
/// contract, and mangling a panic message's spaces there loses fidelity for
/// nothing. Use this wherever the only requirement is "stay on one line".
fn push_oneline<S: Sink>(out: &mut S, s: &str) {
    for &b in s.as_bytes() {
        out.put(&[if b < 0x20 { b'_' } else { b }]);
    }
}

/// The crash record's head, up to and including the `panic:   ` label — shared
/// verbatim by the panic hook's first phase and by the allocation-failure
/// handler, so the file a human opens has one format regardless of which of the
/// two ways this process died. The caller writes the message itself (the two
/// have nothing in common there) and then calls [`push_crash_tail`].
fn push_crash_head<S: Sink>(out: &mut S, app_version: &str, now: u64, thread: &str) {
    out.put(b"loomux crash log\nversion: ");
    out.put(app_version.as_bytes());
    out.put(b"\ntime:    ");
    push_stamp(out, now);
    out.put(b" UTC (");
    push_dec(out, now, 1);
    out.put(b" ms since epoch)\nthread:  ");
    out.put(thread.as_bytes());
    out.put(b"\npanic:   ");
}

/// …and the tail from the location line on, closing the record with the blank
/// line that separates it from an appended `backtrace:` section.
fn push_crash_tail<S: Sink>(out: &mut S, loc: &str) {
    out.put(b"\nat:      ");
    out.put(loc.as_bytes());
    out.put(b"\n\n");
}

// FMT-FREE REGION END (#1219)

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
    breadcrumb_bytes_in(dir, event, detail.as_bytes())
}

// FMT-FREE REGION BEGIN (#1219) — the panic breadcrumb is written from inside
// the crash hook's first phase, so its line composition is under the same
// no-`core::fmt` rule as the crash record; see the region note above.

/// The breadcrumb write proper. Takes the detail as bytes so the crash path
/// can hand it a buffer it composed itself without a `String` round trip.
fn breadcrumb_bytes_in(dir: &Path, event: &str, detail: &[u8]) {
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
    let mut line = Vec::with_capacity(24 + event.len() + detail.len());
    push_stamp(&mut line, now_ms());
    line.put(b" ");
    line.put(event.as_bytes());
    line.put(b" ");
    line.put(detail);
    line.put(b"\n");
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("breadcrumbs.log"))
    {
        let _ = f.write_all(&line);
        // A no-op today — `fs::File` is unbuffered, so the `write_all` above
        // already *is* the syscall. It is spelled out because the crash path
        // now depends on the last pre-death breadcrumb being durable (#1219):
        // wrapping this handle in a `BufWriter` later would otherwise silently
        // move the `panic` line into a buffer that dies with the process.
        let _ = f.flush();
    }
}

// FMT-FREE REGION END (#1219)

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
/// PARAMETER rather than an `env!("CARGO_PKG_VERSION")` down in the crash
/// writer because of where this code now lives (#888 slice A3 batch 7). That macro
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
        // Armed for the whole of this run, `default(info)` included, and
        // disarmed by its own `Drop`. See `ReentryGuard`.
        let guard = ReentryGuard::enter(&IN_HOOK);
        if guard.is_first() {
            // Never let the hook itself unwind and mask the real panic. On
            // today's std this is belt-and-braces rather than the load-bearing
            // guard it reads as — a panic in here aborts before any unwind
            // begins (see the fmt-free region note) — but it costs nothing and
            // it is what keeps the hook honest if that ever changes.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                write_crash_log(app_version, info)
            }));
            // Hygiene rather than correctness: `HOOK_TARGET` is only ever read
            // from the re-entrant branch below, which only runs while a hook
            // run is in progress, and phase one overwrites it every time. It
            // is cleared here so a later reader cannot find a path from a run
            // that has finished.
            HOOK_TARGET.with(|c| *c.borrow_mut() = None);
        } else {
            // Re-entry: a second panic reached the hook while this thread's
            // first run was still in progress. Do the least possible — one
            // short append to the path phase one already derived, no
            // backtrace, no directory work, no formatting — and then chain, so
            // the process dies exactly the way std would have made it die.
            let loc = panic_location(info);
            HOOK_TARGET.with(|c| {
                if let Some(p) = c.borrow().as_deref() {
                    write_emergency(p, panic_message(info), &loc);
                }
            });
        }
        default(info);
    }));
}

thread_local! {
    /// Armed for the whole of this thread's crash-hook run (#1219).
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The crash-log path phase one derived, so a re-entrant run has an
    /// **already-derivable** target for its one emergency line: re-deriving it
    /// would mean `logs_dir()` — an env read, the data-root `OnceLock`, and a
    /// `PathBuf` join — in a process that has now panicked twice.
    static HOOK_TARGET: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// RAII arm/disarm for a thread-local re-entry latch (#1219). `enter()` always
/// yields a guard; `is_first()` says whether this is the OUTERMOST run on this
/// thread — i.e. whether the full recorder is safe to attempt, or whether this
/// is a death-while-recording that gets the minimal fallback instead.
///
/// One type, two latches (`IN_HOOK`, `IN_ALLOC_FAILURE`), because the policy —
/// arm on entry, divert on re-entry, and let *only* the outermost run disarm —
/// is the same in both places and must not acquire a second implementation
/// that can drift from the first.
///
/// **What a latch here can and cannot catch, stated plainly.** For the panic
/// hook, the common double panic never reaches it at all: `panic_count::
/// increase` sees its own `in_panic_hook` flag, returns
/// `MustAbort::PanicInHook`, prints "thread panicked while processing panic"
/// and aborts *before* the hook is called a second time. So the load-bearing
/// defence against #1218's panic-shape is the write-first ordering below, not
/// this latch, and the latch covers a hook reached outside that path. On the
/// **allocation** side it is stronger than that: nothing in std stops
/// `GlobalAlloc::alloc` being re-entered, so this is what keeps a failure
/// inside the reporter from recursing into an allocator that has just refused.
struct ReentryGuard {
    latch: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
    first: bool,
}

impl ReentryGuard {
    fn enter(latch: &'static std::thread::LocalKey<std::cell::Cell<bool>>) -> Self {
        let already = latch.with(|c| c.replace(true));
        ReentryGuard { latch, first: !already }
    }

    fn is_first(&self) -> bool {
        self.first
    }
}

impl Drop for ReentryGuard {
    fn drop(&mut self) {
        // Only the outermost run disarms. An inner guard clearing the latch
        // would hand a *third* death the full path again, which is the one
        // thing this exists to stop.
        if self.first {
            self.latch.with(|c| c.set(false));
        }
    }
}

// FMT-FREE REGION BEGIN (#1219) — the crash hook's first phase and its
// double-panic fallback; see the region note above `push_dec`.

/// The panic payload as a borrowed `&str` — no allocation, no `Display`
/// dispatch, covering the two payload types `panic!` produces.
fn panic_message<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or("<non-string panic payload>")
}

/// `file:line:col` for the panic site, composed byte-wise.
fn panic_location(info: &std::panic::PanicHookInfo<'_>) -> String {
    match info.location() {
        Some(l) => {
            let mut v = Vec::with_capacity(l.file().len() + 16);
            v.extend_from_slice(l.file().as_bytes());
            v.push(b':');
            push_dec(&mut v, l.line() as u64, 1);
            v.push(b':');
            push_dec(&mut v, l.column() as u64, 1);
            // ASCII plus a `&str`'s own bytes — the fallback is unreachable,
            // and is a fallback rather than an `unwrap` for the reason given
            // on `stamp`.
            String::from_utf8(v).unwrap_or_default()
        }
        None => String::from("<unknown location>"),
    }
}

/// The single line a re-entrant hook run appends: short enough to be one
/// `write`, and composed from byte slices so nothing on this path can panic a
/// third time.
///
/// `push_oneline`, not `push_spaceless` — this lands in a crash log, where the
/// panic message's own spaces are content, not a field separator. Only bytes
/// that would end the line early are replaced.
fn emergency_line(msg: &str, loc: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(32 + msg.len() + loc.len());
    v.extend_from_slice(b"\ndouble-panic: ");
    push_oneline(&mut v, msg);
    v.extend_from_slice(b" at ");
    push_oneline(&mut v, loc);
    v.push(b'\n');
    v
}

/// Append one emergency line to `path`, creating it if the outer run never got
/// as far as opening it. Append, never truncate: the point is to add to
/// whatever phase one already flushed, not to replace it.
fn write_emergency(path: &Path, msg: &str, loc: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(&emergency_line(msg, loc));
        let _ = f.flush();
    }
}

/// **Phase one of the crash write, and the whole point of #1219.** Open the
/// crash log and flush the minimal record — version, time, thread, panic
/// message, location — BEFORE anything fragile runs, and hand the open handle
/// back so phase two can append through it.
///
/// The previous version captured and symbolicated a backtrace *first* and
/// wrote afterwards, so a panic during that capture left nothing at all on
/// disk: at panic-count 2 std aborts immediately, with no unwind for the
/// hook's `catch_unwind` to catch and no second hook call. #1218 is that
/// happening twice in two days — demangle frames live at abort time, zero
/// bytes written.
///
/// `None` means the file could not be opened or the header could not be
/// written; the caller then skips phase two rather than writing a backtrace
/// with no record above it.
fn record_crash_first_phase(
    dir: &Path,
    app_version: &str,
    thread: &str,
    msg: &str,
    loc: &str,
) -> Option<fs::File> {
    let _ = fs::create_dir_all(dir);
    let now = now_ms();
    let mut name = Vec::with_capacity(32);
    name.extend_from_slice(b"crash-");
    push_stamp(&mut name, now);
    name.extend_from_slice(b".log");
    let path = dir.join(String::from_utf8(name).unwrap_or_default());
    // Remember the target BEFORE opening, so a re-entrant run has somewhere to
    // put its one line even when the open itself is what died.
    HOOK_TARGET.with(|c| *c.borrow_mut() = Some(path.clone()));

    let mut head =
        Vec::with_capacity(96 + app_version.len() + thread.len() + msg.len() + loc.len());
    push_crash_head(&mut head, app_version, now, thread);
    head.put(msg.as_bytes());
    push_crash_tail(&mut head, loc);

    // Append rather than truncate: two threads panicking into the same-second
    // filename both leave a record instead of one clobbering the other.
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    f.write_all(&head).ok()?;
    // Durable before phase two starts. `fs::File` is unbuffered, so the
    // `write_all` already is the syscall; the call is spelled out so the "phase
    // one is on disk" contract cannot be quietly broken by a `BufWriter` later.
    let _ = f.flush();
    Some(f)
}

/// Phase two: append the backtrace through the handle phase one opened. A
/// separate `write_all` on purpose — if this never runs because the process
/// died taking the backtrace, the file still holds a complete, readable
/// record instead of nothing.
fn append_backtrace(f: &mut fs::File, backtrace: &str) {
    let mut tail = Vec::with_capacity(16 + backtrace.len());
    tail.extend_from_slice(b"backtrace:\n");
    tail.extend_from_slice(backtrace.as_bytes());
    tail.push(b'\n');
    let _ = f.write_all(&tail);
    let _ = f.flush();
}

/// The two-phase crash write. `logs_dir()` and the backtrace source are
/// injected so the ORDER is testable: the capture closure runs only once the
/// minimal record and the `panic` breadcrumb are on disk, and a test can
/// assert exactly that by reading the log directory from inside it.
fn write_crash_log_in(
    dir: &Path,
    app_version: &str,
    thread: &str,
    msg: &str,
    loc: &str,
    capture_backtrace: impl FnOnce() -> String,
) {
    // ---- phase one: the record that has to survive ----
    let opened = record_crash_first_phase(dir, app_version, thread, msg, loc);
    // The `panic` breadcrumb belongs in this phase too (#1219). It used to be
    // written *after* the backtrace, so every crash that died capturing one
    // lost the breadcrumb along with the crash log — and the breadcrumb tail is
    // what the design note promises still survives an abort.
    let mut detail = Vec::with_capacity(16 + thread.len() + loc.len());
    detail.extend_from_slice(b"thread=");
    push_spaceless(&mut detail, thread);
    detail.extend_from_slice(b" at ");
    push_spaceless(&mut detail, loc);
    breadcrumb_bytes_in(dir, "panic", &detail);

    // ---- phase two: everything that can die trying ----
    let bt = capture_backtrace();
    if let Some(mut f) = opened {
        append_backtrace(&mut f, &bt);
    }
}

// FMT-FREE REGION END (#1219)

fn write_crash_log(app_version: &str, info: &std::panic::PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let tname = thread.name().unwrap_or("<unnamed>");
    let loc = panic_location(info);
    write_crash_log_in(
        &logs_dir(),
        app_version,
        tname,
        panic_message(info),
        &loc,
        // force_capture ignores RUST_BACKTRACE, so a crash log always carries a
        // backtrace when the process survives long enough to take one. Frame
        // *symbols* depend on the release profile keeping a symbol table (see
        // the design note); the addresses are always useful. This is PHASE TWO
        // for a reason: symbolication runs dbghelp and rustc-demangle and
        // allocates without bound, and a panic in there kills the process on
        // the spot (#1218).
        || std::backtrace::Backtrace::force_capture().to_string(),
    );
}

// ---------- allocation-failure reporting (#1219, after #1218 round 3) ----------
//
// The panic hook above cannot see this class at all, and that is not a bug in
// it. When a global allocator returns null, `RawVec` calls
// `handle_alloc_error`, which calls `abort()` — it never enters
// `std::panicking`, so no hook of any kind runs. #1218's three production
// crashes were all exactly this: a `Vec<u8>` grow of 64 MiB and then 128 MiB
// refused on a machine pinned at its commit limit, `handle_alloc_error` →
// `abort` → `__fastfail(7)` → `0xc0000409`. A write-first panic hook would
// still have produced nothing for any of them.
//
// `std::alloc::set_alloc_error_hook` is the matching seam and is nightly-only.
// The stable one is this: a `#[global_allocator]` that delegates every call to
// `System` and, on a null return, writes the record BEFORE handing the null
// back to `handle_alloc_error`. The host installs it — see
// `src-tauri/src/lib.rs` — because a `#[global_allocator]` may be declared only
// once per artifact and that is the artifact's decision, not the engine's.
//
// **The discipline this path is written under.** It runs inside
// `GlobalAlloc::alloc`, on the return path of a request that has just failed,
// so it may not allocate — an allocation here would re-enter the allocator in
// the one state where it cannot serve, and a large enough failure would recurse.
// Hence: the file is opened ONCE at startup and the handle kept
// (`ALLOC_LOG`); the record is composed into a stack `FixedBuf` through the same
// `Sink` composers the panic hook's first phase uses; and a thread-local latch
// makes a nested failure a no-op rather than a recursion. `write_all` and
// `flush` on an already-open `fs::File` allocate nothing.

/// Size of the stack buffer the allocation-failure record is composed into.
/// The record is bounded — a version string, two timestamps, two integers and
/// fixed labels — so this is generous rather than tight, and [`FixedBuf`]
/// truncates rather than growing if that estimate is ever wrong.
const ALLOC_RECORD_CAP: usize = 512;

/// The crash log the allocation-failure handler appends to, opened once at
/// startup so the failure path never has to.
///
/// It is a **fixed name** (`crash-alloc.log`) rather than a per-run timestamped
/// one because the name has to be derivable before the crash, and appending
/// keeps every occurrence. It matches `crash-*.log`, so
/// [`newest_crash_log_since`] names it in the next-launch toast — which is only
/// correct because that function ignores zero-length files: this one exists,
/// empty, for the whole of every healthy run.
static ALLOC_LOG: std::sync::OnceLock<fs::File> = std::sync::OnceLock::new();

/// The app version to stamp into that record, captured at install time.
static ALLOC_VERSION: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

thread_local! {
    /// Re-entry latch for the allocation-failure handler. Distinct from
    /// `IN_HOOK`: a panic and an allocation failure are different deaths and a
    /// process can, in principle, be in both.
    static IN_ALLOC_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arm allocation-failure reporting: open `<logs>/crash-alloc.log` and remember
/// the version to stamp. Call once at startup, **after** [`init_data_root`]
/// (so the handle is on the settled root) and after [`check_and_arm`] (so the
/// empty file this creates cannot be mistaken for the previous run's crash log
/// — see the zero-length rule on [`newest_crash_log_since`]).
///
/// Allocation failures before this runs are not recorded: there is no derived
/// path to write to, and deriving one is exactly what the failure path may not
/// do. Startup allocations are small and precede any of the large, unbounded
/// ones this exists to catch.
pub fn install_alloc_error_reporting(app_version: &'static str) {
    install_alloc_error_reporting_in(&logs_dir(), app_version);
}

fn install_alloc_error_reporting_in(dir: &Path, app_version: &'static str) -> bool {
    let _ = fs::create_dir_all(dir);
    let Ok(f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("crash-alloc.log"))
    else {
        return false;
    };
    let _ = ALLOC_VERSION.set(app_version);
    ALLOC_LOG.set(f).is_ok()
}

// FMT-FREE REGION BEGIN (#1219) — the allocation-failure path. Everything here
// runs inside `GlobalAlloc::alloc` after a failed request; see the section note
// above for why it may not allocate.

/// Compose the allocation-failure record. Shares `push_crash_head` /
/// `push_crash_tail` with the panic hook's first phase, so the file reads the
/// same either way, and differs only where the two deaths genuinely differ: the
/// `panic:` line carries the refused `Layout` instead of a message.
///
/// The **thread name is deliberately not read.** `std::thread::current()`
/// materialises the `Thread` for any thread std did not start, which allocates
/// — on the one path that must not. The faulting thread is in the WER dump
/// (#1218 read it straight off both), and the refused size is the field that
/// actually localises the site.
fn alloc_failure_record(app_version: &str, now: u64, size: usize, align: usize) -> FixedBuf<ALLOC_RECORD_CAP> {
    let mut b = FixedBuf::<ALLOC_RECORD_CAP>::new();
    push_crash_head(&mut b, app_version, now, "<not read: see alloc_failure_record>");
    b.put(b"allocation of ");
    push_dec(&mut b, size as u64, 1);
    b.put(b" bytes (align ");
    push_dec(&mut b, align as u64, 1);
    b.put(b") was refused by the system allocator");
    push_crash_tail(&mut b, "<global allocator> (handle_alloc_error aborts next)");
    b
}

/// Write one allocation-failure record through an already-open handle. One
/// `write_all` of one composed buffer, for the same `O_APPEND`-atomicity reason
/// the breadcrumb writer gives.
fn write_alloc_failure(f: &fs::File, app_version: &str, now: u64, size: usize, align: usize) {
    let record = alloc_failure_record(app_version, now, size, align);
    let mut w = f;
    let _ = w.write_all(record.as_slice());
    let _ = w.flush();
}

/// The handler proper: everything the `GlobalAlloc` impl does on a null return.
/// Best-effort at every step, and a no-op when reporting was never armed or
/// when this thread is already inside it.
fn on_alloc_failure(size: usize, align: usize) {
    let guard = ReentryGuard::enter(&IN_ALLOC_FAILURE);
    if !guard.is_first() {
        return; // already reporting on this thread — do not recurse
    }
    if let (Some(f), Some(v)) = (ALLOC_LOG.get(), ALLOC_VERSION.get()) {
        write_alloc_failure(f, v, now_ms(), size, align);
    }
}

/// A `#[global_allocator]` that delegates everything to [`System`] and reports
/// the one thing `System` cannot tell anybody about: a refused request.
///
/// **Success path.** `alloc`/`alloc_zeroed`/`realloc` add one null test on a
/// pointer already in a register and nothing else; `dealloc` is a bare
/// delegation with no added instruction. Nothing is counted, logged or locked
/// per allocation — this is not an allocation profiler and must never become
/// one, because it sits under every `Vec` push in the process.
///
/// **Failure path.** [`on_alloc_failure`], which allocates nothing (section
/// note above), and then the null is returned unchanged so `handle_alloc_error`
/// aborts exactly as it would have. This adds a record; it does not change what
/// the process does.
///
/// No new dependency: `System` is `std` (CLAUDE.md constraint 2 — nothing here
/// can reach `getrandom`).
pub struct CrashReportingAlloc;

unsafe impl GlobalAlloc for CrashReportingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if p.is_null() {
            on_alloc_failure(layout.size(), layout.align());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if p.is_null() {
            on_alloc_failure(layout.size(), layout.align());
        }
        p
    }

    unsafe fn realloc(
        &self,
        ptr: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if p.is_null() {
            // The REQUESTED size, not the old one: `new_size` with the old
            // alignment is the `Layout` `handle_alloc_error` is about to abort
            // on, and #1218's whole diagnosis turned on reading that number.
            on_alloc_failure(new_size, layout.align());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

// FMT-FREE REGION END (#1219)

// ---------- unclean-exit detection ----------

/// Outcome of the startup check: whether the previous run ended uncleanly and
/// the newest crash log (if any) to point the user at.
pub struct StartupCheck {
    pub unclean: bool,
    pub crash_log: Option<PathBuf>,
}

/// Breadcrumb event naming the "unclean previous exit, and yet no crash log
/// from it" contradiction (#1219).
const CRASH_LOG_GAP_EVENT: &str = "crash-log-gap";

/// …and its detail. One line, no free text: the **named likely class** first,
/// then the records that DO exist when ours does not.
///
/// The class is named because #1218 answered it. All three of its production
/// crashes were a refused heap allocation — `handle_alloc_error` → `abort()` →
/// `__fastfail(7)` → `0xc0000409` — which never enters `std::panicking`, so no
/// panic hook of any shape would have written a thing. That is what
/// `install_alloc_error_reporting` now covers, and a gap therefore means
/// *neither* recorder finished: the alloc wrapper was not yet armed, or the
/// death was one of the classes nothing in-process can catch (a stack overflow,
/// an FFI access violation, an external kill).
///
/// On Windows — the platform this app ships on — such a death is recorded by
/// Windows Error Reporting rather than by us: `%LOCALAPPDATA%\CrashDumps` holds
/// a dump *if local dump collection has been enabled* (it is off by default),
/// and the Application event log holds the "Application Error" entry with the
/// exception code either way. See `doc/design/crash-observability.md`.
const CRASH_LOG_GAP_DETAIL: &str = "unclean_prev=true crash_log=none \
     likely=heap_alloc_abort(handle_alloc_error->abort,0xc0000409_param7) \
     or=stack_overflow|FFI_access_violation|external_kill \
     look=%LOCALAPPDATA%\\CrashDumps \
     and=EventViewer>WindowsLogs>Application(source:Application_Error)";

/// Is this startup the #1218 signature — the sentinel says the previous run did
/// not exit cleanly, and yet no `crash-*.log` newer than that run's start
/// exists to say why?
///
/// Both inputs are read off the SAME [`StartupCheck`]: `unclean` and the crash
/// log `newest_crash_log_since` found under that same sentinel's mtime. There
/// is deliberately no second source for either — a gap detector that took
/// "unclean" from the sentinel and "is there a log" from a fresh directory
/// scan would disagree with itself exactly in the window it exists to name.
pub fn is_crash_log_gap(unclean: bool, crash_log: Option<&Path>) -> bool {
    unclean && crash_log.is_none()
}

impl StartupCheck {
    /// True when this startup has a contradiction to name: see
    /// [`is_crash_log_gap`].
    pub fn crash_log_gap(&self) -> bool {
        is_crash_log_gap(self.unclean, self.crash_log.as_deref())
    }

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
                     (a hard abort, not an unwinding panic); see breadcrumbs.log \
                     and %LOCALAPPDATA%\\CrashDumps"
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
    let check = StartupCheck { unclean, crash_log };
    // Name the gap in the breadcrumb log (#1219). A hard abort, a nounwind/FFI
    // fault, or a death inside the hook itself all present identically at the
    // next launch — leftover sentinel, nothing to read — and until now that
    // combination was reported to the *user* as a toast and to nobody at all in
    // the durable record. It goes in before this run's own `startup`
    // breadcrumb because it is a statement about the PREVIOUS run.
    if check.crash_log_gap() {
        breadcrumb_in(dir, CRASH_LOG_GAP_EVENT, CRASH_LOG_GAP_DETAIL);
    }
    check
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
/// mtime is at or after `since` **and which has something in it**. The mtime
/// gate is what keeps a hard abort (no crash log written) from mis-attributing
/// an older log to this crash; pass `since = None` to disable it (best effort
/// when the sentinel mtime is unreadable). `None` when nothing qualifies.
///
/// **The zero-length rule (#1219).** An empty crash log is not a crash record,
/// and naming one in the next-launch toast is a lie that also suppresses the
/// `crash-log-gap` breadcrumb — the one thing that says a death went
/// unrecorded. It became load-bearing rather than merely tidy when
/// `install_alloc_error_reporting` started opening `crash-alloc.log` at every
/// startup: that file exists, empty, for the whole of a healthy run, and
/// without this filter every unclean exit would point the user at it.
fn newest_crash_log_since(dir: &Path, since: Option<SystemTime>) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_crash_log(p))
        .filter(|p| {
            let Ok(meta) = fs::metadata(p) else { return false };
            if meta.len() == 0 {
                return false;
            }
            match since {
                Some(t) => meta.modified().map(|m| m >= t).unwrap_or(false),
                None => true,
            }
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
    ///
    /// **Always locked with `lock_safe`, never `.lock().unwrap()`.** A test that
    /// fails while holding this poisons it, and every later `.unwrap()` on it
    /// then panics with `PoisonError` — so ONE genuine failure is reported as
    /// three, two of them in tests that were never run against the change. That
    /// is the mutex-poison cascade `LockExt` exists to stop (see its doc), and
    /// a harness that reproduced it in miniature was making its own evidence
    /// unattributable: a red-before-green round is only usable if the tests it
    /// reddens are the tests the mutation actually broke.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Restores the log-dir override on the way out **however the scope ends**.
    /// Without this, a test that fails inside `with_log_dir` leaves the
    /// override pointing at its own deleted temp dir, and the next test to read
    /// `logs_dir()` fails for a reason that has nothing to do with it.
    struct LogDirOverride;

    impl Drop for LogDirOverride {
        fn drop(&mut self) {
            *LOG_DIR_OVERRIDE.lock_safe() = None;
        }
    }

    fn with_log_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        *LOG_DIR_OVERRIDE.lock_safe() = Some(dir.to_path_buf());
        let _restore = LogDirOverride;
        f()
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

    /// **The escape hatch, pinned.** The PR shipped this policy ahead of the
    /// human's answer on the strength of "reverting is one arm of
    /// `plan_default_root`" (since ratified — #1153 q-4 — which retires the
    /// urgency, not the guarantee: a ratified policy is still one somebody may
    /// need to undo). That claim is only true if the dispatch below it
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

    /// The one `crash-*.log` in `dir`, or `None` when none exists yet. Used to
    /// read the file *mid-write* as well as after, so it must not assume one
    /// is there.
    fn only_crash_log(dir: &Path) -> Option<String> {
        let mut found: Vec<PathBuf> = fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_crash_log(p))
            .collect();
        assert!(found.len() <= 1, "expected at most one crash log, got {found:?}");
        fs::read_to_string(found.pop()?).ok()
    }

    /// The crash-log FORMAT, driven through the two shipped phases rather than
    /// through a test-only one-shot wrapper — `record_crash` was exactly such a
    /// wrapper and #1219 removed it, because a format pinned on a path the app
    /// does not take is a format free to drift.
    #[test]
    fn records_crash_file_with_context() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = record_crash_first_phase(
            tmp.path(),
            "9.9.9-test",
            "worker-3",
            "boom",
            "src/pty.rs:42:9",
        )
        .expect("phase one must open and write the record");
        append_backtrace(&mut f, "0: frame\n1: frame");
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
        // The two phases must JOIN into the one format a human reads — the
        // split is an ordering change, not a format change, and the only place
        // that is observable is the seam between them.
        assert!(
            body.starts_with("loomux crash log\nversion: 9.9.9-test\ntime:    "),
            "phase one owns the head, unchanged — got:\n{body}"
        );
        assert!(
            body.ends_with(
                "\nthread:  worker-3\npanic:   boom\nat:      src/pty.rs:42:9\n\n\
                 backtrace:\n0: frame\n1: frame\n"
            ),
            "the seam between the phases must leave the historical bytes — got:\n{body}"
        );
    }

    // ---------- #1219: write-first, double-panic, startup gap ----------

    /// **The write-first ordering, pinned at its own seam.** The backtrace
    /// source is injected, so this reads the log directory from INSIDE the
    /// capture — the exact instant the old code spent in dbghelp
    /// symbolication and rustc-demangle, and the instant #1218's process died
    /// at. Whatever is on disk here is what such a death leaves behind.
    #[test]
    fn the_minimal_record_and_the_breadcrumb_land_before_the_backtrace_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let at_capture: std::cell::RefCell<Option<(String, String)>> =
            std::cell::RefCell::new(None);
        write_crash_log_in(
            tmp.path(),
            "9.9.9-test",
            "worker-3",
            "boom",
            "src/pty.rs:42:9",
            || {
                *at_capture.borrow_mut() = Some((
                    only_crash_log(tmp.path()).unwrap_or_default(),
                    fs::read_to_string(tmp.path().join("breadcrumbs.log")).unwrap_or_default(),
                ));
                "0: frame\n1: frame".to_string()
            },
        );
        let (log, crumbs) = at_capture
            .borrow()
            .clone()
            .expect("the injected backtrace capture must actually have been called");

        assert!(log.contains("version: 9.9.9-test"), "no record on disk at capture time:\n{log}");
        assert!(log.contains("thread:  worker-3"), "…and it must name the thread:\n{log}");
        assert!(log.contains("panic:   boom"), "…and the message:\n{log}");
        assert!(log.contains("at:      src/pty.rs:42:9"), "…and the location:\n{log}");
        assert!(
            log.ends_with('\n'),
            "phase one must leave a COMPLETE record, not a truncated line:\n{log}"
        );
        assert!(
            !log.contains("backtrace"),
            "phase one must not have waited on the backtrace — it is the step that kills \
             the process:\n{log}"
        );
        assert!(
            crumbs.contains("panic thread=worker-3 at src/pty.rs:42:9"),
            "the panic breadcrumb must land in the FIRST flush, not after the capture — \
             got {crumbs:?}"
        );

        // …and phase two still appends, so the finished file is the one format.
        let full = only_crash_log(tmp.path()).expect("a crash log must exist");
        assert!(full.starts_with(&log), "phase two must APPEND, never rewrite phase one");
        assert!(full.ends_with("backtrace:\n0: frame\n1: frame\n"), "got:\n{full}");
    }

    /// **The double-panic latch.** Four distinct ways to get this wrong, each
    /// with its own assertion: never arming it, disarming it from an inner
    /// run, making it process-wide, and never clearing it.
    #[test]
    fn the_hook_reentry_latch_is_thread_local_and_only_the_outer_run_disarms_it() {
        let outer = ReentryGuard::enter(&IN_HOOK);
        assert!(outer.is_first(), "the first hook run on a thread takes the full two-phase path");
        {
            let nested = ReentryGuard::enter(&IN_HOOK);
            assert!(
                !nested.is_first(),
                "a panic reaching the hook while this thread is already in it must be \
                 diverted to the emergency path, never handed the backtrace path again"
            );
        }
        let third = ReentryGuard::enter(&IN_HOOK);
        assert!(
            !third.is_first(),
            "an inner guard's drop must not disarm the OUTER run — a third panic would \
             otherwise get the full capture path back"
        );
        drop(third);
        let elsewhere =
            std::thread::spawn(|| ReentryGuard::enter(&IN_HOOK).is_first()).join().unwrap();
        assert!(
            elsewhere,
            "the latch must be per-thread: a concurrent panic on another thread is not a \
             double panic and still deserves a full crash log"
        );
        drop(outer);
        assert!(
            ReentryGuard::enter(&IN_HOOK).is_first(),
            "the latch must clear when the outer run ends"
        );
    }

    /// The **same** guard on the allocation latch, because one implementation
    /// serving two latches is only safe if both are exercised: a guard that
    /// hard-coded `IN_HOOK` would pass the test above and leave the allocation
    /// path with no latch at all.
    #[test]
    fn the_alloc_reentry_latch_is_the_same_guard_on_a_different_latch() {
        let outer = ReentryGuard::enter(&IN_ALLOC_FAILURE);
        assert!(outer.is_first());
        assert!(
            !ReentryGuard::enter(&IN_ALLOC_FAILURE).is_first(),
            "a failure inside the reporter must not recurse into an allocator that just \
             refused"
        );
        assert!(
            ReentryGuard::enter(&IN_HOOK).is_first(),
            "…and arming the allocation latch must not arm the PANIC latch — they are \
             different deaths, and a process can be in both"
        );
        drop(outer);
        assert!(ReentryGuard::enter(&IN_ALLOC_FAILURE).is_first(), "clears with the outer run");
    }

    /// The emergency write ADDS to whatever phase one managed to flush. If it
    /// truncated, a double panic would destroy the very record the write-first
    /// ordering exists to bank.
    #[test]
    fn the_emergency_write_appends_one_line_and_never_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("crash-19700101-000000.log");
        let banked = "loomux crash log\nversion: 9.9.9-test\npanic:   first\n";
        fs::write(&p, banked).unwrap();

        write_emergency(&p, "second boom", "src/x.rs:1:1");

        let body = fs::read_to_string(&p).unwrap();
        assert!(
            body.starts_with(banked),
            "phase one's record must survive a double panic byte for byte — got:\n{body}"
        );
        assert!(
            body.contains("double-panic: second boom at src/x.rs:1:1"),
            "the emergency line must name the second panic — got:\n{body}"
        );
        assert_eq!(body.matches("double-panic").count(), 1, "exactly one line, not a loop");
        assert!(body.ends_with('\n'), "and it must terminate its own line");

        // The message's own spaces are CONTENT here, not a field separator —
        // this lands in a crash log, not in a breadcrumb. But a payload
        // carrying a newline must still not break the record into two lines.
        write_emergency(&p, "line one\nline two", "src/y.rs:2:2\r");
        let body = fs::read_to_string(&p).unwrap();
        let line = body
            .lines()
            .find(|l| l.starts_with("double-panic: line one"))
            .expect("the message must survive with its spaces intact");
        assert_eq!(
            line, "double-panic: line one_line two at src/y.rs:2:2_",
            "only the bytes that would end the line early may be replaced"
        );
    }

    /// Exactly one of the four crossings is a gap. The set assertion is what
    /// stops "always true" (every clean start shouting) and "always false"
    /// (the silent abort that started all this) from both passing.
    #[test]
    fn only_an_unclean_start_with_no_crash_log_is_a_gap() {
        let some = Path::new("crash-20260705-120000.log");
        assert!(is_crash_log_gap(true, None), "#1218's signature: died, and said nothing");
        assert!(!is_crash_log_gap(true, Some(some)), "an unclean exit that explained itself");
        assert!(!is_crash_log_gap(false, None), "a clean exit has no contradiction to name");
        assert!(!is_crash_log_gap(false, Some(some)), "…nor one that left an older log around");
    }

    /// …and the startup path actually writes it, once, with the pointers a
    /// human needs when our own record is the one that is missing.
    #[test]
    fn an_unclean_start_with_no_crash_log_names_the_gap_in_a_breadcrumb() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();

        // Run 1: nothing known yet, and nothing to say.
        let first = check_and_arm_in(tmp.path());
        assert!(!first.crash_log_gap());
        assert!(
            !tmp.path().join("breadcrumbs.log").exists(),
            "a first, clean start must not manufacture a gap"
        );

        // Run 2: the sentinel survived and no crash log from run 1 exists.
        let gap = check_and_arm_in(tmp.path());
        assert!(gap.unclean && gap.crash_log.is_none(), "the state under test");
        assert!(gap.crash_log_gap());
        let crumbs = fs::read_to_string(tmp.path().join("breadcrumbs.log")).unwrap();
        assert_eq!(crumbs.lines().count(), 1, "exactly one line names the gap: {crumbs:?}");
        assert!(crumbs.contains(" crash-log-gap "), "got {crumbs:?}");
        assert!(crumbs.contains("unclean_prev=true crash_log=none"), "got {crumbs:?}");
        assert!(
            crumbs.contains(r"%LOCALAPPDATA%\CrashDumps"),
            "must point at WER's dump directory — the record that DOES exist: {crumbs:?}"
        );
        assert!(
            crumbs.contains("Application_Error"),
            "…and at the Application event log entry: {crumbs:?}"
        );

        // Run 3: an unclean exit that DID leave a crash log is not a gap. The
        // contradiction is the trigger, not the unclean exit.
        let start3 = fs::metadata(running_lock(tmp.path())).unwrap().modified().unwrap();
        let fresh = tmp.path().join("crash-20260705-120000.log");
        fs::write(&fresh, "boom").unwrap();
        set_mtime(&fresh, start3 + Duration::from_secs(5));

        let explained = check_and_arm_in(tmp.path());
        assert!(explained.unclean && explained.crash_log.is_some(), "the state under test");
        assert!(!explained.crash_log_gap());
        assert_eq!(
            fs::read_to_string(tmp.path().join("breadcrumbs.log")).unwrap().lines().count(),
            1,
            "no second gap line when the crash explained itself"
        );
    }

    // ---------- #1219: the allocation-failure class (#1218 round 3) ----------

    /// **The wrapper actually reports a refused allocation, and returns null.**
    /// Not a simulation: it asks `System` — through the shipped `GlobalAlloc`
    /// impl — for an allocation no machine can serve, and reads what landed on
    /// disk. The panic hook cannot reach this class at all
    /// (`handle_alloc_error` never enters `std::panicking`), so if this test is
    /// wrong there is nothing else that would notice.
    ///
    /// This is the one test that arms the process-wide `ALLOC_LOG`/
    /// `ALLOC_VERSION` cells, which are `OnceLock`s — no other test may arm
    /// them, or one of the two would silently observe the other's directory.
    #[test]
    fn a_refused_allocation_writes_a_record_and_still_returns_null() {
        let _serial = SERIAL.lock_safe();
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            install_alloc_error_reporting_in(tmp.path(), "9.9.9-test"),
            "arming must succeed on a writable dir"
        );
        let log = tmp.path().join("crash-alloc.log");
        assert!(log.is_file(), "the handle is opened at ARM time, not at failure time");
        assert_eq!(fs::metadata(&log).unwrap().len(), 0, "…and nothing is written until it fails");

        // The success path must stay silent — the negative control, without
        // which "writes on every allocation" would pass everything below.
        let small = Layout::from_size_align(64, 8).unwrap();
        let ok = unsafe { CrashReportingAlloc.alloc(small) };
        assert!(!ok.is_null(), "a 64-byte allocation must succeed");
        unsafe { CrashReportingAlloc.dealloc(ok, small) };
        assert_eq!(
            fs::metadata(&log).unwrap().len(),
            0,
            "the success path must write NOTHING — this sits under every Vec push"
        );

        // …and now one no allocator on any machine can serve. 4 EiB exceeds
        // the address space on every 64-bit target this ships to, so `System`
        // returns null rather than over-committing.
        let huge = Layout::from_size_align(1usize << 62, 1).unwrap();
        let p = unsafe { CrashReportingAlloc.alloc(huge) };
        assert!(
            p.is_null(),
            "the wrapper must hand the failure BACK unchanged — handle_alloc_error is \
             what aborts, and it is not this code's job to change that"
        );

        let body = fs::read_to_string(&log).unwrap();
        assert!(body.contains("loomux crash log"), "same record format as a panic: {body}");
        assert!(body.contains("version: 9.9.9-test"), "carries the host's version: {body}");
        assert!(
            body.contains(&format!("allocation of {} bytes (align 1)", 1usize << 62)),
            "the refused Layout is the field that localises the site — #1218 was \
             diagnosed off exactly this number. Got: {body}"
        );
        assert!(body.contains("global allocator"), "…and says which recorder wrote it: {body}");

        // It is a crash log by the same rules as any other, so the next launch
        // names it instead of reporting a gap.
        assert!(is_crash_log(&log), "must match the crash-*.log glob");
        assert_eq!(
            newest_crash_log_since(tmp.path(), None).as_deref(),
            Some(log.as_path()),
            "an alloc-abort record must be findable as THE crash log of that run"
        );
    }

    /// The record composition, away from the live allocator: same head/tail as
    /// a panic record, and it **truncates instead of growing** when the buffer
    /// is too small — the one behaviour that keeps the failure path from
    /// needing an allocation it cannot have.
    #[test]
    fn the_alloc_record_shares_the_panic_format_and_truncates_rather_than_growing() {
        let rec = alloc_failure_record("9.9.9-test", 1_783_209_600_000, 67_108_864, 1);
        let text = String::from_utf8(rec.as_slice().to_vec()).unwrap();
        assert!(text.starts_with("loomux crash log\nversion: 9.9.9-test\ntime:    20260705-000000"));
        assert!(text.contains("allocation of 67108864 bytes (align 1)"), "got:\n{text}");
        assert!(text.ends_with("\n\n"), "the record must close like a panic record: {text:?}");
        assert!(!rec.overflow, "{ALLOC_RECORD_CAP} bytes must hold the whole record");

        // A sink far too small must lose bytes, not grow and not panic.
        let mut tiny = FixedBuf::<8>::new();
        tiny.put(b"0123456789");
        assert_eq!(tiny.as_slice(), b"01234567", "truncates at the cap");
        assert!(tiny.overflow, "and records that it did");
        tiny.put(b"more");
        assert_eq!(tiny.as_slice(), b"01234567", "a full buffer stays full rather than panicking");
    }

    /// **An empty crash log is not a crash record.** Load-bearing since
    /// `crash-alloc.log` exists, empty, for the whole of every healthy run:
    /// without this the next launch would name it and the `crash-log-gap`
    /// breadcrumb — the only thing that says a death went unrecorded — would
    /// never fire again.
    #[test]
    fn a_zero_length_crash_log_is_never_named_and_never_hides_the_gap() {
        let tmp = tempfile::tempdir().unwrap();
        check_and_arm_in(tmp.path()); // run 1 arms the sentinel
        let empty = tmp.path().join("crash-alloc.log");
        fs::write(&empty, "").unwrap();

        assert!(is_crash_log(&empty), "it matches the glob — the filter is on CONTENT");
        assert_eq!(
            newest_crash_log_since(tmp.path(), None),
            None,
            "an empty crash log must not be named"
        );
        let gap = check_and_arm_in(tmp.path());
        assert!(gap.crash_log_gap(), "…and must not suppress the gap breadcrumb");

        // One byte in it and it becomes a real record again.
        fs::write(&empty, "boom").unwrap();
        assert_eq!(newest_crash_log_since(tmp.path(), None).as_deref(), Some(empty.as_path()));
        assert!(!check_and_arm_in(tmp.path()).crash_log_gap());
    }

    /// **The fmt-free property of the crash path, pinned by inspection.**
    ///
    /// Nothing behavioural can catch a `format!` creeping back into phase one:
    /// it would pass every test above and only show itself as a zero-byte
    /// crash log on a user's machine, which is precisely the failure #1219
    /// exists to end. So this is a source scan, and it is written the way this
    /// repo's other scans are: it decides on a **shape** (a macro token inside
    /// a marked region), never on a binding's name, and it default-denies.
    ///
    /// **Blind spot, stated rather than hidden:** a textual scan cannot follow
    /// a call out of the region. The functions the regions call are checked by
    /// hand and are exactly these — `now_ms`, `civil_from_days`,
    /// `rotate_if_needed`, `fs::*`, `Path::join`, `String::from_utf8` — none of
    /// which reaches `core::fmt`. Adding a call to anything else from inside a
    /// region means re-checking that list by hand; the scan will not do it for
    /// you.
    #[test]
    fn the_first_phase_composes_without_the_formatting_machinery() {
        // Built with `concat!` so this source file does not itself contain the
        // marker as a contiguous string: a scan that could match its OWN test
        // body would go green with the real markers deleted.
        let begin = concat!("FMT-FREE", " REGION BEGIN");
        let end = concat!("FMT-FREE", " REGION END");
        let src = include_str!("obs.rs");

        let mut regions: Vec<&str> = Vec::new();
        let mut rest = src;
        while let Some(b) = rest.find(begin) {
            let after = &rest[b + begin.len()..];
            let e = after.find(end).expect("every region opener needs its closer");
            regions.push(&after[..e]);
            rest = &after[e..];
        }
        assert_eq!(
            regions.len(),
            4,
            "expected four marked regions (the composition primitives, the breadcrumb \
             line, the crash hook's two phases, and the allocation-failure path) — a \
             renamed or deleted marker must fail here rather than silently shrink what \
             is scanned"
        );
        let scanned: usize = regions.iter().map(|r| r.len()).sum();
        assert!(
            scanned > 3000,
            "the marked regions collapsed to {scanned} bytes — the markers are no longer \
             around the code they were written for"
        );

        // Scan the CODE, not the prose: these regions carry comments that name
        // the very macros they exist to keep out, and a scan that reddened on
        // an explanation would teach the next author to delete the
        // explanation. Line comments only — no region contains a `//` inside a
        // string literal, and a block comment would have to be added to break
        // this, which is itself worth failing on.
        let code: Vec<String> = regions
            .iter()
            .map(|r| {
                r.lines()
                    .map(|l| l.split("//").next().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();

        // Default-deny: every route back into `core::fmt`, plus the two
        // shapes that panic outright.
        for banned in ["format!", "write!", "writeln!", "format_args!", ".unwrap()", ".expect("] {
            for (i, region) in code.iter().enumerate() {
                assert!(
                    !region.contains(banned),
                    "marked region {i} contains `{banned}` — the crash hook's first phase \
                     must compose from byte slices and must not panic; see the region note \
                     above `push_dec`"
                );
            }
        }
    }

    #[test]
    fn forced_panic_in_background_thread_writes_crash_log() {
        let _serial = SERIAL.lock_safe();
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
            // `write_crash_log_in`, `record_crash_first_phase` — and lands in
            // the file. `records_crash_file_with
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
        let _serial = SERIAL.lock_safe();
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
