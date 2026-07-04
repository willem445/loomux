//! Custom agent profiles: repo-defined worker/reviewer personas, read from
//! the **standard** `.github/agents/<name>.agent.md` files (the same ones
//! Copilot CLI uses natively), so personas live with the workspace instead
//! of a loomux-specific config.
//!
//! ```markdown
//! ---
//! name: sempkg
//! description: >
//!   Version-accurate code research agent ...
//! tools: [agent, search, read, sempkg/*]   # copilot-specific, ignored here
//! # optional loomux extensions:
//! model: opus                      # overrides the role default
//! kind: worker                     # worker (default) | reviewer
//! allow: Bash(make:*), mcp__probe  # extra pre-approved tools
//! ---
//! <persona instructions>
//! ```
//!
//! MCP tool servers come from the repo's standard `.mcp.json` (see
//! `repo_mcp_servers` in mod.rs) rather than per-profile config. On Copilot
//! the profile maps to its native `--agent <name>`; on Claude the
//! instructions body is injected as the agent's system prompt. Profiles are
//! re-read from disk on every spawn so edits apply to the next agent.

use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    /// Role instructions (frontmatter stripped).
    pub instructions: String,
    pub model: Option<String>,
    /// "worker" | "reviewer".
    pub kind: String,
    /// Extra pre-approved tool patterns (Claude allowedTools / copilot --allow-tool).
    pub allow: Vec<String>,
    /// Copilot custom agent name (`--agent <name>`); defaults to `name`,
    /// which copilot resolves against the same .github/agents files.
    pub copilot_agent: Option<String>,
}

/// Tool patterns land inside double quotes on a shell line; strip anything
/// that could escape them.
fn sanitize_allow(s: &str) -> Option<String> {
    let cleaned: String = s
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '(' | ')' | ':' | '*' | '_' | '-' | '.' | ' ' | '/'))
        .collect();
    (!cleaned.trim().is_empty()).then(|| cleaned.trim().to_string())
}

fn sanitize_name(s: &str) -> Option<String> {
    let cleaned: String = s
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Minimal YAML-ish frontmatter reader: `key: value` lines, where indented
/// continuation lines (folded scalars like `description: >`) append to the
/// previous key. Returns (key, folded value) pairs in order.
fn parse_frontmatter(front: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in front.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            if let Some((key, value)) = line.split_once(':') {
                out.push((key.trim().to_lowercase(), value.trim().to_string()));
                continue;
            }
        }
        // Continuation of the previous key's value.
        if let Some(last) = out.last_mut() {
            if !last.1.is_empty() {
                last.1.push(' ');
            }
            last.1.push_str(line.trim());
        }
    }
    // Fold-scalar markers themselves aren't content.
    for (_, v) in &mut out {
        if let Some(rest) = v.strip_prefix('>').or_else(|| v.strip_prefix('|')) {
            *v = rest.trim().to_string();
        }
        *v = v.trim().trim_matches('"').trim_matches('\'').to_string();
    }
    out
}

/// Parse a profile file. `stem` (file name without `.md`, with any trailing
/// `.agent` dropped) is the default profile name. Returns None for files
/// without a frontmatter block — they're not agent definitions.
pub fn parse_profile(stem: &str, text: &str) -> Option<AgentProfile> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---")?;
    let (front, body) = rest.split_once("\n---")?;
    let default_name = stem.strip_suffix(".agent").unwrap_or(stem);
    let mut p = AgentProfile {
        name: sanitize_name(default_name)?,
        kind: "worker".into(),
        instructions: body.trim_start_matches(['-']).trim().to_string(),
        ..Default::default()
    };
    for (key, value) in parse_frontmatter(front) {
        match key.as_str() {
            "name" => {
                if let Some(n) = sanitize_name(&value) {
                    p.name = n;
                }
            }
            "description" => p.description = value,
            "model" => p.model = (!value.is_empty()).then_some(value),
            "kind" => {
                if value == "reviewer" {
                    p.kind = "reviewer".into();
                }
            }
            "allow" => p.allow = value.split(',').filter_map(sanitize_allow).collect(),
            "copilot-agent" | "copilot_agent" => p.copilot_agent = sanitize_name(&value),
            // tools / agents / etc. are copilot-native; copilot reads them
            // itself via --agent.
            _ => {}
        }
    }
    if p.copilot_agent.is_none() {
        p.copilot_agent = Some(p.name.clone());
    }
    (!p.instructions.is_empty()).then_some(p)
}

