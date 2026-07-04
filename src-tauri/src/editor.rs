//! "Open in editor": launch the user's configured external editor on a
//! workspace directory.
//!
//! The editor command (e.g. `code`, `zed`, or a full path to an executable)
//! is stored client-side and passed in per call. We spawn it **detached**,
//! passing the directory as a single element of a direct argument vector —
//! never a shell command string. Nothing the user typed for the editor, and
//! no character in the directory path, is ever handed to a shell for
//! re-parsing, so there is no command-injection surface.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Default Windows executable extensions, used when `PATHEXT` is unset. This
/// is what lets a bare `code` resolve to `code.cmd` and `zed` to `zed.exe`.
#[cfg(windows)]
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Validate the two inputs. Returns the trimmed editor + directory on success,
/// or a user-facing message (surfaced as a toast) describing what is wrong.
fn validate<'a>(editor: &'a str, dir: &'a str) -> Result<(&'a str, &'a str), String> {
    let editor = editor.trim();
    if editor.is_empty() {
        return Err("No editor configured — set one with the ⧉ button.".into());
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

/// Resolve an editor command to a concrete executable path.
///
/// An explicit path (one containing a path separator) is used verbatim when it
/// names an existing file. A bare command is looked up on `path_env`, trying
/// the name as-is first and then with each `pathext` extension appended — so
/// `code` resolves to `code.cmd`, `zed` to `zed.exe`, and an already-qualified
/// `git.exe` matches directly. Returns `None` when nothing matches; the caller
/// turns that into a "not found" toast.
fn resolve_program(editor: &str, path_env: &str, pathext: &str) -> Option<PathBuf> {
    if editor.contains('/') || editor.contains('\\') {
        let p = PathBuf::from(editor);
        return p.is_file().then_some(p);
    }
    // "" tries the name verbatim (covers a bare name that already carries its
    // extension, and every non-Windows case where PATHEXT is empty).
    let exts = std::iter::once("").chain(pathext.split(';').filter(|e| !e.is_empty()));
    let sep = if cfg!(windows) { ';' } else { ':' };
    for ext in exts {
        for dir in path_env.split(sep) {
            let dir = dir.trim();
            if dir.is_empty() {
                continue;
            }
            let cand = Path::new(dir).join(format!("{editor}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// PATH the editor should be looked up and launched with. Prefers the freshly
/// resolved registry PATH on Windows (an editor installed while loomux is
/// running is still found) and falls back to the inherited value.
fn launch_path() -> String {
    crate::winpath::fresh_path().unwrap_or_else(|| std::env::var("PATH").unwrap_or_default())
}

/// The extension list to try when resolving a bare command name.
fn launch_pathext() -> String {
    #[cfg(windows)]
    {
        std::env::var("PATHEXT").unwrap_or_else(|_| DEFAULT_PATHEXT.to_string())
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
}

/// Open `dir` in the configured `editor`, spawned detached. Returns a
/// user-facing error string on any failure (unconfigured, missing folder,
/// editor not found, or spawn failure) so the frontend can toast it.
#[tauri::command]
pub fn open_in_editor(editor: String, dir: String) -> Result<(), String> {
    let (editor, dir) = validate(&editor, &dir)?;
    let path_env = launch_path();
    let program = resolve_program(editor, &path_env, &launch_pathext())
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
    use std::fs;

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
    fn dir_is_a_single_arg_never_shell_interpolated() {
        // A directory full of shell metacharacters must remain exactly one
        // argv element — this is the whole injection-safety guarantee.
        let evil = r#"C:\repo & calc.exe | echo "pwned" `whoami`"#;
        let args = editor_args(evil);
        assert_eq!(args, vec![evil.to_string()]);
        assert_eq!(args.len(), 1, "the dir must never be split into shell tokens");
    }

    #[test]
    fn resolve_finds_bare_command_via_pathext() {
        let tmp = tempfile::tempdir().unwrap();
        // A fake editor discoverable only by appending an extension.
        let exe = tmp.path().join("myeditor.exe");
        fs::write(&exe, b"binary").unwrap();
        let found =
            resolve_program("myeditor", tmp.path().to_str().unwrap(), ".EXE;.CMD").unwrap();
        // The resolved path may carry PATHEXT's casing (".EXE"); Windows'
        // case-insensitive filesystem still opens it. Compare accordingly.
        assert!(
            found.to_string_lossy().eq_ignore_ascii_case(&exe.to_string_lossy()),
            "resolved {found:?}, expected {exe:?}"
        );
    }

    #[test]
    fn resolve_matches_name_that_already_has_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd = tmp.path().join("ed.cmd");
        fs::write(&cmd, b"@echo off").unwrap();
        // Even though PATHEXT lists .EXE first, the verbatim name wins.
        let found = resolve_program("ed.cmd", tmp.path().to_str().unwrap(), ".EXE").unwrap();
        assert_eq!(found, cmd);
    }

    #[test]
    fn resolve_uses_explicit_path_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("zed.bin");
        fs::write(&exe, b"x").unwrap();
        let p = exe.to_str().unwrap();
        assert_eq!(resolve_program(p, "", "").unwrap(), exe);
    }

    #[test]
    fn resolve_missing_bare_command_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            resolve_program("no_such_editor_xyz", tmp.path().to_str().unwrap(), ".EXE").is_none()
        );
    }

    #[test]
    fn resolve_missing_explicit_path_is_none() {
        assert!(resolve_program("/nope/ghost/editor.exe", "", "").is_none());
    }
}
