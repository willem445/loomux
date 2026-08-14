//! Repo-defined agent personas, read from the **standard** GitHub Copilot
//! *agents.md* convention: `<repo>/.github/agents/<name>.md` (also
//! `<name>.agent.md`). These are the same custom-agent files Copilot CLI reads
//! natively, so a repo's personas live with the workspace instead of in a
//! loomux-specific config — and are shareable between developers via the repo
//! (issue #51).
//!
//! **Harvested from PR #105** (`agent-prototype`, superseded by #222): the
//! parser, the discovery walk, the append/replace modes and the
//! non-overridable mechanics core are all its design, brought over rather than
//! rebased. What changed in the move to the block model (#222):
//!
//! - A profile no longer *maps itself onto a role*. `.loomux/workflow.yml` says
//!   which block uses which persona (`profile: .github/agents/x.md`), so
//!   `profile_for_role`'s auto-mapping-by-filename is gone: a persona file can
//!   no longer take effect just by existing. It is opt-in, by reference. The
//!   `role:`/`kind:` frontmatter survives only as a **compatibility check** —
//!   if a file says `kind: planner` and a `worker` block points at it, that is
//!   an error rather than a silent capability change.
//! - Claude's persona delivery has changed mechanism twice since #105:
//!   `claude --agents '<json>' --agent <id>` (#222, post-dating #105)
//!   carried it inline for a time, until round #417 correction 6 moved it
//!   BACK to a file — `--agent <handle>` alone, naming a loomux-generated
//!   `~/.claude/agents/<handle>.md` — after a live demo showed the inline
//!   payload (widened by #416 to carry the full role contract, not just a
//!   short persona) could exceed Windows `CreateProcessW`'s 32,767-character
//!   command-line limit. `--append-system-prompt-file` itself is back too,
//!   as the write-failure fallback (see `PersonaInject::claude_append_
//!   system_prompt_file`'s doc in `mod.rs`) — the #222 move away from it was
//!   about `--agents` being newer and native, never about the file-based
//!   flag having a functional problem, so nothing disqualified it from
//!   returning as a fallback. See `persona_inject` in `mod.rs`.
//!
//! ```markdown
//! ---
//! name: worker
//! description: Repo-specific worker persona.
//! tools: [read, edit, shell]      # copilot-native; loomux READS it (see below)
//! model: opus                     # copilot-native; loomux ignores it (see below)
//! # loomux extensions:
//! kind: worker                    # compatibility CHECK against the block's kind
//! allow: Bash(make:*), mcp__probe # extra pre-approved tool patterns
//! mode: append                    # append (default) | replace
//! ---
//! <persona instructions>
//! ```
//!
//! **`tools:` is copilot's, but loomux cannot ignore it (#802).** It is a
//! FILTER, not an addition. Per GitHub's [custom-agents configuration
//! reference](https://docs.github.com/en/copilot/reference/custom-agents-configuration),
//! *Tools processing*: *"The `tools` list filters the set of tools that are made
//! available to the agent - whether built-in or sourced from MCP servers"*, and
//! *"If no tools are specified, all available tools are enabled"*. So a persona
//! file carrying `tools: [read, edit]` and pointed at by a `cli: copilot` block
//! silently strips **loomux's own MCP tools** from the delegate it launches: the
//! server still loads (it rides `--additional-mcp-config`, so the CLI lists it)
//! and every one of its tools is filtered out — an agent that cannot `report`,
//! cannot read the task board, and cannot be steered. That is the live failure
//! #802 reports, and it is invisible from argv, which is why the list is parsed
//! here now instead of being skipped as "copilot's business".
//!
//! What loomux does about it lives in `persona_inject`/`write_copilot_agent_file`
//! (`mod.rs`): it never edits the user's file (#222), it re-points `--agent` at a
//! loomux-owned copy carrying the SAME list plus the loomux grant. The grant
//! spelling is the documented one: *"You can also explicitly enable all tools
//! from a specific MCP server using `some-mcp-server/*`"*.
//!
//! **`model:` is not a loomux knob.** Copilot reads it itself when it loads the
//! file via `--agent`; loomux takes the model from the *block* (`model:` in
//! `workflow.yml`), which is the one place that works on every CLI and is the
//! value the launcher, the audit log and the guardrails all agree on. It is
//! parsed here only so a reader (and the workflow pane) can see what the file
//! says. Two sources of truth for one pinned model is exactly the kind of
//! silent-divergence bug this whole issue is about.
//!
//! ## Append vs replace (`mode:`)
//!
//! - `mode: append` (**default**) — the persona is an *addendum*: loomux's
//!   built-in role contract (the `report`/git-workflow guarantees, MCP tool
//!   guidance, session discipline) still applies and the repo text layers on
//!   top.
//! - `mode: replace` — the persona replaces the built-in role *body*. loomux
//!   still injects the **non-overridable mechanics core**
//!   (`orchestration::mechanics_core`, which stays in `src-tauri` next to the
//!   role templates it renders), so a replace persona that
//!   forgets to mention `report()` or the branch→PR discipline stays
//!   functional. That is the invariant `replace_mode_persona_keeps_mechanics_core`
//!   pins: replace can change *who the agent is*, never *what loomux guarantees*.
//!
//! A persona can never change what an agent is *allowed* to do — the capability
//! class comes from the block's `kind`, which is a closed enum (see
//! [`workflow`](crate::workflow)). `allow:` can only add pre-approved tool
//! patterns *within* what the class permits: deny rules beat allow rules on
//! both CLIs, so a reviewer persona cannot allow itself back into `git push`.

