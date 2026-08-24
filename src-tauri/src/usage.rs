//! Per-session token usage and dollar cost, read from each agent CLI's own
//! transcript records rather than scraped from the pane statusline.
//!
//! # Source of truth per CLI (and its limits)
//!
//! **Claude Code** writes one JSONL line per message into
//! `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`. Each assistant
//! message carries an exact `usage` object (`input_tokens`, `output_tokens`,
//! `cache_creation_input_tokens`, `cache_read_input_tokens`) and the `model`
//! that produced it. We sum those — deduplicating by message id so a resumed
//! or replayed transcript isn't double-counted — and derive dollars from a
//! small, dated price table (`price_for`). Token counts are therefore *exact*;
//! the dollar figure is an *estimate* (subscription/Max accounts pay no
//! marginal dollar cost at all, so the statusline shows `$0.00` regardless of
//! real usage — tokens are the honest metric there).
//!
//! **Copilot CLI** keeps only `session-state/<id>/workspace.yaml`, which
//! records no token counts we can read today. So copilot sessions have no
//! transcript usage source; the orchestration layer falls back to the
//! last-resort statusline parse for them. If a future copilot build writes a
//! usage record, add a `copilot_session_usage` reader here and it slots in
//! ahead of the fallback with no other changes.
//!
//! **OpenCode** writes no transcript file at all: sessions, messages and parts
//! live in one SQLite database, and the `session` row itself already carries
//! the dollar cost OpenCode computed *and* five token counters. So
//! `opencode_session_usage` reads one row instead of folding message records —
//! and the dollars are **reported by the CLI, not estimated here**, which is
//! why OpenCode needs no `price_for` entry. The read itself lives in
//! `crate::opencodedb`; this module only maps its shape onto
//! [`SessionUsage`], which is the lossy half and belongs where the other CLIs'
//! mappings are.
//!
//! Everything here is best-effort and pure where it matters: the parser
//! (`parse_claude_transcript`) takes text and is exercised by fixture tests,
//! never a live CLI.

use loomux_engine::obs::LockExt;
use loomux_engine::pathseg::PathSegment;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Exact token counts for a session, split by kind so the UI can show tokens
/// even when no dollar figure is available.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl TokenUsage {
    /// Every token the session touched — the headline "tokens" figure.
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }
}

/// One session's usage, tokens plus a best-effort dollar estimate.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SessionUsage {
    pub tokens: TokenUsage,
    /// Dollar cost derived from the price table, or `None` when no message in
    /// the transcript used a model we have a price for (token display only).
    pub cost_usd: Option<f64>,
    /// The model the cost was priced against (the one with the most output
    /// tokens), for display and debugging. `None` when unpriced.
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// Price table
// ---------------------------------------------------------------------------

/// USD per **one million** tokens for a model family. Cache-write is the
/// 5-minute-ephemeral rate (1.25× input) — Claude Code's default breakpoint;
/// cache-read is 0.1× input.
#[derive(Clone, Copy, Debug)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

/// Model prices in USD per 1M tokens. **Updated 2026-07-04** from Anthropic's
/// published rates (see the claude-api reference). Matching is by substring of
/// the transcript's model id, so `claude-opus-4-8`, `claude-opus-4-7`, … all
/// resolve to the Opus row. Unknown models return `None` and fall back to
/// token-only display. To update: change the numbers here and the date above.
///
/// Note: these are standard rates. Sonnet 5 has a lower introductory rate
/// ($2/$10 per 1M) through 2026-08-31; we use the standard $3/$15 so the
/// estimate never *under*-reports spend. Revisit if the intro rate outlives it.
pub fn price_for(model: &str) -> Option<ModelPrice> {
    let m = model.to_ascii_lowercase();
    // Order matters only in that each family is a distinct substring.
    if m.contains("opus") {
        Some(ModelPrice { input: 5.0, output: 25.0, cache_write: 6.25, cache_read: 0.5 })
    } else if m.contains("sonnet") {
        Some(ModelPrice { input: 3.0, output: 15.0, cache_write: 3.75, cache_read: 0.3 })
    } else if m.contains("haiku") {
        Some(ModelPrice { input: 1.0, output: 5.0, cache_write: 1.25, cache_read: 0.1 })
    } else if m.contains("fable") || m.contains("mythos") {
        Some(ModelPrice { input: 10.0, output: 50.0, cache_write: 12.5, cache_read: 1.0 })
    } else {
        None
    }
}

/// Dollar cost of a token bundle at a given price (per-1M rates).
fn cost_of(t: &TokenUsage, p: &ModelPrice) -> f64 {
    (t.input_tokens as f64 * p.input
        + t.output_tokens as f64 * p.output
        + t.cache_creation_tokens as f64 * p.cache_write
        + t.cache_read_tokens as f64 * p.cache_read)
        / 1_000_000.0
}

// ---------------------------------------------------------------------------
// Claude Code transcript parsing
// ---------------------------------------------------------------------------

/// Pull a u64 usage field, tolerating absent/null.
fn u64_field(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Parse a Claude Code session transcript (JSONL text) into summed usage plus
/// a token-derived cost estimate. Pure and fixture-testable.
///
/// Rules mirroring how Claude Code writes transcripts:
/// - Only `assistant` messages carry a `usage` object; user/summary lines are
///   skipped.
/// - The same assistant message can appear more than once (streaming replays,
///   `--resume` re-emits); dedupe by `message.id` so tokens aren't
///   double-counted. Lines without an id are always counted (can't dedupe).
/// - Synthetic messages (`model` == `"<synthetic>"`) are not billable and
///   never contribute a model/price.
/// - Cost accumulates per message at its own model's price, so a session that
///   switched models is priced correctly; if no message used a priced model,
///   `cost_usd` is `None`.
pub fn parse_claude_transcript(text: &str) -> SessionUsage {
    parse_claude_transcript_lines(text.lines())
}

/// [`parse_claude_transcript`] over a LINE ITERATOR instead of one `&str`.
///
/// Same rules, same result — the `&str` form is literally this function fed
/// `text.lines()`, so the two cannot drift. It exists because the caller that
/// reads a transcript off disk must never hold the whole file: see
/// [`claude_session_usage_in`] and #1218.
///
/// The fold is per-line and lives in [`TranscriptFold`]; nothing but the
/// source of the lines changes between callers. Memory is bounded by the
/// longest single line plus the dedupe set of message ids — not by the file.
pub fn parse_claude_transcript_lines<I, S>(lines: I) -> SessionUsage
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut fold = TranscriptFold::default();
    for line in lines {
        fold.push(line.as_ref());
    }
    fold.usage()
}

/// The running state of a transcript fold: the four token totals, the accrued
/// dollar cost, the message-id dedupe set, and the best-priced model so far.
///
/// **Why it is a struct (#1239).** The fold was already per-line, but it lived
/// inside `parse_claude_transcript_lines` as six locals, which meant it could
/// only ever run to completion in one call. The incremental reader
/// ([`TranscriptCursors`]) keeps one of these alive ACROSS polls and pushes
/// only the newly-appended lines into it, so a tick's work is proportional to
/// what the agent just wrote rather than to the whole session.
///
/// Extracting it is also what keeps the whole-file parse and the incremental
/// one from drifting: there is exactly one implementation of the rules
/// documented on [`parse_claude_transcript`], and both paths are that
/// implementation fed different lines. Two folds could not have been kept in
/// step by review.
#[derive(Default)]
struct TranscriptFold {
    totals: TokenUsage,
    cost: f64,
    any_priced: bool,
    seen: HashSet<String>,
    /// The priced model with the most output tokens, for display.
    best_model: Option<(String, u64)>,
}

