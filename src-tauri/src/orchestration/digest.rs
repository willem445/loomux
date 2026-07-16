//! Session-digest friction extraction (#250/#324 slice B).
//!
//! A worker's raw transcript (Claude `.jsonl` or Copilot `session-state`) is
//! too large and too noisy to feed an agent directly — full file contents,
//! tool output, thinking blocks. This module normalizes both source shapes
//! into one small event stream, then reduces that stream, deterministically
//! and without any LLM, into "friction windows": the wall, the attempts, the
//! fix. Only those windows (plus three cheap anchors) are meant to reach an
//! agent — see `session_digest` in `mcp.rs` / `OrchRegistry::session_digest`
//! in `mod.rs` for the registry-facing side that resolves a task/agent/pr
//! into a transcript and calls into here.
//!
//! Everything in this file is pure: it takes text/events in and returns data
//! out, so it is fixture-tested without touching disk or a real agent CLI
//! (never spawn one to produce test data — CLAUDE.md constraint 3).

use serde::Serialize;
use serde_json::Value;

/// One event in a normalized transcript, source-agnostic — the same shape
/// whether it came from a Claude `.jsonl` line or a Copilot session-state
/// read. `role` is `"user"` or `"assistant"`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptEvent {
    pub role: String,
    pub kind: EventKind,
    pub ts_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        /// Shell-like tools carry their invocation here (`input.command`);
        /// drives near-duplicate-rerun and test-red-to-green detection.
        command: Option<String>,
        /// Edit/Write-like tools carry the touched path here
        /// (`input.file_path`); drives reverted-edit detection.
        file_path: Option<String>,
        /// The `Edit` tool's `input.old_string`/`input.new_string`, when
        /// present — the exact text a call replaced and what replaced it.
        /// Reverted-edit detection needs this: "same file touched twice" is
        /// too loose (two unrelated edits to one file aren't friction); "the
        /// second edit's old_string is the first edit's new_string" is cheap,
        /// deterministic evidence the second call is undoing or overwriting
        /// what the first one just wrote. `Write`/`MultiEdit` calls don't
        /// populate these (different input shape), so they never trigger the
        /// signature — conservative under-detection beats a noisy one.
        /// Truncated like every other text field here (`TEXT_CAP`), so two
        /// very long, genuinely-different strings sharing a common prefix
        /// could in principle compare equal after truncation — accepted for
        /// the same reason the rest of this module truncates: an
        /// approximate mechanical signal, not a byte-exact diff.
        old_string: Option<String>,
        new_string: Option<String>,
        /// Short display string for a window's summary text — the command
        /// if there is one, else a truncated compact form of the input.
        summary: String,
    },
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        text: String,
    },
}

const TEXT_CAP: usize = 300;

fn truncate(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        s.to_string()
    } else {
        let head: String = s.chars().take(cap).collect();
        format!("{head}…")
    }
}

// ---------------------------------------------------------------------------
// Claude .jsonl normalization
// ---------------------------------------------------------------------------

/// Parse a Claude Code transcript (one JSON object per line) into a
/// normalized event stream. Only `"user"`/`"assistant"` lines carry
/// conversation content — kickoff/meta lines (`mode`, `permission-mode`,
/// `file-history-snapshot`, `attachment`, `ai-title`, `summary`) are skipped,
/// matching `sessions.rs::scan_claude_jsonl`'s own line-type filtering.
/// Malformed lines are skipped, not fatal — a transcript is append-only and
/// can be read mid-write.
pub fn parse_claude_transcript_events(text: &str) -> Vec<TranscriptEvent> {
    let mut events = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let ty = v.get("type").and_then(Value::as_str);
        if !matches!(ty, Some("user") | Some("assistant")) {
            continue;
        }
        let role = ty.unwrap().to_string();
        let ts_ms = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso8601_ms);
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else { continue };
        match content {
            Value::String(s) => {
                if !s.is_empty() {
                    events.push(TranscriptEvent { role, kind: EventKind::Text { text: truncate(s, TEXT_CAP) }, ts_ms });
                }
            }
            Value::Array(blocks) => {
                for b in blocks {
                    push_claude_block(&mut events, &role, ts_ms, b);
                }
            }
            _ => {}
        }
    }
    events
}

