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
//! **Two facts about this request that loomux cannot verify.** Anthropic types
//! the control envelope (`@anthropic-ai/claude-agent-sdk`'s `sdk.d.ts`; the
//! request is `SDKControlListModelsRequest = { subtype: 'list_models' }`) and
//! documents every flag below at
//! <https://code.claude.com/docs/en/cli-reference>. But the string
//! `list_models` appears nowhere in Anthropic's documentation corpus, so the
//! claim that it is a metadata message which does **not** consume completion
//! credits is third-party (`stablyai/orca`, MIT) and UNVERIFIED by the vendor.
//!
//! **What loomux does about that changed in #1002, by the human's own
//! direction, and this module is where the change lives.** #993 shipped the
//! conservative reading of constraint 3: nothing here ran on its own, and the
//! request was only ever reached from an explicit click. The human then
//! validated the cost themselves and directed that detection become automatic
//! and interaction-free — so the ask now runs unbidden, once per CLI per app
//! run, from [`start_startup_sweep`]. The record of that decision, and what
//! remains unverified about it, is `doc/design/model-catalog.md` §Credit
//! safety.
//!
//! **The bound that replaced the click, and why it is a boundary rather than a
//! budget.** A gesture was a natural rate limit — one ask per finger — and
//! automatic detection has none. The tempting shape is to keep
//! [`list_cli_models`] an ASK and ration it with a memo, so a picker that opens
//! before the sweep has answered can start its own. That was considered and
//! rejected (`doc/design/model-catalog.md` §"Why the command cannot spawn"): it
//! puts a subprocess spawn back on a render path — precisely the boundary #993
//! drew — and then needs two separate guards to make it safe again.
//!
//! So the spawn lives in exactly ONE place. [`start_startup_sweep`] is the only
//! function in loomux that reaches an agent CLI unbidden; [`list_cli_models`]
//! is a **lookup**, incapable of spawning anything, and a picker that asks
//! before the sweep has answered is simply told there is nothing yet. The
//! answer reaches it moments later on the `models-detected` event instead. The
//! property that buys: no render path, present or future, can spend the human's
//! money — enforced by there being no code here that could.
//!
//! Its cost, accepted by the human's own direction: a CLI installed *after*
//! loomux started is not detected until the next restart. There is no re-ask
//! affordance, by the same direction.
//!
//! Adding Copilot or opencode is a `PROTOCOLS` row, not a branch — the
//! `ENUMERATORS`/`CLI_CAPS` data pattern this repo already uses for per-CLI
//! differences.

use loomux_engine::model::SUPPORTED_CLIS;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

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

/// Whether a reply is worth remembering for the rest of the app run.
///
/// **The backend cannot judge this as finely as the frontend can, and says so
/// rather than pretending otherwise.** This module parses nothing (see the
/// header), so "worth keeping" here can only mean *the CLI ran and said
/// something*: a reply that errored is a CLI that was missing, refused the
/// request, or timed out, and every one of those is a state a later ask might
/// find changed. `src/modelcatalog.ts`'s `reportWorthKeeping` applies the real
/// test — did the reply parse into models — one layer up.
///
/// The residual: output that is non-empty but unparseable IS kept here, so an
/// upgrade that fixes the shape is not re-asked within the app run. That is
/// accepted rather than overlooked. Judging it would mean parsing the reply in
/// Rust, which is the split this module exists to avoid, and the frontend memo
/// in front of it does not keep such an answer either — so the surfaces still
/// show their seed, and a restart re-detects (the human's stated model for
/// #1002, there being no re-ask affordance any more).
fn worth_keeping(reply: &CliModelReply) -> bool {
    reply.error.is_none() && !reply.output.trim().is_empty()
}

/// The error a lookup reports when the sweep has not answered for this program
/// yet. Diagnostic only — every surface degrades to its seed either way — but
/// deliberately distinct from the no-protocol message, because the two are
/// different futures: this one is about to change, and that one never will.
const NOT_YET_DETECTED: &str = "no models detected for this CLI yet";

/// What the startup sweep learned, by lower-cased program.
///
/// A value rather than a static so its rules are testable: a process-global map
/// is shared by every `cargo test` thread at once, and a test that reached one
/// would be pinning the other tests' interleaving instead of this module's
/// rules. The process gets exactly one instance, from [`memo`].
///
/// No gate, no in-flight tracking, no re-entrancy story — because there is only
/// ever one writer. [`start_startup_sweep`] runs its CLIs sequentially on one
/// thread and is started once; [`Memo::read`] cannot write at all. That
/// simplicity is a direct consequence of the boundary in the module header: a
/// design where a render path could also ask would need every one of those
/// things.
#[derive(Default)]
struct Memo {
    kept: Mutex<HashMap<String, CliModelReply>>,
}

