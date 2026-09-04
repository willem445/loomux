//! Harness adapters — a pane that **reports** what it did, instead of a pane
//! loomux has to **read** (#84, slice R1).
//!
//! `doc/design/harness-adapters.md` is this module's contract (PR #2193). Every
//! name below is fixed by its §1, and R2 may not change one without amending
//! that note in the same PR. What follows is the reason each shape is what it
//! is; the note carries the argument in full.
//!
//! # What a harness adapter replaces
//!
//! Today loomux drives every agent CLI through a ConPTY: it types an
//! echo-verified bracketed paste, waits for the screen to go quiet, and learns
//! what happened by **scraping the pane's own output ring** — readiness markers,
//! question grids, a statusline. That machinery is the source of a long tail of
//! delivery bugs (#1663, #903, #871, #1591, #112, #179), and none of it can tell
//! a remote client what a pane is waiting for, because the answer only exists as
//! pixels.
//!
//! A structured harness replaces the reading half. Claude Code's
//! `--output-format stream-json` emits one JSON object per line describing
//! exactly what the session did — turn boundaries, assistant text, tool calls
//! and their results, token usage, the session id. This module turns those lines
//! into [`HarnessEvent`]s, which every other part of loomux consumes instead of
//! a screen.
//!
//! # The boundary
//!
//! This is an **engine leaf**: `serde`/`serde_json` and `std`, nothing else. No
//! `tauri`, no pty, no pane, no `AppHandle` — and, in R1, no caller. It is
//! deliberately not wired into any spawn path (that is R2), so it can land
//! parallel with the #888 A4 chain without touching
//! `src-tauri/src/orchestration/mod.rs` at all.
//!
//! # What R1 ships, and what it deliberately does not
//!
//! Ships: the vocabulary below, the stream-json decoder and the child driver
//! ([`claude`]), the VT renderer ([`transcript`]), and the per-pane event log
//! ([`EventLog`]).
//!
//! Does not ship, and says so rather than stubbing it silently:
//! [`AgentPane::answer`] on a Claude pane **refuses**. Answering a permission
//! request needs `permissions.json` and the `permission_prompt` MCP tool, which
//! are R2's; a refusal that names R2 is honest, where an `Ok(())` that dropped
//! the decision would be a permission system that silently approves nothing and
//! reports success.
//!
//! # Two rules about *not knowing*, which are the whole reason this enum is
//! shaped the way it is
//!
//! A PTY pane implements the same [`AgentPane`] trait and emits the same
//! [`HarnessEvent`], because a second vocabulary would mean a second consumer
//! for every feature that reads panes. It knows far less, and the note's §1.3
//! fixes exactly how that is spelled:
//!
//! 1. **A fact the pane does not have is `None`, never a sentinel.** An
//!    `"unknown"` string or a `-1` is a value every `match` arm can forget to
//!    check, and it reads as data.
//! 2. **A fact the pane only INFERRED is [`HarnessEvent::Observed`], never the
//!    reported variant.** Scraped evidence and reported fact must not share a
//!    constructor: a consumer written for [`HarnessEvent::PermissionRequest`]
//!    would otherwise silently accept a grid heuristic, and the entire point of
//!    the structured path is that it does not have to. This is the one place the
//!    enum is deliberately asymmetric, and
//!    [`ObservedEvent`] exists only so that asymmetry has a name.

pub mod claude;
pub mod transcript;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pathseg::PathSegment;

// ── identities ──────────────────────────────────────────────────────────────

/// Which structured harness a pane is driven through.
///
/// R1 has one variant. opencode (R3) and ACP/Copilot (R4) join it; `gemini` is
/// deliberately absent because it has no structured surface and stays PTY-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
}

/// Which implementation of [`AgentPane`] a pane is.
///
/// Persisted onto the roster as `pane_kind` by R2 — additive, absent meaning
/// [`PaneKind::Pty`], so every roster written before R2 reads correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    Pty,
    Structured(Harness),
}

/// A turn's identity within one pane's session.
///
/// Deliberately a **counter local to the decoder**, not the CLI's own `uuid`.
/// The stream carries several uuids per turn (a `uuid` per message, a
/// `message.id` shared by the messages of one API step) and none of them is "the
/// turn"; a monotonic local number is the only identifier that means the same
/// thing on every harness, which matters because R3 and R4 emit this same event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TurnId(pub u64);

