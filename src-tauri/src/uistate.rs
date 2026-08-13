//! Durable UI state persisted across launches — the project-tab set (#63),
//! since #370 app-wide terminal settings, and since #887 the user's SSH
//! connection profiles. All three are app-global (not per-group), so they live
//! directly under the app data dir as
//! `tabs.json`/`settings.json`/`sshprofiles.json`, siblings of
//! `orchestration/` and `logs/` — the same `<data dir>/loomux/…` tree the rest
//! of the app's durable state uses (see `OrchRegistry::default_root`,
//! `obs::logs_dir`).
//!
//! Each blob is an OPAQUE JSON string the frontend owns the schema for
//! (`src/tabstore.ts` / `src/settings.ts` / `src/sshprofile.ts` encode/decode
//! and validate their own shape — this file never parses any of them beyond
//! "is it JSON at all"). The backend's job here is narrow but critical, and
//! identical for all three:
//!
//!  1. **Atomic writes.** Serialize to a sibling temp file, then rename over the
//!     target. A bare `fs::write` truncates the file in place, so a crash / kill
//!     mid-write destroys the data — exactly the hazard that wiped the task board
//!     in #133. A temp-file + rename leaves either the old (valid) file or the
//!     temp file behind, never a half-written target.
//!  2. **Corrupt-file fail-safe.** On load, if the file is present but not valid
//!     JSON at all (truncated / garbled), it is *quarantined* — renamed aside to
//!     `tabs.corrupt.json` so a later save can't clobber it and a human can
//!     inspect it — and `None` is returned so the caller degrades to defaults
//!     WITHOUT silently losing the user's tabs or the evidence.
//!
//! Frontend never touches Tauri IPC directly (CLAUDE.md constraint 5): the two
//! `#[tauri::command]`s below are wrapped by typed helpers in `src/pty.ts`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Disambiguates concurrent temp files (with the pid), mirroring
/// `orchestration::atomic_write` — two saves must not collide on the temp name.
static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Test-only override for the state dir, so the atomic-write / quarantine logic
/// is exercised against a tempdir without touching the real user data dir
/// (mirrors `obs::LOG_DIR_OVERRIDE`). `None` in production.
static STATE_DIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Hands out the dispatch tickets [`write_atomic_seq`] orders writes by. See
/// [`next_write_ticket`].
static WRITE_TICKET: AtomicU64 = AtomicU64::new(0);

/// The newest ticket already durably written, per path — the high-water mark
/// [`write_atomic_seq`] compares against. Keyed by path because `tabs.json`,
/// `settings.json` and `sshprofiles.json` are independent files with
/// independent save gestures; a slow settings write must not make a tab save
/// look stale. A `Vec` and not a `HashMap`: it holds three entries in
/// production, and a linear scan of three is not worth a hasher.
static WRITE_HIGH_WATER: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());

/// Take the next dispatch ticket. Called by a save command **before** its first
/// `.await` — that is the whole point (#746).
///
/// Tauri dispatches a command's future on the webview thread up to its first
/// await, so a ticket taken there is stamped in ARRIVAL order, the same order
/// synchronous dispatch used to impose on the writes themselves. One relaxed
/// `fetch_add` is in-memory work of the same class as the `cheap` commands that
/// run entirely on that thread, and is what lets everything after it move off.
fn next_write_ticket() -> u64 {
    WRITE_TICKET.fetch_add(1, Ordering::Relaxed)
}