impl Memo {
    /// Ask the CLI and remember the answer if it is [`worth_keeping`]. **The
    /// sweep's method — nothing reachable from the webview calls it.** `run` is
    /// injected for the same reason [`ask_with`] takes it: constraint 3 bars a
    /// test from spawning a real agent CLI.
    fn ask(
        &self,
        program: &str,
        run: impl Fn(&str, &str, Option<&str>) -> Result<String, String>,
    ) -> CliModelReply {
        // RED-BEFORE-GREEN MUTATION D (#1020): the store removed, so the sweep's
        // answer never reaches the lookup.
        ask_with(program, run)
    }

    /// What is known about `program` right now. Never spawns, never blocks on
    /// anything but a `HashMap` lookup, and never writes.
    ///
    /// Three answers, and a caller can tell them apart by the error alone:
    /// the sweep's reply; [`NOT_YET_DETECTED`]; or the no-protocol message for
    /// a CLI loomux has no way to ask at all, taken straight from [`ask_with`]
    /// with a runner that can never be reached — the table decides before the
    /// runner would be called, and passing one that panics is how this states
    /// that rather than trusting it.
    fn read(&self, program: &str) -> CliModelReply {
        if protocol_for(program).is_none() {
            return ask_with(program, |_, _, _| unreachable!("a lookup never runs a CLI"));
        }
        if let Some(hit) = self.kept.lock().unwrap().get(program) {
            return hit.clone();
        }
        CliModelReply {
            output: String::new(),
            request_id: String::new(),
            error: Some(NOT_YET_DETECTED.to_string()),
        }
    }
}

fn memo() -> &'static Memo {
    static MEMO: OnceLock<Memo> = OnceLock::new();
    MEMO.get_or_init(Memo::default)
}

/// The event name carrying a sweep result to the webview. Read by
/// `src/pty.ts`'s `onModelsDetected`, which is the only place that spells it on
/// the frontend side (constraint 5).
pub const MODELS_DETECTED_EVENT: &str = "models-detected";

/// One CLI's detection result, pushed as it lands.
#[derive(Clone, Serialize)]
pub struct ModelsDetected {
    pub program: String,
    pub reply: CliModelReply,
}

/// The sweep, with every effect injected so the ORDER and the COUNT of asks are
/// testable without a process (constraint 3).
///
/// Two passes per CLI, and they answer different questions. `probe` is
/// `cliprobe`'s help/enumerator read — the thing every picker already merges,
/// warmed here so the launcher's first paint does not wait eight seconds for
/// it. `ask` is the control request, and it runs ONLY where a `PROTOCOLS` row
/// exists: a CLI without one is never spawned for a list it has no way to
/// give, which is the same safety property `a_cli_with_no_protocol_row_is_never_spawned`
/// pins on the ask itself.
///
/// Sequential, not parallel. Four CLIs times a bounded probe is seconds of
/// background work on a thread nothing is waiting on, and spawning eight
/// subprocesses at once during app startup is the kind of thing that competes
/// with the window actually appearing.
fn sweep_with(
    clis: &[&str],
    probe: impl Fn(&str),
    ask: impl Fn(&str) -> CliModelReply,
    emit: impl Fn(ModelsDetected),
) {
    // `&cli` destructures the `&&str` the iterator yields, so every call below
    // takes the `&str` its parameter is written as rather than leaning on a
    // coercion through a closure's `Fn` bound.
    for &cli in clis {
        probe(cli);
        if protocol_for(cli).is_none() {
            continue;
        }
        // RED-BEFORE-GREEN MUTATION E (#1020): the emit removed, so nothing the
        // sweep learns ever reaches a form that is already open.
        let _reply = ask(cli);
        let _ = &emit;
    }
}

