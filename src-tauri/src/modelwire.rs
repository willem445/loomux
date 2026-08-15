//! Ask an agent CLI, in its own control protocol, which models this host offers
//! (#993).
//!
//! `cliprobe.rs` answers "which model strings does this CLI *accept*" by reading
//! what the CLI prints unprompted — its `--help` prose, or a list subcommand
//! where one exists. That is as far as printing gets you. Claude Code has no
//! list subcommand (`claude models` starts a chat), but it does speak a control
//! protocol on its `stream-json` input stream, and one `list_models` control
//! request over it returns the model picker the human's own install would show:
//! the real per-host ids, and each model's supported effort levels. Neither is
//! knowable from any page this repo could cite.
//!
//! **This module deliberately understands nothing about the reply.** It writes
//! one request line, reads stdout, and hands the bytes to the frontend, where
//! `src/modelwire.ts` parses them under `node --test`. That split is the whole
//! design: the parsing is the fragile part, it is written against a payload
//! shape the vendor does not document, and it belongs where a test round costs
//! a second rather than a CI build. What stays here is what only a backend can
//! do — spawn a process, write its stdin, and bound it.
//!
//! **Two facts about this request that loomux cannot verify, and treats
//! accordingly.** Anthropic types the control envelope
//! (`@anthropic-ai/claude-agent-sdk`'s `sdk.d.ts`; the request is
//! `SDKControlListModelsRequest = { subtype: 'list_models' }`) and documents
//! every flag below at <https://code.claude.com/docs/en/cli-reference>. But the
//! string `list_models` appears nowhere in Anthropic's documentation corpus, so
//! the claim that it is a metadata message which does **not** consume
//! completion credits is third-party (`stablyai/orca`, MIT) and UNVERIFIED. Per
//! constraint 3, loomux does not bet the human's money on it: nothing here runs
//! on its own: `list_cli_models` is only ever reached from an explicit human
//! gesture, and the automatic model list stays exactly what `cliprobe.rs` made
//! it. The human's own live validation is what could change that.
//!
//! Adding Copilot or opencode is a `PROTOCOLS` row, not a branch — the
//! `ENUMERATORS`/`CLI_CAPS` data pattern this repo already uses for per-CLI
//! differences.

use serde::Serialize;

/// The correlation id put on the request, and handed back with the reply so the
/// frontend parser can match the answer to it.
///
/// A constant, not a generated id: the request is the only one loomux sends on
/// a freshly spawned process, so there is nothing for a random value to
/// disambiguate — and constraint 2 bars the `getrandom`-backed crates that would
/// produce one. It travels back on the reply rather than being duplicated as a
/// literal in `src/modelwire.ts`, because a correlation id that drifted between
/// the sender and the reader would silently stop correlating.
/// A macro rather than a plain `const` because the id has to appear inside a
/// `concat!` (which takes literals, not constants) *and* be returned at
/// runtime. Spelling it twice is exactly the drift this whole correlation
/// mechanism exists to avoid, so it is spelled once, here.
macro_rules! request_id {
    () => {
        "loomux-list-models"
    };
}
const REQUEST_ID: &str = request_id!();

/// Hard cap on the stdout handed over IPC. A control reply is a few kilobytes;
/// anything approaching this is a CLI streaming something else entirely, and
/// shipping megabytes into the webview to be JSON-parsed line by line would be
/// a stall with no upside. Truncation degrades exactly like an unreadable
/// reply — the parser finds no complete line and the caller keeps its seed.
const MAX_OUTPUT: usize = 256 * 1024;

#[derive(Clone, Serialize)]
pub struct CliModelReply {
    /// The CLI's stdout, verbatim and capped at `MAX_OUTPUT`. Parsed by
    /// `src/modelwire.ts`.
    pub output: String,
    /// The id on the request loomux sent. Empty when nothing was sent.
    pub request_id: String,
    /// Human-readable reason nothing could be asked, or `None`. Diagnostic
    /// only: every surface degrades to its seed list either way.
    pub error: Option<String>,
}