fn push_claude_block(events: &mut Vec<TranscriptEvent>, role: &str, ts_ms: Option<u64>, b: &Value) {
    match b.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(t) = b.get("text").and_then(Value::as_str) {
                if !t.is_empty() {
                    events.push(TranscriptEvent {
                        role: role.to_string(),
                        kind: EventKind::Text { text: truncate(t, TEXT_CAP) },
                        ts_ms,
                    });
                }
            }
        }
        Some("tool_use") => {
            let id = b.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            let name = b.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            let input = b.get("input");
            let command = input
                .and_then(|i| i.get("command"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let file_path = input
                .and_then(|i| i.get("file_path"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let old_string = input
                .and_then(|i| i.get("old_string"))
                .and_then(Value::as_str)
                .map(|s| truncate(s, TEXT_CAP));
            let new_string = input
                .and_then(|i| i.get("new_string"))
                .and_then(Value::as_str)
                .map(|s| truncate(s, TEXT_CAP));
            let summary = command
                .clone()
                .or_else(|| file_path.clone())
                .unwrap_or_else(|| truncate(&input.map(Value::to_string).unwrap_or_default(), 120));
            events.push(TranscriptEvent {
                role: role.to_string(),
                kind: EventKind::ToolCall { id, name, command, file_path, old_string, new_string, summary: truncate(&summary, 120) },
                ts_ms,
            });
        }
        Some("tool_result") => {
            let tool_use_id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            let text = truncate(&tool_result_text(b.get("content")), TEXT_CAP);
            events.push(TranscriptEvent {
                role: role.to_string(),
                kind: EventKind::ToolResult { tool_use_id, is_error, text },
                ts_ms,
            });
        }
        _ => {}
    }
}

/// A `tool_result` block's `content` is either a bare string or an array of
/// `{type:"text", text}` blocks (real transcripts observed carry both
/// shapes depending on the tool). Join text parts; ignore non-text blocks
/// (e.g. images) — nothing in the friction heuristics reads them.
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Minimal `YYYY-MM-DDTHH:MM:SS[.fff]Z` → ms-since-epoch parser. No `chrono`
/// dependency (kept out deliberately — see the getrandom ban's neighboring
/// dependency-audit convention in `Cargo.toml`); Claude's transcript
/// timestamps are always this exact UTC/`Z` shape.
fn parse_iso8601_ms(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.splitn(3, '-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.splitn(3, ':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;
    let millis: i64 = format!("{frac:0<3}").get(0..3)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    let total_ms = days * 86_400_000 + hour * 3_600_000 + min * 60_000 + sec * 1000 + millis;
    (total_ms >= 0).then_some(total_ms as u64)
}

/// Howard Hinnant's `days_from_civil` — days since the Unix epoch for a
/// proleptic-Gregorian civil date. Well-known, integer-only, no external date
/// crate needed for the one thing we use it for (chronological ts_ms).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ---------------------------------------------------------------------------
// Copilot session-state normalization
// ---------------------------------------------------------------------------

/// Parse a Copilot session's `session-state/<id>/` files into the same
/// normalized event stream Claude produces. Copilot keeps no per-turn
/// conversation log today (`workspace.yaml` records only session metadata;
/// `checkpoints/index.md` is a chronological title table, not a transcript —
/// confirmed against a real `~/.copilot/session-state/*` tree, structure
/// only) — so this is deliberately thin: the session title stands in for the
/// initial prompt, and each checkpoint title becomes one assistant `Text`
/// event. No `tool_call`/`tool_result`/`is_error` signal is available from
/// this source, so friction-window extraction over a Copilot digest will
/// find little to nothing — expected and acceptable; Claude is this
/// feature's deterministic primary (see the plan's risk section).
pub fn parse_copilot_session_events(workspace_yaml: &str, checkpoints_index_md: &str) -> Vec<TranscriptEvent> {
    let mut events = Vec::new();
    if let Some(title) = crate::sessions::yaml_field(workspace_yaml, "name") {
        if !title.is_empty() {
            events.push(TranscriptEvent { role: "user".into(), kind: EventKind::Text { text: title }, ts_ms: None });
        }
    }
    for (idx, title) in parse_checkpoint_titles(checkpoints_index_md) {
        events.push(TranscriptEvent {
            role: "assistant".into(),
            kind: EventKind::Text { text: format!("checkpoint {idx}: {title}") },
            ts_ms: None,
        });
    }
    events
}

/// Read the `| # | Title | File |` rows out of a checkpoints `index.md`.
/// Header/separator rows fail the leading-`#`-cell `usize` parse and are
/// skipped for free.
fn parse_checkpoint_titles(md: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for line in md.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 2 {
            continue;
        }
        let Ok(idx) = cells[0].parse::<usize>() else { continue };
        if cells[1].is_empty() {
            continue;
        }
        out.push((idx, cells[1].to_string()));
    }
    out
}

// ---------------------------------------------------------------------------
// Friction-window extraction (Stage-1 mechanical reduction, no LLM)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrictionSignature {
    /// (a) a `tool_result` flagged `is_error:true`, and the `tool_use` that
    /// preceded it.
    ToolError,
    /// (b) the same shell-like tool re-run with a one-token-substituted
    /// command shortly after (the "tried npm, this repo is pnpm" shape).
    NearDuplicateCommand,
    /// (c) a test invocation that failed, followed later by one that passed.
    TestRedToGreen,
    /// (d) an `Edit` call whose `new_string` a LATER `Edit` call to the same
    /// file names as its own `old_string` — i.e. the second edit operates on
    /// exactly the text the first one just wrote (undoing it, or replacing
    /// it with something else). Tighter than "same file touched twice" on
    /// purpose (review finding NB2, #250/#324 slice B): two unrelated edits
    /// landing in one file are routine, not friction.
    RevertedEdit,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrictionWindow {
    pub signature: FrictionSignature,
    /// Index into the event slice the window was extracted from (inclusive).
    pub start: usize,
    pub end: usize,
    /// Short human-readable description of the wall + (if resolved) the fix.
    pub summary: String,
}

fn is_edit_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit")
}

fn looks_like_test_command(cmd: &str) -> bool {
    let lc = cmd.to_lowercase();
    lc.contains("test")
}

/// Two commands are a "near-duplicate rerun" when they're the same tool
/// invoked with the same token count and every token but the first
/// (typically the runner name — `npm` vs `pnpm`) is identical.
fn is_near_duplicate_command(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let ta: Vec<&str> = a.split_whitespace().collect();
    let tb: Vec<&str> = b.split_whitespace().collect();
    ta.len() > 1 && ta.len() == tb.len() && ta[0] != tb[0] && ta[1..] == tb[1..]
}

/// (a) tool_result is_error:true + preceding tool_use. The window closes at
/// the next successful result for the SAME tool (the recovery), else at the
/// end of the transcript (never recovered, at least not mechanically).
fn detect_tool_error_windows(events: &[TranscriptEvent]) -> Vec<FrictionWindow> {
    let mut windows = Vec::new();
    for (i, e) in events.iter().enumerate() {
        let EventKind::ToolResult { tool_use_id, is_error: true, text } = &e.kind else { continue };
        let Some(call_idx) = events[..i].iter().rposition(|c| matches!(&c.kind, EventKind::ToolCall { id, .. } if id == tool_use_id))
        else {
            continue;
        };
        let EventKind::ToolCall { name, .. } = &events[call_idx].kind else { unreachable!() };
        let mut end = events.len() - 1;
        for (j, e2) in events.iter().enumerate().skip(i + 1) {
            let EventKind::ToolResult { tool_use_id: id2, is_error: false, .. } = &e2.kind else { continue };
            let Some(call2) = events[..j].iter().rposition(|c| matches!(&c.kind, EventKind::ToolCall { id, .. } if id == id2))
            else {
                continue;
            };
            let EventKind::ToolCall { name: name2, .. } = &events[call2].kind else { unreachable!() };
            if name2 == name {
                end = j;
                break;
            }
        }
        windows.push(FrictionWindow {
            signature: FrictionSignature::ToolError,
            start: call_idx,
            end,
            summary: format!("{name} failed: {}", truncate(text, 120)),
        });
    }
    windows
}

/// (b) near-duplicate command re-runs among consecutive tool calls (text/
/// tool-result events in between don't break adjacency — only other tool
/// calls do).
fn detect_near_duplicate_commands(events: &[TranscriptEvent]) -> Vec<FrictionWindow> {
    let calls: Vec<(usize, &str, &str)> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.kind {
            EventKind::ToolCall { name, command: Some(cmd), .. } => Some((i, name.as_str(), cmd.as_str())),
            _ => None,
        })
        .collect();
    calls
        .windows(2)
        .filter(|w| w[0].1 == w[1].1 && is_near_duplicate_command(w[0].2, w[1].2))
        .map(|w| FrictionWindow {
            signature: FrictionSignature::NearDuplicateCommand,
            start: w[0].0,
            end: w[1].0,
            summary: format!("re-ran with a substituted first token: {:?} then {:?}", w[0].2, w[1].2),
        })
        .collect()
}

