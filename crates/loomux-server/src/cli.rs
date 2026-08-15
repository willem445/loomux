//! The daemon's command line, and what it does with a valid one.
//!
//! `run` returns an [`Outcome`] — an exit code plus the two streams — instead
//! of printing and calling `std::process::exit`. That is what makes the
//! startup path testable at all: every assertion below is about what an
//! operator would see, and none of it needs a process to be spawned.

use std::path::PathBuf;

use crate::config::{ConfigError, ServerConfig};

/// The daemon started and did what it was asked.
pub const EXIT_OK: i32 = 0;
/// The command line or the config file was wrong. Includes §1.2's routable
/// bind refusal: it is a configuration error, not a runtime failure.
pub const EXIT_CONFIG: i32 = 2;
/// The config is fine and a listener would be allowed — but this build has no
/// listener (slice C2 adds it).
///
/// A distinct non-zero code rather than `EXIT_OK`, because the caller of a
/// daemon is usually a service manager: exiting 0 having served nothing is
/// indistinguishable from "ran, then shut down cleanly", and that is exactly
/// the lie that gets a skeleton deployed and reported as working.
pub const EXIT_NO_LISTENER: i32 = 3;

/// What the parsed command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Version,
    Run {
        config: Option<PathBuf>,
        /// `--check-config`: resolve everything, print the summary, exit 0
        /// without attempting to serve.
        check_only: bool,
    },
}

/// An exit code and the two streams that go with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub out: String,
    pub err: String,
}

pub const USAGE: &str = "\
loomux-server — the remote loomux engine daemon (#888)

USAGE:
    loomux-server [--config <path>] [--check-config]
    loomux-server --help | --version

OPTIONS:
    --config <path>    YAML config file. Without it, every setting takes its
                       default, which binds loopback.
    --check-config     Resolve the config, print the summary, and exit without
                       serving.
    --help, -h         This text.
    --version, -V      Version.

This build has NO LISTENER: it resolves its configuration and exits (see
doc/design/remote-engine-daemon.md). The daemon has no authentication at all —
read doc/design/remote-engine-protocol.md §1.2 and §1.3 before running it
anywhere but a workstation you reach over SSH.
";

/// Parse argv (WITHOUT argv[0]).
///
/// Errors are `String` rather than [`ConfigError`] because a usage error has
/// nothing to do with the config file and the caller does not branch on which
/// one it was — both exit [`EXIT_CONFIG`].
pub fn parse_args<I, S>(args: I) -> Result<Invocation, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut config: Option<PathBuf> = None;
    let mut check_only = false;
    let mut it = args.into_iter();

    while let Some(arg) = it.next() {
        match arg.as_ref() {
            "--help" | "-h" => return Ok(Invocation::Help),
            "--version" | "-V" => return Ok(Invocation::Version),
            "--check-config" => check_only = true,
            "--config" => {
                let value = it
                    .next()
                    .ok_or_else(|| "--config needs a path".to_string())?;
                config = Some(PathBuf::from(value.as_ref()));
            }
            other if other.starts_with("--config=") => {
                config = Some(PathBuf::from(&other["--config=".len()..]));
            }
            // Fail closed on anything unrecognised. A daemon that ignores a
            // flag it does not know is a daemon that silently ignores
            // `--allow-routable-bind`-shaped intent from a future version.
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }

    Ok(Invocation::Run { config, check_only })
}

/// Execute a parsed invocation.
pub fn run(inv: Invocation, version: &str) -> Outcome {
    match inv {
        Invocation::Help => Outcome {
            code: EXIT_OK,
            out: USAGE.to_string(),
            err: String::new(),
        },
        Invocation::Version => Outcome {
            code: EXIT_OK,
            out: format!("loomux-server {version}\n"),
            err: String::new(),
        },
        Invocation::Run { config, check_only } => {
            let loaded = match &config {
                Some(path) => ServerConfig::load(path),
                None => Ok(ServerConfig::default()),
            };
            // One failure path, because there is one gate: a config that would
            // bind a routable address without saying so does not load at all
            // (config.rs), so there is no second check to perform here and no
            // way to reach the summary below holding an unchecked target.
            let cfg = match loaded {
                Ok(cfg) => cfg,
                Err(e) => return config_failure(e),
            };
            let target = cfg.listen();

            let mut out = String::new();
            out.push_str(&format!("loomux-server {version}\n"));
            out.push_str(&format!(
                "config:     {}\n",
                config
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(defaults; no --config given)".to_string())
            ));
            out.push_str(&format!("listen:     {}\n", target.describe()));
            out.push_str(&format!("state root: {}\n", cfg.state_root().display()));

            let mut err = String::new();
            if target.is_routable() {
                // The loud half of "an explicit, loudly-named flag". The flag
                // makes the bind possible; this makes it impossible to have
                // done by accident and not know.
                err.push_str(
                    "WARNING: allow_routable_bind is set, so this daemon is reachable from other \
                     machines. It has NO AUTHENTICATION: anything that can reach it can run \
                     arbitrary commands (doc/design/remote-engine-protocol.md §1.3).\n",
                );
            }

            if check_only {
                Outcome {
                    code: EXIT_OK,
                    out,
                    err,
                }
            } else {
                err.push_str(
                    "no listener in this build: the daemon skeleton resolves its configuration and \
                     stops (#888 slice C1a; the listener is slice C2). Use --check-config to make \
                     this the expected outcome.\n",
                );
                Outcome {
                    code: EXIT_NO_LISTENER,
                    out,
                    err,
                }
            }
        }
    }
}

