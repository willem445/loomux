//! Custom agent profiles: repo-defined worker/reviewer personas.
//!
//! A profile is a markdown file at `<repo>/.loomux/agents/<name>.md` with a
//! small frontmatter block and role instructions as the body:
//!
//! ```markdown
//! ---
//! description: Embedded developer with hardware tools
//! model: opus                      # optional, overrides the role default
//! kind: worker                     # worker (default) | reviewer
//! mcp: .loomux/mcp/embedded.json   # optional extra MCP servers (repo-relative)
//! allow: Bash(make:*), mcp__probe  # optional extra pre-approved tools
//! copilot-agent: embedded          # optional: copilot --agent mapping
//! ---
//! You are the embedded developer for this project. ...
//! ```
//!
//! Profiles are re-read from disk on every spawn so edits apply to the next
//! agent without relaunching the group. The instructions body is injected
//! as the agent's system prompt on Claude (`--append-system-prompt-file`);
//! Copilot uses its native custom agent when `copilot-agent` is set, and
//! the kickoff references the rendered brief either way.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    /// Role instructions (frontmatter stripped).
    pub instructions: String,
    pub model: Option<String>,
    /// "worker" | "reviewer".
    pub kind: String,
    /// Repo-relative (or absolute) path to an extra MCP servers JSON file.
    pub mcp: Option<String>,
    /// Extra pre-approved tool patterns (Claude allowedTools / copilot --allow-tool).
    pub allow: Vec<String>,
    /// Copilot custom agent name (`--agent <name>`).
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

/// Parse a profile file. `stem` (the file name without extension) is the
/// default profile name. Returns None for files without a frontmatter block
/// — they're not profiles.
pub fn parse_profile(stem: &str, text: &str) -> Option<AgentProfile> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---")?;
    let (front, body) = rest.split_once("\n---")?;
    let mut p = AgentProfile {
        name: sanitize_name(stem)?,
        kind: "worker".into(),
        instructions: body.trim_start_matches(['-']).trim().to_string(),
        ..Default::default()
    };
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim().to_lowercase().as_str() {
            "name" => {
                if let Some(n) = sanitize_name(value) {
                    p.name = n;
                }
            }
            "description" => p.description = value.to_string(),
            "model" => p.model = (!value.is_empty()).then(|| value.to_string()),
            "kind" => {
                if value == "reviewer" {
                    p.kind = "reviewer".into();
                }
            }
            "mcp" => p.mcp = (!value.is_empty()).then(|| value.to_string()),
            "allow" => p.allow = value.split(',').filter_map(sanitize_allow).collect(),
            "copilot-agent" | "copilot_agent" => p.copilot_agent = sanitize_name(value),
            _ => {}
        }
    }
    (!p.instructions.is_empty()).then_some(p)
}

/// All profiles defined in a repo, sorted by name.
pub fn discover_profiles(repo: &str) -> Vec<AgentProfile> {
    let dir = Path::new(repo).join(".loomux").join("agents");
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

/// Resolve a profile's extra MCP servers file against the repo.
pub fn mcp_path(repo: &str, mcp: &str) -> PathBuf {
    let p = Path::new(mcp);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(repo).join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\ndescription: Embedded developer\nmodel: opus\nmcp: .loomux/mcp/embedded.json\nallow: Bash(make:*), mcp__probe, bad\"quote\nkind: worker\ncopilot-agent: embedded\n---\nYou are the embedded developer. Use the HW tools.\n";

    #[test]
    fn parses_frontmatter_and_body() {
        let p = parse_profile("embedded-dev", SAMPLE).unwrap();
        assert_eq!(p.name, "embedded-dev");
        assert_eq!(p.description, "Embedded developer");
        assert_eq!(p.model.as_deref(), Some("opus"));
        assert_eq!(p.kind, "worker");
        assert_eq!(p.mcp.as_deref(), Some(".loomux/mcp/embedded.json"));
        assert_eq!(p.copilot_agent.as_deref(), Some("embedded"));
        assert!(p.instructions.starts_with("You are the embedded developer"));
        // Allow entries are shell-quoted later: quote characters must not survive.
        assert_eq!(p.allow, vec!["Bash(make:*)", "mcp__probe", "badquote"]);
    }

    #[test]
    fn reviewer_kind_and_name_override() {
        let text = "---\nname: Test Dev!!\nkind: reviewer\n---\nReview firmware tests.";
        let p = parse_profile("test-dev", text).unwrap();
        assert_eq!(p.kind, "reviewer");
        assert_eq!(p.name, "TestDev", "names are sanitized for ids/paths");
    }

    #[test]
    fn non_profiles_are_rejected() {
        assert!(parse_profile("readme", "# just a doc\nno frontmatter").is_none());
        assert!(parse_profile("empty", "---\ndescription: x\n---\n\n").is_none(), "no instructions = no profile");
    }

    #[test]
    fn discovery_reads_only_md_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join(".loomux").join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("po.md"), "---\ndescription: Product owner\n---\nOwn the requirements.").unwrap();
        fs::write(agents.join("notes.txt"), "not a profile").unwrap();
        fs::write(agents.join("plain.md"), "no frontmatter here").unwrap();
        let repo = dir.path().to_string_lossy().into_owned();
        let profiles = discover_profiles(&repo);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "po");
        assert!(discover_profiles("C:/definitely/not/a/repo").is_empty());
    }
}