/// All agent definitions in a repo (`.github/agents/*.md`), sorted by name.
pub fn discover_profiles(repo: &str) -> Vec<AgentProfile> {
    let dir = Path::new(repo).join(".github").join("agents");
    let Ok(entries) = fs::read_dir(&dir) else {
        return vec![];
    };
    let mut out: Vec<AgentProfile> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            parse_profile(&stem, &fs::read_to_string(&path).ok()?)
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the real-world copilot agent file shape (folded description,
    /// copilot-specific keys) that discovery must digest.
    const COPILOT_STYLE: &str = "---\nname: sempkg\ndescription: >\n  Version-accurate code research agent.\n  Use when: exploring an unfamiliar dependency; looking up API symbols.\ntools: [agent, search, todo, read, execute, web, edit, sempkg/*]\nagents: [\"*\"]\n---\n\n# sempkg Research Agent\n\nYou are a precision code research assistant.\n\n---\n\n## Workflow\nmore body\n";

    #[test]
    fn parses_copilot_agent_md_with_folded_description() {
        let p = parse_profile("sempkg.agent", COPILOT_STYLE).unwrap();
        assert_eq!(p.name, "sempkg");
        assert!(
            p.description.starts_with("Version-accurate code research agent."),
            "folded (>) descriptions must join their continuation lines, got: {}",
            p.description
        );
        assert!(p.description.contains("Use when: exploring"),
            "continuation lines containing colons are description text, not keys");
        assert!(p.instructions.contains("## Workflow"), "body --- separators must not truncate instructions");
        assert_eq!(p.copilot_agent.as_deref(), Some("sempkg"),
            "copilot --agent defaults to the profile name (same .github/agents source)");
        assert_eq!(p.kind, "worker");
        assert!(p.model.is_none(), "copilot-specific keys must not bleed into loomux fields");
    }

    #[test]
    fn stem_fallback_strips_agent_suffix_and_loomux_extensions_apply() {
        let text = "---\ndescription: Embedded developer\nmodel: opus\nkind: reviewer\nallow: Bash(make:*), bad\"quote\n---\nYou are the embedded developer.";
        let p = parse_profile("embedded-dev.agent", text).unwrap();
        assert_eq!(p.name, "embedded-dev");
        assert_eq!(p.model.as_deref(), Some("opus"));
        assert_eq!(p.kind, "reviewer");
        assert_eq!(p.allow, vec!["Bash(make:*)", "badquote"]);
    }

    #[test]
    fn non_profiles_are_rejected() {
        assert!(parse_profile("readme", "# just a doc\nno frontmatter").is_none());
        assert!(parse_profile("empty", "---\ndescription: x\n---\n\n").is_none(), "no instructions = no profile");
    }

    #[test]
    fn discovery_reads_github_agents_dir() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join(".github").join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("po.agent.md"), "---\ndescription: Product owner\n---\nOwn the requirements.").unwrap();
        fs::write(agents.join("plain.md"), "---\ndescription: also valid\n---\nBody.").unwrap();
        fs::write(agents.join("notes.txt"), "not a profile").unwrap();
        fs::write(agents.join("no-front.md"), "no frontmatter here").unwrap();
        let repo = dir.path().to_string_lossy().into_owned();
        let names: Vec<String> = discover_profiles(&repo).into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["plain", "po"]);
        assert!(discover_profiles("C:/definitely/not/a/repo").is_empty());
    }
}