use crate::model::Role;
use std::fs;
use std::path::Path;

/// How a persona relates to the built-in role contract. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProfileMode {
    /// Repo instructions layer on top of the built-in role contract (default).
    #[default]
    Append,
    /// Repo instructions replace the built-in role body. The mechanics core is
    /// injected regardless — it is not overridable.
    Replace,
}

impl ProfileMode {
    /// Wire/label string (`"append"` | `"replace"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileMode::Append => "append",
            ProfileMode::Replace => "replace",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfile {
    /// Persona name — the Copilot `--agent <name>` handle and the display
    /// default. Derived from the file stem unless frontmatter `name:` overrides.
    pub name: String,
    pub description: String,
    /// Persona instructions (frontmatter stripped).
    pub instructions: String,
    /// The file's `model:`, verbatim. **loomux does not apply this** — it is
    /// Copilot's own key, and the block's `model:` is loomux's single source of
    /// truth (see the module docs). Parsed for display/inspection only; it never
    /// reaches a command line, which is also why it is not `sanitize_model`'d.
    pub model: Option<String>,
    /// The capability class this persona declares it is written for, if any.
    /// **Checked against the block's `kind`, never used to change it** — a repo
    /// file can never move an agent into a different capability class.
    pub kind: Option<Role>,
    /// Append (default) or replace the built-in role body.
    pub mode: ProfileMode,
    /// Extra pre-approved tool patterns (Claude `--allowedTools` / Copilot
    /// `--allow-tool`).
    pub allow: Vec<String>,
    /// Copilot custom-agent name (`--agent <name>`); defaults to `name`, which
    /// Copilot resolves against the same `.github/agents` files.
    pub copilot_agent: Option<String>,
    /// The file's `tools:` list, verbatim (#802). `None` = the key is absent,
    /// which Copilot documents as "all available tools"; `Some(vec![])` = an
    /// explicit `tools: []`, which it documents as "disables all tools" — two
    /// states an `Option` keeps apart and a plain `Vec` would not.
    ///
    /// loomux does not *apply* this (it is Copilot's own filter) but it must
    /// READ it: see the module docs for why a list that omits loomux's MCP
    /// server produces a delegate that cannot report.
    pub tools: Option<Vec<String>>,
    /// Whether the file declares its own `mcp-servers:` block (#802).
    ///
    /// Presence only — loomux neither parses nor reproduces the block. It is
    /// recorded because it is the one case where loomux must NOT re-point
    /// `--agent` at a generated copy to repair a `tools:` gap: the copy would
    /// silently drop MCP servers the user declared, trading one missing-tools
    /// bug for another. Such a file gets the loud warning and no rewrite.
    pub has_mcp_servers: bool,
    /// The file's frontmatter block, **verbatim** (between the `---` fences,
    /// fences excluded) (#802).
    ///
    /// Kept because loomux's #802 repair writes a *stand-in* for this file, and
    /// a stand-in carrying only the keys loomux happens to parse is a persona
    /// silently changing shape: `model:`, `infer:`, `target:`,
    /// `disable-model-invocation:` and whatever Copilot documents next are all
    /// real keys loomux has no opinion about, and dropping them is not loomux's
    /// call to make. Only `name`/`description`/`tools` are re-authored in the
    /// copy (see `write_copilot_agent_file`); every other line is reproduced as
    /// written.
    ///
    /// Never parsed beyond the key skim above — it is carried, not understood.
    pub frontmatter: String,
}

impl AgentProfile {
    /// Whether this persona's `tools:` filter leaves **every** tool of MCP
    /// server `server` available to the agent (#802).
    ///
    /// True when the key is absent (*"If no tools are specified, all available
    /// tools are enabled"*), when it carries the `*` wildcard (*"or use `tools:
    /// ["*"]` to enable all available tools"*), or when it names the server's
    /// own wildcard (*"You can also explicitly enable all tools from a specific
    /// MCP server using `some-mcp-server/*`"*) — all three quoted from the
    /// custom-agents configuration reference's *Tools* section.
    ///
    /// A bare `<server>` entry counts too. The reference does not document that
    /// spelling for `tools:` (it documents it for the CLI's `--allow-tool`), but
    /// it also says *"All unrecognized tool names are ignored"* — so a user who
    /// wrote the argv spelling into their agent file has either already granted
    /// the server or written an inert entry, and in neither case is loomux's
    /// rewrite the thing that fixes it. Treating it as a grant keeps loomux from
    /// second-guessing a file that may well be correct on a newer CLI.
    pub fn grants_mcp_server(&self, server: &str) -> bool {
        tools_grant_mcp_server(self.tools.as_deref(), server)
    }