/// How one CLI answers "which models does this host offer". Per-CLI differences
/// live here as DATA, the way `ENUMERATORS` and `CLI_CAPS` carry the rest of
/// them.
struct Protocol {
    /// The program name as probed (`list_cli_models` lower-cases first).
    program: &'static str,
    /// Arguments appended to the program. Compile-time constants only — never
    /// anything a caller supplied; `run_cli` re-checks that rather than
    /// trusting it.
    args: &'static str,
    /// One line written to the CLI's stdin, after which stdin is closed.
    request: &'static str,
}

/// Claude Code's `stream-json` control protocol.
///
/// The flags are all documented at
/// <https://code.claude.com/docs/en/cli-reference>: `--input-format` and
/// `--output-format` "Specify input/output format for print mode (options:
/// `text`, `stream-json`)", and `--verbose` "Enable verbose logging, shows full
/// turn-by-turn output". `--verbose` is carried because print mode is reported
/// to reject `stream-json` output without it; no Anthropic page states that
/// requirement, so it is an UNVERIFIED belt to a documented flag — passing it
/// costs nothing if the requirement does not exist.
///
/// The request body is Anthropic's own typed shape: `SDKControlRequest = {
/// type: 'control_request', request_id: string, request: SDKControlRequestInner
/// }`, with `SDKControlListModelsRequest = { subtype: 'list_models' }` a member
/// of that union (`sdk.d.ts`, read 2026-08-14). It contains no prompt and asks
/// for no completion.
const PROTOCOLS: &[Protocol] = &[Protocol {
    program: "claude",
    args: "-p --input-format stream-json --output-format stream-json --verbose",
    request: concat!(
        r#"{"type":"control_request","request_id":""#,
        request_id!(),
        r#"","request":{"subtype":"list_models"}}"#
    ),
}];

fn protocol_for(program: &str) -> Option<&'static Protocol> {
    PROTOCOLS.iter().find(|p| p.program == program)
}

/// The probe itself, with process spawning factored out behind `run(program,
/// args, stdin) -> stdout` so every rule below is testable without spawning an
/// agent CLI (constraint 3 — a real spawn may spend the human's money, and a
/// test never gets to make that trade).
fn ask_with(
    program: &str,
    run: impl Fn(&str, &str, Option<&str>) -> Result<String, String>,
) -> CliModelReply {
    let Some(proto) = protocol_for(program) else {
        return CliModelReply {
            output: String::new(),
            request_id: String::new(),
            error: Some(format!("loomux has no list-models protocol for '{program}'")),
        };
    };
    match run(program, proto.args, Some(proto.request)) {
        Ok(mut out) => {
            if out.len() > MAX_OUTPUT {
                // On a char boundary, so the string stays valid UTF-8 — and by
                // truncating rather than rejecting, a reply that arrived before
                // the flood is still readable.
                let mut cut = MAX_OUTPUT;
                while cut > 0 && !out.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.truncate(cut);
            }
            CliModelReply { output: out, request_id: REQUEST_ID.to_string(), error: None }
        }
        Err(e) => CliModelReply { output: String::new(), request_id: REQUEST_ID.to_string(), error: Some(e) },
    }
}

