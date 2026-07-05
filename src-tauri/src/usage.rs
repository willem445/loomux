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
use std::path::PathBuf;

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

// ---------------------------------------------------------------------------
// Transcript location
// ---------------------------------------------------------------------------

/// Root under which Claude Code keeps per-project transcript folders.
/// `LOOMUX_CLAUDE_PROJECTS_DIR` overrides it so tests (and unusual installs)
/// can point at a fixture tree without a real `~/.claude`.
fn claude_projects_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LOOMUX_CLAUDE_PROJECTS_DIR") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Locate a session's transcript file by scanning the project folders for
/// `<session-id>.jsonl`. Claude encodes the cwd into the folder name, so the
/// file could be under any of them; a direct scan avoids re-deriving that
/// encoding. `None` if no transcript exists yet.
fn claude_transcript_path(session_id: &str) -> Option<PathBuf> {
    let root = claude_projects_root()?;
    let name = format!("{session_id}.jsonl");
    let projects = fs::read_dir(&root).ok()?;
    for project in projects.flatten() {
        let candidate = project.path().join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Read and sum a Claude session's usage from its transcript. `None` when the
/// transcript can't be found or opened (session not started, wrong CLI, etc.).
pub fn claude_session_usage(session_id: &str) -> Option<SessionUsage> {
    let path = claude_transcript_path(session_id)?;
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
}
