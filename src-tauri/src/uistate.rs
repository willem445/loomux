//! Durable UI state persisted across launches — the project-tab set (#63) and,
//! since #370, app-wide terminal settings. Both are app-global (not per-group),
//! so they live directly under the app data dir as `tabs.json`/`settings.json`,
//! siblings of `orchestration/` and `logs/` — the same `<data dir>/loomux/…`
//! tree the rest of the app's durable state uses (see
//! `OrchRegistry::default_root`, `obs::logs_dir`).
//!
//! Each blob is an OPAQUE JSON string the frontend owns the schema for
//! (`src/tabstore.ts` / `src/settings.ts` encode/decode and validate their own
//! shape — this file never parses either beyond "is it JSON at all"). The
//! backend's job here is narrow but critical, and identical for both files:
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

/// `<user data dir>/loomux` — the app-global state root. Falls back to the
/// system temp dir if the platform can't report a data dir (same degradation as
/// the orchestration root / logs dir), so persistence is best-effort, never a
/// hard failure.
fn state_dir() -> PathBuf {
    if let Some(dir) = STATE_DIR_OVERRIDE.lock().unwrap().clone() {
        return dir;
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("loomux")
}

/// Absolute path of the persisted tab set.
fn tabs_path() -> PathBuf {
    state_dir().join("tabs.json")
}

/// Absolute path of the persisted app settings (#370).
fn settings_path() -> PathBuf {
    state_dir().join("settings.json")
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

/// Read the persisted tab set as an opaque JSON string, or `null` if there's
/// nothing durable yet (first run) or the stored file was corrupt (quarantined).
#[tauri::command]
pub fn load_ui_tabs() -> Option<String> {
    load_or_quarantine(&tabs_path())
}

/// Persist the tab set (an opaque JSON string produced by `tabstore.ts`),
/// atomically. Errors surface to the caller, which treats persistence as
/// best-effort and never blocks the UI on it.
#[tauri::command]
pub fn save_ui_tabs(contents: String) -> Result<(), String> {
    write_atomic(&tabs_path(), &contents)
}

/// Read the persisted app settings (#370: `terminal.pasteOnPlainCtrlV` and
/// whatever else lands here later) as an opaque JSON string, or `null` on
/// first run / a quarantined corrupt file — `src/settings.ts` degrades that
/// to its defaults, exactly like `load_ui_tabs`/`tabstore.ts`.
#[tauri::command]
pub fn load_settings() -> Option<String> {
    load_or_quarantine(&settings_path())
}

/// Persist app settings (an opaque JSON string produced by `settings.ts`),
/// atomically. Same best-effort contract as `save_ui_tabs`.
#[tauri::command]
pub fn save_settings(contents: String) -> Result<(), String> {
    write_atomic(&settings_path(), &contents)
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
