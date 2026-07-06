//! "Open in editor": launch the user's configured external editor on a
//! workspace directory.
//!
//! The editor command (e.g. `code`, `zed`, or a full path to an executable)
//! is stored client-side and passed in per call. We spawn it **detached**,
//! passing the directory as a single element of a direct argument vector —
//! never a shell command string. Nothing the user typed for the editor, and
//! no character in the directory path, is ever handed to a shell for
//! re-parsing, so there is no command-injection surface.

use std::path::Path;
use std::process::{Command, Stdio};

/// Validate the two inputs. Returns the trimmed editor + directory on success,
/// or a user-facing message (surfaced as a toast) describing what is wrong.
fn validate<'a>(editor: &'a str, dir: &'a str) -> Result<(&'a str, &'a str), String> {
    let editor = editor.trim();
    if editor.is_empty() {
        return Err("No editor configured — right-click the </> button to set one.".into());
    }
    let dir = dir.trim();
    if dir.is_empty() {
        return Err("No workspace folder to open.".into());
    }
    if !Path::new(dir).is_dir() {
        return Err(format!("Folder does not exist: {dir}"));
    }
    Ok((editor, dir))
}

/// The argument vector passed to the editor: just the directory, as a single
/// element. Isolated (and unit-tested) to make the "no shell string, ever"
/// guarantee explicit — a directory containing spaces, quotes, ampersands, or
/// any other shell metacharacter stays one argv element and can never be
/// reinterpreted as syntax.
fn editor_args(dir: &str) -> Vec<String> {
    vec![dir.to_string()]
}

/// Open `dir` in the configured `editor`, spawned detached. Returns a
/// user-facing error string on any failure (unconfigured, missing folder,
/// editor not found, or spawn failure) so the frontend can toast it.
#[tauri::command]
pub fn open_in_editor(editor: String, dir: String) -> Result<(), String> {
    let (editor, dir) = validate(&editor, &dir)?;
    let path_env = crate::winpath::launch_path();
    let program = crate::winpath::resolve_program(editor, &path_env, &crate::winpath::launch_pathext())
        .ok_or_else(|| format!("Editor not found on PATH: {editor}"))?;

    let mut cmd = Command::new(&program);
    cmd.args(editor_args(dir))
        .current_dir(dir)
        .env("PATH", &path_env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS: the editor gets no inherited console, so launching
        // a GUI editor (or a .cmd shim like VS Code's) never flashes a window.
        // CREATE_NEW_PROCESS_GROUP: it keeps running after loomux exits and is
        // not signalled by a Ctrl-C aimed at loomux.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    // Fire and forget: the child is detached, so we drop its handle without
    // waiting. std does not kill on drop, so the editor lives on.
    cmd.spawn()
        .map(|_child| ())
        .map_err(|e| format!("Failed to launch editor '{editor}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_empty_editor() {
        let err = validate("   ", "C:\\").unwrap_err();
        assert!(err.contains("No editor configured"), "got: {err}");
    }

    #[test]
    fn validate_rejects_empty_dir() {
        assert!(validate("code", "  ").is_err());
    }

    #[test]
    fn validate_rejects_missing_dir() {
        let err = validate("code", "/no/such/loomux/dir/zzz").unwrap_err();
        assert!(err.to_lowercase().contains("does not exist"), "got: {err}");
    }

    #[test]
    fn validate_trims_and_accepts_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let (editor, d) = validate("  code  ", dir).unwrap();
        assert_eq!(editor, "code", "editor must be trimmed");
        assert_eq!(d, dir);
    }

    #[test]
    fn dir_is_a_single_argv_element() {
        // A directory full of shell metacharacters must remain exactly one
        // argv element — this is the whole injection-safety guarantee. (The
        // end-to-end cmd.exe escaping for .cmd shims is a std guarantee,
        // covered by rustc, not exercised here.)
        let evil = r#"C:\repo & calc.exe | echo "pwned" `whoami`"#;
        let args = editor_args(evil);
        assert_eq!(args, vec![evil.to_string()]);
        assert_eq!(args.len(), 1, "the dir must never be split into shell tokens");
    }

    // Program resolution (PATH + PATHEXT) is shared with pane spawn and unit-
    // tested in `winpath` — see the `resolve_*` / `native_executable_*` tests
    // there.
}