impl TranscriptFold {
    /// Fold ONE transcript line in, per the rules on
    /// [`parse_claude_transcript`]. A line that is blank, unparseable, not an
    /// assistant turn, carries no `usage`, or repeats an already-seen message
    /// id contributes nothing — which is what makes the fold safe to feed the
    /// same line twice.
    fn push(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        let Some(msg) = v.get("message") else { return };
        let Some(usage) = msg.get("usage") else { return };

        // Dedupe by message id when present.
        if let Some(id) = msg.get("id").and_then(Value::as_str) {
            if !self.seen.insert(id.to_string()) {
                return;
            }
        }

        let t = TokenUsage {
            input_tokens: u64_field(usage, "input_tokens"),
            output_tokens: u64_field(usage, "output_tokens"),
            cache_creation_tokens: u64_field(usage, "cache_creation_input_tokens"),
            cache_read_tokens: u64_field(usage, "cache_read_input_tokens"),
        };
        self.totals.input_tokens += t.input_tokens;
        self.totals.output_tokens += t.output_tokens;
        self.totals.cache_creation_tokens += t.cache_creation_tokens;
        self.totals.cache_read_tokens += t.cache_read_tokens;

        let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
        if model.is_empty() || model == "<synthetic>" {
            return;
        }
        if let Some(p) = price_for(model) {
            self.cost += cost_of(&t, &p);
            self.any_priced = true;
            let out = t.output_tokens;
            match &mut self.best_model {
                Some((_, best_out)) if *best_out >= out => {}
                _ => self.best_model = Some((model.to_string(), out)),
            }
        }
    }

    /// The session usage as of everything folded in so far. Cheap enough to
    /// call on every poll — it clones one model id and copies four counters.
    fn usage(&self) -> SessionUsage {
        SessionUsage {
            tokens: self.totals,
            cost_usd: self.any_priced.then_some(self.cost),
            model: self.best_model.as_ref().map(|(m, _)| m.clone()),
        }
    }
}

/// Approximate current context-window usage from a Claude Code transcript
/// (#328): the LATEST assistant message's `input_tokens +
/// cache_creation_input_tokens + cache_read_input_tokens` — the size of
/// everything sent as context for that turn. This is a materially different
/// question from `parse_claude_transcript`'s cumulative totals (which sum
/// every turn's input across the WHOLE session, for cost/billing purposes);
/// the context window's current fullness is what the MOST RECENT turn sent,
/// not the running lifetime sum. Self-correcting after a compaction — the
/// next turn's input tokens drop back down, exactly reflecting the freshly
/// summarized context. `output_tokens` is excluded: it's what the turn
/// PRODUCED, not what was IN context going in. `None` if no real (non-
/// synthetic) assistant `usage` line is found. Exact (an API-reported figure
/// from the CLI's own transcript), not a byte-count proxy — see
/// `doc/design/orchestration.md`'s Compact-nudge section for why this beats
/// inventing one.
pub fn latest_context_tokens(text: &str) -> Option<u64> {
    let v = latest_real_assistant_turn(text)?;
    let usage = v.get("message")?.get("usage")?;
    let input = u64_field(usage, "input_tokens");
    let cache_creation = u64_field(usage, "cache_creation_input_tokens");
    let cache_read = u64_field(usage, "cache_read_input_tokens");
    Some(input + cache_creation + cache_read)
}

/// Production bug fix (PR #329 round 7): the model id the LATEST real turn
/// ran on — the exact `"model"` field `latest_context_tokens` already reads
/// past on its way to the token count, now also surfaced so a caller can
/// derive the ACTUAL context-window size for this session
/// (`claude_context_window_tokens`) instead of assuming a flat one. Shares
/// `latest_real_assistant_turn`'s "which turn is latest" definition with
/// `latest_context_tokens`, so the two can never disagree about which turn
/// they're each reading.
pub fn latest_context_model(text: &str) -> Option<String> {
    let v = latest_real_assistant_turn(text)?;
    v.get("message")?.get("model")?.as_str().map(str::to_string)
}

/// The LATEST real (non-synthetic, `usage`-bearing) assistant turn in a
/// transcript, scanning newest-to-oldest — shared scan behind both `latest_
/// context_tokens` and `latest_context_model`.
fn latest_real_assistant_turn(text: &str) -> Option<Value> {
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(msg) = v.get("message") else { continue };
        if msg.get("usage").is_none() {
            continue;
        }
        let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
        if model.is_empty() || model == "<synthetic>" {
            continue; // not a real turn's context
        }
        return Some(v);
    }
    None
}

/// Production bug fix (PR #329 round 7): the standard Claude context window
/// (tokens) — the conservative fallback `claude_context_window_tokens` uses
/// for an absent/unrecognized model id. UNDER-estimating the window (reading
/// a HIGHER percent than reality) nudges toward compacting SOONER, never
/// later — the safe direction when genuinely unsure, since the alternative
/// (silently assuming a bigger window than reality) risks the escalation
/// threshold firing too LATE and letting the CLI's own emergency auto-
/// compact land with no offload.
pub const DEFAULT_CLAUDE_CONTEXT_WINDOW_TOKENS: u64 = 200_000;

/// Context-window size (tokens) for a Claude model, matched the SAME way
/// `price_for` matches (substring of the transcript's own model id) — real
/// evidence (a live demo, PR #329 round 7) showed a flat 200K denominator
/// reads badly wrong for a model actually running with a much larger window:
/// the CLI's own `/context` reported ~5% for a token count loomux read as
/// ~26% under the flat assumption. Opus is the one family with concrete,
/// user-reported evidence of a 1M-token tier; everything else (and an
/// absent/unrecognized model id) falls back to the documented default. This
/// is a best-effort GUESS, not a guarantee — Claude's actual context tier is
/// ultimately a per-request API setting this transcript field doesn't fully
/// pin down — so callers needing certainty should prefer an explicit
/// human-set override over this function's return value; see `doc/design/
/// orchestration.md`'s Compact-nudge section.
pub fn claude_context_window_tokens(model: Option<&str>) -> u64 {
    let Some(model) = model else { return DEFAULT_CLAUDE_CONTEXT_WINDOW_TOKENS };
    let m = model.to_ascii_lowercase();
    if m.contains("opus") {
        1_000_000
    } else {
        DEFAULT_CLAUDE_CONTEXT_WINDOW_TOKENS
    }
}

