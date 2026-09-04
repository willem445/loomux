//! The Claude Code adapter: `--output-format stream-json` in, [`HarnessEvent`]
//! out (#84 slice R1, `doc/design/harness-adapters.md` §6–§8).
//!
//! # Every CLI fact here is cited, and none was observed
//!
//! No `claude` process was run to write this module (CLAUDE.md constraint 3),
//! and nothing below is recalled. Each flag and each message shape carries the
//! official page it came from, read 2026-09-03:
//!
//! - flags: <https://code.claude.com/docs/en/cli-reference>
//! - the stream and its `result` line: <https://code.claude.com/docs/en/headless>
//! - message shapes: <https://code.claude.com/docs/en/agent-sdk/typescript>
//! - usage scope: <https://code.claude.com/docs/en/agent-sdk/cost-tracking>
//! - resume: <https://code.claude.com/docs/en/sessions>
//!
//! The consequence is worth stating plainly rather than leaving for a reader to
//! infer: this decoder is proved against the **documented** contract and not
//! against the real CLI's bytes. The fixtures under
//! `crates/loomux-engine/tests/fixtures/harness/claude/` are **synthesized from
//! those pages**, not recorded from a session, and say so in their own header.
//! Replacing them with a human-recorded capture is a live-validation item, in
//! the same family as the note's §9.
//!
//! # Why a `serde_json::Value` walk rather than derived structs
//!
//! The stream's union has ~35 message types and the docs call several of its
//! field sets open (`capabilities`, `terminal_reason`). A derived struct per
//! message would turn "a field this build has not heard of" into a parse error,
//! which is the opposite of the protocol note's §4.4 rule — *both sides ignore
//! what they do not know*. `notify.rs` walks `gh --json` payloads the same way
//! and for the same reason. What R1 must never do is discard the line as well:
//! an unrecognized message is kept verbatim in the pane log
//! ([`super::LogBody::Unknown`]), because a silently dropped message is one
//! nobody can add support for later.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;

use serde_json::Value;

use super::{
    AgentPane, CompactTrigger, Cost, CostBasis, Decision, EventLog, EventRx, Harness, HarnessEvent,
    LogBody, ModelTokens, PaneKind, RequestId, SendReceipt, StopReason, Tokens, ToolUseId, Turn,
    TurnId, Usage,
};
use crate::obs::LockExt;

// ── the launch line (§6.1) ──────────────────────────────────────────────────

/// Whether this pane is starting a session or continuing one.
///
/// Both carry **the same id**, which is the point: loomux mints the session id
/// with `--session-id` and resumes it with `--resume`, so the two argv lines
/// differ in exactly one element — the flag name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionArg {
    /// `--session-id <uuid>`: "Use a specific session ID for the conversation
    /// (must be a valid UUID)" (cli-reference).
    Fresh(String),
    /// `--resume <id>`: a `-p` session is out of the picker and out of
    /// `--continue`, but "You can still resume one by passing its session ID"
    /// (sessions).
    Resume(String),
}

impl SessionArg {
    pub fn id(&self) -> &str {
        match self {
            SessionArg::Fresh(id) | SessionArg::Resume(id) => id,
        }
    }
}

/// Everything the argv builder needs.
///
/// Deliberately **not** `Option`-heavy where the flag is mandatory:
/// `permission_mode` is a required field rather than an option, because
/// `claude -p`'s "built-in starting permission mode is Manual on every plan, so
/// pass the permission mode you want" (headless) — and a resumed `-p` run does
/// not restore the mode either (sessions). A pane that forgot it would silently
/// deny everything, which looks like a broken agent rather than a missing flag.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub session: SessionArg,
    pub mcp_config: PathBuf,
    /// The MCP tool name permission prompts are routed to. Built from
    /// `brand::MCP_TOOL_PREFIX` by the caller, never spelled as a literal here.
    pub permission_prompt_tool: String,
    pub permission_mode: String,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub settings: Option<PathBuf>,
    pub agent: Option<String>,
    pub effort: Option<String>,
}

impl LaunchSpec {
    /// The argv after the program name.
    ///
    /// **Every flag is repeated on a resume**, and that is the rule this
    /// function exists to hold: "If the session depended on `--mcp-config`,
    /// `--settings`, `--plugin-dir`, `--fallback-model`, or directories added
    /// with `--add-dir`, pass them again when you resume" (sessions). Building
    /// the resume line by *subtracting* from the fresh one is how that gets
    /// forgotten, so there is one builder and the session flag is its only
    /// branch.
    ///
    /// Two flags are deliberately absent, and their absence is a decision:
    ///
    /// - `--bare` "skip\[s\] auto-discovery of hooks, skills, custom commands,
    ///   subagents, plugins, MCP servers, auto memory, and CLAUDE.md"
    ///   (headless). A pane that does not load the repo's CLAUDE.md is not the
    ///   pane loomux launches today.
    /// - `--no-session-persistence` makes the session unresumable
    ///   (cli-reference), and resume is the feature.
    pub fn argv(&self) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        let mut push = |s: &str| a.push(s.to_string());
        push("-p");
        push("--input-format");
        push("stream-json");
        push("--output-format");
        push("stream-json");
        // `--verbose` and `--include-partial-messages` both require
        // `--output-format stream-json`; the latter also requires `--print`,
        // which `-p` is (cli-reference).
        push("--verbose");
        push("--include-partial-messages");
        match &self.session {
            SessionArg::Fresh(_) => push("--session-id"),
            SessionArg::Resume(_) => push("--resume"),
        }
        a.push(self.session.id().to_string());
        a.push("--mcp-config".into());
        a.push(self.mcp_config.to_string_lossy().into_owned());
        a.push("--strict-mcp-config".into());
        a.push("--permission-prompt-tool".into());
        a.push(self.permission_prompt_tool.clone());
        a.push("--permission-mode".into());
        a.push(self.permission_mode.clone());
        if !self.allowed_tools.is_empty() {
            a.push("--allowedTools".into());
            a.extend(self.allowed_tools.iter().cloned());
        }
        if !self.disallowed_tools.is_empty() {
            a.push("--disallowedTools".into());
            a.extend(self.disallowed_tools.iter().cloned());
        }
        if let Some(agent) = &self.agent {
            a.push("--agent".into());
            a.push(agent.clone());
        }
        if let Some(effort) = &self.effort {
            a.push("--effort".into());
            a.push(effort.clone());
        }
        a
    }
}