/// (c) a test invocation's result comes back an error, and a LATER test
/// invocation's result comes back clean.
fn detect_test_red_to_green(events: &[TranscriptEvent]) -> Vec<FrictionWindow> {
    let mut windows = Vec::new();
    let mut pending_fail: Option<usize> = None;
    for (i, e) in events.iter().enumerate() {
        let EventKind::ToolCall { id, command: Some(cmd), .. } = &e.kind else { continue };
        if !looks_like_test_command(cmd) {
            continue;
        }
        let result = events[i + 1..].iter().enumerate().find_map(|(k, e2)| match &e2.kind {
            EventKind::ToolResult { tool_use_id, is_error, .. } if tool_use_id == id => Some((i + 1 + k, *is_error)),
            _ => None,
        });
        let Some((ridx, is_error)) = result else { continue };
        if is_error {
            pending_fail = Some(i);
        } else if let Some(fail_idx) = pending_fail.take() {
            windows.push(FrictionWindow {
                signature: FrictionSignature::TestRedToGreen,
                start: fail_idx,
                end: ridx,
                summary: "a test run failed, then a later run passed".into(),
            });
        }
    }
    windows
}

/// "Evidence of restoration" (review finding NB2) — cheap and deterministic,
/// no file access: the second edit's `old_string` is exactly the first
/// edit's `new_string`, so the second call operates on the exact text the
/// first one just introduced. Not "some other edit happened to land in the
/// same file" — the original, false-positive-heavy check this replaces.
fn edit_overlaps(first_new: Option<&str>, second_old: Option<&str>) -> bool {
    matches!((first_new, second_old), (Some(a), Some(b)) if !a.is_empty() && a == b)
}