fn config_failure(e: ConfigError) -> Outcome {
    Outcome {
        code: EXIT_CONFIG,
        out: String::new(),
        err: format!("loomux-server: {e}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_LISTEN;

    fn run_args(args: &[&str]) -> Outcome {
        run(parse_args(args.iter().copied()).expect("parses"), "0.0.0-test")
    }

    #[test]
    fn no_arguments_means_run_with_defaults() {
        assert_eq!(
            parse_args::<_, &str>([]).expect("parses"),
            Invocation::Run {
                config: None,
                check_only: false
            }
        );
    }

    #[test]
    fn the_config_path_is_accepted_in_both_spellings() {
        for args in [vec!["--config", "a.yml"], vec!["--config=a.yml"]] {
            assert_eq!(
                parse_args(args.iter().copied()).expect("parses"),
                Invocation::Run {
                    config: Some(PathBuf::from("a.yml")),
                    check_only: false
                }
            );
        }
    }

    #[test]
    fn an_unrecognized_argument_is_refused_rather_than_ignored() {
        for args in [
            vec!["--serve"],
            vec!["--check-config", "--allow-routable-bind"],
            vec!["-x"],
            vec!["extra"],
        ] {
            assert!(
                parse_args(args.iter().copied()).is_err(),
                "{args:?} must be refused: silently ignoring an unknown flag is how a future \
                 version's security-relevant option becomes a no-op"
            );
        }
        assert!(parse_args(["--config"]).is_err(), "--config with no value");
    }

    #[test]
    fn check_config_on_a_default_config_succeeds_and_reports_loopback() {
        let outcome = run_args(&["--check-config"]);
        assert_eq!(outcome.code, EXIT_OK, "stderr was: {}", outcome.err);
        assert!(outcome.out.contains(DEFAULT_LISTEN), "{}", outcome.out);
        assert!(outcome.out.contains("loopback"), "{}", outcome.out);
        assert!(
            !outcome.err.contains("WARNING"),
            "a loopback bind must not warn: a warning that fires every time is one nobody reads"
        );
    }

    #[test]
    fn a_run_without_check_config_refuses_to_look_like_a_running_daemon() {
        let outcome = run_args(&[]);
        assert_eq!(
            outcome.code, EXIT_NO_LISTENER,
            "a skeleton that exits 0 having served nothing is indistinguishable from a clean \
             shutdown, which is how it gets deployed and reported as working"
        );
        assert!(outcome.err.contains("no listener"), "{}", outcome.err);
    }

    #[test]
    fn a_config_that_names_a_routable_bind_exits_config_and_serves_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("loomux-server.yml");
        std::fs::write(&path, "listen: \"0.0.0.0:8788\"\n").expect("write");

        let outcome = run_args(&["--check-config", "--config", path.to_str().expect("utf8")]);
        assert_eq!(outcome.code, EXIT_CONFIG, "out was: {}", outcome.out);
        assert!(
            outcome.err.contains("allow_routable_bind"),
            "{}",
            outcome.err
        );
        assert!(
            outcome.out.is_empty(),
            "a refused config must not print a startup summary — it did not start: {}",
            outcome.out
        );
    }

    #[test]
    fn an_allowed_routable_bind_warns_on_stderr() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("loomux-server.yml");
        std::fs::write(
            &path,
            "listen: \"0.0.0.0:8788\"\nallow_routable_bind: true\n",
        )
        .expect("write");

        let outcome = run_args(&["--check-config", "--config", path.to_str().expect("utf8")]);
        assert_eq!(outcome.code, EXIT_OK, "stderr was: {}", outcome.err);
        assert!(
            outcome.err.contains("NO AUTHENTICATION"),
            "an exposed unauthenticated daemon must say so every time it starts: {}",
            outcome.err
        );
    }

    #[test]
    fn a_missing_config_file_is_an_error_not_a_silent_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.yml");
        let outcome = run_args(&["--check-config", "--config", missing.to_str().expect("utf8")]);
        assert_eq!(outcome.code, EXIT_CONFIG);
        assert!(
            outcome.err.contains("cannot read config"),
            "asking for a config file that is not there must not quietly run with defaults: {}",
            outcome.err
        );
    }

    #[test]
    fn help_and_version_serve_nothing_and_succeed() {
        for args in [vec!["--help"], vec!["-h"], vec!["--version"], vec!["-V"]] {
            let outcome = run_args(&args);
            assert_eq!(outcome.code, EXIT_OK, "{args:?}");
            assert!(!outcome.out.is_empty(), "{args:?}");
        }
        assert!(run_args(&["--help"]).out.contains("NO LISTENER"));
    }
}