/// The `tool_use` block's own id, as the harness reported it.
///
/// A harness-supplied string rather than a local counter, because it is the join
/// key: the `tool_result` that answers a call carries the same id, and a local
/// number could not match them up.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolUseId(pub String);

/// A pending permission request's id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

// ── what a pane is told ─────────────────────────────────────────────────────

/// One thing loomux sends into a pane.
///
/// The four variants are today's delivery vocabulary named once, so the queue
/// drainer's last step is `pane.send(turn)` on both pane kinds:
/// `Kickoff`/`Prompt` are `Delivery::{FreshKickoff, ResumeKickoff}` and
/// `Delivery::MidSession`, `Notice` is the `[orrerix]` channel, and `Human` is
/// the compose strip (#43).
///
/// **The variant is not decoration.** A structured driver renders `Notice` with
/// its marker prefix and `Human` without one, and R2's `permissions.json` policy
/// is allowed to read it; a driver that flattened all four to a string would
/// make "who said this" unrecoverable one layer down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Turn {
    Kickoff(String),
    Prompt(String),
    Notice(String),
    Human(String),
}

impl Turn {
    /// The text, whichever variant carries it.
    pub fn text(&self) -> &str {
        match self {
            Turn::Kickoff(t) | Turn::Prompt(t) | Turn::Notice(t) | Turn::Human(t) => t,
        }
    }
}

/// What [`AgentPane::send`] gives back.
///
/// `accepted_at_bytes` is the byte offset the driver had written to the child's
/// stdin *after* this turn — a receipt that the bytes left loomux, and nothing
/// stronger. It is deliberately **not** called an acknowledgement: Claude Code
/// only echoes a user message back under `--replay-user-messages`, which R1 does
/// not pass (§6.1), so no acknowledgement exists to report. Claiming one here
/// would be exactly the "submitted, probably" guess the PTY path had to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    pub accepted_at_bytes: u64,
}

/// How a permission request was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
}

/// **Who** settled it — a property of the entry point, never an argument a
/// caller supplies.
///
/// The same closed-enum shape `humanq::AnswerSource` uses, and for the same
/// reason: "settle as someone else" must have no spelling, so there is nothing
/// to validate and nothing to forge. R2 adds the entry points; R1 fixes the
/// vocabulary so they cannot each invent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// A rule in `permissions.json` decided it without asking anyone.
    Policy,
    /// A human answered through a trusted surface.
    Human,
    /// The pane went away with the request still pending.
    PaneExited,
}

// ── what a pane reports ─────────────────────────────────────────────────────

/// Token counts for one `result` message.
///
/// **The two fields answer different questions and are never added together**,
/// which is why they are named rather than merged. Both come from the same
/// line; the docs are explicit that their scopes differ
/// (<https://code.claude.com/docs/en/agent-sdk/cost-tracking>):
///
/// - [`Usage::call_cumulative`] is the running total for the whole call,
///   subagents included, taken from `modelUsage`. It is **cumulative**, so a
///   consumer reads the latest one and never sums across turns — summing would
///   multiply a long-lived pane's usage by roughly its turn count.
/// - [`Usage::this_turn_main_loop`] is the `usage` field: this turn only, and
///   only the top-level loop, so it *undercounts* as soon as subagents run.
///
/// Anything that wants "what did this pane cost" wants the first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub call_cumulative: Tokens,
    pub this_turn_main_loop: Option<Tokens>,
    /// Per-model breakdown of `call_cumulative`, in the stream's own order of
    /// appearance. Kept because a pane can switch models mid-session and a
    /// single total then attributes spend to whichever model it happens to be on.
    pub per_model: Vec<ModelTokens>,
}

/// loomux's four token buckets.
///
/// **Thinking tokens fold into `output`.** The docs say `outputTokens` already
/// includes `thinkingTokens` and that the two must not be added
/// (<https://code.claude.com/docs/en/agent-sdk/typescript#modelusage>), and
/// `doc/design/opencode.md` already folded opencode's separate reasoning counter
/// the same way — so one bucket means one thing across harnesses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }
}

/// One model's share of [`Usage::call_cumulative`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTokens {
    pub model: String,
    pub tokens: Tokens,
}