/// (d) an `Edit` call's `new_string` overlaps a LATER same-file `Edit`
/// call's `old_string` (in the tool-call stream — other files' edits in
/// between don't break adjacency). `Write`/`MultiEdit` calls carry no
/// `old_string`/`new_string` (see the field doc on `EventKind::ToolCall`),
/// so they never match here — this signature only fires where the evidence
/// is cheaply checkable, never on a same-file guess.
fn detect_reverted_edits(events: &[TranscriptEvent]) -> Vec<FrictionWindow> {
    let edits: Vec<(usize, &str, Option<&str>, Option<&str>)> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.kind {
            EventKind::ToolCall { name, file_path: Some(fp), old_string, new_string, .. } if is_edit_tool(name) => {
                Some((i, fp.as_str(), old_string.as_deref(), new_string.as_deref()))
            }
            _ => None,
        })
        .collect();
    edits
        .windows(2)
        .filter(|w| w[0].1 == w[1].1 && edit_overlaps(w[0].3, w[1].2))
        .map(|w| FrictionWindow {
            signature: FrictionSignature::RevertedEdit,
            start: w[0].0,
            end: w[1].0,
            summary: format!("{} edited again, touching the exact text the previous edit wrote", w[0].1),
        })
        .collect()
}

/// Run every friction signature over one normalized event stream and return
/// the windows in transcript order. Windows can overlap across signatures
/// (e.g. a failing test run is both a `ToolError` and half of a
/// `TestRedToGreen` pair) — deliberately: each signature is an independent,
/// cheap, mechanical lens, and de-duplicating them is exactly the kind of
/// judgment call left to the LLM stage that reads these windows, not this
/// deterministic one.
pub fn extract_friction_windows(events: &[TranscriptEvent]) -> Vec<FrictionWindow> {
    let mut windows = detect_tool_error_windows(events);
    windows.extend(detect_near_duplicate_commands(events));
    windows.extend(detect_test_red_to_green(events));
    windows.extend(detect_reverted_edits(events));
    windows.sort_by_key(|w| w.start);
    windows
}

