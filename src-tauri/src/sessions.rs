//! Discovery of resumable AI agent sessions on the local machine.
//!
//! Claude Code:    ~/.claude/projects/<encoded-path>/<uuid>.jsonl
//! Copilot CLI:    ~/.copilot/session-state/<uuid>/workspace.yaml
//!
//! Both scanners are best-effort: unreadable or malformed entries are
//! skipped, and a missing tool simply yields an empty list. New agent
//! sources can be added by implementing another `scan_*` function and
//! extending `list_sessions`.

use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Serialize)]
pub struct SessionInfo {
    /// Session id understood by the agent's `--resume` flag.
    pub id: String,
    /// Which agent owns the session: "claude" | "copilot".
    pub source: String,
    /// Human-readable one-liner (first prompt or session name).
    pub title: String,
    /// Working directory the session ran in.
    pub cwd: String,
    /// Last-modified time, unix millis.
    pub modified_ms: u64,
    /// Shell command line that resumes this session.
    pub resume_command: String,
}

fn mtime_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Extract plain text from a Claude message `content` field, which is
/// either a string or an array of {type:"text"} blocks.
fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => blocks.iter().find_map(|b| {
            (b.get("type")?.as_str()? == "text")
                .then(|| b.get("text")?.as_str().map(str::to_string))
                .flatten()
        }),
        _ => None,
    }
}

fn tidy_title(raw: &str, limit: usize) -> String {
    let line = raw.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut t: String = line.trim().chars().take(limit).collect();
    if line.trim().chars().count() > limit {
        t.push('…');
    }
    t
}

/// Pull title/cwd out of a session jsonl by scanning its head. Summary
/// lines and the first real (non-meta, non-command) user prompt are the
/// best title candidates.
fn scan_claude_jsonl(path: &Path) -> (String, String) {
    let mut title = String::new();
    let mut summary = String::new();
    let mut cwd = String::new();

    let Ok(file) = fs::File::open(path) else {
        return (title, cwd);
    };
    let reader = BufReader::new(file);

    for line in reader.lines().take(60).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                cwd = c.to_string();
            }
        }
        match v.get("type").and_then(Value::as_str) {
            Some("summary") => {
                if let Some(s) = v.get("summary").and_then(Value::as_str) {
                    summary = s.to_string();
                }
            }
            Some("user") => {
                let is_meta = v.get("isMeta").and_then(Value::as_bool).unwrap_or(false);
                if is_meta || !title.is_empty() {
                    continue;
                }
                if let Some(text) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(content_text)
                {
                    let trimmed = text.trim();
                    // Skip injected command/caveat wrappers.
                    if !trimmed.is_empty() && !trimmed.starts_with('<') {
                        title = tidy_title(trimmed, 90);
                    }
                }
            }
            _ => {}
        }
        if !title.is_empty() && !cwd.is_empty() {
            break;
        }
    }

    if title.is_empty() {
        title = if summary.is_empty() {
            "(no prompt)".to_string()
        } else {
            tidy_title(&summary, 90)
        };
    }
    (title, cwd)
}

fn scan_claude(out: &mut Vec<SessionInfo>) {
    let Some(root) = dirs::home_dir().map(|h| h.join(".claude").join("projects")) else {
        return;
    };
    let Ok(projects) = fs::read_dir(&root) else {
        return;
    };
    for project in projects.flatten() {
        let Ok(files) = fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let (title, cwd) = scan_claude_jsonl(&path);
            out.push(SessionInfo {
                resume_command: format!("claude --resume {id}"),
                id: id.to_string(),
                source: "claude".to_string(),
                title,
                cwd,
                modified_ms: mtime_ms(&path),
            });
        }
    }
}

/// Minimal single-level YAML field lookup — enough for workspace.yaml
/// without pulling in a YAML dependency.
fn yaml_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    text.lines().find_map(|l| {
        l.strip_prefix(&prefix)
            .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
    })
}

fn scan_copilot(out: &mut Vec<SessionInfo>) {
    let Some(root) = dirs::home_dir().map(|h| h.join(".copilot").join("session-state")) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let ws = entry.path().join("workspace.yaml");
        let Ok(text) = fs::read_to_string(&ws) else {
            continue;
        };
        let Some(id) = yaml_field(&text, "id") else {
            continue;
        };
        let title = yaml_field(&text, "name")
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Copilot session".to_string());
        let cwd = yaml_field(&text, "cwd").unwrap_or_default();
        out.push(SessionInfo {
            resume_command: format!("copilot --resume {id}"),
            id,
            source: "copilot".to_string(),
            title: tidy_title(&title, 90),
            cwd,
            modified_ms: mtime_ms(&ws),
        });
    }
}

#[tauri::command]
pub fn list_sessions() -> Vec<SessionInfo> {
    let mut sessions = Vec::new();
    scan_claude(&mut sessions);
    scan_copilot(&mut sessions);
    sessions.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    sessions.truncate(300);
    sessions
}