/// [`write_atomic`], plus the ordering the conversion took away (#746).
///
/// **What broke.** `save_ui_tabs`/`save_settings` were synchronous, so Tauri ran
/// them one at a time on the webview thread and the LAST gesture's bytes were
/// necessarily the last on disk. Off-thread, two saves are two pool tasks and
/// the rename order is whatever the scheduler picks: an older layout can land
/// after a newer one. That is not a transient wobble — `main.ts`'s `persistTabs`
/// suppresses a save whose bytes match the last one it ISSUED, so once a stale
/// write wins, the correct bytes are never re-offered and the user's tab layout
/// stays one gesture behind until something else changes it.
///
/// **What replaces it.** A ticket per save, taken in arrival order on the
/// webview thread, and a per-path high-water mark. A write whose ticket is older
/// than what is already on disk is DROPPED, not queued — the newer content is
/// already durable, so writing the older bytes could only undo it. The lock is
/// held across the write itself as well, so two accepted writes cannot interleave
/// inside `write_atomic` and land their renames out of order either.
///
/// Dropping reports `Ok`: the caller asked for its content to be durable, and
/// content at least as new as it is. `flushTabs` (the quit path, #219) awaits
/// this and must not be told the last save of the session failed when it did
/// not.
///
/// The mark advances only on a write that actually LANDED. A failed newer write
/// must not veto an older one — the older bytes are then the freshest thing
/// anybody managed to persist, and the frontend's retry (`persistTabs` clears
/// `lastPersisted` on error) is what eventually supersedes them.
///
/// Public so the unit tests below can drive the ordering directly; there is no
/// production caller outside this file.
pub fn write_atomic_seq(path: &Path, contents: &str, ticket: u64) -> Result<(), String> {
    let mut marks = crate::obs::LockExt::lock_safe(&WRITE_HIGH_WATER);
    // `>` and not `>=`: tickets are unique (`fetch_add`), so the two differ only
    // for a ticket compared against itself, which cannot happen.
    if marks.iter().any(|(p, seen)| p == path && *seen > ticket) {
        return Ok(());
    }
    let wrote = write_atomic(path, contents);
    if wrote.is_ok() {
        match marks.iter_mut().find(|(p, _)| p == path) {
            Some((_, seen)) => *seen = ticket,
            None => marks.push((path.to_path_buf(), ticket)),
        }
    }
    wrote
}

/// The app-global state root (see `obs::data_root`), or the test override
/// when one is set.
fn state_dir() -> PathBuf {
    if let Some(dir) = STATE_DIR_OVERRIDE.lock().unwrap().clone() {
        return dir;
    }
    crate::obs::data_root()
}

/// Absolute path of the persisted tab set.
fn tabs_path() -> PathBuf {
    state_dir().join("tabs.json")
}

/// Absolute path of the persisted app settings (#370).
fn settings_path() -> PathBuf {
    state_dir().join("settings.json")
}

/// Absolute path of the persisted SSH connection profiles (#887).
///
/// A sibling file rather than a key inside `settings.json`: this is a
/// multi-entry LIST with its own lifecycle (add/edit/delete a profile) next to
/// a flat bag of app-wide scalars, and keeping them separate keeps both schemas
/// simple. It is deliberately not `localStorage` either — profiles must survive
/// a webview data clear, the same argument that moved the tab set here in #63.
///
/// Nothing secret is ever stored in it: a profile holds hostnames, ports and
/// the PATH of an identity file, never key material or a password (see
/// `src/sshprofile.ts`, which owns and enforces that schema). Authentication is
/// the user's ssh_config/ssh-agent's job, mirroring `gh.rs`'s no-token posture.
fn ssh_profiles_path() -> PathBuf {
    state_dir().join("sshprofiles.json")
}

/// Atomically write `contents` to `path`: create the parent dir, write a unique
/// sibling temp file, **fsync it**, then rename it over the target. This mirrors
/// the canonical `orchestration::atomic_write` (#133/#161) — the fsync is the
/// disk-full guard: without it a rename could expose a metadata-only file whose
/// data blocks never reached disk, the exact failure mode #133 hit. A crash
/// leaves either the old (valid) file or the temp, never a truncated target.
/// Public so the integration test can drive it against a tempdir.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // Unique temp (pid + seq) in the same dir, so concurrent saves and a
    // cross-volume rename fallback can't collide or land on another writer's temp.
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{stem}.{}.{seq}.tmp", std::process::id()));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(contents.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?; // durable before the rename
    }
    // `fs::rename` replaces an existing destination on both Windows and Unix, so
    // this is the atomic swap. It can still fail if the destination is briefly
    // locked (a virus scanner / another handle on Windows); fall back to a direct
    // write so the update isn't lost, keeping the temp for recovery on failure.
    if fs::rename(&tmp, path).is_err() {
        fs::write(path, contents).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&tmp);
    }
    Ok(())
}