// ---------------------------------------------------------------------------
// Session digest — friction windows + cheap anchors
// ---------------------------------------------------------------------------

/// Cap on the number of friction windows a `SessionDigest` carries (review
/// finding NB1): per-window text is already capped (`TEXT_CAP`/120 chars),
/// but the WINDOW COUNT was not — a long, flailing session could still emit
/// hundreds of them into an agent's context, exactly what this module exists
/// to avoid. A generous handful; see `cap_windows`.
const MAX_WINDOWS: usize = 20;

/// Keep the `max` most RECENT windows (highest `start`, i.e. closest to
/// however the session ended — the freshest friction is the most likely to
/// still matter) and report how many older ones were dropped, so a consumer
/// can tell "a clean session" from "a truncated digest" instead of silently
/// losing the tail. Windows are re-sorted chronologically on the way out —
/// only the SELECTION is recency-biased, not the order a reader sees them in.
fn cap_windows(mut windows: Vec<FrictionWindow>, max: usize) -> (Vec<FrictionWindow>, usize) {
    if windows.len() <= max {
        return (windows, 0);
    }
    windows.sort_by_key(|w| w.start);
    let dropped = windows.len() - max;
    let kept = windows.split_off(dropped);
    (kept, dropped)
}

/// `session_id` is usually system-assigned (Claude's own session uuid), but
/// `Task.session` can be set by an agent through `upsert_task`'s free-form
/// `session` field, and it reaches a filesystem path join in
/// `OrchRegistry::read_session_transcript_events`. Reject anything that
/// isn't a plain path component before that ever happens (review finding
/// NB4) — defense in depth: the join target is always under a fixed root
/// (`~/.claude/projects/*` or `~/.copilot/session-state`), but a `..` or a
/// separator in the id could still walk it outside that root.
pub fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != ".."
}

/// The reduced signal handed to an agent (process-pro) in place of a raw
/// transcript: the friction windows plus three anchors — what the worker was
/// asked to do, what its work resolved to, and how that turned out. The last
/// two are pass-through: this module has no git/gh access, so
/// `OrchRegistry::session_digest` supplies them from the task board (PR ref,
/// status) it already holds, never by shelling out here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionDigest {
    pub initial_prompt: Option<String>,
    pub final_diff_ref: Option<String>,
    pub outcome: Option<String>,
    pub windows: Vec<FrictionWindow>,
    /// How many older windows `cap_windows` cut to stay under `MAX_WINDOWS`
    /// — 0 for an uncapped digest. Always present, never silent (review
    /// finding NB1): a consumer can tell a clean session from a truncated one.
    pub dropped_windows: usize,
}