    /// Whether the `tools:` list names `server` at all — including a per-tool
    /// grant like `loomux/report` (#802).
    ///
    /// Only used to word the warning: "your list grants `loomux/report` but not
    /// the rest" is a different sentence from "your list never mentions loomux",
    /// and a human chasing this bug deserves the accurate one.
    pub fn mentions_mcp_server(&self, server: &str) -> bool {
        tools_mention_mcp_server(self.tools.as_deref(), server)
    }
}

/// [`AgentProfile::grants_mcp_server`] over a bare list, so the same rule serves
/// a parsed profile and an `orchestration::ResolvedPersona` (which
/// carries the list onward to the spawn) without either re-deciding it.
pub fn tools_grant_mcp_server(tools: Option<&[String]>, server: &str) -> bool {
    let Some(tools) = tools else { return true };
    let server_wildcard = format!("{server}/*");
    tools.iter().any(|t| {
        let t = t.trim();
        t == "*" || t == server || t == server_wildcard
    })
}

/// Keys a loomux-generated stand-in re-authors for itself, and therefore the
/// only ones [`carry_frontmatter`] drops from the user's block (#802).
///
/// - `name` **must** be the loomux handle: Copilot resolves `--agent` against
///   the filename stem and the copy's stem is loomux's, so a carried-over `name`
///   would leave the file disagreeing with its own filename *and* would put a
///   second file into the world claiming the user's handle.
/// - `description` is Required by the schema and is loomux's own bookkeeping
///   label for the copy — routing text, not behavior.
/// - `tools` is the whole point: carried verbatim *plus* the loomux grant.
const LOOMUX_OWNED_FRONTMATTER_KEYS: [&str; 3] = ["name", "description", "tools"];

/// The user's frontmatter with [`LOOMUX_OWNED_FRONTMATTER_KEYS`] removed, so a
/// generated stand-in can re-author those three and reproduce **everything
/// else** as written (#802) — `model:` above all, but equally any key Copilot
/// documents that loomux has never heard of.
///
/// Line-oriented on purpose. A key's indented continuation lines (a folded
/// scalar, a nested block like `mcp-servers:`) belong to it and are dropped or
/// kept with it; a re-serialization through a YAML parser would instead
/// normalize quoting, ordering and comments — i.e. would silently rewrite the
/// user's file while claiming to copy it.
pub fn carry_frontmatter(front: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut dropping = false;
    for line in front.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if indented || line.trim().is_empty() {
            // Continuation (or blank) — it belongs to whatever key is open.
            if !dropping {
                out.push(line);
            }
            continue;
        }
        dropping = line
            .split_once(':')
            .is_some_and(|(k, _)| LOOMUX_OWNED_FRONTMATTER_KEYS.contains(&k.trim().to_lowercase().as_str()));
        if !dropping {
            out.push(line);
        }
    }
    let kept = out.join("\n");
    let kept = kept.trim_matches('\n');
    if kept.is_empty() { String::new() } else { format!("{kept}\n") }
}

