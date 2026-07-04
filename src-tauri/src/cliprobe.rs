//! Agent CLI probing: is a program on PATH, and which models does it offer?
//!
//! There is no models API on the agent CLIs, but both document their model
//! strings in the `--model` section of their help text, so we run
//! `<program> --help` once (hidden, with a timeout), parse that section, and
//! cache the result for the app's lifetime. The launcher merges the parsed
//! list with curated fallbacks, so a parse miss degrades to suggestions
//! rather than an empty dropdown.

use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const HELP_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Serialize)]
pub struct CliProbe {
    /// The program ran and produced help output.
    pub available: bool,
    /// Model ids parsed from the `--model` help section (may be empty).
    pub models: Vec<String>,
    /// Human-readable failure reason when not available.
    pub error: Option<String>,
}

fn cache() -> &'static Mutex<HashMap<String, CliProbe>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CliProbe>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Extract model ids from a CLI's help text. Strategy: isolate the `--model`
/// option's description block, then collect quoted tokens plus bare tokens
/// that look like model ids (contain a digit, e.g. `gpt-5.2`,
/// `claude-sonnet-4.6`) and the literal `auto`.
pub fn parse_models_from_help(help: &str) -> Vec<String> {
    let Some(idx) = help.find("--model") else {
        return vec![];
    };
    // The block ends at the next option definition (a line whose first
    // non-space char is '-'), skipping the `--model` line itself.
    let mut block = String::new();
    for (i, line) in help[idx..].lines().enumerate() {
        if i > 0 && line.trim_start().starts_with('-') {
            break;
        }
        block.push_str(line);
        block.push('\n');
        if i > 14 {
            break;
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        let ok = !s.is_empty()
            && s.len() <= 48
            && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
            && (s == "auto" || s.chars().any(|c| c.is_ascii_digit()) || !s.contains(' '));
        if ok && !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };

    // Quoted tokens: 'fable', "gpt-5.3-codex".
    for quote in ['\'', '"'] {
        let mut rest = block.as_str();
        while let Some(start) = rest.find(quote) {
            let after = &rest[start + 1..];
            let Some(end) = after.find(quote) else { break };
            push(&after[..end]);
            rest = &after[end + 1..];
        }
    }
    // Bare model-ish tokens (digit + dash, so prose words don't match) and
    // the literal `auto` (copilot's pick-for-me value).
    for token in block.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | ':' | ';')) {
        let t = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if t == "auto" || (t.chars().any(|c| c.is_ascii_digit()) && t.contains('-')) {
            push(t);
        }
    }
    out
}

/// Run `<program> --help` without a console window, bounded by a timeout.
fn run_help(program: &str) -> Result<String, String> {
    // The program name is interpolated into a shell line on Windows (npm
    // shims are .cmd files that CreateProcess can't exec directly).
    if !program.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("invalid program name".into());
    }
    #[cfg(target_os = "windows")]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = Command::new("cmd");
        c.args(["/C", &format!("{program} --help")]).creation_flags(CREATE_NO_WINDOW);
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-lc", &format!("{program} --help")]);
        c
    };
    // Fresh PATH: a CLI installed after loomux started must still probe as
    // available (its dir is already in the registry PATH).
    if let Some(path) = crate::winpath::fresh_path() {
        cmd.env("PATH", path);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    // Drain both pipes on threads (help can exceed the pipe buffer) while
    // we poll for exit with a deadline. Stderr matters for diagnosis: the
    // shell's "not recognized" complaint lands there.
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        buf
    });
    let mut stderr = child.stderr.take().unwrap();
    let err_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + HELP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().unwrap_or_default();
                if out.trim().is_empty() && !status.success() {
                    let err = err_reader.join().unwrap_or_default();
                    let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                    if first.contains("not recognized") || first.contains("not found") {
                        return Err(format!("'{program}' was not found on PATH"));
                    }
                    return Err(format!(
                        "`{program} --help` failed (exit {:?}){}",
                        status.code(),
                        if first.is_empty() { String::new() } else { format!(": {first}") }
                    ));
                }
                return Ok(out);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(format!("`{program} --help` timed out"));
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn probe_uncached(program: &str) -> CliProbe {
    match run_help(program) {
        Ok(help) => CliProbe {
            available: true,
            models: parse_models_from_help(&help),
            error: None,
        },
        Err(e) => CliProbe {
            available: false,
            models: vec![],
            error: Some(if e.contains("cannot find") || e.contains("not found") || e.contains("os error 2") {
                format!("'{program}' was not found on PATH")
            } else {
                e
            }),
        },
    }
}

/// Probe an agent CLI (availability + model list). Successful probes are
/// cached for the app run; failures are NOT — a CLI installed while loomux
/// is running must become launchable on the next probe (spawns already see
/// it via fresh-PATH resolution).
#[tauri::command]
pub fn probe_agent_cli(program: String) -> CliProbe {
    let program = program.trim().to_lowercase();
    if let Some(hit) = cache().lock().unwrap().get(&program) {
        return hit.clone();
    }
    let probe = probe_uncached(&program);
    if probe.available {
        cache().lock().unwrap().insert(program, probe.clone());
    }
    probe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_style_quoted_aliases() {
        let help = "\
  --mcp-config <configs...>             Load MCP servers\n\
  --model <model>                       Model for the current session. Provide\n\
                                        an alias for the latest model (e.g.\n\
                                        'fable', 'opus', or 'sonnet') or a\n\
                                        model's full name (e.g.\n\
                                        'claude-fable-5').\n\
  -n, --name <name>                     Set a display name\n";
        let models = parse_models_from_help(help);
        assert!(models.contains(&"fable".to_string()));
        assert!(models.contains(&"opus".to_string()));
        assert!(models.contains(&"sonnet".to_string()));
        assert!(models.contains(&"claude-fable-5".to_string()));
        assert!(!models.iter().any(|m| m == "name"), "must not leak the next option: {models:?}");
    }

    #[test]
    fn parses_copilot_style_bare_ids() {
        let help = "\
  --model MODEL        Set the AI model. Pass auto to pick automatically.\n\
                       Available: gpt-5.2, claude-sonnet-4.6, claude-haiku-4.5,\n\
                       gpt-5.3-codex\n\
  --no-color           Disable color\n";
        let models = parse_models_from_help(help);
        for m in ["gpt-5.2", "claude-sonnet-4.6", "claude-haiku-4.5", "gpt-5.3-codex"] {
            assert!(models.contains(&m.to_string()), "missing {m} in {models:?}");
        }
        assert!(!models.iter().any(|m| m == "no-color"), "next option leaked: {models:?}");
    }

    #[test]
    fn no_model_section_yields_empty() {
        assert!(parse_models_from_help("usage: foo [-h]").is_empty());
    }
}