/// Build a digest from a normalized event stream. `final_diff_ref`/`outcome`
/// are supplied by the caller (see the struct doc); `initial_prompt` is the
/// first user `Text` event in the stream, falling back to
/// `initial_prompt_fallback` (e.g. the task's title) when the transcript
/// itself has none — Copilot's thin normalization commonly hits that case.
pub fn build_digest(
    events: &[TranscriptEvent],
    final_diff_ref: Option<String>,
    outcome: Option<String>,
    initial_prompt_fallback: Option<String>,
) -> SessionDigest {
    let initial_prompt = events
        .iter()
        .find_map(|e| match (&e.role[..], &e.kind) {
            ("user", EventKind::Text { text }) => Some(text.clone()),
            _ => None,
        })
        .or(initial_prompt_fallback);
    let (windows, dropped_windows) = cap_windows(extract_friction_windows(events), MAX_WINDOWS);
    SessionDigest { initial_prompt, final_diff_ref, outcome, windows, dropped_windows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claude_line(v: Value) -> String {
        v.to_string()
    }

    // ---- step 1: normalization ----

    #[test]
    fn claude_and_copilot_normalize_to_the_same_event_shape() {
        let claude_text = include_str!("../../tests/fixtures/digest/claude_sample.jsonl");
        let claude_events = parse_claude_transcript_events(claude_text);
        assert!(!claude_events.is_empty(), "fixture should yield at least one event");
        assert!(
            claude_events.iter().any(|e| matches!(e.kind, EventKind::Text { .. })),
            "fixture should include a text event"
        );
        assert!(
            claude_events.iter().any(|e| matches!(e.kind, EventKind::ToolCall { .. })),
            "fixture should include a tool_call event"
        );
        assert!(
            claude_events.iter().any(|e| matches!(e.kind, EventKind::ToolResult { .. })),
            "fixture should include a tool_result event"
        );

        let workspace = include_str!("../../tests/fixtures/digest/copilot/workspace.yaml");
        let checkpoints = include_str!("../../tests/fixtures/digest/copilot/checkpoints_index.md");
        let copilot_events = parse_copilot_session_events(workspace, checkpoints);
        assert!(!copilot_events.is_empty(), "fixture should yield at least one event");

        // Same Rust type on both sides (this compiles only if true) — and
        // every field the friction detectors read is present on both.
        for e in claude_events.iter().chain(copilot_events.iter()) {
            assert!(e.role == "user" || e.role == "assistant");
        }
    }

    #[test]
    fn claude_transcript_skips_non_conversation_line_types() {
        let text = format!(
            "{}\n{}\n",
            claude_line(json!({"type":"mode","mode":"default","sessionId":"s1"})),
            claude_line(json!({"type":"user","timestamp":"2026-07-15T10:00:00.000Z","message":{"role":"user","content":"hello"}})),
        );
        let events = parse_claude_transcript_events(&text);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].role, "user");
        assert_eq!(events[0].ts_ms, Some(1_784_109_600_000));
    }

    #[test]
    fn claude_transcript_reads_tool_use_and_tool_result_blocks() {
        let text = format!(
            "{}\n{}\n",
            claude_line(json!({"type":"assistant","message":{"role":"assistant","content":[
                {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test"}}
            ]}})),
            claude_line(json!({"type":"user","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"FAIL"}
            ]}})),
        );
        let events = parse_claude_transcript_events(&text);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0].kind, EventKind::ToolCall{name, command, ..} if name=="Bash" && command.as_deref()==Some("npm test")));
        assert!(matches!(&events[1].kind, EventKind::ToolResult{is_error: true, text, ..} if text=="FAIL"));
    }

    // ---- step 2: friction-window extraction, one test per signature, red-before-green ----

    fn tool_call(idx: usize, id: &str, name: &str, command: Option<&str>, file_path: Option<&str>) -> TranscriptEvent {
        let _ = idx;
        TranscriptEvent {
            role: "assistant".into(),
            kind: EventKind::ToolCall {
                id: id.into(),
                name: name.into(),
                command: command.map(str::to_string),
                file_path: file_path.map(str::to_string),
                old_string: None,
                new_string: None,
                summary: command.or(file_path).unwrap_or_default().into(),
            },
            ts_ms: None,
        }
    }

    /// An `Edit`-tool call carrying `old_string`/`new_string` — the shape
    /// `detect_reverted_edits` actually needs evidence from.
    fn edit_call(id: &str, file_path: &str, old_string: &str, new_string: &str) -> TranscriptEvent {
        TranscriptEvent {
            role: "assistant".into(),
            kind: EventKind::ToolCall {
                id: id.into(),
                name: "Edit".into(),
                command: None,
                file_path: Some(file_path.into()),
                old_string: Some(old_string.into()),
                new_string: Some(new_string.into()),
                summary: file_path.into(),
            },
            ts_ms: None,
        }
    }

    fn tool_result(id: &str, is_error: bool, text: &str) -> TranscriptEvent {
        TranscriptEvent { role: "user".into(), kind: EventKind::ToolResult { tool_use_id: id.into(), is_error, text: text.into() }, ts_ms: None }
    }

    #[test]
    fn signature_a_tool_error_windows_the_call_and_its_recovery() {
        let events = vec![
            tool_call(0, "t1", "Bash", Some("cargo build"), None),
            tool_result("t1", true, "error[E0433]: unresolved import"),
            tool_call(2, "t2", "Bash", Some("cargo build"), None),
            tool_result("t2", false, "Compiling loomux v0.10.0"),
        ];
        let windows = extract_friction_windows(&events);
        let w = windows.iter().find(|w| w.signature == FrictionSignature::ToolError).expect("a ToolError window");
        assert_eq!((w.start, w.end), (0, 3), "window spans the failing call through the same tool's next success");
    }

    #[test]
    fn signature_b_near_duplicate_command_reruns() {
        let events = vec![
            tool_call(0, "t1", "Bash", Some("npm install"), None),
            tool_result("t1", true, "npm: command not found"),
            tool_call(2, "t2", "Bash", Some("pnpm install"), None),
            tool_result("t2", false, "done"),
        ];
        let windows = extract_friction_windows(&events);
        let w = windows.iter().find(|w| w.signature == FrictionSignature::NearDuplicateCommand).expect("a NearDuplicateCommand window");
        assert_eq!((w.start, w.end), (0, 2));
    }

    #[test]
    fn signature_c_test_red_to_green() {
        let events = vec![
            tool_call(0, "t1", "Bash", Some("cargo test orchestration"), None),
            tool_result("t1", true, "FAILED tests::foo"),
            tool_call(2, "t2", "Edit", None, Some("src/orchestration/mod.rs")),
            tool_result("t2", false, "ok"),
            tool_call(4, "t3", "Bash", Some("cargo test orchestration"), None),
            tool_result("t3", false, "test result: ok"),
        ];
        let windows = extract_friction_windows(&events);
        let w = windows.iter().find(|w| w.signature == FrictionSignature::TestRedToGreen).expect("a TestRedToGreen window");
        assert_eq!((w.start, w.end), (0, 5));
    }

    #[test]
    fn signature_d_reverted_edit_requires_evidence_of_restoration() {
        let events = vec![
            edit_call("t1", "src/foo.rs", "let x = 1;", "let x = 2;"),
            tool_result("t1", false, "ok"),
            // t2's old_string is exactly t1's new_string: it's undoing t1.
            edit_call("t2", "src/foo.rs", "let x = 2;", "let x = 1;"),
            tool_result("t2", false, "ok"),
        ];
        let windows = extract_friction_windows(&events);
        let w = windows.iter().find(|w| w.signature == FrictionSignature::RevertedEdit).expect("a RevertedEdit window");
        assert_eq!((w.start, w.end), (0, 2));
    }

    /// Review finding NB2: the original check fired on ANY two edits to the
    /// same file, which is routine incremental work, not friction.
    #[test]
    fn signature_d_ignores_unrelated_edits_to_the_same_file() {
        let events = vec![
            edit_call("t1", "src/foo.rs", "fn a() {}", "fn a() { println!(\"a\"); }"),
            tool_result("t1", false, "ok"),
            edit_call("t2", "src/foo.rs", "fn b() {}", "fn b() { println!(\"b\"); }"),
            tool_result("t2", false, "ok"),
        ];
        let windows = extract_friction_windows(&events);
        assert!(
            !windows.iter().any(|w| w.signature == FrictionSignature::RevertedEdit),
            "unrelated edits to the same file must not be flagged: {windows:?}"
        );
    }

    /// `Write`/`MultiEdit` calls carry no old_string/new_string, so two
    /// same-file `Write`s never trigger the signature either — conservative
    /// under-detection where the evidence isn't cheaply available.
    #[test]
    fn signature_d_ignores_same_file_writes_with_no_old_new_evidence() {
        let events = vec![
            tool_call(0, "t1", "Write", None, Some("src/foo.rs")),
            tool_result("t1", false, "ok"),
            tool_call(2, "t2", "Write", None, Some("src/foo.rs")),
            tool_result("t2", false, "ok"),
        ];
        let windows = extract_friction_windows(&events);
        assert!(!windows.iter().any(|w| w.signature == FrictionSignature::RevertedEdit));
    }

    #[test]
    fn no_friction_signatures_means_no_windows() {
        let events = vec![
            tool_call(0, "t1", "Bash", Some("cargo check"), None),
            tool_result("t1", false, "ok"),
        ];
        assert!(extract_friction_windows(&events).is_empty());
    }

    // ---- window count cap (review finding NB1) ----

    #[test]
    fn build_digest_caps_windows_and_reports_how_many_were_dropped() {
        let total_pairs = MAX_WINDOWS + 5;
        let mut events = Vec::new();
        for i in 0..total_pairs {
            // Distinct tool name per pair so signature (a)'s "next success
            // for the SAME tool" search can't accidentally bridge pairs.
            let name = format!("Tool{i}");
            events.push(tool_call(0, &format!("{i}a"), &name, Some("run"), None));
            events.push(tool_result(&format!("{i}a"), true, "boom"));
            events.push(tool_call(0, &format!("{i}b"), &name, Some("run"), None));
            events.push(tool_result(&format!("{i}b"), false, "ok"));
        }
        let digest = build_digest(&events, None, None, None);
        assert_eq!(digest.windows.len(), MAX_WINDOWS);
        assert_eq!(digest.dropped_windows, 5);
        // The oldest 5 pairs were dropped; the kept windows start at pair 5's
        // failing call (index 5*4 = 20) — recency-biased selection, still
        // returned in chronological order.
        assert_eq!(digest.windows[0].start, 20);
        assert_eq!(digest.windows.last().unwrap().start, 4 * (total_pairs - 1));
    }

    #[test]
    fn build_digest_does_not_report_a_drop_when_under_the_cap() {
        let events = vec![
            tool_call(0, "t1", "Bash", Some("cargo build"), None),
            tool_result("t1", true, "boom"),
        ];
        let digest = build_digest(&events, None, None, None);
        assert_eq!(digest.dropped_windows, 0);
    }

    // ---- session id path-safety guard (review finding NB4) ----

    #[test]
    fn safe_session_ids_are_accepted() {
        assert!(is_safe_session_id("64f4d4f6-5201-4da9-8ed9-e0827ffae7df"));
    }

    #[test]
    fn session_ids_with_path_separators_or_traversal_are_rejected() {
        assert!(!is_safe_session_id("../../../../etc/passwd"));
        assert!(!is_safe_session_id("a/b"));
        assert!(!is_safe_session_id("a\\b"));
        assert!(!is_safe_session_id(".."));
        assert!(!is_safe_session_id("."));
        assert!(!is_safe_session_id(""));
    }

    // ---- build_digest anchors ----

    #[test]
    fn build_digest_takes_initial_prompt_from_the_first_user_text_event() {
        let events = vec![
            TranscriptEvent { role: "user".into(), kind: EventKind::Text { text: "fix the flaky test".into() }, ts_ms: None },
            tool_call(1, "t1", "Bash", Some("cargo test"), None),
        ];
        let digest = build_digest(&events, Some("#42".into()), Some("pr".into()), None);
        assert_eq!(digest.initial_prompt.as_deref(), Some("fix the flaky test"));
        assert_eq!(digest.final_diff_ref.as_deref(), Some("#42"));
        assert_eq!(digest.outcome.as_deref(), Some("pr"));
    }

    #[test]
    fn build_digest_falls_back_to_the_task_title_when_the_transcript_has_no_user_text() {
        let events = vec![tool_call(0, "t1", "Bash", Some("cargo test"), None)];
        let digest = build_digest(&events, None, None, Some("fix the flaky test".into()));
        assert_eq!(digest.initial_prompt.as_deref(), Some("fix the flaky test"));
    }

    // ---- iso8601 parsing ----

    #[test]
    fn iso8601_parses_the_unix_epoch() {
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00.000Z"), Some(0));
    }

    #[test]
    fn iso8601_parses_a_known_date() {
        // 2024-01-01T00:00:00Z is 1704067200 seconds after the epoch.
        assert_eq!(parse_iso8601_ms("2024-01-01T00:00:00.000Z"), Some(1_704_067_200_000));
    }

    #[test]
    fn iso8601_rejects_a_non_utc_shape() {
        assert_eq!(parse_iso8601_ms("2024-01-01T00:00:00+02:00"), None);
    }
}