/// A dollar figure, with the basis that makes it readable.
///
/// There is one variant today and it is still an enum, because the field this
/// feeds — `group_usage`'s basis — already distinguishes a vendor-computed
/// figure (opencode's, `reported`) from a price-table guess, and a bare `f64`
/// here would lose which one this is on the way in.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub usd: f64,
    pub basis: CostBasis,
}

/// Where a [`Cost`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostBasis {
    /// The harness computed it itself and **calls it an estimate**. Claude
    /// Code's `total_cost_usd` is "a client-side estimate, not authoritative
    /// billing data … Do not bill end users or trigger financial decisions from
    /// these fields" (<https://code.claude.com/docs/en/agent-sdk/cost-tracking>),
    /// so it lands in `group_usage`'s *estimated* basis rather than *reported*
    /// — the same basis its PTY sibling uses. Only the source tag changes.
    HarnessEstimate,
}

/// Why a turn ended.
///
/// `Other` carries the raw string rather than collapsing to a catch-all: the
/// docs list nineteen `terminal_reason` values today and call the set open, and
/// a value this build does not know is exactly the thing a human debugging a
/// stuck pane needs to see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Completed,
    MaxTurns,
    Aborted,
    Error,
    Other(String),
}

/// What triggered a compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactTrigger {
    Manual,
    Auto,
}

/// Evidence a PTY pane **inferred** from its own screen.
///
/// Structurally separate from every reported variant of [`HarnessEvent`] so that
/// a consumer of a reported fact cannot be handed a heuristic (see this module's
/// header, rule 2). A structured pane never emits one of these; R1 defines the
/// vocabulary and the PTY adapter (#888 A4-18′) fills it in.
///
/// **The `tag` is load-bearing, not style.** [`HarnessEvent`] is internally
/// tagged, and an internally-tagged newtype variant can only hold something that
/// serializes as a map — a unit variant like `ReadyMarker` would otherwise
/// serialize as the bare string `"ready_marker"` and the whole event would fail
/// to serialize at runtime, on the one variant no decoder test produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "observed", rename_all = "snake_case")]
pub enum ObservedEvent {
    /// The question detector's grid evidence matched.
    QuestionSuspected { matched: String },
    /// The CLI's readiness marker appeared.
    ReadyMarker,
    /// The screen stopped changing.
    Quiet,
    /// The screen changed.
    Painted,
}

/// Everything a pane can report.
///
/// The variants a **structured** pane emits are reported facts. The variants a
/// **PTY** pane emits are `Booted`, `Exited` and `Observed(..)` — the note's
/// §1.3 table is the full mapping, and its "never emitted" columns are the
/// contract rather than an omission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HarnessEvent {
    /// The session exists. `session` is `None` on a pane that has not learned
    /// its id yet — a PTY pane before the session watcher binds one — never an
    /// empty string standing in for one.
    Booted {
        session: Option<String>,
        model: Option<String>,
        capabilities: Vec<String>,
    },
    TurnStarted {
        turn: TurnId,
    },
    Text {
        turn: TurnId,
        delta: String,
    },
    ToolCall {
        turn: TurnId,
        id: ToolUseId,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        turn: TurnId,
        id: ToolUseId,
        ok: bool,
    },
    PermissionRequest {
        id: RequestId,
        tool: String,
        input: serde_json::Value,
    },
    PermissionSettled {
        id: RequestId,
        decision: Decision,
        by: DecisionSource,
    },
    TurnEnded {
        turn: TurnId,
        usage: Option<Usage>,
        cost: Option<Cost>,
        stop: StopReason,
    },
    Compacted {
        trigger: CompactTrigger,
        pre_tokens: Option<u64>,
    },
    Exited {
        code: Option<i32>,
    },
    Observed(ObservedEvent),
}

impl HarnessEvent {
    /// Whether this event is **decision-grade** — the note's §4.3 split between
    /// what the group audit log gets and what stays in the per-pane log.
    ///
    /// The audit log is small, permanently retained and read by a human
    /// reconstructing what happened. A transcript in it would drown the
    /// decisions it exists for, so assistant text, tool results, deltas and
    /// `Booted` detail stay in the per-pane log.
    pub fn is_decision_grade(&self) -> bool {
        matches!(
            self,
            HarnessEvent::ToolCall { .. }
                | HarnessEvent::PermissionRequest { .. }
                | HarnessEvent::PermissionSettled { .. }
                | HarnessEvent::TurnEnded { .. }
                | HarnessEvent::Exited { .. }
        )
    }
}