/// Production bug fix (PR #329, rev-42 delta): count of `type: "system",
/// subtype: "compact_boundary"` lines in a Claude transcript — the CLI's own
/// structural marker for "a compaction just completed here", written by the
/// CLI the INSTANT compaction finishes, carrying the exact `preTokens`/
/// `postTokens` it measured. Unlike `latest_context_tokens`'s drop, this
/// needs no following turn to observe: real transcript evidence (a genuine
/// dogfood session on this repo, `1aadeb3f-e8a1-4d29-88d4-7cf4b44ddf2a.jsonl`)
/// shows the boundary line lands 20 lines before the next real assistant
/// `usage` line — several non-assistant bookkeeping lines (a synthetic
/// continuation summary, attachment deltas, a `last-prompt` marker) sit in
/// between with no `type: "assistant"` at all. `latest_context_tokens`
/// genuinely cannot see a compact happened until that next turn exists; this
/// function can, immediately. Monotonically non-decreasing across a growing
/// transcript (more compactions only ever ADD boundary lines), so comparing a
/// later count against a baseline captured earlier is a clean "did a NEW
/// compaction happen since then" signal — see `orchestration::
/// inferred_compaction_confirmed`, its consumer.
pub fn compact_boundary_count(text: &str) -> u64 {
    text.lines()
        .filter(|line| {
            let line = line.trim();
            if line.is_empty() {
                return false;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
            v.get("type").and_then(Value::as_str) == Some("system")
                && v.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
        })
        .count() as u64
}

/// Both compaction-confirmation signals from a single transcript read
/// (rev-42 Q4: the two separate whole-file reads `claude_context_tokens_in`
/// and `agent_context_percents` each did are replaced by callers sharing
/// this one bounded read).
pub struct CompactionSignal {
    pub tokens: Option<u64>,
    pub compact_boundary_count: u64,
    /// Production bug fix (PR #329 round 7): the model the latest real turn
    /// ran on (`latest_context_model`) — lets a caller derive the ACTUAL
    /// context-window size (`claude_context_window_tokens`) for the percent
    /// this reading feeds, instead of assuming a flat one.
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// Transcript location
// ---------------------------------------------------------------------------

/// Default root under which Claude Code keeps per-project transcript folders.
/// Callers can override it (see `claude_session_usage_in`) so tests point at a
/// fixture tree without a real `~/.claude` and without touching global state.
pub fn default_claude_projects_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Locate a session's transcript file under `root` by scanning the project
/// folders for `<session-id>.jsonl`. Claude encodes the cwd into the folder
/// name, so the file could be under any of them; a direct scan avoids
/// re-deriving that encoding. `None` if no transcript exists yet.
///
/// `pub(crate)`: `orchestration::digest` reuses this resolver rather than
/// re-deriving the same project-folder scan (#250/#324 slice B).
///
/// **The single declared assembly point for a claude transcript path (#925),
/// and it takes a [`PathSegment`] rather than a `&str` for the same reason
/// `group_dir_at` takes a `GroupId`.** The id is interpolated into a file name
/// that is then joined onto a directory this process did not choose, so a caller
/// holding an unvalidated string has no business reaching here: `..`, a
/// separator, or a Windows drive prefix in that id walks the join out of the
/// projects root. Requiring proof at the signature is what makes "validated
/// exactly once, at the boundary" a fact the compiler keeps rather than a
/// convention every future caller has to remember.
pub(crate) fn claude_transcript_path(root: &Path, session: &PathSegment) -> Option<PathBuf> {
    let name = format!("{session}.jsonl");
    let projects = fs::read_dir(root).ok()?;
    for project in projects.flatten() {
        let candidate = project.path().join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Read and sum a Claude session's usage from a transcript under the default
/// `~/.claude/projects` root. `None` when the root can't be resolved or the
/// transcript can't be found/opened.
pub fn claude_session_usage(session_id: &str) -> Option<SessionUsage> {
    let root = default_claude_projects_root()?;
    claude_session_usage_in(&root, session_id)
}

/// Read and sum a Claude session's usage from a transcript under an explicit
/// projects `root`, parsing the file from byte zero. `None` when the
/// transcript can't be found or opened — or when `session_id` is not a usable
/// path component (#925).
///
/// The refusal shares the existing `None` channel deliberately. This is a
/// polled usage meter reading an id off a persisted index entry; an entry
/// written by an older build is not evidence of anything, and "no usage for
/// that id" is the same answer it already gives for a transcript that has not
/// been written yet. Same fail-closed-into-an-existing-degrade shape #904 used
/// for `promptsubmit_marker_path`.
///
/// **This is no longer what the poll calls.** Since #1239 the polled path is
/// [`TranscriptCursors::session_usage`], which reaches the same totals by
/// folding only the bytes appended since the previous tick. This function
/// remains the whole-file read: `claude_session_usage`'s implementation, the
/// answer a fresh cursor computes on its first tick, and the thing the
/// incremental result is checked against. Both go through [`fold_appended`],
/// so #1218's streaming property is shared by construction rather than by two
/// readers being kept in step.
pub fn claude_session_usage_in(root: &Path, session_id: &str) -> Option<SessionUsage> {
    let session = PathSegment::parse(session_id).ok()?;
    let path = claude_transcript_path(root, &session)?;
    let mut cursor = TranscriptCursor::new(path);
    fold_appended(&mut cursor, false).ok()?;
    Some(cursor.fold.usage())
}

// ---------------------------------------------------------------------------
// Incremental transcript reading (#1239)
// ---------------------------------------------------------------------------

/// Bytes of ALREADY-FOLDED content a cursor re-reads and compares before it
/// folds anything new on top.
///
/// **What it proves, exactly: that the LAST `ANCHOR_BYTES` of the consumed
/// region are still what was folded there.** Not that the consumed region is
/// intact — that is a strictly stronger statement this check does not make,
/// and reading it as the stronger one is the mistake #1361's review caught.
///
/// It buys the cases `len`+`mtime` cannot see: an in-place rewrite landing on
/// the same length, or one inside a single coarse-`mtime` tick. A wholesale
/// replacement shifts the content, and a rewrite of the most recent record
/// lands inside the window, so both change what sits there and the cursor
/// throws itself away.
///
/// **What it does NOT see**, stated here because three other passages used to
/// imply otherwise: an in-place edit to a consumed byte at an offset BELOW
/// `offset - ANCHOR_BYTES`, on a file that goes on being appended to normally.
/// `len` grew, mtime moved forward, the creation time is unchanged and the
/// anchor window is untouched, so every guard agrees and the edited bytes are
/// never re-read. Nothing in the stat or the anchor will ever notice; what
/// bounds it is [`CURSOR_REVALIDATE_AFTER`], which throws the whole cursor
/// away on a timer so the error is corrected within one interval instead of
/// persisting for the life of the session.
///
/// 64 bytes keeps the work bound this whole change exists for intact, and a
/// transcript line is hundreds of bytes at minimum, so the anchor never spans
/// more than the tail of one record.
const ANCHOR_BYTES: usize = 64;

/// How long a cursor may keep folding incrementally before it is discarded and
/// the transcript re-parsed from byte zero, whatever the stat and the anchor
/// say.
///
/// **This is what makes "fails toward slow, never toward wrong" true rather
/// than nearly true** (#1361 review B1). The stat arms and the anchor between
/// them catch replacement, truncation, rotation, a backwards mtime, and any
/// rewrite touching the last [`ANCHOR_BYTES`] of the consumed region. They do
/// NOT catch an in-place edit further back on a still-appending file — see the
/// note on `ANCHOR_BYTES`. Without a timer that case is wrong forever, which
/// is a different kind of claim from "one poll window of staleness"; with one
/// it is wrong for at most this long.
///
/// Five minutes against a 1 s poll means the incremental path still does
/// roughly 1/300th of the work the whole-file re-parse did, so the guarantee
/// costs almost none of the win. It is deliberately shorter than
/// [`CURSOR_TTL`], so a cursor that survives eviction has revalidated at least
/// once in between.
const CURSOR_REVALIDATE_AFTER: Duration = Duration::from_secs(300);

/// A cursor untouched for this long is dropped, so the map is bounded by
/// transcripts being POLLED rather than by every transcript this process ever
/// read. There is no lifecycle event to hang eviction on — a cursor outlives
/// its agent's pane on purpose (`mark_dead` reads usage after teardown) — so a
/// TTL is the honest bound. Ten minutes is far longer than the 1 s poll, so a
/// live agent never loses its cursor and a gone one costs at most one more
/// full re-parse than it had to.
const CURSOR_TTL: Duration = Duration::from_secs(600);

/// What one cursor read actually cost and what it decided.
///
/// The whole point of #1239 is a WORK bound, and a work bound is not
/// observable from the totals — the old whole-file re-parse produced exactly
/// the same numbers. So the reader reports its own cost, incremented at the
/// single place bytes leave the disk, and the tests assert on that. Production
/// ignores it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorWork {
    /// Bytes pulled off the disk by this read: the appended region, plus the
    /// anchor re-read, plus (on a reset or a first read) the whole file.
    pub bytes_read: u64,
    /// An existing cursor was discarded and the transcript re-folded from byte
    /// zero. False on a first read, which has no cursor to discard.
    pub reset: bool,
    /// The file's `len` and `mtime` were unchanged, so the cached totals were
    /// served without opening it at all.
    pub served_cached: bool,
    /// The projects root was scanned for the transcript file. True on a first
    /// read and whenever the remembered path stops being a file.
    pub scanned_root: bool,
    /// The cursor was discarded because it had been folding incrementally for
    /// [`CURSOR_REVALIDATE_AFTER`], not because any guard objected. Always
    /// accompanied by `reset`; this only says WHY.
    pub revalidated: bool,
}

/// One transcript's parse position and everything needed to resume from it.
struct TranscriptCursor {
    /// The transcript file this cursor is bound to.
    ///
    /// Remembered rather than re-resolved: `claude_transcript_path` scans
    /// every project folder under the root, and doing that once a second per
    /// live agent is the same class of waste as the re-parse. It is
    /// re-validated with one `is_file()` per tick and re-scanned when that
    /// fails. The scan picks whichever project folder yields a match first,
    /// which is directory order and was already arbitrary when it ran every
    /// tick; pinning it makes that choice stable instead of stable-by-luck.
    path: PathBuf,
    /// The fold this cursor resumes.
    fold: TranscriptFold,
    /// Bytes CONSUMED. Always sits immediately after a `\n`, so resuming from
    /// it can never land mid-record.
    offset: u64,
    /// File length as of the stat this cursor last acted on. May exceed
    /// `offset` when the tail is a line whose newline has not arrived yet.
    len: u64,
    modified: Option<SystemTime>,
    /// Creation time where the platform reports one (`Err` on filesystems
    /// without a birth time — Linux ext4 without statx, for instance — and
    /// then simply not a signal).
    ///
    /// It is a weaker signal on Windows than it looks: NTFS **file tunneling**
    /// deliberately restores the ORIGINAL creation timestamp when a name is
    /// deleted and recreated in the same directory inside a ~15 s window, so a
    /// quick rotation through the same path can present as the same file. That
    /// is why this arm is defence-in-depth over the anchor and the length
    /// rather than the thing relied on, and why the test for it probes the
    /// real before/after times instead of assuming they differ.
    created: Option<SystemTime>,
    /// The last [`ANCHOR_BYTES`] of the consumed region.
    anchor: Vec<u8>,
    /// When this cursor last started folding from byte zero — the clock
    /// [`CURSOR_REVALIDATE_AFTER`] runs against.
    built: Instant,
}

/// The stat-level decision for a cursor, before any bytes are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatVerdict {
    /// Nothing changed: serve the cached totals, open nothing.
    Serve,
    /// The file grew (or its mtime moved): fold the appended region on.
    Extend,
    /// The file was replaced, truncated or rewritten: throw the cursor away.
    Reset,
}

impl TranscriptCursor {
    fn new(path: PathBuf) -> Self {
        TranscriptCursor {
            path,
            fold: TranscriptFold::default(),
            offset: 0,
            len: 0,
            modified: None,
            created: None,
            anchor: Vec::new(),
            built: Instant::now(),
        }
    }

    /// Classify what the file's current stat means for this cursor.
    ///
    /// The three reset arms, and why each is a reset rather than an extend:
    ///
    /// - **A different creation time** is a different FILE at the same path —
    ///   a rotation, or a delete-and-rewrite. Nothing about the old offset
    ///   refers to anything in it.
    /// - **A shorter file** cannot be the one we consumed: a transcript only
    ///   ever grows, so a shrink means it was truncated or rewritten. This is
    ///   the arm the issue calls out as the one to get right, and it is
    ///   deliberately `len < self.len` (shrank AT ALL) rather than
    ///   `len < self.offset` (shrank past what we consumed): a truncation that
    ///   still leaves our offset addressable tells us just as loudly that the
    ///   file is not the one we were reading.
    /// - **An mtime that moved BACKWARDS** is a file restored from elsewhere —
    ///   a copy, a sync, a checkout — over the one we were reading.
    ///
    /// `Serve` demands BOTH an unchanged length and an unchanged mtime, so a
    /// same-length rewrite reaches [`StatVerdict::Extend`] and then has to get
    /// past the anchor.
    ///
    /// **What no arm here detects** (#1361 review B1, corrected from a much
    /// narrower claim): an in-place edit to a consumed byte earlier than
    /// `offset - ANCHOR_BYTES`, on a file that keeps being appended to. It
    /// does not shrink the file, does not move the mtime backwards, does not
    /// change the creation time, and does not touch the anchor window — so
    /// every arm below agrees, the edited bytes are never re-read, and no
    /// later append re-decides anything. It is NOT "one poll window of stale
    /// totals": nothing in this function or the anchor will ever notice it.
    /// [`CURSOR_REVALIDATE_AFTER`] is what bounds it, by discarding the cursor
    /// on a timer regardless of what these arms say.
    fn stat_verdict(
        &self,
        len: u64,
        modified: Option<SystemTime>,
        created: Option<SystemTime>,
    ) -> StatVerdict {
        if let (Some(now), Some(then)) = (created, self.created) {
            if now != then {
                return StatVerdict::Reset;
            }
        }
        // [scratch] #1239 round 4: the shorter-than-last-stat arm is gone.
        if let (Some(now), Some(then)) = (modified, self.modified) {
            if now < then {
                return StatVerdict::Reset;
            }
        }
        if len == self.len && modified == self.modified {
            return StatVerdict::Serve;
        }
        StatVerdict::Extend
    }

    /// Extend the anchor with bytes just consumed, keeping only the last
    /// [`ANCHOR_BYTES`].
    fn push_anchor(&mut self, bytes: &[u8]) {
        self.anchor.extend_from_slice(bytes);
        let excess = self.anchor.len().saturating_sub(ANCHOR_BYTES);
        if excess > 0 {
            self.anchor.drain(..excess);
        }
    }
}

/// What an attempt to advance a cursor produced.
enum Advance {
    /// The appended complete lines were folded on; the payload is the bytes
    /// read off disk.
    Folded(u64),
    /// The anchor re-read did not match what the cursor folded there, so the
    /// consumed region has changed under us and NOTHING was folded. The
    /// caller discards the cursor and re-parses from zero.
    AnchorMismatch(u64),
}

/// Fold every COMPLETE line from `cursor.offset` to end-of-file into the
/// cursor, advancing its offset and anchor.
///
/// **`verify_anchor` reads the proof through the SAME handle as the fold, and
/// that is a correctness requirement, not a saved syscall.** Checking the
/// anchor through a handle of its own would leave a window in which the file
/// is replaced between the proof and the read — the cursor would verify one
/// file and then resume into another, which is the one way this design could
/// produce a WRONG total rather than merely a slow tick. Reading the anchor
/// also leaves the handle sitting at exactly `offset`, so the check costs one
/// seek and [`ANCHOR_BYTES`].
///
/// An I/O failure on the anchor read is a mismatch, not an error: a cursor
/// that cannot prove its position is not one to resume from. A cursor with no
/// anchor yet (nothing consumed) has nothing to disagree with and never
/// reports one.
///
/// **A partial trailing line is read but NOT consumed.** A JSONL writer
/// appends a record and its newline as separate bytes, so a poll can land
/// between them; the offset therefore only ever advances past a `\n`. The
/// alternative — folding a torn record — is not "one tick early", it is
/// permanently wrong: a truncated line either fails to parse (and is then
/// skipped forever, losing that message's tokens) or, worse, parses with
/// truncated numbers. Holding back costs at most one poll window of freshness
/// on the newest message, and the bytes are re-read next tick.
///
/// **A line that is not valid UTF-8 is skipped, not fatal.** The reader this
/// replaced used `.lines().map_while(Result::ok)`, which STOPS at the first
/// such line — one bad byte silently truncated a whole session's usage to
/// whatever preceded it. Skipping is both the strictly better answer and the
/// only one a cursor can hold: stalling at a line forever would freeze the
/// offset there.
///
/// Streaming, per #1218: one reusable line buffer, never the file. Peak live
/// bytes are the longest single line plus the fold's message-id set.
fn fold_appended(cursor: &mut TranscriptCursor, verify_anchor: bool) -> std::io::Result<Advance> {
    let mut file = fs::File::open(&cursor.path)?;
    let mut bytes_read = 0u64;
    if verify_anchor && !cursor.anchor.is_empty() {
        let start = cursor.offset.saturating_sub(cursor.anchor.len() as u64);
        let mut got = vec![0u8; cursor.anchor.len()];
        let read = file.seek(SeekFrom::Start(start)).is_ok() && file.read_exact(&mut got).is_ok();
        bytes_read += got.len() as u64;
        if !read || got != cursor.anchor {
            return Ok(Advance::AnchorMismatch(bytes_read));
        }
        // The handle now sits at exactly `cursor.offset`.
    } else if cursor.offset > 0 {
        // Reached only when the anchor branch above did NOT run, which is sound
        // because a non-zero offset always carries a non-empty anchor:
        // `push_anchor` runs on every consumed line, so `anchor.len()` is
        // `min(offset, ANCHOR_BYTES)` by construction. If that ever stopped
        // holding, a verify_anchor tick with a non-empty offset would skip the
        // seek and re-fold the whole file from byte zero onto a cursor that
        // already holds it (#1361 review, latent-coupling note).
        file.seek(SeekFrom::Start(cursor.offset))?;
    }
    let mut reader = BufReader::new(file);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break; // end of file
        }
        bytes_read += n as u64;
        if buf.last() != Some(&b'\n') {
            break; // partial trailing line — leave it for a later tick
        }
        if let Ok(line) = std::str::from_utf8(&buf) {
            cursor.fold.push(line);
        }
        cursor.offset += n as u64;
        cursor.push_anchor(&buf);
    }
    Ok(Advance::Folded(bytes_read))
}

