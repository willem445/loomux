//! Fresh PATH resolution on Windows.
//!
//! Processes inherit their parent's environment, so a CLI installed while
//! loomux (or the terminal that launched it) is already running is
//! invisible to new panes and probes until the whole chain restarts —
//! observed live when a winget-installed `copilot` couldn't be found. For
//! an agent manager whose users install agent CLIs mid-session, that's not
//! acceptable: every spawned process gets a PATH rebuilt from the current
//! registry values (machine + user), merged with the inherited one.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