/// Detect every supported CLI's models in the background, once, at startup
/// (#1020).
///
/// **This is the automatic path the human directed in #1002, and the only
/// place in loomux that reaches an agent CLI without being asked to.** It is
/// bounded three ways: started once, sequential, and only for CLIs with a
/// `PROTOCOLS` row. Every other route to a list-models spawn was deleted rather
/// than guarded — see the module header.
///
/// Its own thread, like `metrics::start` and the orchestration loops beside it
/// in `lib.rs`'s setup: `probe_agent_cli` alone can sit for eight seconds per
/// CLI, and setup must return for the window to appear.
///
/// **The emit can outrun the listener, and the lookup is what covers it.** The
/// webview registers `onModelsDetected` when `main.ts` boots, which is normally
/// long before a spawned CLI answers — but nothing orders the two, and a CLI
/// that is not installed fails fast enough to lose that race. A missed event is
/// not a lost answer: it is in the memo, and the first picker paint reads it
/// out through [`list_cli_models`]. Push for the forms already open, pull for
/// the ones that open later; one memo behind both.
pub fn start_startup_sweep(app: AppHandle) {
    std::thread::spawn(move || {
        sweep_with(
            &SUPPORTED_CLIS,
            |cli| {
                crate::cliprobe::probe_cached(cli);
            },
            |cli| memo().ask(cli, crate::cliprobe::run_cli),
            |detected| {
                // Best-effort, like every other emit in this app: a webview
                // that has gone away is not an error worth propagating out of a
                // background sweep.
                let _ = app.emit(MODELS_DETECTED_EVENT, detected);
            },
        );
    });
}

