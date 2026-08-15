//! The daemon binary. Deliberately thin: argv in, [`cli::Outcome`] out,
//! streams printed, exit code returned. Everything worth testing lives in the
//! library next to it, so the startup path is covered by unit tests rather
//! than by spawning a process (`crates/loomux-server/src/cli.rs`).

use std::io::Write;
use std::process::ExitCode;

use loomux_server::cli::{self, EXIT_CONFIG};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let outcome = match cli::parse_args(&args) {
        Ok(inv) => cli::run(inv, env!("CARGO_PKG_VERSION")),
        Err(msg) => cli::Outcome {
            code: EXIT_CONFIG,
            out: String::new(),
            err: format!("loomux-server: {msg}\n\n{}", cli::USAGE),
        },
    };

    if !outcome.out.is_empty() {
        print!("{}", outcome.out);
        let _ = std::io::stdout().flush();
    }
    if !outcome.err.is_empty() {
        eprint!("{}", outcome.err);
        let _ = std::io::stderr().flush();
    }

    // `ExitCode` rather than `std::process::exit`, so destructors run and the
    // streams above are actually flushed on every platform.
    ExitCode::from(u8::try_from(outcome.code).unwrap_or(1))
}