// ── the pane ────────────────────────────────────────────────────────────────

/// The receiving half of a pane's event stream.
pub type EventRx = std::sync::mpsc::Receiver<HarnessEvent>;

/// One agent pane, whatever drives it.
///
/// `PaneHost::request_pane` hands one of these back instead of a byte pipe
/// (#888 A4-18′, and the paragraph this slice added to
/// `doc/design/engine-extraction.md` §2). That is what keeps one spawn path, one
/// delivery front door and one idle model across both pane kinds, and it is what
/// the daemon-side `PaneHost` implements once instead of twice.
pub trait AgentPane: Send + Sync {
    fn kind(&self) -> PaneKind;
    /// Deliver one turn. See [`SendReceipt`] for what the receipt does and does
    /// not claim.
    fn send(&self, turn: Turn) -> Result<SendReceipt, String>;
    /// Settle a pending permission request. **R1 refuses on every
    /// implementation** — the registry and the MCP tool that make an answer
    /// meaningful are R2's.
    fn answer(&self, req: RequestId, decision: Decision) -> Result<(), String>;
    /// End the turn in progress without killing the pane.
    fn interrupt(&self) -> Result<(), String>;
    /// The event stream. Callable once per pane; a second call gets `None`,
    /// because an `mpsc::Receiver` has exactly one consumer and handing out a
    /// second would silently split the stream between them.
    fn events(&self) -> Option<EventRx>;
    /// `None` until the pane knows its id.
    fn session_id(&self) -> Option<String>;
}

// ── the per-pane event log (§4) ─────────────────────────────────────────────

/// Bytes per segment before rotation. 8 MiB × [`SEGMENTS_RETAINED`] is the
/// per-pane ceiling the note contracts, and the ceiling is what §4.4's
/// "stronger than H4" reattach claim is scoped to.
pub const SEGMENT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// How many segments survive. The oldest is deleted, never renamed.
pub const SEGMENTS_RETAINED: usize = 4;

/// How much of an unrecognized stream line is kept. Enough to identify the
/// message type and its shape, bounded so a pathological line cannot make one
/// log record dominate a segment.
pub const UNKNOWN_LINE_MAX_BYTES: usize = 4096;

/// One line of a pane's event log.
///
/// `seq` is monotonic **within a pane's log**, across segments, so a reader
/// stitching segments together can tell a gap from a reordering. `ts` is
/// wall-clock millis since the epoch, for a human reading the file; nothing
/// orders by it, because a wall clock can go backwards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    pub seq: u64,
    /// Millis since the epoch, as **`u64`** rather than the `u128`
    /// `Duration::as_millis` hands back. `#[serde(flatten)]` below routes the
    /// whole struct through serde's `Content` buffer, which has no 128-bit
    /// variant — a `u128` here fails at *runtime*, on the one path a type check
    /// cannot see. `obs::now_ms` already narrows the same way.
    pub ts: u64,
    #[serde(flatten)]
    pub body: LogBody,
}

/// What a [`LogRecord`] carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum LogBody {
    /// A decoded event.
    Event { event: HarnessEvent },
    /// A stream line this build did not recognize.
    ///
    /// **Unknown message types are ignored as events, not as evidence.**
    /// Discarding the line entirely would satisfy the protocol note's
    /// ignore-what-you-do-not-know rule and leave nobody able to add support for
    /// it later, so the raw line is kept here, truncated to
    /// [`UNKNOWN_LINE_MAX_BYTES`].
    Unknown { raw: String, truncated: bool },
    /// A stream line that is real but is not a pane event — `system/api_retry`
    /// is the one R1 knows.
    Note { note: String },
}

/// The append-only, segmented log behind one pane.
///
/// **Segments are numbered and never renamed** (§4.2). The live segment is the
/// highest `n`, rotation *starts* `n + 1`, and retention *deletes* the lowest.
/// The obvious alternative — shifting `.1` → `.2` on each rotation, the
/// logrotate shape — renames a file while an append-only writer holds it open
/// and a replay reader may be reading it, which on this project's Windows
/// baseline is the classic sharing-violation shape and would appear only under
/// the exact load this feature creates. Numbering removes the question rather
/// than answering it: a reader names a segment by `n`, so nothing it holds ever
/// moves underneath it.
///
/// What is deliberately **not** decided here, and is R2's with its own note:
/// when the rotation check runs relative to the append, how a reader learns a
/// new segment exists, and what a reader does when retention deletes the segment
/// it is mid-replay of.
pub struct EventLog {
    dir: PathBuf,
    agent: PathSegment,
    segment: u64,
    written: u64,
    seq: u64,
}

