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

// ---------------------------------------------------------------------------
// Usage limits (statusline-scraped)
// ---------------------------------------------------------------------------
//
// Unlike token usage, the *limit* a session is consuming is not written to any
// local file we can read. We verified `~/.claude` (settings.json,
// stats-cache.json, session transcripts): none carry a session or weekly limit
// percentage — token counts and a $0 subscription cost are all that is there.
// The only place the figure surfaces is what the CLI renders in its own
// statusline (Claude Code's built-in limit widget, or a third-party one such as
// ccstatusline). So we scrape it best-effort from the ANSI-stripped pane tail —
// the same last-resort channel `parse_session_cost` uses for the dollar figure.
//
// **Copilot** deliberately contributes nothing here. Its local state
// (`~/.copilot/session-state/<id>/`) records per-session premium-request
// *counts* (`totalPremiumRequests`, only flushed in the shutdown event) and an
// opaque per-request `creditsUsed`, but nowhere is the account's premium-request
// *allowance* — the denominator a "% of limit" needs. That number is server-side
// only. Showing a raw consumption count with no ceiling would be the kind of
// fabricated figure the #42 cost work is careful to avoid, so we show nothing
// for Copilot until a local allowance source exists.

/// The limit window a percentage refers to. Claude enforces a rolling ~5-hour
/// "session" allowance and a longer "weekly" one; either (or both) can appear in
/// a statusline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LimitScope {
    Session,
    Weekly,
}

/// One "N% consumed" reading scraped from a statusline, tagged with its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LimitReading {
    pub scope: LimitScope,
    /// Consumed percentage, 0..=100.
    pub percent: u8,
}

