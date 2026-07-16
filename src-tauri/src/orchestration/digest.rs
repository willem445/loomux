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
            let summary = command
                .clone()
                .or_else(|| file_path.clone())
                .unwrap_or_else(|| truncate(&input.map(Value::to_string).unwrap_or_default(), 120));
            events.push(TranscriptEvent {
                role: role.to_string(),
                kind: EventKind::ToolCall { id, name, command, file_path, summary: truncate(&summary, 120) },
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
    /// (d) an edit/write to a file, followed later by another edit/write to
    /// the same file (an overwrite or revert-and-redo).
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

/// (d) an edit/write tool touches the same file twice (in the tool-call
/// stream — other files' edits in between don't break adjacency).
fn detect_reverted_edits(events: &[TranscriptEvent]) -> Vec<FrictionWindow> {
    let edits: Vec<(usize, &str)> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.kind {
            EventKind::ToolCall { name, file_path: Some(fp), .. } if is_edit_tool(name) => Some((i, fp.as_str())),
            _ => None,
        })
        .collect();
    edits
        .windows(2)
        .filter(|w| w[0].1 == w[1].1)
        .map(|w| FrictionWindow {
            signature: FrictionSignature::RevertedEdit,
            start: w[0].0,
            end: w[1].0,
            summary: format!("{} edited again shortly after", w[0].1),
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
    SessionDigest { initial_prompt, final_diff_ref, outcome, windows: extract_friction_windows(events) }
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
                summary: command.or(file_path).unwrap_or_default().into(),
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
    fn signature_d_reverted_edit_same_file_touched_twice() {
        let events = vec![
            tool_call(0, "t1", "Edit", None, Some("src/foo.rs")),
            tool_result("t1", false, "ok"),
            tool_call(2, "t2", "Edit", None, Some("src/foo.rs")),
            tool_result("t2", false, "ok"),
        ];
        let windows = extract_friction_windows(&events);
        let w = windows.iter().find(|w| w.signature == FrictionSignature::RevertedEdit).expect("a RevertedEdit window");
        assert_eq!((w.start, w.end), (0, 2));
    }

    #[test]
    fn no_friction_signatures_means_no_windows() {
        let events = vec![
            tool_call(0, "t1", "Bash", Some("cargo check"), None),
            tool_result("t1", false, "ok"),
        ];
        assert!(extract_friction_windows(&events).is_empty());
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