impl EventLog {
    /// Open (or start) the log for one pane under an already-resolved directory.
    ///
    /// `dir` is passed in rather than derived: building it from a group id is a
    /// `Path::join` on a `GroupId`, and CLAUDE.md constraint 6 gives that
    /// exactly one home (`group_dir_at`, in `src-tauri`). R1 is an engine leaf
    /// and adds no second join; R2 passes `<group dir>/panes`.
    ///
    /// `agent` is a [`PathSegment`], so an unvalidated id cannot reach the file
    /// name in [`Self::segment_path`] — the type is the proof the
    /// `src-tauri/tests/pathseg.rs` filename scan's allowlist row names.
    pub fn open(dir: &Path, agent: PathSegment) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let mut log = EventLog {
            dir: dir.to_path_buf(),
            agent,
            segment: 0,
            written: 0,
            seq: 0,
        };
        let existing = log.segments()?;
        if let Some(&top) = existing.last() {
            log.segment = top;
            log.written = fs::metadata(log.segment_path(top))?.len();
            log.seq = last_seq_in(&log.segment_path(top))?.map_or(0, |s| s + 1);
        }
        Ok(log)
    }

    /// The path of segment `n`.
    ///
    /// The one interpolation of an identifier into a file name in this module,
    /// and the reason [`EventLog::agent`] is a [`PathSegment`] rather than a
    /// `String`.
    /// The [`PathSegment`] is interpolated **directly**, not bound to a `&str`
    /// first: `format!` accepts either, so a local `let agent = ….as_str()`
    /// would put a raw string at the interpolation and leave the type argument
    /// one line further away than the guard that reads this line.
    pub fn segment_path(&self, n: u64) -> PathBuf {
        self.dir.join(format!("{}.events.{n}.jsonl", self.agent))
    }

    /// Every segment number present, ascending.
    pub fn segments(&self) -> std::io::Result<Vec<u64>> {
        let prefix = format!("{}.events.", self.agent.as_str());
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Some(num) = rest.strip_suffix(".jsonl") else {
                continue;
            };
            if let Ok(n) = num.parse::<u64>() {
                out.push(n);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// The next `seq` this log will write. Exposed for tests and for a reader
    /// that wants to know whether anything has been written at all.
    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    /// Append one record, rotating first if the live segment is full.
    ///
    /// Rotation is checked **before** the write rather than after, so a segment
    /// never exceeds [`SEGMENT_MAX_BYTES`] by more than nothing: the check is
    /// "would this line take us over", and a single line longer than the whole
    /// budget still gets its own segment rather than being refused.
    pub fn append(&mut self, body: LogBody) -> std::io::Result<u64> {
        let rec = LogRecord {
            seq: self.seq,
            ts: now_millis(),
            body,
        };
        let mut line = serde_json::to_string(&rec)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        line.push('\n');
        let bytes = line.len() as u64;

        let rotated = self.written > 0 && self.written.saturating_add(bytes) > SEGMENT_MAX_BYTES;
        if rotated {
            self.segment += 1;
            self.written = 0;
            self.retain()?;
        }

        let path = self.segment_path(self.segment);
        let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(line.as_bytes())?;
        self.written += bytes;
        let seq = self.seq;
        self.seq += 1;

        // Retention runs AFTER the new segment exists, not before it. Counting
        // the window while the new segment is still absent would keep one
        // segment too many — an off-by-one that only appears once the log has
        // rotated `SEGMENTS_RETAINED` times, which is late enough that no early
        // test would see it.
        if rotated {
            self.retain()?;
        }
        Ok(seq)
    }

    /// Record a decoded event.
    pub fn record(&mut self, event: HarnessEvent) -> std::io::Result<u64> {
        self.append(LogBody::Event { event })
    }

    /// Record a stream line this build did not recognize, truncated.
    ///
    /// Truncation is by **bytes at a char boundary**, not by chars: the cap
    /// bounds what one record can cost the segment, and a slice taken mid-UTF-8
    /// would not serialize at all.
    pub fn record_unknown(&mut self, raw: &str) -> std::io::Result<u64> {
        let (raw, truncated) = truncate_on_char_boundary(raw, UNKNOWN_LINE_MAX_BYTES);
        self.append(LogBody::Unknown {
            raw: raw.to_string(),
            truncated,
        })
    }

    /// Delete every segment below the retention window.
    fn retain(&self) -> std::io::Result<()> {
        let segments = self.segments()?;
        if segments.len() <= SEGMENTS_RETAINED {
            return Ok(());
        }
        for n in &segments[..segments.len() - SEGMENTS_RETAINED] {
            // A retention delete is best-effort by design: a reader holding the
            // segment open is the case §4.2 leaves to R2, and failing the append
            // that triggered the retention would lose a live event to protect a
            // dead one.
            let _ = fs::remove_file(self.segment_path(*n));
        }
        Ok(())
    }
}

/// The highest `seq` in a segment, or `None` for an empty/absent one.
///
/// Read by scanning the file rather than by trusting its length, because a
/// crash mid-append can leave a partial last line; a line that does not parse is
/// skipped rather than fatal, so a torn tail costs one `seq` and not the pane.
fn last_seq_in(path: &Path) -> std::io::Result<Option<u64>> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(None);
    };
    let mut top = None;
    for line in text.lines() {
        if let Ok(rec) = serde_json::from_str::<LogRecord>(line) {
            top = Some(top.map_or(rec.seq, |t: u64| t.max(rec.seq)));
        }
    }
    Ok(top)
}

