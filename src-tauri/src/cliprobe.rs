//! Agent CLI probing: is a program on PATH, and which models does it offer?
//!
//! Most agent CLIs have no models API, but they document their model strings
//! in the `--model` section of their help text, so we run `<program> --help`
//! once (hidden, with a timeout), parse that section, and cache the result for
//! the app's lifetime.
//!
//! A CLI that can *enumerate* its models beats any parse of its own prose,
//! because it reports what the machine in front of the human is actually
//! configured for rather than what the vendor wrote in a help page. opencode
//! has one — `opencode models`, "List all available models from configured
//! providers" (<https://opencode.ai/docs/cli/>) — so that lives in
//! `ENUMERATORS` as DATA (the `CLI_CAPS` pattern): a second CLI gaining a list
//! command is a row there, not another branch in `probe_with`.
//!
//! The launcher merges whatever comes back with curated fallbacks, so a parse
//! miss degrades to suggestions rather than an empty dropdown.

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

/// How one CLI enumerates its own models. Per-CLI differences live here as
/// DATA, the way `CLI_CAPS` carries the rest of them: adding a CLI that gained
/// a list command is a row, not a new code path.
struct Enumerator {
    /// The program name as probed (`probe_agent_cli` lower-cases before it
    /// looks anything up).
    program: &'static str,
    /// Arguments appended to the program. Constants only — never anything a
    /// caller supplied; see `run_cli`.
    args: &'static str,
    /// Parser for that command's stdout.
    parse: fn(&str) -> Vec<String>,
}

/// `opencode models` — "List all available models from configured providers"
/// (<https://opencode.ai/docs/cli/>). Deliberately WITHOUT `--refresh`: that
/// flag "[r]efresh[es] the models cache from models.dev" (same page), and a
/// background probe must not re-pull a remote catalog on the human's behalf —
/// the cached list is the one their own CLI would use anyway.
const ENUMERATORS: &[Enumerator] = &[Enumerator {
    program: "opencode",
    args: "models",
    parse: parse_models_from_list,
}];

fn enumerator_for(program: &str) -> Option<&'static Enumerator> {
    ENUMERATORS.iter().find(|e| e.program == program)
}

/// Strip ANSI escape sequences and other control bytes from one line.
///
/// Best-effort by design: the probe runs the CLI with pipes rather than a TTY,
/// so styling should already be off, and the failure mode of getting this
/// wrong is that a line stops looking like an id and is dropped — never that
/// junk is admitted, since every surviving token is validated below.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Swallow the sequence up to its terminating letter (CSI `m`, `K`,
            // …). An OSC string ends at BEL instead.
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() || n == '\u{7}' {
                    break;
                }
            }
            continue;
        }
        if !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Parse a listing command's stdout into model ids: one `provider/model` id
/// per line, which is the shape opencode documents ("displays all models
/// available across your configured providers in the form of
/// `provider/model`" — <https://opencode.ai/docs/cli/>).
///
/// The docs state the id format but not the surrounding layout, so this keeps
/// only what *is* an id and drops every other line rather than modelling
/// headings, spinners or progress chatter it has never seen: an unfamiliar
/// layout yields nothing, and `probe_with` then leaves the help-parsed list
/// alone. A CLI that listed bare ids instead would carry its own parser in its
/// `ENUMERATORS` row.
pub fn parse_models_from_list(out: &str) -> Vec<String> {
    // SCRATCH NEUTER (red evidence for #935 slice A, PR #939) — the id
    // recognition is set aside and every non-empty line is taken as a model.
    // DO NOT MERGE.
    out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// Run `<program> <args>` without a console window, bounded by a timeout.
fn run_cli(program: &str, args: &str) -> Result<String, String> {
    // The program name is interpolated into a shell line on Windows (npm
    // shims are .cmd files that CreateProcess can't exec directly).
    if !program.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("invalid program name".into());
    }
    // `args` is only ever a literal (`--help`) or an `ENUMERATORS` row, both
    // compile-time constants — this checks that rather than trusting it, so
    // the shell line above stays obviously safe if a row is ever added.
    if !args.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ')) {
        return Err("invalid probe arguments".into());
    }
    #[cfg(target_os = "windows")]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = Command::new("cmd");
        c.args(["/C", &format!("{program} {args}")]).creation_flags(CREATE_NO_WINDOW);
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-lc", &format!("{program} {args}")]);
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
                        "`{program} {args}` failed (exit {:?}){}",
                        status.code(),
                        if first.is_empty() { String::new() } else { format!(": {first}") }
                    ));
                }
                return Ok(out);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(format!("`{program} {args}` timed out"));
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The probe itself, with process spawning factored out behind `run(program,
/// args) -> stdout` so the strategy can be tested without spawning an agent
/// CLI (constraint 3 — a real spawn burns the human's credits, and tests never
/// get to make that trade).
///
/// Two properties are structural here rather than asserted at a call site:
///
/// - **`--help` alone decides availability.** An enumerator that fails, times
///   out, or prints something unrecognisable must never turn an installed CLI
///   into a missing one — the launcher refuses a whole launch on `available:
///   false`.
/// - **Only a non-empty enumeration replaces the help-parsed list.** So the
///   failure path is exactly today's behaviour, not a worse one.
fn probe_with(program: &str, run: impl Fn(&str, &str) -> Result<String, String>) -> CliProbe {
    let help = match run(program, "--help") {
        Ok(help) => help,
        Err(e) => {
            return CliProbe {
                available: false,
                models: vec![],
                error: Some(if e.contains("cannot find") || e.contains("not found") || e.contains("os error 2") {
                    format!("'{program}' was not found on PATH")
                } else {
                    e
                }),
            }
        }
    };
    let mut models = parse_models_from_help(&help);
    if let Some(en) = enumerator_for(program) {
        // A second subprocess, once per CLI per app run (the cache below), on
        // the blocking pool and under the same timeout as the help run.
        let listed = run(program, en.args).map(|out| (en.parse)(&out)).unwrap_or_default();
        if !listed.is_empty() {
            models = listed;
        }
    }
    CliProbe { available: true, models, error: None }
}