/// Per-transcript parse cursors, so a usage poll parses only what an agent
/// has WRITTEN since the previous poll (#1239).
///
/// **The problem.** `compute_group_usage` runs on the app's hottest poll, at
/// most once per `USAGE_POLL_MAX_AGE` (1 s), and re-read and re-parsed every
/// live agent's ENTIRE transcript on each tick — tens of MiB per agent on a
/// multi-day session, `serde_json` over every line of it, the message-id
/// dedupe set rebuilt from scratch — to advance four totals by a few lines.
/// #1218/#1237 bounded the MEMORY of that read (it streams); they did not
/// remove the work. The minidump behind #1239 records 1,701,161,634 page
/// faults on the process, and this loop is that churn.
///
/// **The contract.** A cursor holds the byte offset it has consumed up to, the
/// fold resumed from there (totals, cost, best model, and the dedupe set —
/// which is what makes resuming safe against a `--resume` re-emit), and the
/// stat it last acted on. Per tick:
///
/// - unchanged `len` and `mtime` → the cached totals, with the file not
///   opened at all;
/// - grown → through ONE handle: re-read the anchor, and if it still matches,
///   fold the appended complete lines on from where that read left off;
/// - replaced, truncated or rewritten → discard the cursor and re-parse from
///   zero, which is exactly the pre-#1239 cost. The failure mode of every
///   guard here is "as slow as it used to be", never a wrong total.
///
/// **`Arc<Mutex<..>>` per transcript, not one map lock** — the same map-lock →
/// release → leaf-lock rule the usage memo follows. The outer lock is held
/// only long enough to clone one cell out, so a full re-parse for one agent
/// never blocks another agent's tick.
pub struct TranscriptCursors {
    cursors: Mutex<HashMap<(PathBuf, String), CursorEntry>>,
    /// How long any one cursor may fold incrementally before it is discarded
    /// and re-parsed from zero. [`CURSOR_REVALIDATE_AFTER`] in production;
    /// a parameter only so the tests can exercise the timer without waiting
    /// on a real clock.
    revalidate_after: Duration,
}