/// Truncate to at most `max` **bytes**, never splitting a character.
pub(crate) fn truncate_on_char_boundary(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(s: &str) -> PathSegment {
        PathSegment::parse(s).expect("test agent id must be a valid segment")
    }

    fn tmpdir(tag: &str) -> PathBuf {
        // No `tempfile` with default features anywhere near this crate
        // (CLAUDE.md constraint 2 — it switches on getrandom); uniqueness comes
        // from a process id plus a counter, the same std-only shape
        // `fsatomic` uses.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "loomux-harness-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn an_unknown_fact_is_none_and_never_a_sentinel_string() {
        // Rule 1 of the module header, pinned where a future edit would break
        // it: the absence of a session id must not be spellable as a value.
        let ev = HarnessEvent::Booted {
            session: None,
            model: None,
            capabilities: vec![],
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"session\":null"),
            "absence must serialize as null, not as a sentinel: {json}"
        );
        assert!(
            !json.contains("unknown"),
            "no sentinel string may appear: {json}"
        );
    }

    #[test]
    fn observed_evidence_and_a_reported_fact_have_different_constructors() {
        // Rule 2. The assertion is on the WIRE tag rather than on the Rust
        // variant, because that is the form a consumer in another process (a
        // remote client, a replay reader) actually branches on.
        let inferred = HarnessEvent::Observed(ObservedEvent::QuestionSuspected {
            matched: "Do you want to proceed?".into(),
        });
        let reported = HarnessEvent::PermissionRequest {
            id: RequestId("r1".into()),
            tool: "Bash".into(),
            input: serde_json::json!({"command": "git push"}),
        };
        let a = serde_json::to_string(&inferred).unwrap();
        let b = serde_json::to_string(&reported).unwrap();
        assert!(a.contains("\"kind\":\"observed\""), "{a}");
        assert!(b.contains("\"kind\":\"permission_request\""), "{b}");
        assert_ne!(a, b);
        // And the negative control the pair exists for: an `Observed` event must
        // not deserialize into the reported variant.
        let back: HarnessEvent = serde_json::from_str(&a).unwrap();
        assert!(!matches!(back, HarnessEvent::PermissionRequest { .. }));
    }

    #[test]
    fn only_decision_grade_events_reach_the_audit_log() {
        // §4.3, as a set assertion rather than a list walk: a variant folded
        // into the wrong half later fails here even though every individual
        // `matches!` arm still looks right.
        let decision_grade = [
            HarnessEvent::ToolCall {
                turn: TurnId(1),
                id: ToolUseId("t".into()),
                name: "Bash".into(),
                input: serde_json::Value::Null,
            },
            HarnessEvent::PermissionRequest {
                id: RequestId("r".into()),
                tool: "Bash".into(),
                input: serde_json::Value::Null,
            },
            HarnessEvent::PermissionSettled {
                id: RequestId("r".into()),
                decision: Decision::Deny,
                by: DecisionSource::Policy,
            },
            HarnessEvent::TurnEnded {
                turn: TurnId(1),
                usage: None,
                cost: None,
                stop: StopReason::Completed,
            },
            HarnessEvent::Exited { code: Some(0) },
        ];
        let per_pane_only = [
            HarnessEvent::Booted {
                session: Some("s".into()),
                model: None,
                capabilities: vec![],
            },
            HarnessEvent::TurnStarted { turn: TurnId(1) },
            HarnessEvent::Text {
                turn: TurnId(1),
                delta: "hi".into(),
            },
            HarnessEvent::ToolResult {
                turn: TurnId(1),
                id: ToolUseId("t".into()),
                ok: true,
            },
            HarnessEvent::Compacted {
                trigger: CompactTrigger::Auto,
                pre_tokens: Some(1),
            },
            HarnessEvent::Observed(ObservedEvent::Quiet),
        ];
        for e in &decision_grade {
            assert!(e.is_decision_grade(), "{e:?} must reach the audit log");
        }
        for e in &per_pane_only {
            assert!(!e.is_decision_grade(), "{e:?} must NOT reach the audit log");
        }
        // Positive control on the population itself: the two lists together
        // must cover every variant the enum has, or this test silently stops
        // checking a variant somebody adds.
        assert_eq!(
            decision_grade.len() + per_pane_only.len(),
            11,
            "HarnessEvent gained or lost a variant — classify it in §4.3 and \
             add it to one of these two lists"
        );
    }

    #[test]
    fn every_event_variant_survives_a_json_round_trip() {
        // Not a formality. `HarnessEvent` is internally tagged, which can only
        // carry a payload that serializes as a MAP — so `Observed(ReadyMarker)`,
        // a unit variant, is the one shape that would fail at runtime while
        // every decoder test stayed green, because the decoder never produces
        // an `Observed` at all. The round trip is what catches it.
        let all = [
            HarnessEvent::Booted {
                session: None,
                model: None,
                capabilities: vec![],
            },
            HarnessEvent::TurnStarted { turn: TurnId(1) },
            HarnessEvent::Text {
                turn: TurnId(1),
                delta: "x".into(),
            },
            HarnessEvent::ToolCall {
                turn: TurnId(1),
                id: ToolUseId("t".into()),
                name: "Bash".into(),
                input: serde_json::json!({"a": 1}),
            },
            HarnessEvent::ToolResult {
                turn: TurnId(1),
                id: ToolUseId("t".into()),
                ok: false,
            },
            HarnessEvent::PermissionRequest {
                id: RequestId("r".into()),
                tool: "Bash".into(),
                input: serde_json::Value::Null,
            },
            HarnessEvent::PermissionSettled {
                id: RequestId("r".into()),
                decision: Decision::Allow,
                by: DecisionSource::Human,
            },
            HarnessEvent::TurnEnded {
                turn: TurnId(1),
                usage: Some(Usage::default()),
                cost: Some(Cost {
                    usd: 1.5,
                    basis: CostBasis::HarnessEstimate,
                }),
                stop: StopReason::Other("something_new".into()),
            },
            HarnessEvent::Compacted {
                trigger: CompactTrigger::Manual,
                pre_tokens: None,
            },
            HarnessEvent::Exited { code: None },
            HarnessEvent::Observed(ObservedEvent::ReadyMarker),
            HarnessEvent::Observed(ObservedEvent::QuestionSuspected {
                matched: "?".into(),
            }),
        ];
        for ev in &all {
            let json = serde_json::to_string(ev)
                .unwrap_or_else(|e| panic!("{ev:?} does not serialize: {e}"));
            let back: HarnessEvent = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("{ev:?} does not round trip from {json}: {e}"));
            assert_eq!(&back, ev);
        }
    }

    #[test]
    fn tokens_never_add_thinking_twice() {
        // The trap named on `Tokens`: `output` already contains thinking, so a
        // total that added a thinking field would double count. There is no
        // thinking field, and this pins that there is not one to add.
        let t = Tokens {
            input: 10,
            output: 20,
            cache_read: 3,
            cache_creation: 4,
        };
        assert_eq!(t.total(), 37);
        let json = serde_json::to_string(&t).unwrap();
        assert!(
            !json.contains("thinking"),
            "thinking must stay folded into output, not become a fifth bucket: {json}"
        );
    }

    #[test]
    fn a_segment_is_never_renamed_and_the_oldest_is_deleted() {
        // §4.2's contracted half. The check is on the SET of file names before
        // and after rotation: a logrotate-style shift would leave the same
        // count with different contents, which a count alone cannot see.
        let dir = tmpdir("rotate");
        let mut log = EventLog::open(&dir, seg("w-1")).unwrap();
        assert_eq!(log.segments().unwrap(), Vec::<u64>::new());

        // Force rotation without writing 8 MiB: drive `segment`/`written` the
        // way a full segment would leave them.
        for n in 0..(SEGMENTS_RETAINED as u64 + 2) {
            log.segment = n;
            log.written = 0;
            log.record(HarnessEvent::TurnStarted { turn: TurnId(n) })
                .unwrap();
        }
        let after = log.segments().unwrap();
        assert_eq!(
            after,
            vec![0, 1, 2, 3, 4, 5],
            "retention has not been triggered by these direct writes"
        );

        // Now the real path: an append that overflows the live segment.
        log.written = SEGMENT_MAX_BYTES;
        log.record(HarnessEvent::TurnStarted { turn: TurnId(99) })
            .unwrap();
        let after = log.segments().unwrap();
        assert_eq!(
            after,
            vec![3, 4, 5, 6],
            "rotation must START a new segment and retention must DELETE the \
             lowest — never rename one"
        );
        // The surviving segments still hold what they held: a rename would have
        // moved segment 3's contents somewhere else.
        let three = fs::read_to_string(log.segment_path(3)).unwrap();
        assert!(three.contains("\"turn\":3"), "segment 3's content moved: {three}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn seq_is_monotonic_across_segments_and_across_reopen() {
        let dir = tmpdir("seq");
        let mut log = EventLog::open(&dir, seg("w-2")).unwrap();
        for _ in 0..3 {
            log.record(HarnessEvent::Observed(ObservedEvent::Quiet))
                .unwrap();
        }
        assert_eq!(log.next_seq(), 3);
        drop(log);

        let mut reopened = EventLog::open(&dir, seg("w-2")).unwrap();
        assert_eq!(
            reopened.next_seq(),
            3,
            "a reopened log must continue the sequence, not restart it — a \
             restart makes a gap indistinguishable from a reorder"
        );
        let seq = reopened
            .record(HarnessEvent::Observed(ObservedEvent::Painted))
            .unwrap();
        assert_eq!(seq, 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_line_is_kept_truncated_rather_than_discarded() {
        let dir = tmpdir("unknown");
        let mut log = EventLog::open(&dir, seg("w-3")).unwrap();
        let short = r#"{"type":"something_new","v":1}"#;
        log.record_unknown(short).unwrap();
        let long = "x".repeat(UNKNOWN_LINE_MAX_BYTES + 500);
        log.record_unknown(&long).unwrap();

        let text = fs::read_to_string(log.segment_path(0)).unwrap();
        let recs: Vec<LogRecord> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(recs.len(), 2);
        match &recs[0].body {
            LogBody::Unknown { raw, truncated } => {
                assert_eq!(raw, short);
                assert!(!truncated);
            }
            other => panic!("expected an unknown record, got {other:?}"),
        }
        match &recs[1].body {
            LogBody::Unknown { raw, truncated } => {
                assert_eq!(raw.len(), UNKNOWN_LINE_MAX_BYTES);
                assert!(truncated, "a truncated line must SAY it was truncated");
            }
            other => panic!("expected an unknown record, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // The cap is in bytes and the content is UTF-8, so the boundary walk is
        // load-bearing: a naive `&s[..max]` panics here.
        let s = "é".repeat(10); // 20 bytes, 10 chars
        let (cut, truncated) = truncate_on_char_boundary(&s, 5);
        assert!(truncated);
        assert_eq!(cut.len(), 4, "must back off to the char boundary below 5");
        assert!(cut.chars().all(|c| c == 'é'));
        let (whole, truncated) = truncate_on_char_boundary(&s, 100);
        assert_eq!(whole, s);
        assert!(!truncated);
    }
}
