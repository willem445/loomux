//! Durable UI state persisted across launches — currently the project-tab set
//! (#63). This is app-global UI state (not per-group), so it lives directly
//! under the app data dir as `tabs.json`, a sibling of `orchestration/` and
//! `logs/` — the same `<data dir>/loomux/…` tree the rest of the app's durable
//! state uses (see `OrchRegistry::default_root`, `obs::logs_dir`).
//!
//! The blob is an OPAQUE JSON string the frontend owns the schema for
//! (`src/tabstore.ts` encodes/decodes and validates the shape — the single
//! source of the tab schema). The backend's job here is narrow but critical:
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
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

/// Atomically write `contents` to `path`: create the parent dir, write a sibling
/// `*.tmp`, then rename it over the target. See the module note — this is the
/// #133 anti-truncation guarantee. Public so the integration test can drive it
/// against a tempdir.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    // `fs::rename` replaces an existing destination on both Windows and Unix, so
    // this is the atomic swap. It can still fail if the destination is briefly
    // locked (a virus scanner / another handle on Windows); fall back to a direct
    // write so the update isn't lost, then drop the temp. The fallback is the
    // only path that can truncate, and only when rename is impossible.
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
        // No stray temp file left behind on the happy path.
        assert!(!path.with_extension("json.tmp").exists());
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