// ── the session id (§6.2) ───────────────────────────────────────────────────

/// Does the id `system/init` reported name the session loomux minted?
///
/// **Canonicalized, never a string compare.** cli-reference says `--session-id`
/// "must be a valid UUID"; **no page states the textual form `system/init`
/// echoes back**, so a literal `==` would kill every structured pane at spawn on
/// any build that answered in a different case or spelling — a total outage
/// produced by the safety check rather than by the failure it guards.
///
/// Both sides are parsed as a UUID and compared as 128 bits. A value that does
/// not parse **is a mismatch**, so the relaxation opens no hole: the check still
/// fails closed on anything that is not a UUID at all, which is the case §9's
/// item 6 asks the first real session to record.
pub fn session_ids_match(minted: &str, reported: &str) -> bool {
    match (canonical_uuid(minted), canonical_uuid(reported)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// 32 lowercase hex nibbles, or `None` if this is not a UUID.
///
/// Hyphens are ignored wherever they fall rather than positionally: the point is
/// tolerance of *spelling*, and a positional parse would reject the very
/// variants the tolerance exists for while adding no safety — the 32 nibbles are
/// the identity.
fn canonical_uuid(s: &str) -> Option<String> {
    let mut out = String::with_capacity(32);
    for c in s.chars() {
        match c {
            '-' => continue,
            c if c.is_ascii_hexdigit() => out.push(c.to_ascii_lowercase()),
            _ => return None,
        }
    }
    (out.len() == 32).then_some(out)
}

// ── the decoder (§8.1) ──────────────────────────────────────────────────────

/// What one stream line became.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    /// A pane event.
    Event(HarnessEvent),
    /// A real message that is not a pane event. `system/api_retry` is the one
    /// R1 knows: it is diagnostic (attempt number, backoff, error class), so it
    /// belongs in the pane log and not in the transcript a human reads.
    Note(String),
    /// A message type this build does not know. Ignored as an event, kept as
    /// evidence.
    Unknown(String),
}

/// Turns stream-json lines into events.
///
/// Stateful for exactly two reasons, both of which the stream cannot supply:
/// the **turn number** (see [`TurnId`]) and the **session id** learned from
/// `system/init`.
#[derive(Debug, Default)]
pub struct Decoder {
    open_turn: Option<TurnId>,
    next_turn: u64,
    session: Option<String>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The session id `system/init` reported, once it has.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Decode one line.
    ///
    /// Returns a **vector** because one line is legitimately several events: an
    /// `assistant` message carries a content array, so a single line can hold
    /// text and two tool calls, and it may also be the line that opens a turn.
    /// Flattening that to one event would mean either dropping content or
    /// inventing a batch variant nothing else uses.
    ///
    /// A line that is not JSON at all is [`Decoded::Unknown`], not an error: the
    /// child's stdout is a pipe, and a decoder that failed the pane on one
    /// malformed line would turn a cosmetic glitch into an outage.
    pub fn decode_line(&mut self, line: &str) -> Vec<Decoded> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![Decoded::Unknown(line.to_string())];
        };
        match v.get("type").and_then(Value::as_str) {
            Some("system") => self.system(&v, line),
            Some("assistant") => self.assistant(&v),
            Some("user") => self.user(&v),
            Some("stream_event") => self.stream_event(&v),
            Some("result") => self.result(&v),
            _ => vec![Decoded::Unknown(line.to_string())],
        }
    }

    fn system(&mut self, v: &Value, raw: &str) -> Vec<Decoded> {
        match v.get("subtype").and_then(Value::as_str) {
            Some("init") => {
                let session = v
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.session = session.clone();
                vec![Decoded::Event(HarnessEvent::Booted {
                    session,
                    model: v.get("model").and_then(Value::as_str).map(str::to_string),
                    // "an open set: ignore values you don't recognize"
                    // (agent-sdk/typescript). Carried through verbatim rather
                    // than filtered to a set this build knows, because the
                    // whole point of the field is feature detection by a later
                    // consumer.
                    capabilities: v
                        .get("capabilities")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                })]
            }
            Some("compact_boundary") => {
                let meta = v.get("compact_metadata");
                let trigger = match meta.and_then(|m| m.get("trigger")).and_then(Value::as_str) {
                    Some("manual") => CompactTrigger::Manual,
                    _ => CompactTrigger::Auto,
                };
                vec![Decoded::Event(HarnessEvent::Compacted {
                    trigger,
                    pre_tokens: meta.and_then(|m| m.get("pre_tokens")).and_then(Value::as_u64),
                })]
            }
            Some("api_retry") => {
                let attempt = v.get("attempt").and_then(Value::as_u64).unwrap_or(0);
                let err = v.get("error").and_then(Value::as_str).unwrap_or("unknown");
                vec![Decoded::Note(format!("api_retry attempt {attempt}: {err}"))]
            }
            _ => vec![Decoded::Unknown(raw.to_string())],
        }
    }

    fn assistant(&mut self, v: &Value) -> Vec<Decoded> {
        let mut out = Vec::new();
        let turn = self.ensure_turn(&mut out);
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            return out;
        };
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            out.push(Decoded::Event(HarnessEvent::Text {
                                turn,
                                delta: t.to_string(),
                            }));
                        }
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    out.push(Decoded::Event(HarnessEvent::ToolCall {
                        turn,
                        id: ToolUseId(id),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input: block.get("input").cloned().unwrap_or(Value::Null),
                    }));
                }
                // `thinking` and `redacted_thinking` blocks are deliberately not
                // events: they are the model's reasoning, and the transcript a
                // human reads in the pane is what the agent DID. They are not
                // lost — the line is still the line the pane log records.
                _ => {}
            }
        }
        out
    }

    fn user(&mut self, v: &Value) -> Vec<Decoded> {
        let mut out = Vec::new();
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            return out;
        };
        let turn = self.ensure_turn(&mut out);
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                let id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                out.push(Decoded::Event(HarnessEvent::ToolResult {
                    turn,
                    id: ToolUseId(id),
                    // `is_error` absent means the call succeeded; the field is
                    // only written on the failing side.
                    ok: !block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }));
            }
        }
        out
    }

    fn stream_event(&mut self, v: &Value) -> Vec<Decoded> {
        let mut out = Vec::new();
        let text = v
            .get("event")
            .and_then(|e| e.get("delta"))
            .and_then(|d| {
                (d.get("type").and_then(Value::as_str) == Some("text_delta"))
                    .then(|| d.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .unwrap_or_default();
        if text.is_empty() {
            return out;
        }
        let turn = self.ensure_turn(&mut out);
        out.push(Decoded::Event(HarnessEvent::Text {
            turn,
            delta: text.to_string(),
        }));
        out
    }

    fn result(&mut self, v: &Value) -> Vec<Decoded> {
        let mut out = Vec::new();
        let turn = self.ensure_turn(&mut out);
        self.open_turn = None;
        out.push(Decoded::Event(HarnessEvent::TurnEnded {
            turn,
            usage: usage_from_result(v),
            cost: v
                .get("total_cost_usd")
                .and_then(Value::as_f64)
                .map(|usd| Cost {
                    usd,
                    basis: CostBasis::HarnessEstimate,
                }),
            stop: stop_reason(v),
        }));
        out
    }

    /// The turn every content event belongs to, opening one if none is.
    ///
    /// **Turn boundaries are derived, not read**, and the reason is a flag this
    /// slice does not pass. Claude Code echoes the user message that starts a
    /// turn only under `--replay-user-messages` (cli-reference), which §6.1's
    /// launch line omits — so there is no line that says "a turn began". The
    /// rule here is therefore: the first turn-scoped message opens a turn, and
    /// `result` closes it. That is deterministic and flag-independent.
    ///
    /// What it costs, stated rather than hidden: the boundary is the first
    /// *output* of the turn, not the moment the prompt was accepted, so a turn
    /// that spends time before its first token appears to start late. Adding
    /// `--replay-user-messages` would move the boundary to the true start and
    /// give [`AgentPane::send`] a real acknowledgement at the same time; that is
    /// an R2 decision, because it changes the launch line the note contracts.
    fn ensure_turn(&mut self, out: &mut Vec<Decoded>) -> TurnId {
        if let Some(t) = self.open_turn {
            return t;
        }
        let t = TurnId(self.next_turn);
        self.next_turn += 1;
        self.open_turn = Some(t);
        out.push(Decoded::Event(HarnessEvent::TurnStarted { turn: t }));
        t
    }
}

/// Why the turn ended.
///
/// `terminal_reason` is preferred over `subtype` when present because it is the
/// finer of the two — the docs list nineteen values for it against five
/// subtypes — and an unrecognized value is carried through as
/// [`StopReason::Other`] rather than collapsed, since the docs call the set
/// open and a value this build has not heard of is exactly what a human
/// debugging a stuck pane needs to see.
fn stop_reason(v: &Value) -> StopReason {
    if let Some(r) = v.get("terminal_reason").and_then(Value::as_str) {
        return match r {
            "completed" => StopReason::Completed,
            "max_turns" => StopReason::MaxTurns,
            "aborted_streaming" | "aborted_tools" => StopReason::Aborted,
            "api_error" | "model_error" | "turn_setup_failed" => StopReason::Error,
            other => StopReason::Other(other.to_string()),
        };
    }
    match v.get("subtype").and_then(Value::as_str) {
        Some("success") => StopReason::Completed,
        Some("error_max_turns") => StopReason::MaxTurns,
        Some(other) => StopReason::Other(other.to_string()),
        None => StopReason::Other(String::new()),
    }
}

/// Token counts out of a `result` line (§7).
///
/// Reads **`modelUsage`** for the cumulative figure, not `usage`: "Use
/// `modelUsage` … for whole-tree token accounting; the `usage` field
/// undercounts as soon as nesting occurs" (cost-tracking). `usage` is still
/// carried, as [`Usage::this_turn_main_loop`], because it is the only per-turn
/// figure the line has — the two answer different questions and the type keeps
/// them apart.
///
/// Per-model rows are sorted by model id. `serde_json`'s map is a `BTreeMap`
/// without the `preserve_order` feature, so "the stream's order" is not
/// available here at all; sorting makes the order a stated property rather than
/// an accident of the map implementation, and keeps golden tests stable.
pub fn usage_from_result(v: &Value) -> Option<Usage> {
    let per_model_obj = v.get("modelUsage").and_then(Value::as_object);
    let this_turn = v.get("usage").and_then(Value::as_object).map(|u| Tokens {
        input: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        output: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        cache_read: u
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_creation: u
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    });
    let Some(models) = per_model_obj else {
        // No `modelUsage` at all: report what there is rather than nothing, but
        // do not pretend the turn figure is the call total.
        return this_turn.map(|t| Usage {
            call_cumulative: Tokens::default(),
            this_turn_main_loop: Some(t),
            per_model: Vec::new(),
        });
    };
    let mut per_model: Vec<ModelTokens> = models
        .iter()
        .map(|(model, u)| ModelTokens {
            model: model.clone(),
            tokens: Tokens {
                input: u.get("inputTokens").and_then(Value::as_u64).unwrap_or(0),
                // `outputTokens` already includes `thinkingTokens`; adding the
                // latter would double count (agent-sdk/typescript).
                output: u.get("outputTokens").and_then(Value::as_u64).unwrap_or(0),
                cache_read: u
                    .get("cacheReadInputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                cache_creation: u
                    .get("cacheCreationInputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            },
        })
        .collect();
    per_model.sort_by(|a, b| a.model.cmp(&b.model));
    let call_cumulative = per_model.iter().fold(Tokens::default(), |acc, m| Tokens {
        input: acc.input.saturating_add(m.tokens.input),
        output: acc.output.saturating_add(m.tokens.output),
        cache_read: acc.cache_read.saturating_add(m.tokens.cache_read),
        cache_creation: acc.cache_creation.saturating_add(m.tokens.cache_creation),
    });
    Some(Usage {
        call_cumulative,
        this_turn_main_loop: this_turn,
        per_model,
    })
}

// ── the two things R1 refuses, and why each refuses ─────────────────────────
//
// Both are free functions rather than string literals inside the `impl`, so a
// test can assert what they SAY without constructing a live pane. What that
// costs, stated: the wiring from each trait method to its function is a one-line
// body the compiler checks and no test does — a residual, not a covered case.

/// Why a Claude pane cannot settle a permission request yet.
///
/// Settling one needs `permissions.json` and the `permission_prompt` MCP tool,
/// which are R2's (§3, §8.2). An `Ok(())` that dropped the decision would be a
/// permission system reporting success while approving nothing, so the refusal
/// names what is missing instead.
pub fn permission_answer_unavailable(req: &RequestId, decision: Decision) -> String {
    format!(
        "cannot settle {req:?} as {decision:?}: the permission channel \
         (permissions.json + the permission_prompt MCP tool) lands in R2 — see \
         doc/design/harness-adapters.md §3"
    )
}

/// Why a Claude pane cannot interrupt a turn yet.
///
/// This one is a **docs gap, not a missing slice**. The docs prescribe SIGINT
/// before SIGTERM to end a turn and record its result (headless), and note that
/// a client driving the control protocol directly can send an `interrupt`
/// control request (agent-sdk/typescript) — but no page states that request's
/// envelope, and this project's Windows baseline has no SIGINT to send.
/// Inventing a wire format here is exactly the guess the citation discipline
/// exists to prevent.
pub fn interrupt_unavailable() -> String {
    "interrupt is not wired: the `interrupt` control-request envelope is \
     undocumented and this baseline has no SIGINT — see \
     doc/design/harness-adapters.md §9 item 4. close_input() is the documented \
     alternative that IS available."
        .to_string()
}

// ── the child driver ────────────────────────────────────────────────────────

/// Read every line of a stream, decode it, log it, and publish the events.
///
/// Separated from process spawning so the whole decode-and-publish path is
/// exercised over an in-memory reader: R1's tests never start a child, which is
/// how "never spawn a real agent CLI" (CLAUDE.md constraint 3) is satisfied by
/// construction rather than by remembering.
///
/// A send failure ends the pump: the receiver is gone, so there is nobody left
/// to publish to, and continuing would be a loop that reads a whole session into
/// a dropped channel.
pub fn pump<R: BufRead>(
    reader: R,
    decoder: &mut Decoder,
    log: &mut Option<EventLog>,
    tx: &Sender<HarnessEvent>,
) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        for decoded in decoder.decode_line(&line) {
            match decoded {
                Decoded::Event(ev) => {
                    if let Some(log) = log.as_mut() {
                        let _ = log.record(ev.clone());
                    }
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
                Decoded::Note(note) => {
                    if let Some(log) = log.as_mut() {
                        let _ = log.append(LogBody::Note { note });
                    }
                }
                Decoded::Unknown(raw) => {
                    if let Some(log) = log.as_mut() {
                        let _ = log.record_unknown(&raw);
                    }
                }
            }
        }
    }
}

/// One Claude Code pane, driven over stdin/stdout.
pub struct ClaudePane {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    rx: Mutex<Option<EventRx>>,
    session: Mutex<Option<String>>,
    sent_bytes: AtomicU64,
}

impl ClaudePane {
    /// Spawn the user's own `claude` binary on `spec`.
    ///
    /// **`program` is a parameter and never a literal**, so nothing in this
    /// crate can start a real CLI on its own: R1's tests exercise [`pump`] over
    /// an in-memory reader and never reach this function (constraint 3).
    ///
    /// stderr is inherited rather than piped. A piped stderr nobody drains fills
    /// its buffer and blocks the child — the classic deadlock — and R1 has no
    /// consumer for it; inheriting sends it where the app's own stderr goes,
    /// which is where a human already looks.
    pub fn spawn(program: &std::path::Path, spec: &LaunchSpec, log: Option<EventLog>) -> std::io::Result<Self> {
        let mut child = std::process::Command::new(program)
            .args(spec.argv())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no stdout pipe"))?;
        let (tx, rx) = mpsc::channel();
        let mut log = log;
        std::thread::spawn(move || {
            let mut decoder = Decoder::new();
            pump(std::io::BufReader::new(stdout), &mut decoder, &mut log, &tx);
        });
        Ok(ClaudePane {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            rx: Mutex::new(Some(rx)),
            session: Mutex::new(None),
            sent_bytes: AtomicU64::new(0),
        })
    }

    /// The stream-json line one [`Turn`] becomes.
    ///
    /// The shape is the documented `SDKUserMessage`: `type`, a `message` with a
    /// `role` and `content`, and `parent_tool_use_id` — which is `null` for a
    /// message from the main conversation
    /// (<https://code.claude.com/docs/en/agent-sdk/typescript#sdkusermessage>).
    ///
    /// A `Notice` keeps its marker prefix in the text, because on a structured
    /// pane the marker is a **display** convention and nothing scrapes for it —
    /// the PTY path's maskability contract has no counterpart here.
    pub fn wire_line(turn: &Turn) -> String {
        let payload = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": turn.text() },
            "parent_tool_use_id": Value::Null,
        });
        format!("{payload}\n")
    }

    /// Close the child's stdin.
    ///
    /// This is the one **documented** way to end a pending prompt without a
    /// signal: "the SDK ends Claude Code's input before sending any signal, and
    /// Claude Code cancels the prompt as soon as the input ends" (headless). It
    /// is a close, not an interrupt — see [`AgentPane::interrupt`] for why the
    /// two are not the same method.
    pub fn close_input(&self) -> Result<(), String> {
        *self.stdin.lock_safe() = None;
        Ok(())
    }

    /// Record the session id `system/init` reported, refusing a mismatch.
    ///
    /// **A mismatch kills the pane** (§6.2): it means the flag did not take
    /// effect, so the pane is not the session loomux believes it is, and every
    /// downstream record — usage, resume, transcript — would key on the wrong
    /// one. Fail closed; do not reconcile.
    pub fn bind_session(&self, minted: &str, reported: &str) -> Result<(), String> {
        if !session_ids_match(minted, reported) {
            return Err(format!(
                "session id mismatch: minted {minted}, the CLI reported {reported} — \
                 the --session-id flag did not take effect, so this pane is not the \
                 session loomux thinks it is"
            ));
        }
        *self.session.lock_safe() = Some(minted.to_string());
        Ok(())
    }
}

impl AgentPane for ClaudePane {
    fn kind(&self) -> PaneKind {
        PaneKind::Structured(Harness::Claude)
    }

    fn send(&self, turn: Turn) -> Result<SendReceipt, String> {
        let line = Self::wire_line(&turn);
        let mut guard = self.stdin.lock_safe();
        let stdin = guard
            .as_mut()
            .ok_or_else(|| "this pane's stdin is closed".to_string())?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("writing to the pane failed: {e}"))?;
        let total = self
            .sent_bytes
            .fetch_add(line.len() as u64, Ordering::Relaxed)
            + line.len() as u64;
        Ok(SendReceipt {
            accepted_at_bytes: total,
        })
    }

    /// **Refused in R1, deliberately.**
    ///
    /// Settling a permission request needs `permissions.json` and the
    /// `permission_prompt` MCP tool, which are R2's (§8.2). An `Ok(())` that
    /// dropped the decision would be a permission system reporting success
    /// while approving nothing, so the refusal names what is missing instead.
    fn answer(&self, req: RequestId, decision: Decision) -> Result<(), String> {
        Err(permission_answer_unavailable(&req, decision))
    }

    /// **Refused in R1, and the reason is a docs gap rather than a missing
    /// slice.**
    ///
    /// The docs prescribe SIGINT before SIGTERM to end a turn and record its
    /// result (headless), and mention that a client driving the control protocol
    /// directly can send an `interrupt` control request
    /// (agent-sdk/typescript) — but **no page states that request's envelope**,
    /// and this project's Windows baseline has no SIGINT to send. Inventing a
    /// wire format here would be exactly the guess the citation discipline
    /// exists to prevent, so this refuses and points at the live-validation item
    /// that resolves it. [`ClaudePane::close_input`] is the documented behaviour
    /// that IS available today.
    fn interrupt(&self) -> Result<(), String> {
        Err(interrupt_unavailable())
    }

    fn events(&self) -> Option<EventRx> {
        self.rx.lock_safe().take()
    }

    fn session_id(&self) -> Option<String> {
        self.session.lock_safe().clone()
    }
}

impl Drop for ClaudePane {
    /// Kill the child if it is still running.
    ///
    /// Best effort and deliberately blunt: the graceful path (end the turn, then
    /// stop the process) needs the interrupt this slice refuses, so a `Drop`
    /// that pretended to be graceful would be the guess `interrupt` declines to
    /// make. R2 replaces this with the SIGINT-then-SIGTERM sequence §6.4
    /// contracts.
    fn drop(&mut self) {
        *self.stdin.lock_safe() = None;
        let _ = self.child.lock_safe().kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(session: SessionArg) -> LaunchSpec {
        LaunchSpec {
            session,
            mcp_config: PathBuf::from("C:/x/cfg.json"),
            permission_prompt_tool: "mcp__orrerix__permission_prompt".into(),
            permission_mode: "acceptEdits".into(),
            allowed_tools: vec!["mcp__orrerix".into()],
            disallowed_tools: vec!["Edit".into()],
            settings: Some(PathBuf::from("C:/x/hooks.json")),
            agent: Some("w-1".into()),
            effort: Some("high".into()),
        }
    }

    #[test]
    fn a_resume_line_repeats_every_flag_and_differs_in_exactly_one_element() {
        // §6.3's first resume fact, as an assertion rather than a promise: the
        // docs say a resumed session restores none of the configuration flags,
        // so the resume argv must be the fresh argv with one word changed.
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let fresh = spec(SessionArg::Fresh(id.into())).argv();
        let resume = spec(SessionArg::Resume(id.into())).argv();
        assert_eq!(fresh.len(), resume.len());
        let diff: Vec<_> = fresh
            .iter()
            .zip(&resume)
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        assert_eq!(
            diff,
            vec![("--session-id".to_string(), "--resume".to_string())],
            "fresh and resume argv must differ in exactly the session flag"
        );
        // And the flags that a resume is documented NOT to restore are all still
        // there — the check that would catch a resume line built by subtraction.
        for flag in [
            "--mcp-config",
            "--strict-mcp-config",
            "--settings",
            "--permission-mode",
            "--permission-prompt-tool",
            "--agent",
        ] {
            assert!(resume.iter().any(|a| a == flag), "resume dropped {flag}");
        }
    }

    #[test]
    fn the_launch_line_never_carries_bare_or_no_session_persistence() {
        // Two absences that are decisions, so they get an assertion rather than
        // a comment: `--bare` skips CLAUDE.md discovery and
        // `--no-session-persistence` makes the session unresumable.
        let argv = spec(SessionArg::Fresh("x".into())).argv();
        assert!(!argv.iter().any(|a| a == "--bare"));
        assert!(!argv.iter().any(|a| a == "--no-session-persistence"));
        assert!(argv.iter().any(|a| a == "--include-partial-messages"));
        assert!(argv.iter().any(|a| a == "--verbose"));
    }

    #[test]
    fn a_session_id_is_compared_canonically_and_a_non_uuid_is_a_mismatch() {
        let minted = "550e8400-e29b-41d4-a716-446655440000";
        // The case the premortem named: same 128 bits, different spelling. A
        // string compare would kill the pane here.
        assert!(session_ids_match(minted, "550E8400-E29B-41D4-A716-446655440000"));
        assert!(session_ids_match(minted, "550e8400e29b41d4a716446655440000"));
        // A genuinely different session.
        assert!(!session_ids_match(
            minted,
            "550e8400-e29b-41d4-a716-446655440001"
        ));
        // Fail closed: not a UUID at all is a mismatch, on either side.
        assert!(!session_ids_match(minted, "not-a-uuid"));
        assert!(!session_ids_match("", minted));
        assert!(!session_ids_match(minted, "550e8400-e29b-41d4-a716-4466554400"));
    }

    #[test]
    fn init_booted_result_is_the_decoder_walking_one_turn() {
        let mut d = Decoder::new();
        let out = d.decode_line(
            r#"{"type":"system","subtype":"init","session_id":"s-1","model":"opus","capabilities":["interrupt_receipt_v1","a_future_one"]}"#,
        );
        assert_eq!(
            out,
            vec![Decoded::Event(HarnessEvent::Booted {
                session: Some("s-1".into()),
                model: Some("opus".into()),
                capabilities: vec!["interrupt_receipt_v1".into(), "a_future_one".into()],
            })]
        );
        assert_eq!(d.session(), Some("s-1"));

        let out = d.decode_line(
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"on it"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"git status"}}]}}"#,
        );
        assert_eq!(
            out,
            vec![
                Decoded::Event(HarnessEvent::TurnStarted { turn: TurnId(0) }),
                Decoded::Event(HarnessEvent::Text {
                    turn: TurnId(0),
                    delta: "on it".into()
                }),
                Decoded::Event(HarnessEvent::ToolCall {
                    turn: TurnId(0),
                    id: ToolUseId("toolu_1".into()),
                    name: "Bash".into(),
                    input: serde_json::json!({"command": "git status"}),
                }),
            ]
        );

        let out = d.decode_line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1"}]}}"#,
        );
        assert_eq!(
            out,
            vec![Decoded::Event(HarnessEvent::ToolResult {
                turn: TurnId(0),
                id: ToolUseId("toolu_1".into()),
                ok: true,
            })]
        );

        let out = d.decode_line(r#"{"type":"result","subtype":"success","terminal_reason":"completed","total_cost_usd":0.25,"usage":{"input_tokens":10,"output_tokens":20},"modelUsage":{"opus":{"inputTokens":10,"outputTokens":20,"cacheReadInputTokens":5,"cacheCreationInputTokens":1}}}"#);
        match &out[..] {
            [Decoded::Event(HarnessEvent::TurnEnded {
                turn,
                usage,
                cost,
                stop,
            })] => {
                assert_eq!(*turn, TurnId(0));
                assert_eq!(*stop, StopReason::Completed);
                assert_eq!(cost.unwrap().usd, 0.25);
                let u = usage.as_ref().unwrap();
                assert_eq!(u.call_cumulative.output, 20);
                assert_eq!(u.call_cumulative.cache_read, 5);
                assert_eq!(u.this_turn_main_loop.unwrap().input, 10);
            }
            other => panic!("expected one TurnEnded, got {other:?}"),
        }

        // The next content opens turn 1, which is what makes the counter mean
        // "turn" rather than "message".
        let out = d.decode_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"again"}]}}"#);
        assert_eq!(out[0], Decoded::Event(HarnessEvent::TurnStarted { turn: TurnId(1) }));
    }

    #[test]
    fn an_unknown_message_type_is_ignored_as_an_event_and_kept_as_evidence() {
        // The protocol note's ignore-what-you-do-not-know rule, applied inward —
        // and the half that is easy to get wrong: ignoring must not mean
        // DISCARDING, or nobody can add support for the message later.
        let mut d = Decoder::new();
        for line in [
            r#"{"type":"a_message_type_from_the_future","v":1}"#,
            r#"{"type":"system","subtype":"a_new_subtype"}"#,
            "not json at all",
        ] {
            let out = d.decode_line(line);
            assert_eq!(
                out,
                vec![Decoded::Unknown(line.to_string())],
                "{line} must be kept verbatim, not dropped and not fatal"
            );
        }
        // And it must not have disturbed the turn counter: an unknown line is
        // not turn-scoped content.
        let out = d.decode_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#);
        assert_eq!(out[0], Decoded::Event(HarnessEvent::TurnStarted { turn: TurnId(0) }));
    }

    #[test]
    fn api_retry_is_a_log_note_and_never_a_pane_event() {
        let mut d = Decoder::new();
        let out = d.decode_line(
            r#"{"type":"system","subtype":"api_retry","attempt":2,"error":"overloaded","max_retries":5}"#,
        );
        assert_eq!(
            out,
            vec![Decoded::Note("api_retry attempt 2: overloaded".into())]
        );
    }

    #[test]
    fn a_stream_event_delta_is_text_and_a_non_text_delta_is_not() {
        let mut d = Decoder::new();
        let out = d.decode_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"par"}}}"#,
        );
        assert_eq!(
            out,
            vec![
                Decoded::Event(HarnessEvent::TurnStarted { turn: TurnId(0) }),
                Decoded::Event(HarnessEvent::Text {
                    turn: TurnId(0),
                    delta: "par".into()
                }),
            ]
        );
        // A thinking delta is a delta too, and must not become transcript text.
        let out = d.decode_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}}}"#,
        );
        assert!(out.is_empty(), "a thinking delta is not transcript text: {out:?}");
    }

    #[test]
    fn usage_reads_the_whole_tree_figure_and_keeps_the_turn_figure_apart() {
        // §7's trap 4: `usage` undercounts once subagents run, `modelUsage` does
        // not. The fixture makes them DISAGREE, so a reader that took the wrong
        // one fails here — equal numbers would pass under either.
        let v: Value = serde_json::from_str(
            r#"{"type":"result","usage":{"input_tokens":100,"output_tokens":50},
                "modelUsage":{"sonnet":{"inputTokens":100,"outputTokens":50},
                              "haiku":{"inputTokens":900,"outputTokens":40,"cacheReadInputTokens":7}}}"#,
        )
        .unwrap();
        let u = usage_from_result(&v).unwrap();
        assert_eq!(u.call_cumulative.input, 1000, "must fold every model");
        assert_eq!(u.call_cumulative.output, 90);
        assert_eq!(u.call_cumulative.cache_read, 7);
        assert_eq!(
            u.this_turn_main_loop.unwrap().input,
            100,
            "the per-turn figure stays its own field"
        );
        // Sorted, so a golden test over this is stable.
        assert_eq!(
            u.per_model.iter().map(|m| m.model.as_str()).collect::<Vec<_>>(),
            vec!["haiku", "sonnet"]
        );
    }

    #[test]
    fn an_unrecognized_terminal_reason_is_carried_not_collapsed() {
        let mut d = Decoder::new();
        let out =
            d.decode_line(r#"{"type":"result","subtype":"success","terminal_reason":"a_new_reason"}"#);
        match &out[..] {
            [Decoded::Event(HarnessEvent::TurnStarted { .. }), Decoded::Event(HarnessEvent::TurnEnded { stop, .. })] => {
                assert_eq!(*stop, StopReason::Other("a_new_reason".into()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_wire_line_is_one_ndjson_user_message() {
        let line = ClaudePane::wire_line(&Turn::Kickoff("do the thing".into()));
        assert!(line.ends_with('\n'), "stream-json is newline delimited");
        assert_eq!(line.matches('\n').count(), 1, "one message, one line");
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "do the thing");
        assert!(v["parent_tool_use_id"].is_null());
    }

    #[test]
    fn a_notice_keeps_its_marker_and_a_multiline_turn_stays_one_line() {
        // Two properties of the wire encoding at once: the marker is display
        // text on a structured pane (nothing scrapes for it), and an embedded
        // newline must be JSON-escaped rather than splitting the message into
        // two — which on an NDJSON pipe would be a torn message, not a bug you
        // find later.
        let line = ClaudePane::wire_line(&Turn::Notice("[orrerix] two\nlines".into()));
        assert_eq!(line.matches('\n').count(), 1);
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["message"]["content"], "[orrerix] two\nlines");
    }

    #[test]
    fn pump_publishes_events_and_logs_the_lines_it_could_not_decode() {
        let stream = concat!(
            r#"{"type":"system","subtype":"init","session_id":"s-9"}"#,
            "\n",
            r#"{"type":"a_future_type"}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
            "\n",
        );
        let (tx, rx) = mpsc::channel();
        let mut d = Decoder::new();
        let mut log = None;
        pump(std::io::Cursor::new(stream), &mut d, &mut log, &tx);
        drop(tx);
        let got: Vec<_> = rx.iter().collect();
        assert_eq!(got.len(), 3, "Booted, TurnStarted, Text — the unknown line publishes nothing: {got:?}");
        assert!(matches!(got[0], HarnessEvent::Booted { .. }));
        assert!(matches!(got[1], HarnessEvent::TurnStarted { .. }));
        assert!(matches!(got[2], HarnessEvent::Text { .. }));
    }

    #[test]
    fn the_fixture_decodes_to_a_whole_turn() {
        // The fixture is SYNTHETIC — built from the documented message shapes,
        // not recorded from a session — and its header says so. It exists so R2
        // has one file to swap for a real capture, and so this decoder is
        // exercised over a stream rather than over hand-picked lines.
        let text = include_str!("../../tests/fixtures/harness/claude/one-turn.jsonl");
        let (tx, rx) = mpsc::channel();
        let mut d = Decoder::new();
        let mut log = None;
        pump(std::io::Cursor::new(text), &mut d, &mut log, &tx);
        drop(tx);
        let got: Vec<HarnessEvent> = rx.iter().collect();
        assert!(matches!(got.first(), Some(HarnessEvent::Booted { .. })));
        assert!(
            got.iter().any(|e| matches!(e, HarnessEvent::ToolCall { .. })),
            "the fixture must exercise a tool call"
        );
        assert!(
            got.iter().any(|e| matches!(e, HarnessEvent::ToolResult { ok: false, .. })),
            "the fixture must exercise a FAILING tool result — the branch an \
             all-success fixture never reaches"
        );
        match got.last() {
            Some(HarnessEvent::TurnEnded { usage, cost, .. }) => {
                assert!(usage.is_some() && cost.is_some());
            }
            other => panic!("the stream must end on a result: {other:?}"),
        }
    }

    #[test]
    fn each_refusal_names_the_thing_that_would_lift_it() {
        // A refusal is a shipped behaviour, so it is pinned like one — but on
        // its SUBSTANCE, not on the whole sentence, or a wording fix reddens a
        // test for no reason. What each must carry is a pointer a reader can
        // act on: which slice or which open question lifts it.
        let answer = permission_answer_unavailable(&RequestId("r-1".into()), Decision::Allow);
        assert!(answer.contains("R2"), "{answer}");
        assert!(answer.contains("permissions.json"), "{answer}");
        assert!(answer.contains("r-1"), "the refusal must name the request: {answer}");

        let interrupt = interrupt_unavailable();
        assert!(interrupt.contains("§9"), "{interrupt}");
        assert!(
            interrupt.contains("close_input"),
            "a refusal must point at the alternative that works: {interrupt}"
        );
        // Neither may claim the thing is impossible — they are both "not yet",
        // and a reader who takes "impossible" at face value stops looking.
        for m in [&answer, &interrupt] {
            assert!(!m.contains("never"), "a not-yet must not read as a never: {m}");
        }
    }
}