impl Default for TranscriptCursors {
    fn default() -> Self {
        TranscriptCursors::with_revalidate_after(CURSOR_REVALIDATE_AFTER)
    }
}

struct CursorEntry {
    /// Last time this cursor was asked for — drives [`CURSOR_TTL`] eviction.
    used: Instant,
    cursor: Arc<Mutex<Option<TranscriptCursor>>>,
}

impl TranscriptCursors {
    /// A cache whose cursors revalidate on the given interval instead of the
    /// production [`CURSOR_REVALIDATE_AFTER`].
    ///
    /// `#[doc(hidden)] pub` for the integration tests. The timer is the whole
    /// of what bounds the blind spot documented on [`ANCHOR_BYTES`], so it has
    /// to be testable; waiting five real minutes in a test is not a test.
    /// `Duration::ZERO` revalidates on every tick. Production constructs this
    /// type through `Default` and therefore never takes another value.
    #[doc(hidden)] // pub for integration tests
    pub fn with_revalidate_after(revalidate_after: Duration) -> Self {
        TranscriptCursors { cursors: Mutex::new(HashMap::new()), revalidate_after }
    }

    /// A Claude session's usage, parsing only what was appended since this
    /// cache last read the same transcript. Same totals as
    /// [`claude_session_usage_in`], same `None` cases.
    pub fn session_usage(&self, root: &Path, session_id: &str) -> Option<SessionUsage> {
        self.session_usage_measured(root, session_id).map(|(u, _)| u)
    }