/// Load the persisted blob from `path`, or `None` if it's absent. If the file is
/// present but not valid JSON, quarantine it (rename aside to `*.corrupt.json`)
/// and return `None` — the caller then degrades to defaults while the bad file
/// survives for inspection. Structural (schema-level) validation is the
/// frontend decoder's job (`tabstore.ts`); this only guards against a file that
/// isn't JSON at all — the truncation/corruption class. Public for the test.
pub fn load_or_quarantine(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    if serde_json::from_str::<serde_json::Value>(&raw).is_err() {
        // Rename over any prior quarantine file: the newest corruption is the
        // most useful to inspect, and this can't grow without bound.
        let _ = fs::rename(path, path.with_extension("corrupt.json"));
        return None;
    }
    Some(raw)
}

// ---------- tauri commands ----------
//
// #746: all four are thin async fns over the blocking pool (P1 of
// `doc/design/performance.md`). The loads read and parse a file — and, when it
// will not parse, RENAME it aside — at launch, which the user feels as a slow
// start. The saves serialize, fsync and rename, which is deliberate durability
// work fired on layout gestures and settings changes, i.e. landing in the
// middle of an interaction. None of it belongs on the thread that paints.

/// Read the persisted tab set as an opaque JSON string, or `null` if there's
/// nothing durable yet (first run) or the stored file was corrupt (quarantined).
///
/// **Reentrancy.** Called once per launch. A load racing a save resolves to
/// whichever the filesystem orders first and reads a *whole* file either way —
/// saves are rename-atomic, which is what `write_atomic` is for. The quarantine
/// rename it can perform is idempotent: a second quarantine of the same corrupt
/// file renames over the same target, which is the behaviour
/// `load_or_quarantine` already documents ("the newest corruption is the most
/// useful to inspect").
#[tauri::command]
pub async fn load_ui_tabs() -> Option<String> {
    crate::blocking::run_blocking(|| load_or_quarantine(&tabs_path())).await
}

/// Persist the tab set (an opaque JSON string produced by `tabstore.ts`),
/// atomically. Errors surface to the caller, which treats persistence as
/// best-effort and never blocks the UI on it.
///
/// **Reentrancy.** The one command in this module that needed a guard BUILT
/// rather than named: sync dispatch was what made the last gesture's bytes the
/// last on disk, and off-thread it is [`write_atomic_seq`]'s ticket that does.
/// See its doc for why a plain mutex is not enough (mutual exclusion without
/// ordering still lets an older save win) and why `persistTabs`'s
/// identical-bytes dedup is what turns "one gesture stale" into "stale until
/// something else changes".
#[tauri::command]
pub async fn save_ui_tabs(contents: String) -> Result<(), String> {
    let ticket = next_write_ticket();
    crate::blocking::run_blocking(move || write_atomic_seq(&tabs_path(), &contents, ticket)).await
}

/// Read the persisted app settings (#370: `terminal.pasteOnPlainCtrlV` and
/// whatever else lands here later) as an opaque JSON string, or `null` on
/// first run / a quarantined corrupt file — `src/settings.ts` degrades that
/// to its defaults, exactly like `load_ui_tabs`/`tabstore.ts`.
///
/// **Reentrancy.** Identical to [`load_ui_tabs`], on the sibling file.
#[tauri::command]
pub async fn load_settings() -> Option<String> {
    crate::blocking::run_blocking(|| load_or_quarantine(&settings_path())).await
}