/// Ask `program` for its model list.
///
/// **Deliberately not cached, and deliberately not called on its own.** Both
/// follow from the same constraint: this spawns an agent CLI, and the claim
/// that the request costs the human nothing is UNVERIFIED (see the module
/// note). So it runs once per explicit human gesture and no more — there is no
/// background path that reaches it, and the answer is memoized on the frontend
/// (`ModelCatalog`) for the app run rather than here, which keeps the one place
/// that decides *when to spend* the same as the one place a human clicked.
///
/// Off-thread (#746 — `crate::blocking::run_blocking`, P1 of
/// `doc/design/performance.md`), for the same reason `probe_agent_cli` is: a
/// process spawn plus a poll-join for up to the shared timeout must not sit on
/// the webview thread.
#[tauri::command]
pub async fn list_cli_models(program: String) -> CliModelReply {
    crate::blocking::run_blocking(move || {
        let program = program.trim().to_lowercase();
        ask_with(&program, crate::cliprobe::run_cli)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A `list_models` reply, written from Anthropic's published `ModelInfo`
    /// type — NOT captured from a run (constraint 3). What it has to be is
    /// representative on one point: it is a complete `control_response` line,
    /// so "the bytes reached the frontend intact" is observable.
    const REPLY: &str = concat!(
        r#"{"type":"system","subtype":"init","session_id":"abc"}"#,
        "\n",
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"loomux-list-models","#,
        r#""response":{"models":[{"value":"opus","displayName":"Opus","description":"","supportsEffort":true,"#,
        r#""supportedEffortLevels":["low","high"]}]}}}"#,
        "\n"
    );

    #[test]
    fn the_claude_protocol_asks_a_control_request_and_nothing_else() {
        // The pin that matters most in this file: the request body must carry
        // no prompt. A `-p` invocation that reached the model would spend the
        // human's money on every click (constraint 3), and the difference is
        // exactly these bytes.
        let calls = RefCell::new(Vec::new());
        let reply = ask_with("claude", |program, args, stdin| {
            calls.borrow_mut().push((program.to_string(), args.to_string(), stdin.map(str::to_string)));
            Ok(REPLY.to_string())
        });
        let seen = calls.borrow().clone();
        assert_eq!(seen.len(), 1, "one spawn per ask, never a retry loop");
        let (program, args, stdin) = &seen[0];
        assert_eq!(program, "claude");
        assert_eq!(args, "-p --input-format stream-json --output-format stream-json --verbose");
        let body = stdin.as_deref().expect("the request is written to stdin, never interpolated into the shell line");
        assert_eq!(
            body,
            r#"{"type":"control_request","request_id":"loomux-list-models","request":{"subtype":"list_models"}}"#
        );
        assert!(!body.contains("\"prompt\""), "a prompt would make this a completion: {body}");
        assert!(!body.contains("\"user\""), "no user turn may ride along: {body}");
        assert_eq!(reply.request_id, REQUEST_ID, "the id travels back so the parser can correlate on it");
        assert!(
            body.contains(REQUEST_ID),
            "the id loomux reports must be the id it actually sent — a drift here would silently stop correlating: {body}"
        );
        assert_eq!(reply.output, REPLY, "stdout is handed over verbatim — this module parses nothing");
        assert!(reply.error.is_none());
    }

    #[test]
    fn a_cli_with_no_protocol_row_is_never_spawned() {
        // The safety property of the data table: an unknown program must not
        // fall through to some default invocation. Nothing to ask means no
        // process at all, and a reason the caller can show.
        let calls = RefCell::new(0);
        let reply = ask_with("copilot", |_p, _a, _s| {
            *calls.borrow_mut() += 1;
            Ok(REPLY.to_string())
        });
        assert_eq!(*calls.borrow(), 0, "spawning an agent CLI on a guess is the one thing this must not do");
        assert!(reply.output.is_empty());
        assert!(reply.request_id.is_empty(), "nothing was sent, so there is no id to correlate on");
        assert!(reply.error.as_deref().unwrap_or_default().contains("copilot"));
    }

    #[test]
    fn a_failed_run_reports_why_and_carries_no_output() {
        let reply = ask_with("claude", |_p, _a, _s| Err("`claude` was not found on PATH".into()));
        assert!(reply.output.is_empty());
        assert_eq!(reply.error.as_deref(), Some("`claude` was not found on PATH"));
    }

    #[test]
    fn a_flood_of_output_is_truncated_on_a_char_boundary() {
        // A CLI that streams something else entirely must not push megabytes
        // through IPC. Truncating mid-codepoint would produce invalid UTF-8,
        // so the cut walks back to a boundary.
        let flood = format!("{}\u{1f600}", "x".repeat(MAX_OUTPUT - 2));
        let reply = ask_with("claude", |_p, _a, _s| Ok(flood.clone()));
        assert!(reply.output.len() <= MAX_OUTPUT, "capped: {}", reply.output.len());
        assert!(reply.output.is_char_boundary(reply.output.len()));
        assert!(!reply.output.contains('\u{fffd}'), "no replacement char — the cut is a boundary, not a byte slice");
    }
}