    /// [`Self::session_usage`], also reporting what the read cost and what it
    /// decided ([`CursorWork`]).
    ///
    /// `#[doc(hidden)] pub` for the integration tests: the work bound is the
    /// property this whole type exists for, and it is invisible in the totals.
    #[doc(hidden)] // pub for integration tests
    pub fn session_usage_measured(
        &self,
        root: &Path,
        session_id: &str,
    ) -> Option<(SessionUsage, CursorWork)> {
        // Same refusal-into-the-existing-`None` channel as
        // `claude_session_usage_in` (#925): an id that is not a usable path
        // component never reaches the join.
        let session = PathSegment::parse(session_id).ok()?;

        let cell = {
            let mut map = self.cursors.lock_safe();
            map.retain(|_, e| e.used.elapsed() < CURSOR_TTL);
            let entry = map
                .entry((root.to_path_buf(), session_id.to_string()))
                .or_insert_with(|| CursorEntry {
                    used: Instant::now(),
                    cursor: Arc::new(Mutex::new(None)),
                });
            entry.used = Instant::now();
            entry.cursor.clone()
        };
        let mut slot = cell.lock_safe();
        let mut work = CursorWork::default();
        // Captured BEFORE the path re-resolution below, which drops a cursor of
        // its own when the remembered file has gone: `reset` means "a cursor was
        // discarded", and reading it off `slot` afterwards would miss that one.
        let had_cursor = slot.is_some();

        // ONE stat per tick, and it does double duty: the same `metadata` call
        // that answers `len`/mtime/creation is what validates the remembered
        // path. Asking `is_file()` first would be a second stat on the app's
        // hottest poll and would falsify the "one stat, then" claim this
        // design is documented with (#1361 review N4).
        let remembered = slot.as_ref().map(|c| c.path.clone());
        let (path, meta) = match remembered.and_then(|p| {
            let m = fs::metadata(&p).ok()?;
            m.is_file().then_some((p, m))
        }) {
            Some(found) => found,
            None => {
                work.scanned_root = true;
                *slot = None;
                let p = claude_transcript_path(root, &session)?;
                let m = fs::metadata(&p).ok()?;
                (p, m)
            }
        };
        let (len, modified, created) = (meta.len(), meta.modified().ok(), meta.created().ok());

        // Decide from the stat; the anchor then gets to overrule an `Extend`,
        // from inside the same read that would have folded onto it.
        //
        // The age check comes FIRST and overrules both. It is not a guard that
        // detected anything — it is the timer that bounds what the guards
        // structurally cannot detect (see `ANCHOR_BYTES` and
        // `CURSOR_REVALIDATE_AFTER`), so it has to fire even on a tick where
        // every other signal is content.
        let mut verdict = match slot.as_ref() {
            None => StatVerdict::Reset,
            Some(c) if c.built.elapsed() >= self.revalidate_after => {
                work.revalidated = true;
                StatVerdict::Reset
            }
            Some(c) => match c.stat_verdict(len, modified, created) {
                StatVerdict::Serve => {
                    work.served_cached = true;
                    return Some((c.fold.usage(), work));
                }
                other => other,
            },
        };

        if verdict == StatVerdict::Extend {
            match fold_appended(slot.as_mut()?, true).ok()? {
                Advance::Folded(n) => work.bytes_read += n,
                Advance::AnchorMismatch(n) => {
                    work.bytes_read += n;
                    verdict = StatVerdict::Reset;
                }
            }
        }

        if verdict == StatVerdict::Reset {
            // `reset` is "a cursor was thrown away", so a first-ever read —
            // which also parses from zero — is not one.
            work.reset = had_cursor;
            *slot = Some(TranscriptCursor::new(path));
            // A fresh cursor has no anchor, so this call cannot report a
            // mismatch; both arms mean the same thing here.
            work.bytes_read += match fold_appended(slot.as_mut()?, false).ok()? {
                Advance::Folded(n) | Advance::AnchorMismatch(n) => n,
            };
        }

        let cursor = slot.as_mut()?;
        cursor.len = len;
        cursor.modified = modified;
        cursor.created = created;
        Some((cursor.fold.usage(), work))
    }
}

// ---------------------------------------------------------------------------
// OpenCode session store (#722)
// ---------------------------------------------------------------------------

/// Read an OpenCode session's usage from the SQLite store at `db` — the
/// group's own database, per `OPENCODE_DB` (see `doc/design/opencode.md`).
///
/// `Ok(None)` is "readable store, no such session yet"; `Err` is a degrade the
/// caller reports as zero usage (`crate::opencodedb::Unavailable`).
///
/// **Two mapping decisions, both lossy, both deliberate:**
///
/// - **Reasoning tokens are folded into `output_tokens`.** OpenCode counts
///   them in a fifth bucket loomux has no column for, and dropping them would
///   under-report — a real session on this machine spent 1193 reasoning
///   tokens against 1115 output ones, so the fold is the difference between
///   roughly right and roughly half. `output` is also where they already sit
///   for the CLI loomux compares against: Claude's transcript counts thinking
///   inside `output_tokens`, so folding makes the two CLIs' `output` bucket
///   mean the same thing rather than two different things under one label.
/// - **`cache_write` maps to `cache_creation_tokens`.** Same quantity under
///   two vendors' names — tokens written INTO the cache, which is what
///   Claude's `cache_creation_input_tokens` counts.
///
/// The dollar figure passes through untouched and is **reported**, not
/// estimated: OpenCode priced it against its own provider table, so no
/// `price_for` lookup happens and callers must not label it an estimate.
/// `Some(0.0)` is a real answer on a free model, not a missing one.
pub fn opencode_session_usage(
    db: &Path,
    session_id: &str,
) -> Result<Option<SessionUsage>, crate::opencodedb::Unavailable> {
    Ok(crate::opencodedb::session_usage(db, session_id)?.map(|t| SessionUsage {
        tokens: TokenUsage {
            input_tokens: t.input,
            output_tokens: t.output + t.reasoning,
            cache_creation_tokens: t.cache_write,
            cache_read_tokens: t.cache_read,
        },
        cost_usd: Some(t.cost_usd),
        model: t.model,
    }))
}

/// Bytes read from the END of a transcript file for the tail-based signals
/// (rev-42 Q4 cost fix): `latest_context_tokens` and `compact_boundary_count`
/// both only ever need RECENT lines — the current context reading and any
/// compaction boundary relevant to the pane's current arm state — never the
/// full session history, which can reach many MB over a long-lived
/// orchestrator. Generous relative to a handful of transcript lines (even a
/// large tool-output turn) so the bound essentially never bites for what
/// these two functions actually look at.
const TRANSCRIPT_TAIL_READ_BYTES: u64 = 256 * 1024;

/// Read the last `TRANSCRIPT_TAIL_READ_BYTES` of `path`, discarding a
/// possibly-truncated leading partial line (unless the read reached the true
/// start of the file, in which case there's nothing to truncate). `None` on
/// any I/O failure.
fn read_transcript_tail(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TRANSCRIPT_TAIL_READ_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    if start == 0 {
        return Some(text);
    }
    match text.find('\n') {
        Some(idx) => Some(text[idx + 1..].to_string()),
        None => Some(String::new()), // the whole read was one truncated line
    }
}

/// Read a Claude session's CURRENT context-window usage (#328) — see
/// `latest_context_tokens` — from a transcript under an explicit projects
/// `root`. `None` when the transcript can't be found/opened or carries no
/// real assistant turn yet. A thin convenience wrapper over
/// `compaction_signal_in` for callers that only need the token half.
pub fn claude_context_tokens_in(root: &Path, session_id: &str) -> Option<u64> {
    compaction_signal_in(root, session_id)?.tokens
}

