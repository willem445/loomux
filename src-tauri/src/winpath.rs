//! Fresh PATH resolution on Windows.
//!
//! Processes inherit their parent's environment, so a CLI installed while
//! loomux (or the terminal that launched it) is already running is
//! invisible to new panes and probes until the whole chain restarts —
//! observed live when a winget-installed `copilot` couldn't be found. For
//! an agent manager whose users install agent CLIs mid-session, that's not
//! acceptable: every spawned process gets a PATH rebuilt from the current
//! registry values (machine + user), merged with the inherited one.
//!
//! This module also owns the `which`-style program resolver (PATH + PATHEXT)
//! shared by "open in editor" and the direct-CLI pane spawn (issue #78): both
//! need to turn a bare command name (`code`, `claude`) into the concrete
//! executable on disk, and to know whether that executable is a native `.exe`
//! (safe to `CreateProcess` directly) or a `.cmd`/`.bat`/`.ps1` shim (which
//! needs a shell interpreter).

use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
pub fn fresh_path() -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    let read = |root, subkey: &str| -> Option<String> {
        RegKey::predef(root).open_subkey(subkey).ok()?.get_value::<String, _>("Path").ok()
    };
    let machine = read(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    );
    let user = read(HKEY_CURRENT_USER, "Environment");
    if machine.is_none() && user.is_none() {
        return None;
    }
    let current = std::env::var("PATH").unwrap_or_default();
    Some(merge_paths(
        &current,
        &expand_env(machine.as_deref().unwrap_or("")),
        &expand_env(user.as_deref().unwrap_or("")),
    ))
}

#[cfg(not(target_os = "windows"))]
pub fn fresh_path() -> Option<String> {
    None
}

/// Inherited PATH first (session-local additions keep priority), then any
/// registry entries it lacks. Deduped case-insensitively, trailing slashes
/// ignored.
pub fn merge_paths(current: &str, machine: &str, user: &str) -> String {
    let norm = |p: &str| p.trim().trim_end_matches(['\\', '/']).to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for part in current.split(';').chain(machine.split(';')).chain(user.split(';')) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if seen.insert(norm(part)) {
            out.push(part.to_string());
        }
    }
    out.join(";")
}

/// Expand `%VAR%` references (registry PATH values are often REG_EXPAND_SZ,
/// which winreg returns unexpanded). Unknown variables are left as-is.
pub fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(val) => out.push_str(&val),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ── Program resolution (PATH + PATHEXT), shared by editor + pane spawn ───────

/// Default Windows executable extensions, used when `PATHEXT` is unset. This is
/// what lets a bare `code` resolve to `code.cmd` and `claude` to `claude.exe`.
#[cfg(windows)]
pub const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The PATH a spawned program should be resolved and launched with. Prefers the
/// freshly-rebuilt registry PATH on Windows (a CLI installed while loomux is
/// running is still found) and falls back to the inherited value.
pub fn launch_path() -> String {
    fresh_path().unwrap_or_else(|| std::env::var("PATH").unwrap_or_default())
}