/// Persist app settings (an opaque JSON string produced by `settings.ts`),
/// atomically. Same best-effort contract as `save_ui_tabs`.
///
/// **Reentrancy.** Same ticket as [`save_ui_tabs`], against its own path's
/// high-water mark — the two files never gate each other.
#[tauri::command]
pub async fn save_settings(contents: String) -> Result<(), String> {
    let ticket = next_write_ticket();
    crate::blocking::run_blocking(move || write_atomic_seq(&settings_path(), &contents, ticket))
        .await
}

/// Read the persisted SSH connection profiles (#887) as an opaque JSON string,
/// or `null` on first run / a quarantined corrupt file — `src/sshprofile.ts`
/// degrades that to an empty profile list, exactly like
/// `load_ui_tabs`/`tabstore.ts` and `load_settings`/`settings.ts`.
///
/// **Reentrancy.** Identical to [`load_ui_tabs`], on its own sibling file.
#[tauri::command]
pub async fn load_ssh_profiles() -> Option<String> {
    crate::blocking::run_blocking(|| load_or_quarantine(&ssh_profiles_path())).await
}

/// Persist SSH connection profiles (an opaque JSON string produced by
/// `sshprofile.ts`), atomically. Same best-effort contract as `save_ui_tabs`.
///
/// **Reentrancy.** Same ticket as [`save_ui_tabs`], against its own path's
/// high-water mark — none of the three files gate each other.
#[tauri::command]
pub async fn save_ssh_profiles(contents: String) -> Result<(), String> {
    let ticket = next_write_ticket();
    crate::blocking::run_blocking(move || write_atomic_seq(&ssh_profiles_path(), &contents, ticket))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tabs.json");
        write_atomic(&path, r#"{"tabs":[],"activeIndex":0}"#).unwrap();
        assert_eq!(
            load_or_quarantine(&path).as_deref(),
            Some(r#"{"tabs":[],"activeIndex":0}"#)
        );
    }

    #[test]
    fn write_atomic_creates_missing_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        // Nested path whose parent doesn't exist yet — first-run case.
        let path = tmp.path().join("loomux").join("tabs.json");
        write_atomic(&path, "{}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        // No stray temp file left behind on the happy path (the rename consumed it).
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp left: {leftovers:?}");
    }

    #[test]
    fn write_atomic_overwrites_without_truncation_hazard() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tabs.json");
        write_atomic(&path, "OLD-CONTENT-LONGER").unwrap();
        write_atomic(&path, "new").unwrap();
        // The replacement is the new content in full, never a mix / truncation.
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn absent_file_loads_as_none_without_creating_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tabs.json");
        assert_eq!(load_or_quarantine(&path), None);
        assert!(!path.exists(), "a load must not create the file");
    }

    #[test]
    fn corrupt_file_is_quarantined_and_load_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tabs.json");
        // A truncated / non-JSON blob — the #133 half-written-file class.
        fs::write(&path, "{ \"tabs\": [ trunc").unwrap();
        assert_eq!(load_or_quarantine(&path), None, "corrupt → degrade to defaults");
        assert!(!path.exists(), "the corrupt file is moved out of the way");
        let quarantined = path.with_extension("corrupt.json");
        assert_eq!(
            fs::read_to_string(&quarantined).unwrap(),
            "{ \"tabs\": [ trunc",
            "the bad file is preserved verbatim for inspection"
        );
    }

    #[test]
    fn a_stale_write_never_overwrites_a_newer_one_that_already_landed() {
        // #746's ordering pin. Synchronous dispatch made the LAST save gesture
        // the last bytes on disk; off-thread, two saves are two pool tasks and
        // the older one can land second. Here the older ticket is applied
        // second on purpose — the scheduling this test cannot control, made
        // deterministic — and must be dropped rather than written.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tabs.json");
        let (older, newer) = (next_write_ticket(), next_write_ticket());
        write_atomic_seq(&path, "NEW-LAYOUT", newer).unwrap();
        write_atomic_seq(&path, "old-layout", older).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "NEW-LAYOUT",
            "an older save landing after a newer one overwrote it — and persistTabs's \
             identical-bytes dedup means the correct layout is never re-offered, so this is \
             stale until some unrelated change happens to fire another save"
        );
    }

    #[test]
    fn the_ordering_guard_is_per_path_and_lets_ordered_writes_through() {
        // The other direction, so the guard cannot pass by refusing everything.
        // (1) a newer ticket on the same path still writes, and (2) a sibling
        // file's older ticket is NOT gated by it — `tabs.json` and
        // `settings.json` have independent save gestures, and a slow settings
        // write must not make a tab save look stale.
        let tmp = tempfile::tempdir().unwrap();
        let tabs = tmp.path().join("tabs.json");
        let settings = tmp.path().join("settings.json");
        let (t1, t2, t3) = (next_write_ticket(), next_write_ticket(), next_write_ticket());
        write_atomic_seq(&settings, "settings-first", t1).unwrap();
        write_atomic_seq(&tabs, "tabs-v1", t2).unwrap();
        write_atomic_seq(&tabs, "tabs-v2", t3).unwrap();
        assert_eq!(fs::read_to_string(&tabs).unwrap(), "tabs-v2", "in-order writes must apply");
        // t1 < t3, but it is a different file: it must not be dropped.
        let t4 = next_write_ticket();
        write_atomic_seq(&settings, "settings-second", t4).unwrap();
        assert_eq!(fs::read_to_string(&settings).unwrap(), "settings-second");
    }

    #[test]
    fn a_failed_write_does_not_veto_a_later_older_one() {
        // The mark advances only on a write that landed. Otherwise a newer save
        // that failed (disk full, a locked destination) would silently gate the
        // older bytes that are then the freshest anybody can persist.
        let tmp = tempfile::tempdir().unwrap();
        // `write_atomic` creates the target's parent, so a missing parent is not
        // a failure. Make the parent a FILE instead: `create_dir_all` then
        // cannot succeed and the write fails for real, on the same path the
        // later write uses.
        let blocker = tmp.path().join("blocked");
        fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("tabs.json");
        let (older, newer) = (next_write_ticket(), next_write_ticket());
        assert!(
            write_atomic_seq(&path, "never-lands", newer).is_err(),
            "the fixture must actually fail, or this test proves nothing"
        );
        fs::remove_file(&blocker).unwrap(); // the transient condition clears
        write_atomic_seq(&path, "older-but-real", older).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "older-but-real",
            "a newer write that never landed gated an older one that could — leaving the file \
             with neither"
        );
    }

    #[test]
    fn the_three_state_files_are_distinct_siblings() {
        // The failure this catches is a copy-paste one, and it is destructive:
        // `ssh_profiles_path` was written by duplicating `settings_path`, and a
        // duplicate that kept the old file name would make every profile save
        // silently overwrite the user's `settings.json` (atomically, durably,
        // with no error anywhere). Pure path math — nothing is read or written.
        let (tabs, settings, ssh) = (tabs_path(), settings_path(), ssh_profiles_path());
        assert_eq!(ssh.file_name().unwrap(), "sshprofiles.json");
        assert_ne!(ssh, tabs, "profiles must not write over the tab set");
        assert_ne!(ssh, settings, "profiles must not write over app settings");
        // All three are siblings in the one app-global state dir, which is what
        // makes the per-path write ordering above the whole story for them.
        assert_eq!(ssh.parent(), tabs.parent());
        assert_eq!(ssh.parent(), settings.parent());
    }

    #[test]
    fn quarantine_never_clobbers_a_good_later_save() {
        // The whole point of the fail-safe: a corrupt read must not cost the user
        // their tabs on the NEXT save. Corrupt read → quarantine → save → reload.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tabs.json");
        fs::write(&path, "garbage-not-json").unwrap();
        assert_eq!(load_or_quarantine(&path), None);
        write_atomic(&path, r#"{"tabs":[{"name":"loomux"}]}"#).unwrap();
        assert_eq!(
            load_or_quarantine(&path).as_deref(),
            Some(r#"{"tabs":[{"name":"loomux"}]}"#)
        );
    }
}