/// [`AgentProfile::mentions_mcp_server`] over a bare list. See
/// [`tools_grant_mcp_server`].
pub fn tools_mention_mcp_server(tools: Option<&[String]>, server: &str) -> bool {
    let Some(tools) = tools else { return false };
    let prefix = format!("{server}/");
    tools.iter().any(|t| {
        let t = t.trim();
        t == server || t.starts_with(&prefix)
    })
}

/// Read a frontmatter `tools:` value into a list (#802).
///
/// Three shapes reach this, because all three are valid YAML and real agent
/// files in the wild use each: a flow sequence (`tools: [read, edit]`, quoted or
/// not), a bare comma-separated scalar (`tools: read, edit`), and a block
/// sequence
///
/// ```yaml
/// tools:
///   - read
///   - edit
/// ```
///
/// which [`parse_frontmatter`] has already folded into the single value
/// `"- read - edit"` (it joins continuation lines with a space). The block form
/// is why this cannot simply split on commas: that form has none. It is detected
/// by the leading `- ` marker and split on ` - `, which no tool name can contain
/// — a hyphenated name (`rubber-duck`, `some-mcp-server/*`) has no spaces around
/// its hyphen, so it survives intact.
fn parse_tools_value(value: &str) -> Vec<String> {
    let v = value.trim();
    let v = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(v);
    let v = v.trim();
    let parts: Vec<&str> = match v.strip_prefix("- ") {
        Some(rest) => rest.split(" - ").collect(),
        None => v.split(',').collect(),
    };
    parts.iter().filter_map(|t| sanitize_tool(t)).collect()
}

/// Tool names are re-emitted by loomux into a generated agent file's YAML
/// frontmatter (`write_copilot_agent_file`), so strip anything that could break
/// out of the double-quoted scalar they land in, or out of the flow sequence
/// that holds them.
///
/// Deliberately narrower than [`sanitize_allow`]: these are tool *names* and
/// namespaced wildcards (`read`, `shell`, `loomux/*`, `azure.ext/some-tool`),
/// never the parenthesised permission patterns `allow:` carries — and the comma
/// is dropped rather than kept, because here it is the separator this function's
/// caller already split on.
fn sanitize_tool(s: &str) -> Option<String> {
    let cleaned: String = s
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '*' | '_' | '-' | '.' | '/' | ':'))
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Map a `kind:`/`role:` hint onto a capability class. `None` for a value that
/// names none — the caller reports it rather than guessing (an unknown kind is
/// never coerced to worker; see `workflow::kind_from_str`).
pub fn kind_from_hint(hint: &str) -> Option<Role> {
    crate::workflow::kind_from_str(hint)
}