/// The extension list to try when resolving a bare command name. Empty off
/// Windows, where a program name is used verbatim.
pub fn launch_pathext() -> String {
    #[cfg(windows)]
    {
        std::env::var("PATHEXT").unwrap_or_else(|_| DEFAULT_PATHEXT.to_string())
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
}

/// Resolve a command to a concrete executable path.
///
/// An explicit path (one containing a path separator) is used verbatim when it
/// names an existing file. A bare command is looked up on `path_env`, trying the
/// name as-is first and then with each `pathext` extension appended — so `code`
/// resolves to `code.cmd`, `claude` to `claude.exe`, and an already-qualified
/// `git.exe` matches directly. Returns `None` when nothing matches.
pub fn resolve_program(program: &str, path_env: &str, pathext: &str) -> Option<PathBuf> {
    if program.contains('/') || program.contains('\\') {
        let p = PathBuf::from(program);
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
            let cand = Path::new(dir).join(format!("{program}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Whether `path` can be handed straight to `CreateProcess` (portable-pty's
/// `CommandBuilder`) as the pty child, versus needing a shell interpreter.
///
/// On Windows only true native images (`.exe`/`.com`) qualify: a `.cmd`/`.bat`
/// batch file or a `.ps1` script is not a PE and `CreateProcess` cannot launch
/// it — those must run through the shell wrapper. This is the safety boundary
/// for direct-CLI pane spawn (issue #78): claude/copilot are native `.exe`, so
/// they spawn directly; a shim CLI (some npm `gemini`/`opencode` installs ship a
/// `.cmd`) is correctly kept on the shell path. Off Windows any resolved file is
/// directly executable.
pub fn is_native_executable(path: &Path) -> bool {
    #[cfg(windows)]
    {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"),
            None => false,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn merge_keeps_inherited_priority_and_appends_new_registry_dirs() {
        let merged = merge_paths(
            r"C:\dev\bin;C:\Windows",
            r"C:\Windows;C:\Program Files\Git\cmd",
            r"C:\Users\x\AppData\Local\Microsoft\WinGet\Links",
        );
        let parts: Vec<&str> = merged.split(';').collect();
        assert_eq!(parts[0], r"C:\dev\bin", "inherited entries must stay first");
        assert!(parts.contains(&r"C:\Program Files\Git\cmd"));
        assert!(
            parts.contains(&r"C:\Users\x\AppData\Local\Microsoft\WinGet\Links"),
            "the freshly-registered dir must be appended — this is the whole point"
        );
        assert_eq!(
            parts.iter().filter(|p| p.eq_ignore_ascii_case(r"C:\Windows")).count(),
            1,
            "duplicates must collapse"
        );
    }

    #[test]
    fn merge_dedupes_case_and_trailing_slash_variants() {
        let merged = merge_paths(r"C:\Tools\;c:\tools", r"C:\TOOLS", "");
        assert_eq!(merged, r"C:\Tools\");
    }

    #[test]
    fn expands_known_vars_and_leaves_unknown_intact() {
        std::env::set_var("LOOMUX_TEST_VAR", r"C:\xyz");
        assert_eq!(expand_env(r"%LOOMUX_TEST_VAR%\bin"), r"C:\xyz\bin");
        assert_eq!(expand_env("%NOPE_NOT_SET%\\bin"), "%NOPE_NOT_SET%\\bin");
        assert_eq!(expand_env("plain"), "plain");
        assert_eq!(expand_env("50%"), "50%");
    }

    #[test]
    fn resolve_finds_bare_command_via_pathext() {
        let tmp = tempfile::tempdir().unwrap();
        // A fake CLI discoverable only by appending an extension. The extension
        // casing matches the file so the lookup also succeeds on case-sensitive
        // filesystems (CI runs this on Linux).
        let exe = tmp.path().join("myagent.exe");
        fs::write(&exe, b"binary").unwrap();
        let found = resolve_program("myagent", tmp.path().to_str().unwrap(), ".exe;.CMD").unwrap();
        assert_eq!(found, exe);
    }

    #[test]
    fn resolve_matches_name_that_already_has_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd = tmp.path().join("shim.cmd");
        fs::write(&cmd, b"@echo off").unwrap();
        // Even though PATHEXT lists .EXE first, the verbatim name wins.
        let found = resolve_program("shim.cmd", tmp.path().to_str().unwrap(), ".EXE").unwrap();
        assert_eq!(found, cmd);
    }

    #[test]
    fn resolve_uses_explicit_path_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("claude.bin");
        fs::write(&exe, b"x").unwrap();
        let p = exe.to_str().unwrap();
        assert_eq!(resolve_program(p, "", "").unwrap(), exe);
    }

    #[test]
    fn resolve_missing_bare_command_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_program("no_such_cli_xyz", tmp.path().to_str().unwrap(), ".EXE").is_none());
    }

    #[test]
    fn resolve_missing_explicit_path_is_none() {
        assert!(resolve_program("/nope/ghost/agent.exe", "", "").is_none());
    }

    /// The direct-spawn safety boundary (issue #78): only native images may be
    /// handed to CreateProcess as the pty child; shims must keep the shell.
    #[cfg(windows)]
    #[test]
    fn native_executable_classification_windows() {
        assert!(is_native_executable(Path::new(r"C:\a\claude.exe")));
        assert!(is_native_executable(Path::new(r"C:\a\tool.COM")));
        assert!(!is_native_executable(Path::new(r"C:\a\gemini.cmd")));
        assert!(!is_native_executable(Path::new(r"C:\a\opencode.bat")));
        assert!(!is_native_executable(Path::new(r"C:\a\wrap.ps1")));
        assert!(!is_native_executable(Path::new(r"C:\a\noext")));
    }

    #[cfg(not(windows))]
    #[test]
    fn native_executable_is_always_true_off_windows() {
        assert!(is_native_executable(Path::new("/usr/bin/claude")));
    }
}