/// Read BOTH compaction-confirmation signals (`latest_context_tokens` and
/// `compact_boundary_count`) from a single bounded tail read of a Claude
/// session's transcript. `None` when the transcript can't be found/opened;
/// `tokens` is separately `None` within a `Some(CompactionSignal)` when no
/// real assistant turn has landed in the tail window (matching `latest_
/// context_tokens`'s own `None` case) — `compact_boundary_count` still
/// reports 0 in that case rather than failing the whole read, since a
/// boundary marker's absence is itself a meaningful, distinct fact.
pub fn compaction_signal_in(root: &Path, session_id: &str) -> Option<CompactionSignal> {
    // Same refusal-into-the-existing-`None` channel as `claude_session_usage_in`
    // (#925): an id that is not a usable path component never reaches the join.
    let session = PathSegment::parse(session_id).ok()?;
    let path = claude_transcript_path(root, &session)?;
    let text = read_transcript_tail(&path)?;
    Some(CompactionSignal {
        tokens: latest_context_tokens(&text),
        compact_boundary_count: compact_boundary_count(&text),
        model: latest_context_model(&text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One assistant transcript line with the given usage + model.
    fn line(id: &str, model: &str, input: u64, output: u64, cw: u64, cr: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "requestId": format!("req_{id}"),
            "message": {
                "id": id,
                "model": model,
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_creation_input_tokens": cw,
                    "cache_read_input_tokens": cr,
                }
            }
        })
        .to_string()
    }

    #[test]
    fn sums_tokens_and_prices_by_model() {
        let text = [
            line("msg-1", "claude-opus-4-8", 100, 200, 50, 1000),
            line("msg-2", "claude-opus-4-8", 10, 20, 0, 500),
        ]
        .join("\n");
        let u = parse_claude_transcript(&text);
        assert_eq!(u.tokens.input_tokens, 110);
        assert_eq!(u.tokens.output_tokens, 220);
        assert_eq!(u.tokens.cache_creation_tokens, 50);
        assert_eq!(u.tokens.cache_read_tokens, 1500);
        assert_eq!(u.tokens.total(), 110 + 220 + 50 + 1500);
        // Opus: (110*5 + 220*25 + 50*6.25 + 1500*0.5) / 1e6
        let expect = (110.0 * 5.0 + 220.0 * 25.0 + 50.0 * 6.25 + 1500.0 * 0.5) / 1_000_000.0;
        assert!((u.cost_usd.unwrap() - expect).abs() < 1e-12, "got {:?}", u.cost_usd);
        assert_eq!(u.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn dedupes_repeated_message_ids() {
        // A resumed transcript re-emits msg-1; it must be counted once.
        let text = [
            line("msg-1", "claude-sonnet-5", 100, 200, 0, 0),
            line("msg-1", "claude-sonnet-5", 100, 200, 0, 0),
            line("msg-2", "claude-sonnet-5", 5, 5, 0, 0),
        ]
        .join("\n");
        let u = parse_claude_transcript(&text);
        assert_eq!(u.tokens.input_tokens, 105, "duplicate id must not double-count");
        assert_eq!(u.tokens.output_tokens, 205);
    }

    #[test]
    fn skips_non_assistant_and_synthetic_and_malformed() {
        let text = [
            r#"{"type":"summary","summary":"a title"}"#.to_string(),
            r#"{"type":"user","message":{"content":"hi"}}"#.to_string(),
            "not json at all".to_string(),
            line("real", "claude-haiku-4-5", 40, 60, 0, 0),
            // Synthetic: contributes tokens but no model/price.
            line("synth", "<synthetic>", 1, 1, 0, 0),
        ]
        .join("\n");
        let u = parse_claude_transcript(&text);
        assert_eq!(u.tokens.input_tokens, 41);
        assert_eq!(u.tokens.output_tokens, 61);
        // Priced only off the haiku line.
        let expect = (40.0 * 1.0 + 60.0 * 5.0) / 1_000_000.0;
        assert!((u.cost_usd.unwrap() - expect).abs() < 1e-12);
        assert_eq!(u.model.as_deref(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn unknown_model_yields_tokens_but_no_cost() {
        let text = line("m", "some-future-model-9", 100, 100, 0, 0);
        let u = parse_claude_transcript(&text);
        assert_eq!(u.tokens.total(), 200);
        assert_eq!(u.cost_usd, None, "unknown model must fall back to token-only");
        assert_eq!(u.model, None);
    }

    #[test]
    fn empty_transcript_is_zero_not_a_panic() {
        let u = parse_claude_transcript("");
        assert_eq!(u.tokens.total(), 0);
        assert_eq!(u.cost_usd, None);
    }

    #[test]
    fn price_table_matches_known_families() {
        assert!(price_for("claude-opus-4-8").is_some());
        assert!(price_for("claude-sonnet-5").is_some());
        assert!(price_for("claude-haiku-4-5").is_some());
        assert!(price_for("claude-fable-5").is_some());
        assert!(price_for("gpt-4o").is_none());
    }

    // ---------- latest_context_tokens (#328) ----------

    #[test]
    fn latest_context_tokens_reads_the_last_real_turn_not_the_cumulative_sum() {
        // The whole point of this fn vs `parse_claude_transcript`: context
        // fullness is what the MOST RECENT turn sent, not the running total
        // across the session.
        let text = [
            line("t1", "claude-sonnet-5", 50_000, 500, 0, 0),
            line("t2", "claude-sonnet-5", 80_000, 500, 0, 20_000),
        ]
        .join("\n");
        // Cumulative sum (what parse_claude_transcript reports) would be
        // 130_000 input tokens; the LATEST turn's context is 80_000 + 20_000
        // (cache read) = 100_000, a materially different figure.
        assert_eq!(latest_context_tokens(&text), Some(100_000));
        let cumulative = parse_claude_transcript(&text);
        assert_eq!(cumulative.tokens.input_tokens, 130_000, "sanity: cumulative really does differ");
    }

    #[test]
    fn latest_context_model_reads_the_same_latest_turn_tokens_does() {
        // PR #329 round 7: the two must never disagree about which turn is
        // "latest" — they share `latest_real_assistant_turn`.
        let text = [
            line("t1", "claude-sonnet-5", 50_000, 500, 0, 0),
            line("t2", "claude-opus-4-8", 80_000, 500, 0, 20_000),
        ]
        .join("\n");
        assert_eq!(latest_context_model(&text).as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn latest_context_model_skips_synthetic_and_non_assistant_lines_same_as_tokens() {
        let text = [
            r#"{"type":"user","message":{"content":"hi"}}"#.to_string(),
            line("synth", "<synthetic>", 1, 1, 0, 0),
            line("real", "claude-opus-4-8", 10, 10, 0, 0),
        ]
        .join("\n");
        assert_eq!(latest_context_model(&text).as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn latest_context_model_none_when_no_real_turn_exists() {
        assert_eq!(latest_context_model(""), None);
        assert_eq!(latest_context_model("not json\n{\"type\":\"user\"}"), None);
    }

    #[test]
    fn claude_context_window_tokens_defaults_conservative_and_widens_only_for_opus() {
        // PR #329 round 7: live evidence — a hardcoded 200K flat assumption
        // read a 1M-context Opus session's usage as ~5x too full (26% vs the
        // CLI's own reported ~5%). Opus is the one family with concrete
        // evidence of a larger tier; everything else, and an absent/
        // unrecognized model, falls back to the documented conservative
        // default (the safe direction when unsure: NEVER assume a bigger
        // window than reality, which would delay a needed compaction).
        assert_eq!(claude_context_window_tokens(Some("claude-opus-4-8")), 1_000_000);
        assert_eq!(claude_context_window_tokens(Some("claude-opus-4-7")), 1_000_000, "matches by family, like price_for");
        assert_eq!(claude_context_window_tokens(Some("claude-sonnet-5")), DEFAULT_CLAUDE_CONTEXT_WINDOW_TOKENS);
        assert_eq!(claude_context_window_tokens(Some("claude-haiku-4-5")), DEFAULT_CLAUDE_CONTEXT_WINDOW_TOKENS);
        assert_eq!(claude_context_window_tokens(Some("some-future-model-nobody-has-heard-of")), DEFAULT_CLAUDE_CONTEXT_WINDOW_TOKENS,
            "an unrecognized model must never silently widen the window — conservative fallback, not a guess in the unsafe direction");
        assert_eq!(claude_context_window_tokens(None), DEFAULT_CLAUDE_CONTEXT_WINDOW_TOKENS);
    }

    #[test]
    fn latest_context_tokens_self_corrects_after_a_compact() {
        // A compact's next turn sends far less context — the figure must
        // reflect that drop, not stay pinned to the pre-compact peak.
        let text = [
            line("before", "claude-sonnet-5", 180_000, 500, 0, 0),
            line("after-compact", "claude-sonnet-5", 8_000, 500, 0, 0),
        ]
        .join("\n");
        assert_eq!(latest_context_tokens(&text), Some(8_000));
    }

    /// A REAL (structurally trimmed, numbers untouched) excerpt from an actual
    /// dogfood session on this repo — `1aadeb3f-e8a1-4d29-88d4-7cf4b44ddf2a.jsonl`,
    /// `~/.claude/projects/C--Projects-loomux/`, 2026-07-15 — captured specifically
    /// to settle the rev-42 delta review's Q1: does `latest_context_tokens` see a
    /// compaction's drop before the next real assistant turn, or only after?
    /// Synthetic injection can't answer this (it assumes the very timing in
    /// question); this is the actual CLI's own transcript shape. Only the huge,
    /// parser-irrelevant fields (`preservedSegment`/`preCompactDiscoveredTools`
    /// arrays, the multi-paragraph summary prose) were elided for fixture size —
    /// every field either `latest_context_tokens` or `compact_boundary_count`
    /// reads is verbatim, including the exact token counts.
    const REAL_DOGFOOD_COMPACT_EXCERPT_PRE: &str =
        r#"{"type":"assistant","message":{"model":"claude-fable-5","usage":{"input_tokens":2,"output_tokens":1305,"cache_creation_input_tokens":48,"cache_read_input_tokens":516543}}}"#;
    const REAL_DOGFOOD_COMPACT_EXCERPT_BOUNDARY: &str =
        r#"{"type":"system","subtype":"compact_boundary","content":"Conversation compacted","level":"info","compactMetadata":{"trigger":"manual","preTokens":518258,"postTokens":7716,"cumulativeDroppedTokens":510542},"timestamp":"2026-07-15T01:46:54.839Z"}"#;
    // Interstitial bookkeeping lines the REAL transcript has between the
    // boundary and the next assistant turn — a synthetic continuation summary,
    // then (in the real file) several attachment-delta lines omitted here as
    // pure repetition, then a last-prompt marker. None are `type: "assistant"`.
    const REAL_DOGFOOD_COMPACT_EXCERPT_SUMMARY: &str =
        r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"[summary text elided for fixture size — real content is a multi-paragraph session recap]"}}"#;
    const REAL_DOGFOOD_COMPACT_EXCERPT_LASTPROMPT: &str =
        r#"{"type":"last-prompt","lastPrompt":"/compact"}"#;
    const REAL_DOGFOOD_COMPACT_EXCERPT_POST: &str =
        r#"{"type":"assistant","message":{"model":"claude-fable-5","usage":{"input_tokens":2,"output_tokens":1568,"cache_creation_input_tokens":15688,"cache_read_input_tokens":28268}}}"#;

    #[test]
    fn real_transcript_proves_the_token_drop_is_a_next_turn_phenomenon_rev42_q1() {
        // The window `compact_nudge_tick`'s resolver actually reads at: a
        // compact just completed (the boundary line exists), but the CLI
        // hasn't produced a new real assistant turn yet — only the synthetic
        // continuation summary and a last-prompt marker sit after it, exactly
        // as the real transcript shows.
        let before_next_turn = [
            REAL_DOGFOOD_COMPACT_EXCERPT_PRE,
            REAL_DOGFOOD_COMPACT_EXCERPT_BOUNDARY,
            REAL_DOGFOOD_COMPACT_EXCERPT_SUMMARY,
            REAL_DOGFOOD_COMPACT_EXCERPT_LASTPROMPT,
        ]
        .join("\n");
        // Real pre-compact figure: 2 + 48 + 516_543 = 516_593. Confirms rev-42's
        // Q1 empirically: `latest_context_tokens` is STILL pinned to the
        // pre-compact peak here — it has no way to know a compaction happened.
        assert_eq!(
            latest_context_tokens(&before_next_turn),
            Some(516_593),
            "before any new assistant turn, the reading must still show the STALE pre-compact value — this is the deadlock"
        );
        // But the boundary marker is ALREADY visible — no next turn required.
        assert_eq!(compact_boundary_count(&before_next_turn), 1,
            "compact_boundary_count sees the compaction immediately, unlike the token reading");

        // Now the next real assistant turn lands (the reinjection's own
        // response, in production) — only THEN does the token reading correct.
        let after_next_turn = format!("{before_next_turn}\n{REAL_DOGFOOD_COMPACT_EXCERPT_POST}");
        assert_eq!(
            latest_context_tokens(&after_next_turn),
            Some(2 + 15_688 + 28_268),
            "only once a new assistant turn exists does the drop become visible — confirms it's a next-turn phenomenon, not an at-compaction one"
        );
        assert_eq!(compact_boundary_count(&after_next_turn), 1, "still just the one real compaction");
    }

    #[test]
    fn compact_boundary_count_is_zero_when_absent_and_counts_every_real_boundary() {
        assert_eq!(compact_boundary_count(""), 0);
        assert_eq!(compact_boundary_count("not json\n{\"type\":\"user\"}"), 0);
        assert_eq!(compact_boundary_count(r#"{"type":"system","subtype":"other_thing"}"#), 0,
            "a different system subtype must not be mistaken for a compaction");
        let two_compactions = [
            REAL_DOGFOOD_COMPACT_EXCERPT_BOUNDARY,
            REAL_DOGFOOD_COMPACT_EXCERPT_POST,
            REAL_DOGFOOD_COMPACT_EXCERPT_BOUNDARY,
        ]
        .join("\n");
        assert_eq!(compact_boundary_count(&two_compactions), 2, "monotonically counts every boundary seen");
    }

    #[test]
    fn latest_context_tokens_skips_synthetic_and_non_assistant_lines() {
        let text = [
            r#"{"type":"summary","summary":"a title"}"#.to_string(),
            line("real", "claude-sonnet-5", 42_000, 100, 0, 1_000),
            // A trailing synthetic line (no real usage) must not be read as
            // "the latest turn" and mask the real one before it.
            line("synth", "<synthetic>", 999_999, 1, 0, 0),
        ]
        .join("\n");
        assert_eq!(latest_context_tokens(&text), Some(43_000));
    }

    #[test]
    fn latest_context_tokens_none_when_no_real_turn_exists() {
        assert_eq!(latest_context_tokens(""), None);
        assert_eq!(latest_context_tokens("not json\n{\"type\":\"user\"}"), None);
    }
}