/// Tool patterns land inside double quotes on a shell line
/// (`--allowedTools "Bash(git *)"`, `--allow-tool "shell(gh:*)"`); strip
/// anything that could escape them.
///
/// The comma is **kept**, and that is load-bearing: real tool patterns contain
/// them — `Bash(gh pr view --json title,body)` is the canonical example. A
/// filter that dropped it would not reject the pattern, it would silently
/// rewrite it to `--json titlebody`, which is a *different, broken* command that
/// the agent would then be pre-approved to run. A comma has no meaning inside a
/// double-quoted string in either PowerShell or POSIX sh, so it is inert here.
/// (Note the frontmatter `allow:` reader below splits on commas *before* calling
/// this, so a comma-bearing pattern must be written in `workflow.yml`'s YAML
/// list, where it is quoted and unambiguous.)
pub fn sanitize_allow(s: &str) -> Option<String> {
    let cleaned: String = s
        .trim()
        .chars()
        .filter(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '(' | ')' | ':' | '*' | '_' | '-' | '.' | ' ' | '/' | ',')
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
///
/// Deliberately NOT the real YAML parser used for `workflow.yml`: these files
/// are *Copilot's* format, and their frontmatter carries copilot-native keys
/// (`tools:`, `agents:`) whose shapes loomux neither owns nor validates. A
/// strict parse would reject perfectly good Copilot files; a lenient key/value
/// skim reads the handful of keys loomux understands and ignores the rest.
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

/// Parse a persona file. `stem` (file name without `.md`, with any trailing
/// `.agent` dropped) is the default name. Returns `None` for files without a
/// frontmatter block or with no instructions body — they're not agent
/// definitions.
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
        kind: None,
        mode: ProfileMode::Append,
        allow: Vec::new(),
        copilot_agent: None,
        tools: None,
        has_mcp_servers: false,
        // Verbatim, before any key skim — `front` is exactly what sat between
        // the fences, and the split above guarantees it contains no `---` line
        // of its own to close the block early when it is re-emitted (#802).
        frontmatter: front.trim_matches('\n').to_string(),
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
            // A declared capability class. Recorded, then CHECKED against the
            // block's kind at spawn — never applied. See the module docs.
            "kind" | "role" => p.kind = kind_from_hint(&value),
            "allow" => p.allow = value.split(',').filter_map(|a| sanitize_allow(a)).collect(),
            // append (default) | replace. Anything unrecognized stays append —
            // the safe default (an addendum can't strip the built-in contract).
            "mode" => {
                if value.trim().eq_ignore_ascii_case("replace") {
                    p.mode = ProfileMode::Replace;
                }
            }
            "copilot-agent" | "copilot_agent" => p.copilot_agent = sanitize_name(&value),
            // #802: copilot-native, and copilot applies it itself — but it is a
            // FILTER over every tool including loomux's own MCP server, so
            // loomux reads it to detect (and repair) a list that would strip
            // the delegate's ability to report. See the module docs.
            "tools" => p.tools = Some(parse_tools_value(&value)),
            // Presence only — see `has_mcp_servers`. `parse_frontmatter` folds
            // the block's indented children into this value, so an empty value
            // here still means the key was written.
            "mcp-servers" | "mcp_servers" => p.has_mcp_servers = true,
            // agents / target / … are copilot-native; copilot reads them
            // itself via --agent.
            _ => {}
        }
    }
    if p.copilot_agent.is_none() {
        p.copilot_agent = Some(p.name.clone());
    }
    (!p.instructions.is_empty()).then_some(p)
}

