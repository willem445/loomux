//! Repo-defined agent profiles, read from the **standard** GitHub Copilot
//! *agents.md* convention: `<repo>/.github/agents/<name>.md` (also
//! `<name>.agent.md`). These are the same custom-agent files Copilot CLI
//! reads natively, so a repo's personas live with the workspace instead of a
//! loomux-specific config — and are shareable between developers via the repo
//! (issue #51).
//!
//! ```markdown
//! ---
//! name: worker
//! description: >
//!   Repo-specific worker addendum: always branch + PR, never push to main.
//! tools: [read, edit, shell]      # copilot-native, ignored by loomux here
//! # optional loomux mapping / extensions:
//! role: worker                    # orchestrator | worker | reviewer | planner
//! model: opus                     # overrides the role default (sanitized)
//! allow: Bash(make:*), mcp__probe # extra pre-approved tool patterns
//! ---
//! <persona / role-addendum instructions>
//! ```
//!
//! ## Mapping `.github/agents/*.md` files onto loomux roles
//!
//! loomux has four roles (orchestrator, worker, reviewer, planner). A profile
//! file is mapped to one of them by, in precedence order:
//!   1. an explicit frontmatter `role:` (or `kind:`) key, else
//!   2. the file's base name (`worker.md` → worker, `reviewer.agent.md` →
//!      reviewer, `orchestrator.md` → orchestrator, `planner.md` → planner),
//!   3. otherwise **worker** (a named specialist like `sempkg.agent.md` with
//!      no role hint is a worker persona).
//!
//! A profile mapped to a role becomes that role's *repo addendum*: its
//! instructions **append to** — never replace — loomux's built-in role
//! contract (the `report`/git-workflow guarantees always hold). The
//! orchestrator role can carry a repo addendum too (the human's "always
//! branch + PR" secondary prompt); loomux still owns the orchestrator's base
//! contract.
//!
//! On Claude the instructions body is injected as the agent's system prompt
//! (`--append-system-prompt-file`) and referenced in the kickoff; on Copilot
//! the persona engages via its native `--agent <name>`. MCP tool servers come
//! from the repo's standard `.mcp.json` (see `repo_mcp_servers` in mod.rs),
//! gated behind the group's `trust_repo_mcp` toggle. Profiles are re-read from
//! disk on every spawn so edits apply to the next agent.

use super::Role;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct AgentProfile {
    /// Profile name — the Copilot `--agent <name>` handle and the display
    /// default. Derived from the file stem unless frontmatter `name:` overrides.
    pub name: String,
    pub description: String,
    /// Role instructions (frontmatter stripped). Appended to loomux's built-in
    /// role contract, never replacing it.
    pub instructions: String,
    /// Per-profile model override (composed with the role's pinned model).
    pub model: Option<String>,
    /// loomux role this profile addends (from `role`/`kind`, else file name).
    pub role: Role,
    /// Extra pre-approved tool patterns (Claude `--allowedTools` / Copilot
    /// `--allow-tool`).
    pub allow: Vec<String>,
    /// Copilot custom-agent name (`--agent <name>`); defaults to `name`, which
    /// Copilot resolves against the same `.github/agents` files.
    pub copilot_agent: Option<String>,
}

/// Map a `role:`/`kind:` hint or file stem onto a loomux role. Returns `None`
/// for a value that names no role, so the caller can fall through to the next
/// precedence source (frontmatter → file name → default worker).
pub fn role_from_hint(hint: &str) -> Option<Role> {
    match hint.trim().to_ascii_lowercase().as_str() {
        "orchestrator" | "orch" => Some(Role::Orchestrator),
        "reviewer" | "review" => Some(Role::Reviewer),
        "planner" | "plan" => Some(Role::Planner),
        "worker" | "dev" | "developer" => Some(Role::Worker),
        _ => None,
    }
}

/// Tool patterns land inside double quotes on a shell line; strip anything
/// that could escape them.
fn sanitize_allow(s: &str) -> Option<String> {
    let cleaned: String = s
        .trim()
        .chars()
        .filter(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '(' | ')' | ':' | '*' | '_' | '-' | '.' | ' ' | '/')
        })
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
        // Continuation of the previous key's value (folded scalar body).
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
/// `.agent` dropped) is the default profile name *and* the default role hint.
/// Returns None for files without a frontmatter block or with no instructions
/// body — they're not agent definitions.
pub fn parse_profile(stem: &str, text: &str) -> Option<AgentProfile> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---")?;
    let (front, body) = rest.split_once("\n---")?;
    let default_name = stem.strip_suffix(".agent").unwrap_or(stem);
    let mut p = AgentProfile {
        name: sanitize_name(default_name)?,
        description: String::new(),
        instructions: body.trim_start_matches(['-']).trim().to_string(),
        model: None,
        // Fall back to the file name for the role, before frontmatter overrides.
        role: role_from_hint(default_name).unwrap_or(Role::Worker),
        allow: Vec::new(),
        copilot_agent: None,
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
            // Explicit role mapping wins over the file name. `kind` is #5's
            // spelling (worker|reviewer); `role` is the fuller loomux mapping.
            "role" | "kind" => {
                if let Some(r) = role_from_hint(&value) {
                    p.role = r;
                }
            }
            "allow" => p.allow = value.split(',').filter_map(sanitize_allow).collect(),
            "copilot-agent" | "copilot_agent" => p.copilot_agent = sanitize_name(&value),
            // tools / agents / target / … are copilot-native; copilot reads
            // them itself via --agent.
            _ => {}
        }
    }
    if p.copilot_agent.is_none() {
        p.copilot_agent = Some(p.name.clone());
    }
    (!p.instructions.is_empty()).then_some(p)
}

