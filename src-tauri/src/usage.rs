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
//! Everything here is best-effort and pure where it matters: the parser
//! (`parse_claude_transcript`) takes text and is exercised by fixture tests,
//! never a live CLI.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

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
    let mut totals = TokenUsage::default();
    let mut cost = 0.0f64;
    let mut any_priced = false;
    let mut seen: HashSet<String> = HashSet::new();
    // Track the priced model with the most output tokens, for display.
    let mut best_model: Option<(String, u64)> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(msg) = v.get("message") else { continue };
        let Some(usage) = msg.get("usage") else { continue };

        // Dedupe by message id when present.
        if let Some(id) = msg.get("id").and_then(Value::as_str) {
            if !seen.insert(id.to_string()) {
                continue;
            }
        }

        let t = TokenUsage {
            input_tokens: u64_field(usage, "input_tokens"),
            output_tokens: u64_field(usage, "output_tokens"),
            cache_creation_tokens: u64_field(usage, "cache_creation_input_tokens"),
            cache_read_tokens: u64_field(usage, "cache_read_input_tokens"),
        };
        totals.input_tokens += t.input_tokens;
        totals.output_tokens += t.output_tokens;
        totals.cache_creation_tokens += t.cache_creation_tokens;
        totals.cache_read_tokens += t.cache_read_tokens;

        let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
        if model.is_empty() || model == "<synthetic>" {
            continue;
        }
        if let Some(p) = price_for(model) {
            cost += cost_of(&t, &p);
            any_priced = true;
            let out = t.output_tokens;
            match &mut best_model {
                Some((_, best_out)) if *best_out >= out => {}
                _ => best_model = Some((model.to_string(), out)),
            }
        }
    }

    SessionUsage {
        tokens: totals,
        cost_usd: any_priced.then_some(cost),
        model: best_model.map(|(m, _)| m),
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
        let Some(usage) = msg.get("usage") else { continue };
        let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
        if model.is_empty() || model == "<synthetic>" {
            continue; // not a real turn's context
        }
        let input = u64_field(usage, "input_tokens");
        let cache_creation = u64_field(usage, "cache_creation_input_tokens");
        let cache_read = u64_field(usage, "cache_read_input_tokens");
        return Some(input + cache_creation + cache_read);
    }
    None
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
pub(crate) fn claude_transcript_path(root: &Path, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
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
/// projects `root`. Lets the orchestration layer (and its tests) point at any
/// tree. `None` when the transcript can't be found or opened.
pub fn claude_session_usage_in(root: &Path, session_id: &str) -> Option<SessionUsage> {
    let path = claude_transcript_path(root, session_id)?;
    let file = fs::File::open(&path).ok()?;
    // Read the whole file line by line rather than into one big string: these
    // transcripts can be large, and we only keep running totals.
    let mut text = String::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        text.push_str(&line);
        text.push('\n');
    }
    Some(parse_claude_transcript(&text))
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
    use std::io::{Read, Seek, SeekFrom};
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
    let path = claude_transcript_path(root, session_id)?;
    let text = read_transcript_tail(&path)?;
    Some(CompactionSignal {
        tokens: latest_context_tokens(&text),
        compact_boundary_count: compact_boundary_count(&text),
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