/// What the startup sweep found for `program` — a **lookup, not an ask**
/// (#1020).
///
/// The name is #993's and the wire shape is unchanged, but what this does is
/// not: it can no longer spawn anything. The frontend calls it on a picker's
/// render path, which under #993 was the one thing forbidden; it is allowed now
/// precisely because the spawn is gone, not because the rule relaxed. A CLI
/// whose sweep result has not landed yet answers [`NOT_YET_DETECTED`], and the
/// webview learns the real answer on the `models-detected` event moments later.
///
/// **Still delegated off-thread, though its body no longer needs it** (#746 —
/// `crate::blocking::run_blocking`, P1 of `doc/design/performance.md`). What is
/// left here is one uncontended mutex around a `HashMap` lookup, which INV-1
/// would happily let run on the webview thread as a `Class::Cheap` sync
/// command. It stays async anyway, for two reasons that outlast this slice:
/// it keeps the shape identical to `probe_agent_cli` beside it, and it means a
/// future protocol row that makes this expensive again is a change to this
/// module rather than a change to this module *plus* the `SYNC_COMMANDS`
/// census. The cost is one thread hop, a handful of times per app run.
///
/// **Reentrancy.** Two webview calls for the same program can interleave
/// freely, and there is nothing to protect: both only read, the map's single
/// writer is the sweep thread, and the `Mutex` is what orders them.
#[tauri::command]
pub async fn list_cli_models(program: String) -> CliModelReply {
    crate::blocking::run_blocking(move || memo().read(program.trim().to_lowercase().as_str())).await
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
    fn a_lookup_answers_from_the_sweep_without_running_anything() {
        // The #1020 boundary, stated as a behaviour rather than as a comment:
        // the webview's route to this module is `read`, and `read` has no
        // runner to call. The `unreachable!` inside it is the enforcement; this
        // is the test that says a real answer still comes back through it.
        let memo = Memo::default();
        let runs = RefCell::new(0);
        memo.ask("claude", |_p, _a, _s| {
            *runs.borrow_mut() += 1;
            Ok(REPLY.to_string())
        });
        let seen = memo.read("claude");
        assert_eq!(*runs.borrow(), 1, "the sweep ran the CLI exactly once; the lookup ran nothing");
        assert_eq!(seen.output, REPLY, "and the lookup hands back what the sweep got, verbatim");
        assert_eq!(seen.request_id, REQUEST_ID);
        assert!(seen.error.is_none());
    }

    #[test]
    fn a_lookup_before_the_sweep_answers_says_so_rather_than_asking() {
        // The race the design accepts: a picker can paint before the sweep has
        // reached its CLI. The honest answer is "nothing yet" — NOT a spawn,
        // which is the whole point, and not the no-protocol message either,
        // because the two have different futures and a caller may want to tell
        // them apart.
        let memo = Memo::default();
        let reply = memo.read("claude");
        assert!(reply.output.is_empty());
        assert_eq!(reply.error.as_deref(), Some(NOT_YET_DETECTED));
        assert_ne!(
            reply.error.as_deref(),
            memo.read("copilot").error.as_deref(),
            "a CLI that has not been swept yet must not read the same as one that can never be asked"
        );
    }

    #[test]
    fn a_failed_ask_is_never_memoized() {
        // The worth-keeping rule. A CLI that was missing or timed out when the
        // sweep ran leaves nothing behind, so the lookup keeps saying "nothing
        // yet" rather than serving the failure as if it were an answer — which
        // is what a surface needs to degrade to its seed.
        let memo = Memo::default();
        let failed = memo.ask("claude", |_p, _a, _s| Err("`claude` was not found on PATH".into()));
        assert!(failed.output.is_empty());
        assert_eq!(failed.error.as_deref(), Some("`claude` was not found on PATH"));
        assert_eq!(
            memo.read("claude").error.as_deref(),
            Some(NOT_YET_DETECTED),
            "the failure was not kept — a lookup must not report it as this CLI's model list"
        );
    }

    #[test]
    fn an_empty_reply_is_never_memoized_either() {
        // A CLI that ran and printed nothing is the same class of answer as one
        // that failed: an older build without the control request. This module
        // parses nothing, so "it said something" is the strictest test it can
        // honestly apply — and whitespace is not something.
        let memo = Memo::default();
        memo.ask("claude", |_p, _a, _s| Ok("   \n".to_string()));
        assert_eq!(memo.read("claude").error.as_deref(), Some(NOT_YET_DETECTED), "nothing was said, so nothing was kept");
    }

    #[test]
    fn the_memo_never_holds_a_program_it_would_not_spawn() {
        // The `PROTOCOLS`-table safety property, restated for the memo: an
        // unknown program fills no slot, so it can never be answered from an
        // entry some other code path put there.
        let memo = Memo::default();
        let calls = RefCell::new(0);
        let reply = memo.ask("copilot", |_p, _a, _s| {
            *calls.borrow_mut() += 1;
            Ok(REPLY.to_string())
        });
        assert_eq!(*calls.borrow(), 0, "no protocol row, no spawn — memoized or not");
        assert!(reply.error.as_deref().unwrap_or_default().contains("copilot"));
        assert!(memo.kept.lock().unwrap().is_empty(), "and nothing was remembered about it");
    }

    #[test]
    fn the_startup_sweep_asks_each_protocol_row_exactly_once() {
        // #1020's automatic path. Three properties in one, because they are one
        // behaviour: every supported CLI gets its help probe warmed, only the
        // CLIs that have something to ask are asked, and each is asked once —
        // a sweep that asked twice would double the one cost this whole design
        // is rationing.
        let probed = RefCell::new(Vec::new());
        let asked = RefCell::new(Vec::new());
        let emitted = RefCell::new(Vec::new());
        sweep_with(
            &SUPPORTED_CLIS,
            |cli| probed.borrow_mut().push(cli.to_string()),
            |cli| {
                asked.borrow_mut().push(cli.to_string());
                CliModelReply { output: REPLY.to_string(), request_id: REQUEST_ID.to_string(), error: None }
            },
            |d| emitted.borrow_mut().push(d.program),
        );
        let every: Vec<String> = SUPPORTED_CLIS.iter().map(|c| (*c).to_string()).collect();
        assert_eq!(
            *probed.borrow(),
            every,
            "every supported CLI's help probe is warmed, whether or not it can be asked for a list"
        );
        assert_eq!(
            *asked.borrow(),
            vec!["claude".to_string()],
            "only a CLI with a PROTOCOLS row is spawned for a list — and exactly once"
        );
        assert_eq!(
            *emitted.borrow(),
            *asked.borrow(),
            "one event per ask: a CLI that was never asked has no result to push, and an ask whose \
             result never reaches the webview is a spawn spent for nothing"
        );
    }

    #[test]
    fn the_sweep_pushes_the_reply_verbatim() {
        // The event is the early-delivery half of the same answer the memo
        // holds; if it carried anything less, a webview that caught it would
        // show something different from one that missed it and asked instead.
        let pushed = RefCell::new(Vec::new());
        sweep_with(
            &["claude"],
            |_| {},
            |_| CliModelReply { output: REPLY.to_string(), request_id: REQUEST_ID.to_string(), error: None },
            |d| pushed.borrow_mut().push(d),
        );
        let seen = pushed.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].program, "claude");
        assert_eq!(seen[0].reply.output, REPLY);
        assert_eq!(seen[0].reply.request_id, REQUEST_ID, "the correlation id has to survive the event too");
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