/// All persona definitions in a repo's `.github/agents/*.md`, sorted by name.
/// A missing directory (the common case) yields an empty list — never an error,
/// so discovery can never block a spawn.
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

/// Look up a persona by its (case-sensitive) name among the discovered files.
pub fn find_named<'a>(profiles: &'a [AgentProfile], name: &str) -> Option<&'a AgentProfile> {
    profiles.iter().find(|p| p.name == name)
}

/// Load the persona a block's `profile:` path points at.
///
/// Two things this deliberately does NOT do:
/// - it does not read outside the repo ([`resolve_profile_path`] rejects `..`
///   and absolute paths — a repo file must not be able to pull
///   `~/.ssh/config` into an agent's system prompt);
/// - it does not let the file override the block's capability class. A `kind:`
///   in the file that disagrees with the block's is an **error**, not a
///   reassignment.
///
/// [`resolve_profile_path`]: crate::workflow::resolve_profile_path
pub fn load_block_profile(
    repo: &str,
    rel: &str,
    block_kind: Role,
) -> Result<AgentProfile, String> {
    let path = crate::workflow::resolve_profile_path(repo, rel)?;
    let text = fs::read_to_string(&path)
        .map_err(|e| format!("persona file {} is unreadable: {e}", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("persona file {} has no name", path.display()))?;
    let p = parse_profile(stem, &text)
        .ok_or_else(|| format!("persona file {rel} has no frontmatter block or no body"))?;
    if let Some(declared) = p.kind {
        if declared != block_kind {
            return Err(format!(
                "persona {rel} declares kind {:?} but the block it is used by is a {:?} block — \
                 a persona file can never move a block into a different capability class",
                declared.as_str(),
                block_kind.as_str()
            ));
        }
    }
    Ok(p)
}

/// Whether a `profile:` path is a **user-authored** Copilot custom agent —
/// i.e. it lives in `.github/agents/`. Only then may a Copilot block use its
/// native `--agent <name>`: that flag resolves a *name* against
/// `.github/agents/`, so it can only ever engage a file the user already wrote.
///
/// loomux never writes generated personas into the user's `.github/agents/` to
/// make `--agent` work — that would dirty their git tree with files they didn't
/// author. A Copilot block with an inline `prompt:` instead falls back to
/// kickoff-prompt injection (`persona_inject` in `mod.rs`).
pub fn is_copilot_native(rel: &str) -> bool {
    let norm = rel.trim().replace('\\', "/");
    norm.starts_with(".github/agents/") && norm.ends_with(".md")
}

/// Whether `--agent <handle>` would load **the file at `rel`** and not some
/// other one.
///
/// This is the check that makes the native Copilot path honest. `--agent` takes
/// a *name*, and a persona's name comes from its frontmatter (`name:`), not from
/// its path — so `.github/agents/security-review.md` can perfectly well declare
/// `name: worker`. loomux would then read, kind-check and audit the
/// security-review file while Copilot went off and loaded the *worker* persona.
///
/// `true` only when exactly one discovered persona answers to `handle` and it is
/// the file the block pointed at. Ambiguity (two files, same name) also returns
/// `false`: Copilot's own resolution order between them is not something loomux
/// should be guessing at.
pub fn handle_resolves_to(repo: &str, handle: &str, rel: &str) -> bool {
    let want = Path::new(repo).join(rel.trim().replace('\\', "/"));
    let dir = Path::new(repo).join(".github").join("agents");
    let Ok(entries) = fs::read_dir(&dir) else {
        return false;
    };
    let mut hits = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .filter(|p| {
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { return false };
            fs::read_to_string(p)
                .ok()
                .and_then(|t| parse_profile(stem, &t))
                .is_some_and(|prof| {
                    prof.copilot_agent.as_deref().unwrap_or(&prof.name) == handle
                })
        });
    match (hits.next(), hits.next()) {
        (Some(only), None) => only == want,
        _ => false, // no match, or an ambiguous one
    }
}