/// Every `NN%` token on a line, as `(byte-offset-of-first-digit, percent)`.
/// A run of digits immediately followed by `%` and no greater than 100 counts;
/// anything else (a bare number, `120%`) is skipped so we never treat a random
/// figure as a usage bar. ASCII-only by construction (digits and `%`), so byte
/// offsets are safe on UTF-8 statusline text.
fn percents_on_line(line: &str) -> Vec<(usize, u8)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut val: u32 = 0;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                val = val.saturating_mul(10).saturating_add(u32::from(bytes[i] - b'0'));
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'%' && val <= 100 {
                out.push((start, val as u8));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Byte offsets of every occurrence of `needle` in `hay` (both already
/// lowercased by the caller). Non-overlapping, left to right.
fn find_all(hay: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut base = 0;
    while let Some(rel) = hay[base..].find(needle) {
        out.push(base + rel);
        base += rel + needle.len();
    }
    out
}

/// Scrape session/weekly limit percentages from a pane's ANSI-stripped
/// statusline text. Scans bottom-up (the freshest render is the lowest line)
/// and returns at most one reading per scope — the first, i.e. freshest, seen.
///
/// Each percentage is bound to the *nearest* scope keyword on its own line, so a
/// combined `Session 34% · Week 12%` statusline parses both correctly and a
/// bare `Context: 45%` (no scope keyword) is ignored rather than mistaken for a
/// limit. Keywords are conservative — only `session` and `week`/`weekly` — to
/// avoid grabbing an unrelated percentage (CPU, context, a progress bar).
pub fn parse_claude_limits(text: &str) -> Vec<LimitReading> {
    let mut session: Option<u8> = None;
    let mut weekly: Option<u8> = None;

    for line in text.lines().rev() {
        if session.is_some() && weekly.is_some() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        let percents = percents_on_line(&lower);
        if percents.is_empty() {
            continue;
        }
        // Percentage nearest (by offset) to any of the given scope keywords.
        let nearest = |keywords: &[&str]| -> Option<u8> {
            let mut best: Option<(usize, u8)> = None;
            for kw in keywords {
                for kpos in find_all(&lower, kw) {
                    for &(ppos, pct) in &percents {
                        let dist = kpos.abs_diff(ppos);
                        if best.is_none_or(|(bd, _)| dist < bd) {
                            best = Some((dist, pct));
                        }
                    }
                }
            }
            best.map(|(_, pct)| pct)
        };
        if session.is_none() {
            session = nearest(&["session"]);
        }
        if weekly.is_none() {
            weekly = nearest(&["week"]);
        }
    }

    let mut out = Vec::new();
    if let Some(p) = session {
        out.push(LimitReading { scope: LimitScope::Session, percent: p });
    }
    if let Some(p) = weekly {
        out.push(LimitReading { scope: LimitScope::Weekly, percent: p });
    }
    out
}

/// Aggregated Claude usage-limit figure for the app: the most-constrained value
/// across every live Claude pane, per scope. `None` fields mean no live pane's
/// statusline exposed that window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ClaudeLimits {
    pub session_pct: Option<u8>,
    pub weekly_pct: Option<u8>,
}

impl ClaudeLimits {
    /// True when neither scope has a reading (nothing to show).
    pub fn is_empty(&self) -> bool {
        self.session_pct.is_none() && self.weekly_pct.is_none()
    }

    /// The most-constrained (highest consumed) percentage across scopes — the
    /// single number a compact chip shows.
    pub fn most_constrained(&self) -> Option<u8> {
        match (self.session_pct, self.weekly_pct) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}

/// Fold per-pane readings into one figure by taking the highest consumed
/// percentage per scope. Every live pane shares the one signed-in account, so
/// the max is the honest "closest to a cutoff" value and tolerates a pane whose
/// statusline has not refreshed yet (a stale, lower reading never wins).
pub fn aggregate_claude_limits(per_pane: &[Vec<LimitReading>]) -> ClaudeLimits {
    let mut out = ClaudeLimits::default();
    for readings in per_pane {
        for r in readings {
            let slot = match r.scope {
                LimitScope::Session => &mut out.session_pct,
                LimitScope::Weekly => &mut out.weekly_pct,
            };
            *slot = Some(slot.map_or(r.percent, |cur| cur.max(r.percent)));
        }
    }
    out
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
fn claude_transcript_path(root: &Path, session_id: &str) -> Option<PathBuf> {
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

    // ---- usage-limit statusline parsing ------------------------------------

    fn reading(scope: LimitScope, percent: u8) -> LimitReading {
        LimitReading { scope, percent }
    }

    #[test]
    fn parses_a_session_percentage() {
        let u = parse_claude_limits("some banner\nSession: 34%  ·  $1.20\n");
        assert_eq!(u, vec![reading(LimitScope::Session, 34)]);
    }

    #[test]
    fn parses_both_scopes_on_one_line_by_nearest_keyword() {
        // Combined statusline: each % must bind to the keyword next to it.
        let u = parse_claude_limits("Session 34% · Week 12%");
        assert_eq!(
            u,
            vec![reading(LimitScope::Session, 34), reading(LimitScope::Weekly, 12)]
        );
    }

    #[test]
    fn weekly_keyword_variants_and_percent_before_label() {
        // "Weekly" contains "week"; the % can also sit before its label.
        let u = parse_claude_limits("12% weekly limit");
        assert_eq!(u, vec![reading(LimitScope::Weekly, 12)]);
    }

    #[test]
    fn ignores_percentages_without_a_scope_keyword() {
        // Context/CPU-style bars must not be mistaken for a usage limit.
        let u = parse_claude_limits("CPU 80%  Context: 45%  MEM 30%");
        assert!(u.is_empty(), "no scope keyword => no reading, got {u:?}");
    }

    #[test]
    fn takes_the_freshest_render_when_a_scope_repeats() {
        // Statusline re-rendered; the lowest (freshest) line wins.
        let text = "Session: 10%\nSession: 42%\n";
        let u = parse_claude_limits(text);
        assert_eq!(u, vec![reading(LimitScope::Session, 42)]);
    }

    #[test]
    fn rejects_out_of_range_and_bare_numbers() {
        // 120% is not a usage bar; a plain "5" (no %) is not a percentage.
        assert!(percents_on_line("session 120% 5 things").is_empty());
        assert_eq!(percents_on_line("session 7%"), vec![(8, 7)]);
    }

    #[test]
    fn empty_or_unrelated_text_yields_no_readings() {
        assert!(parse_claude_limits("").is_empty());
        assert!(parse_claude_limits("just a normal prompt line $ ").is_empty());
    }

    #[test]
    fn aggregates_to_the_most_constrained_per_scope() {
        // Three panes, one account: keep the highest consumed % per scope.
        let panes = vec![
            vec![reading(LimitScope::Session, 20), reading(LimitScope::Weekly, 8)],
            vec![reading(LimitScope::Session, 55)], // stale on weekly, hottest on session
            vec![reading(LimitScope::Weekly, 15)],
        ];
        let agg = aggregate_claude_limits(&panes);
        assert_eq!(agg.session_pct, Some(55));
        assert_eq!(agg.weekly_pct, Some(15));
        assert_eq!(agg.most_constrained(), Some(55));
        assert!(!agg.is_empty());
    }

    #[test]
    fn aggregate_of_nothing_is_empty() {
        let agg = aggregate_claude_limits(&[]);
        assert!(agg.is_empty());
        assert_eq!(agg.most_constrained(), None);
    }

    #[test]
    fn most_constrained_prefers_the_single_present_scope() {
        let only_weekly = ClaudeLimits { session_pct: None, weekly_pct: Some(9) };
        assert_eq!(only_weekly.most_constrained(), Some(9));
    }
}