/// All agent definitions in a repo's `.github/agents/*.md`, sorted by name.
/// A missing directory (the common case) yields an empty list — never an
/// error, so discovery never blocks a spawn.
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

/// The repo profile that addends a given loomux role, if any. When several
/// files map to the same role (e.g. two workers), the first by name wins —
/// deterministic, and the human sees the full list in the launcher.
pub fn profile_for_role(profiles: &[AgentProfile], role: Role) -> Option<&AgentProfile> {
    profiles.iter().find(|p| p.role == role)
}

/// Look up a profile by its (case-sensitive) name — the `spawn_agent(profile:)`
/// path where the orchestrator picks a named persona explicitly.
pub fn find_named<'a>(profiles: &'a [AgentProfile], name: &str) -> Option<&'a AgentProfile> {
    profiles.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the real-world copilot agent-file shape (folded description,
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
        assert!(
            p.description.contains("Use when: exploring"),
            "continuation lines containing colons are description text, not keys"
        );
        assert!(
            p.instructions.contains("## Workflow"),
            "body --- separators must not truncate instructions"
        );
        assert_eq!(
            p.copilot_agent.as_deref(),
            Some("sempkg"),
            "copilot --agent defaults to the profile name (same .github/agents source)"
        );
        // No role hint anywhere -> a named specialist defaults to worker.
        assert_eq!(p.role, Role::Worker);
        assert!(p.model.is_none(), "copilot-specific keys must not bleed into loomux fields");
    }

    #[test]
    fn filename_maps_to_role_and_agent_suffix_stripped() {
        // `reviewer.agent.md` with no role frontmatter maps by file name.
        let p = parse_profile("reviewer.agent", "---\ndescription: repo reviewer\n---\nBe strict.").unwrap();
        assert_eq!(p.name, "reviewer");
        assert_eq!(p.role, Role::Reviewer, "file name maps onto the loomux role");
    }

    #[test]
    fn explicit_role_overrides_filename_and_extensions_apply() {
        // File stem says "worker", but frontmatter role: planner wins.
        let text = "---\nrole: planner\nmodel: opus\nallow: Bash(make:*), bad\"quote\n---\nYou plan.";
        let p = parse_profile("worker", text).unwrap();
        assert_eq!(p.role, Role::Planner, "frontmatter role overrides the file name");
        assert_eq!(p.model.as_deref(), Some("opus"));
        assert_eq!(p.allow, vec!["Bash(make:*)", "badquote"], "allow patterns are sanitized");
    }

    #[test]
    fn orchestrator_addendum_maps_by_name() {
        let p = parse_profile("orchestrator", "---\ndescription: repo rules\n---\nAlways branch + PR; never push to main.").unwrap();
        assert_eq!(p.role, Role::Orchestrator);
        assert!(p.instructions.contains("never push to main"));
    }

    #[test]
    fn kind_reviewer_still_supported() {
        // #5's `kind:` spelling keeps working alongside the new `role:`.
        let p = parse_profile("qa", "---\nkind: reviewer\n---\nReview it.").unwrap();
        assert_eq!(p.role, Role::Reviewer);
    }

    #[test]
    fn non_profiles_are_rejected() {
        assert!(parse_profile("readme", "# just a doc\nno frontmatter").is_none());
        assert!(
            parse_profile("empty", "---\ndescription: x\n---\n\n").is_none(),
            "no instructions = no profile"
        );
    }

    #[test]
    fn discovery_reads_github_agents_dir_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join(".github").join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("worker.md"), "---\ndescription: repo worker\n---\nBranch first.").unwrap();
        fs::write(agents.join("reviewer.agent.md"), "---\ndescription: repo reviewer\n---\nBe strict.").unwrap();
        fs::write(agents.join("notes.txt"), "not a profile").unwrap();
        fs::write(agents.join("no-front.md"), "no frontmatter here").unwrap();
        let repo = dir.path().to_string_lossy().into_owned();
        let profiles = discover_profiles(&repo);
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["reviewer", "worker"], "sorted by name, non-profiles skipped");

        // Role mapping resolves the right addendum per role.
        assert_eq!(profile_for_role(&profiles, Role::Worker).unwrap().name, "worker");
        assert_eq!(profile_for_role(&profiles, Role::Reviewer).unwrap().name, "reviewer");
        assert!(profile_for_role(&profiles, Role::Planner).is_none(), "no planner file -> no addendum");
        assert_eq!(find_named(&profiles, "reviewer").unwrap().role, Role::Reviewer);

        assert!(discover_profiles("C:/definitely/not/a/repo").is_empty());
    }
}