fn probe_uncached(program: &str) -> CliProbe {
    probe_with(program, run_cli)
}

/// Probe an agent CLI (availability + model list). Successful probes are
/// cached for the app run; failures are NOT — a CLI installed while loomux
/// is running must become launchable on the next probe (spawns already see
/// it via fresh-PATH resolution).
///
/// Off-thread (#746 — `crate::blocking::run_blocking`, P1 of
/// `doc/design/performance.md`). On a cache miss this spawns the agent CLI with
/// `--help` and poll-joins it for up to eight seconds, which Tauri ran on the
/// webview thread: the longest single stall any command in the census could
/// produce, and a process spawn there besides (INV-2). A CLI with an
/// `ENUMERATORS` row spends a second such run on its list command, so its
/// worst case is two timeouts, not one — off-thread, once per app run, and
/// bounded either way.
///
/// **Reentrancy — an interleaving accepted, not a guard.** The cache lock is
/// taken twice, released between, and off-thread two probes of the same program
/// can therefore both miss it and both run `--help`. That is deliberate rather
/// than overlooked. The probe only READS the machine — PATH, and for an
/// `ENUMERATORS` CLI the providers that machine has configured — so both
/// computations agree and the second `insert` overwrites an identical value;
/// in the one case they could differ (a provider list that changed between the
/// two runs) both answers are equally current, so keeping the later write is
/// right rather than merely harmless. The whole cost of the race is one
/// duplicate subprocess, once per CLI per session. Holding the lock across `probe_uncached` would fix
/// a non-problem by creating a real one: the launcher probes several CLIs to
/// build its picker, and serializing them behind one lock would turn N
/// independent eight-second worst cases into their SUM — the exact stall this
/// conversion exists to remove, moved rather than deleted.
#[tauri::command]
pub async fn probe_agent_cli(program: String) -> CliProbe {
    crate::blocking::run_blocking(move || {
        let program = program.trim().to_lowercase();
        if let Some(hit) = cache().lock().unwrap().get(&program) {
            return hit.clone();
        }
        let probe = probe_uncached(&program);
        if probe.available {
            cache().lock().unwrap().insert(program, probe.clone());
        }
        probe
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const CLAUDE_STYLE_HELP: &str = "\
  --mcp-config <configs...>             Load MCP servers\n\
  --model <model>                       Model for the current session. Provide\n\
                                        an alias for the latest model (e.g.\n\
                                        'fable', 'opus', or 'sonnet') or a\n\
                                        model's full name (e.g.\n\
                                        'claude-fable-5').\n\
  -n, --name <name>                     Set a display name\n";

    /// Shaped like an `opencode --help` page — a fixture, not a transcript
    /// (constraint 3: no agent CLI is run to collect one). What it has to be
    /// is *representative on one point*: its `--model` section yields a
    /// non-empty help-parsed list (`gpt-5.1`), so "the enumerator replaced it"
    /// and "the enumerator fell back to it" are both observable.
    const OPENCODE_STYLE_HELP: &str = "\
  -h, --help            Print help\n\
  -m, --model <model>   Model to use, as a `provider/model` id (e.g.\n\
                        'opencode/gpt-5.1-codex'), or a configured alias\n\
                        like 'gpt-5.1'.\n\
  -s, --session <id>    Resume a session\n";

    /// `opencode models` output. The docs give the id format ("in the form of
    /// `provider/model`") but not the layout, so this fixture deliberately
    /// wraps the ids in the kinds of line a listing command might also print —
    /// the parser has to drop those rather than admit them.
    const OPENCODE_MODEL_LIST: &str = concat!(
        "Fetching models from models.dev\n",
        "\n",
        "anthropic/claude-sonnet-4-5\n",
        "anthropic/claude-haiku-4-5\n",
        "opencode/deepseek-v4-flash-free\n",
        "openrouter/anthropic/claude-sonnet-4\n",
        "\u{1b}[32mopencode/gpt-5.1-codex\u{1b}[0m\n",
        "anthropic/claude-sonnet-4-5\n",
        "See https://models.dev for the full catalog.\n",
    );

    fn opencode_ids() -> Vec<String> {
        ["anthropic/claude-sonnet-4-5", "anthropic/claude-haiku-4-5", "opencode/deepseek-v4-flash-free", "openrouter/anthropic/claude-sonnet-4", "opencode/gpt-5.1-codex"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn parses_claude_style_quoted_aliases() {
        let help = CLAUDE_STYLE_HELP;
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

    #[test]
    fn parses_a_provider_slash_model_listing() {
        let models = parse_models_from_list(OPENCODE_MODEL_LIST);
        assert_eq!(models, opencode_ids(), "ids in listed order, ANSI stripped, repeats dropped");
        assert!(!models.iter().any(|m| m.contains("models.dev")), "a URL is not an id: {models:?}");
        assert!(!models.iter().any(|m| m == "Fetching"), "prose leaked: {models:?}");
    }

    #[test]
    fn an_unrecognised_listing_yields_no_models() {
        // The failure mode that matters: a layout this parser has never seen
        // must produce NOTHING (so the caller keeps what it had), never a
        // half-scraped list of words.
        assert!(parse_models_from_list("No providers configured. Run `opencode auth login` first.\n").is_empty());
        assert!(parse_models_from_list("").is_empty());
    }

    #[test]
    fn opencode_takes_its_models_from_the_models_subcommand() {
        let calls = RefCell::new(Vec::new());
        let probe = probe_with("opencode", |program, args| {
            calls.borrow_mut().push(format!("{program} {args}"));
            match args {
                "--help" => Ok(OPENCODE_STYLE_HELP.to_string()),
                "models" => Ok(OPENCODE_MODEL_LIST.to_string()),
                other => panic!("probed an unexpected command: {other}"),
            }
        });
        assert!(probe.available);
        assert_eq!(probe.models, opencode_ids());
        assert!(
            !probe.models.iter().any(|m| m == "gpt-5.1"),
            "what the CLI itself reports replaces what its help prose suggested: {:?}",
            probe.models
        );
        // Exactly `models` — never `models --refresh`, which would re-pull a
        // remote catalog behind the human's back.
        let seen: Vec<String> = calls.borrow().clone();
        assert_eq!(seen, vec!["opencode --help".to_string(), "opencode models".to_string()]);
    }

    #[test]
    fn a_failed_models_subcommand_falls_back_to_help_and_stays_available() {
        let probe = probe_with("opencode", |_program, args| match args {
            "--help" => Ok(OPENCODE_STYLE_HELP.to_string()),
            _ => Err("`opencode models` timed out".into()),
        });
        assert!(probe.available, "an installed CLI whose list command failed is still installed");
        assert!(probe.error.is_none(), "and carries no error the launcher would refuse a launch on: {:?}", probe.error);
        assert_eq!(probe.models, vec!["gpt-5.1".to_string()], "falls back to the help-parsed list");
    }

    #[test]
    fn an_unreadable_models_listing_leaves_the_help_parsed_list_alone() {
        let probe = probe_with("opencode", |_program, args| match args {
            "--help" => Ok(OPENCODE_STYLE_HELP.to_string()),
            _ => Ok("No providers configured.\n".to_string()),
        });
        assert!(probe.available);
        assert_eq!(probe.models, vec!["gpt-5.1".to_string()], "an empty parse must not empty the list");
    }

    #[test]
    fn a_cli_without_an_enumerator_runs_only_help() {
        let calls = RefCell::new(Vec::new());
        let probe = probe_with("claude", |program, args| {
            calls.borrow_mut().push(format!("{program} {args}"));
            Ok(if args == "--help" { CLAUDE_STYLE_HELP.to_string() } else { String::new() })
        });
        assert!(probe.models.contains(&"sonnet".to_string()));
        let seen: Vec<String> = calls.borrow().clone();
        assert_eq!(seen, vec!["claude --help".to_string()], "claude has no list command; spawning one costs a subprocess for nothing");
    }

    #[test]
    fn a_missing_program_never_reaches_its_enumerator() {
        let calls = RefCell::new(Vec::new());
        let probe = probe_with("opencode", |_program, args| {
            calls.borrow_mut().push(args.to_string());
            Err("'opencode' was not found on PATH".into())
        });
        assert!(!probe.available);
        assert!(probe.models.is_empty());
        assert_eq!(calls.borrow().len(), 1, "nothing to enumerate for a CLI that isn't installed");
    }
}
