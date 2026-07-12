//! Roles as data: the **block model** and `<repo>/.loomux/workflow.yml` (#222).
//!
//! Until now an agent's identity *was* its [`Role`] — a closed 4-variant enum
//! that simultaneously decided the persona, the template, the model, the CLI
//! and the capabilities. That made "five reviewers, each with its own focus
//! prompt and model" impossible to express.
//!
//! A **block** splits those apart:
//!
//! - the **[`BlockId`]** (a string, e.g. `rev-security`) is the *identity*;
//! - [`Role`] survives as the block's **capability class** (`kind`) — the
//!   structural guarantees loomux enforces (deny-flags, cwd rule, MCP tool
//!   scope) still come from a **closed enum**;
//! - persona (`prompt` / `profile`), `cli` and `model` are **unbounded data**.
//!
//! So you can declare as many reviewers as you like — but every one of them is
//! a *reviewer* in the capability sense, and none of them can push to a branch.
//!
//! # The capability-closure rule (the security spine)
//!
//! **A workflow file can never grant a capability.** `kind` *selects* from the
//! closed enum; there is no `read_only: false` escape hatch, no `allow_write`,
//! no way to spell a fifth capability class. A repo file is untrusted input —
//! it is authored by whoever opened a PR against the repo — and under
//! `auto_ops` nobody approves its agents' tool calls. Everything a block can
//! influence is therefore either (a) inert text (a persona prompt), or (b) a
//! choice from a value set loomux already ships (`kind`, `cli`, `model`).
//! Every string that reaches a shell line is sanitized first ([`sanitize_id`],
//! [`sanitize_display`], `sanitize_allow`, `sanitize_model`), and a `profile:`
//! path is confined to the repo (no `..`, no absolute paths, no drive letters).
//!
//! # Failure policy
//!
//! A broken workflow file is **audited and skipped, never fatal**: the group
//! falls back to [`default_roster`] — today's fixed 4-block roster — and every
//! agent still spawns. The one thing that is *not* silently tolerated is an
//! unknown `kind`: coercing it to `worker` would hand an unrecognized block
//! write access, so it is a hard validation error that drops the file. (The
//! pre-#222 code did exactly that coercion in two places; both are gone.)
//!
//! # Schema
//!
//! ```yaml
//! version: 1
//! name: focused-review
//!
//! blocks:
//!   - id: worker            # IMMUTABLE identity. edges/gates reference THIS.
//!     name: Worker          # display only — renaming never breaks a reference
//!     kind: worker          # capability class (closed enum)
//!     cli: copilot
//!     model: auto
//!     profile: .github/agents/worker.md   # -> copilot --agent worker (NATIVE)
//!
//!   - id: rev-security
//!     name: Security review
//!     kind: reviewer
//!     cli: claude
//!     model: opus
//!     prompt: |            # -> claude --agents '{...}' --agent rev-security
//!       Review ONLY for security defects: injection, authz, secrets.
//!
//! edges:                   # ADVISORY: the declared happy path. The
//!   - { from: worker, to: [rev-security] }   # orchestrator still schedules.
//!
//! gates:                   # DECLARED here; ENFORCED by the gh shim (sub-PR 3).
//!   merge:
//!     require: all-pass    # or: threshold: 2
//!     reviewers: [rev-security]
//! ```
//!
//! `id` is immutable and human-meaningful and `name` is display-only on
//! purpose: n8n keys its graph by *display name*, so renaming a node silently
//! breaks every reference to it. Layout/coordinates live in a separate
//! `.loomux/workflow.layout.json` (the GUI pane's file, sub-PR 2) so a canvas
//! nudge never churns the semantic diff.

use super::{default_model, Role, SUPPORTED_CLIS};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// A block's identity — immutable, human-meaningful, referenced by edges/gates.
pub type BlockId = String;

/// Where in the repo a workflow lives. Committed and shareable: a repo's
/// workflow is a property of the *project*, not of one developer's machine
/// (the #51 requirement).
pub const WORKFLOW_PATH: &str = ".loomux/workflow.yml";

/// Schema version this build understands. Recorded in the file so a future
/// breaking change can be detected rather than mis-parsed.
pub const SCHEMA_VERSION: u32 = 1;

/// The block ids of the built-in roster. These four keep their historic
/// instruction-file names (`worker.md`, …), which is what makes a no-workflow
/// group byte-for-byte identical to pre-#222 loomux.
pub const BUILTIN_IDS: [&str; 4] = ["orchestrator", "worker", "reviewer", "planner"];

// ── the block ───────────────────────────────────────────────────────────────

/// One agent block: an identity, a capability class, and a persona.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Immutable identity (sanitized `[A-Za-z0-9_-]`). Edges and gates
    /// reference this, never `name`.
    pub id: BlockId,
    /// Display name for the pane/roster. Cosmetic — never a reference target.
    pub name: String,
    /// Capability class: the closed enum. A workflow file *selects* one; it can
    /// never define one. This is where every structural guarantee comes from.
    pub kind: Role,
    /// Agent CLI for this block. Empty = inherit the group default `agent_cli`.
    pub cli: String,
    /// Model for this block. Empty = the kind's default for the resolved CLI.
    pub model: String,
    /// Inline persona (the `prompt:` key). Compiled to `claude --agents` JSON,
    /// or injected into the kickoff prompt on CLIs with no inline flag.
    pub prompt: Option<String>,
    /// Repo-relative path to a persona file (the `profile:` key), e.g.
    /// `.github/agents/worker.md`. A `.github/agents/*.md` file is what lets a
    /// Copilot block use its **native** `--agent <name>`.
    pub profile: Option<String>,
    /// Extra pre-approved tool patterns (`--allowedTools` / `--allow-tool`).
    /// Sanitized; may never re-grant what the capability class denies (deny
    /// rules beat allow rules on both CLIs).
    pub allow: Vec<String>,
}

impl Block {
    /// Agent-id prefix (`w-3`, `rev-4`). Moved off `Role` onto the block, but
    /// deliberately still *derived from* the capability class: agent ids are
    /// short, are parsed by the roster/badge conventions, and must stay
    /// byte-identical for the built-in roster. Block identity rides in
    /// [`AgentEntry::block`](super::AgentEntry) and the pane name instead.
    pub fn prefix(&self) -> &'static str {
        self.kind.prefix()
    }

    /// The file in the group dir that carries this block's loomux role
    /// contract, referenced by the kickoff prompt. The built-in blocks keep
    /// their historic names (`worker.md`, …) so a default group's kickoff text
    /// is unchanged; a custom block gets `<id>.md`.
    pub fn instructions_file(&self) -> String {
        if BUILTIN_IDS.contains(&self.id.as_str()) {
            self.kind.instructions_file().to_string()
        } else {
            format!("{}.md", self.id)
        }
    }

    /// Whether this block is one of the four built-in roster entries.
    pub fn is_builtin(&self) -> bool {
        BUILTIN_IDS.contains(&self.id.as_str())
    }

    /// A block with no persona behaves exactly like a pre-#222 role: nothing to
    /// compile into `--agents` / `--agent`, nothing to inject into the kickoff.
    pub fn has_persona(&self) -> bool {
        self.prompt.is_some() || self.profile.is_some()
    }
}

/// An advisory edge: the *declared happy path*, drawn by the GUI and offered to
/// the orchestrator as context. loomux does **not** execute it — the
/// orchestrator keeps its scheduling judgment (mergeability, parallel vs
/// serial, plan-first vs straight-to-worker), which a static DAG cannot make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: BlockId,
    pub to: Vec<BlockId>,
}

/// How many of a gate's reviewers must pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateRequire {
    /// Every named reviewer must have recorded a PASS.
    AllPass,
    /// At least N of the named reviewers must have recorded a PASS.
    Threshold(u32),
}

/// A declared gate (today: only `merge`). **Parsed and validated here; enforced
/// in the `gh` shim** — that work (plus the `review_verdict` tool that records
/// the reviewer-attributed state a gate keys off) is sub-PR 3 of #222. Parsing
/// it now means the file format is settled and a workflow authored today keeps
/// working when the enforcement lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate {
    pub require: GateRequire,
    /// Block ids of the reviewers whose verdicts the gate reads. Validated to
    /// exist and to be `kind: reviewer` — a gate naming a worker would be
    /// unsatisfiable.
    pub reviewers: Vec<BlockId>,
    /// Extra named conditions (e.g. `ci-green`). Opaque to this parser.
    pub also: Vec<String>,
}

/// A parsed, validated workflow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub version: u32,
    pub name: String,
    /// The loomux version that last authored the file (the optional
    /// `authored_with:` key — the workflow pane in #223 writes it). Purely
    /// informational: it is **never** a validation error, whatever it says, and
    /// an old or unrecognized value must not stop a file from loading. Kept on
    /// the parsed workflow so nothing round-trips it away. (Langflow's
    /// `last_tested_version` is the same idea.)
    pub authored_with: String,
    pub blocks: Vec<Block>,
    pub edges: Vec<Edge>,
    pub gates: BTreeMap<String, Gate>,
}

impl Workflow {
    pub fn block(&self, id: &str) -> Option<&Block> {
        self.blocks.iter().find(|b| b.id == id)
    }
}

// ── the built-in roster ─────────────────────────────────────────────────────

/// Today's fixed 4-block roster, synthesized from the launcher's per-role CLI
/// and model picks (#222). This is what a group gets when the repo has no
/// `.loomux/workflow.yml` — and it is deliberately *exactly* the pre-block
/// behavior: the ids are the four role names, so the instruction files keep
/// their historic paths; no block carries a persona, so nothing is added to any
/// command line. `default_roster_command_lines_match_legacy` pins that.
///
/// `pins` is `(kind, cli, model)` per role; an empty `cli`/`model` inherits the
/// group default / the kind's default model, exactly as the flat per-role
/// guardrail fields did.
pub fn default_roster(pins: &[(Role, &str, &str)]) -> Vec<Block> {
    pins.iter()
        .map(|(kind, cli, model)| Block {
            id: kind.as_str().to_string(),
            name: kind.as_str().to_string(),
            kind: *kind,
            cli: cli.trim().to_string(),
            model: model.trim().to_string(),
            prompt: None,
            profile: None,
            allow: Vec::new(),
        })
        .collect()
}

/// The built-in roster with every block on `agent_cli` and its default model —
/// the roster a group gets from a launcher that pinned nothing per role.
pub fn builtin_roster(agent_cli: &str) -> Vec<Block> {
    default_roster(&[
        (Role::Orchestrator, agent_cli, ""),
        (Role::Worker, agent_cli, ""),
        (Role::Reviewer, agent_cli, ""),
        (Role::Planner, agent_cli, ""),
    ])
}

// ── sanitizers ──────────────────────────────────────────────────────────────

/// Longest block id. It becomes a file name (`<id>.md`) and an agent-id suffix,
/// and nothing legible needs more.
pub const MAX_ID_CHARS: usize = 48;

/// Block ids reach the shell (a `--agent <id>` flag, a `--agents` JSON key) and
/// the filesystem (`<id>.md` in the group dir). Keep them to a conservative
/// identifier alphabet so neither surface can be escaped — the `sanitize_model`
/// precedent, applied to identity. Returns `None` for an id with no usable
/// characters left.
///
/// The *parser* rejects an id this would have changed rather than accepting the
/// rewrite (see `parse_workflow`); this is the last-resort filter for ids that
/// arrive from somewhere other than a validated file — a hand-edited group.json.
pub fn sanitize_id(s: &str) -> Option<String> {
    let cleaned: String = s
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(MAX_ID_CHARS)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Display names are cosmetic (pane title, roster row) and are rendered via
/// `textContent`, never HTML — so this is hygiene, not a boundary: drop control
/// characters (a pasted name must not smuggle escape codes into a pane title)
/// and cap the length. Mirrors `sanitize_agent_name`.
pub fn sanitize_display(s: &str) -> String {
    s.trim().chars().filter(|c| !c.is_control()).take(40).collect()
}

/// Persona text ends up inside a **single-quoted** shell token (the `--agents`
/// JSON payload). In both PowerShell and POSIX sh, a single-quoted string is
/// fully literal *except* for the quote character itself — so `'` is the one
/// character that could break out, and it is the only one we have to remove.
/// Mapping it to the typographic apostrophe (U+2019) keeps the prose intact
/// ("don't" stays readable) while making the payload structurally inert; the
/// JSON is then ASCII-escaped ([`ascii_escape_json`]) so the command line stays
/// pure ASCII regardless of the pane's code page.
///
/// Control characters other than newline/tab are dropped: they have no meaning
/// in a persona and would ride straight into a terminal.
pub fn sanitize_persona(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\'' { '\u{2019}' } else { c })
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Escape every non-ASCII character in an already-serialized JSON string as
/// `\uXXXX`. JSON says that is equivalent; the point is that the resulting
/// payload is pure ASCII, so it survives a Windows pane whose code page is not
/// UTF-8. (Used on the `claude --agents` payload, which is the only place
/// loomux puts free text on a command line.)
pub fn ascii_escape_json(json: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(json.len());
    let mut buf = [0u16; 2];
    for c in json.chars() {
        if c.is_ascii() {
            out.push(c);
            continue;
        }
        // Astral-plane chars (emoji in a persona) need both surrogates.
        for unit in c.encode_utf16(&mut buf) {
            let _ = write!(out, "\\u{unit:04x}");
        }
    }
    out
}

/// Confine a `profile:` path to the repo. A workflow file is repo-authored
/// input and its `profile:` names a file loomux **reads and injects into an
/// agent's system prompt** — so an absolute path or a `..` escape would let a
/// repo pull any file on the operator's disk into an agent's context. Rejects
/// absolute paths, drive prefixes, and any parent-dir component; returns the
/// repo-joined path only when it stays inside the repo.
pub fn resolve_profile_path(repo: &str, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err("profile path is empty".into());
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(format!("profile {rel:?} must be a repo-relative path, not absolute"));
    }
    for c in p.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("profile {rel:?} must stay inside the repo (no '..')"))
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(format!("profile {rel:?} must be a repo-relative path"))
            }
        }
    }
    Ok(Path::new(repo).join(p))
}

// ── the YAML wire format ────────────────────────────────────────────────────
//
// Deserialized into `Raw*` mirrors first, then validated into the domain types
// above. Two reasons for the split: `kind` must produce a *readable* error
// rather than serde's "unknown variant" prose, and `deny_unknown_fields` needs
// to sit on the wire types so a typo (`promt:`) is caught instead of ignored —
// the failure mode every surveyed workflow tool has (Dify will happily publish
// a workflow whose plugin node isn't installed).

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkflow {
    version: u32,
    #[serde(default)]
    name: String,
    /// The loomux version that authored the file. Optional, informational, and
    /// **never** a validation error — see [`Workflow::authored_with`]. Declared
    /// here (rather than left to `deny_unknown_fields`) precisely so that a file
    /// written by the workflow pane still loads.
    #[serde(default)]
    authored_with: String,
    #[serde(default)]
    blocks: Vec<RawBlock>,
    #[serde(default)]
    edges: Vec<RawEdge>,
    #[serde(default)]
    gates: BTreeMap<String, RawGate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBlock {
    id: String,
    #[serde(default)]
    name: String,
    kind: String,
    #[serde(default)]
    cli: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    allow: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEdge {
    from: String,
    to: OneOrMany,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGate {
    #[serde(default)]
    require: Option<String>,
    #[serde(default)]
    threshold: Option<u32>,
    #[serde(default)]
    reviewers: Vec<String>,
    #[serde(default)]
    also: Vec<String>,
}

/// `to: worker` and `to: [rev-a, rev-b]` are both legal — a fan-out reads
/// naturally as a list and a single hand-off reads naturally as a scalar.
#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

/// Map a `kind:` string onto a capability class. **`None` for anything
/// unrecognized** — the caller turns that into a hard error. Coercing an
/// unknown kind to `worker` (which is what loomux did before #222, in two
/// places) silently hands an unrecognized block a worktree and write access.
pub fn kind_from_str(s: &str) -> Option<Role> {
    match s.trim().to_ascii_lowercase().as_str() {
        "orchestrator" => Some(Role::Orchestrator),
        "worker" => Some(Role::Worker),
        "reviewer" => Some(Role::Reviewer),
        "planner" => Some(Role::Planner),
        _ => None,
    }
}

/// The kinds a workflow file may name, for error messages.
pub fn kind_names() -> String {
    "orchestrator, worker, reviewer, planner".to_string()
}

// ── parse + validate ────────────────────────────────────────────────────────

/// Parse and validate a workflow document. Returns **every** problem found, not
/// just the first: the whole point of a pre-run validation pass is that the
/// human fixes their file in one pass rather than playing whack-a-mole at spawn
/// time (which is where Flowise, Langflow and Dify all leave you).
pub fn parse_workflow(text: &str) -> Result<Workflow, Vec<String>> {
    let raw: RawWorkflow = serde_norway::from_str(text).map_err(|e| vec![e.to_string()])?;
    let mut errs: Vec<String> = Vec::new();

    if raw.version != SCHEMA_VERSION {
        errs.push(format!(
            "version {} is not supported (this build understands version {SCHEMA_VERSION})",
            raw.version
        ));
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, rb) in raw.blocks.iter().enumerate() {
        // An id is REJECTED rather than quietly rewritten: an author who wrote
        // `rev security` must not end up with a block called `revsecurity` that
        // their own edges and gates can no longer reference.
        if rb.id.trim().chars().count() > MAX_ID_CHARS {
            errs.push(format!(
                "blocks[{i}]: id {:?} is longer than {MAX_ID_CHARS} characters",
                rb.id
            ));
            continue;
        }
        let Some(id) = sanitize_id(&rb.id) else {
            errs.push(format!("blocks[{i}]: id {:?} has no usable characters (allowed: letters, digits, '-', '_')", rb.id));
            continue;
        };
        if id != rb.id.trim() {
            errs.push(format!(
                "blocks[{i}]: id {:?} contains characters that are not allowed (letters, digits, '-', '_')",
                rb.id
            ));
            continue;
        }
        if !seen.insert(id.clone()) {
            errs.push(format!("blocks[{i}]: duplicate block id {id:?}"));
            continue;
        }
        // The capability class. An unknown kind is REJECTED, never coerced —
        // see `kind_from_str`.
        let Some(kind) = kind_from_str(&rb.kind) else {
            errs.push(format!(
                "blocks[{i}] ({id}): unknown kind {:?} — must be one of {}",
                rb.kind,
                kind_names()
            ));
            continue;
        };
        // The four class names are RESERVED as ids for their own class. Without
        // this, `- id: planner, kind: reviewer` is accepted and then two blocks
        // collide: `instructions_file()` keys "is this a built-in?" off the id but
        // names the file from the kind, so that block would write `reviewer.md` —
        // the real reviewer block's contract file — and whichever spawned last
        // would win. (`- id: orchestrator, kind: worker` breaks a second way: the
        // roster has no orchestrator *kind*, so `clamped()` synthesizes one with
        // the id `orchestrator`, and the duplicate id makes the repo's own block
        // permanently unreachable.) Coupling the two removes the whole class of
        // problem, and costs an author nothing: rename the block.
        if let Some(reserved) = kind_from_str(&id) {
            if reserved != kind {
                errs.push(format!(
                    "blocks[{i}]: id {id:?} is reserved for {} blocks — a block with kind {:?} needs a different id",
                    reserved.as_str(),
                    kind.as_str()
                ));
                continue;
            }
        }
        let cli = rb.cli.trim().to_string();
        if !cli.is_empty() && !SUPPORTED_CLIS.contains(&cli.as_str()) {
            errs.push(format!(
                "blocks[{i}] ({id}): unknown cli {cli:?} — supported: {}",
                SUPPORTED_CLIS.join(", ")
            ));
            continue;
        }
        if rb.prompt.is_some() && rb.profile.is_some() {
            errs.push(format!(
                "blocks[{i}] ({id}): set either prompt: (inline persona) or profile: (a persona file), not both"
            ));
            continue;
        }
        if let Some(path) = rb.profile.as_deref() {
            // Validate the shape now; the file is read (and its absence
            // tolerated) at spawn, so a workflow stays usable on a checkout
            // where the persona file hasn't landed yet.
            if let Err(e) = resolve_profile_path(".", path) {
                errs.push(format!("blocks[{i}] ({id}): {e}"));
                continue;
            }
        }
        // CAPABILITY CLOSURE. `allow:` pre-approves tool patterns, and the
        // read-only class is read-only by *denial of a fixed list* — Edit, Write,
        // MultiEdit, NotebookEdit, `git commit`, `git push`. Deny beats allow on
        // both CLIs, so an allow pattern cannot re-grant anything on that list…
        // but it does not have to. `allow: Bash(python *)` (or `cp`, `tee`,
        // `sed -i`, …) hands a planner a shell that writes files and is named
        // nowhere in the deny list, and under `auto_ops` nobody approves the call.
        //
        // Enumerating every write-capable program is not a thing anyone can do.
        // So the rule is the other way round: **a read-only block may not declare
        // `allow:` at all.** That keeps "a workflow file can never grant a
        // capability" a statement about the code rather than about the deny list's
        // completeness. (Worker and reviewer already hold the write/shell surface
        // structurally, so `allow:` widens nothing for them.)
        if !rb.allow.is_empty() && kind.is_read_only() {
            errs.push(format!(
                "blocks[{i}] ({id}): a {} block cannot declare allow: — its class is read-only, \
                 and a pre-approved tool pattern could hand it a shell that writes files. \
                 Move the work to a worker block.",
                kind.as_str()
            ));
            continue;
        }
        let name = sanitize_display(&rb.name);
        blocks.push(Block {
            name: if name.is_empty() { id.clone() } else { name },
            id,
            kind,
            cli,
            model: super::sanitize_model_opt(&rb.model),
            prompt: rb.prompt.as_deref().map(sanitize_persona).filter(|s| !s.trim().is_empty()),
            profile: rb.profile.as_ref().map(|p| p.trim().to_string()),
            allow: rb.allow.iter().filter_map(|a| super::profiles::sanitize_allow(a)).collect(),
        });
    }

    if blocks.is_empty() && errs.is_empty() {
        errs.push("no blocks declared — a workflow needs at least one block".into());
    }

    let known: BTreeSet<&str> = blocks.iter().map(|b| b.id.as_str()).collect();

    let mut edges: Vec<Edge> = Vec::new();
    for (i, re) in raw.edges.into_iter().enumerate() {
        let from = re.from.trim().to_string();
        let to = re.to.into_vec();
        if !known.contains(from.as_str()) {
            errs.push(format!("edges[{i}]: 'from' names no block: {from:?}"));
            continue;
        }
        let mut bad = false;
        for t in &to {
            if !known.contains(t.trim()) {
                errs.push(format!("edges[{i}]: 'to' names no block: {:?}", t.trim()));
                bad = true;
            }
        }
        if bad {
            continue;
        }
        edges.push(Edge { from, to: to.iter().map(|t| t.trim().to_string()).collect() });
    }

    let mut gates: BTreeMap<String, Gate> = BTreeMap::new();
    for (name, rg) in raw.gates {
        let require = match (rg.require.as_deref().map(str::trim), rg.threshold) {
            // `threshold: N` alone implies a threshold gate; spelling `require:
            // threshold` as well is allowed but redundant.
            (Some("threshold") | None, Some(n)) if n > 0 => GateRequire::Threshold(n),
            (Some("threshold") | None, Some(_)) => {
                errs.push(format!("gates.{name}: threshold must be a positive number"));
                continue;
            }
            (Some("threshold"), None) => {
                errs.push(format!(
                    "gates.{name}: require: threshold needs a threshold: N to go with it"
                ));
                continue;
            }
            (Some("all-pass") | Some("all") | None, None) => GateRequire::AllPass,
            (Some("all-pass") | Some("all"), Some(_)) => {
                errs.push(format!(
                    "gates.{name}: require: all-pass takes no threshold — drop it, or use require: threshold"
                ));
                continue;
            }
            (Some(other), _) => {
                errs.push(format!(
                    "gates.{name}: unknown require {other:?} — use 'all-pass', or 'threshold' with threshold: N"
                ));
                continue;
            }
        };
        let mut bad = false;
        for r in &rg.reviewers {
            match blocks.iter().find(|b| b.id == r.trim()) {
                None => {
                    errs.push(format!("gates.{name}: reviewer {:?} names no block", r.trim()));
                    bad = true;
                }
                // A gate reads reviewer verdicts. Naming a worker would make it
                // permanently unsatisfiable — nothing would ever record a
                // verdict for it — which is the "dangling reference the UI
                // happily saves" failure this validation pass exists to prevent.
                Some(b) if b.kind != Role::Reviewer => {
                    errs.push(format!(
                        "gates.{name}: reviewer {:?} is a {} block, not a reviewer — a gate can only require reviewer verdicts",
                        b.id,
                        b.kind.as_str()
                    ));
                    bad = true;
                }
                Some(_) => {}
            }
        }
        if rg.reviewers.is_empty() {
            errs.push(format!("gates.{name}: no reviewers — a gate with no reviewers gates nothing"));
            bad = true;
        }
        if let GateRequire::Threshold(n) = require {
            if n as usize > rg.reviewers.len() {
                errs.push(format!(
                    "gates.{name}: threshold {n} exceeds the {} reviewer(s) named — it could never pass",
                    rg.reviewers.len()
                ));
                bad = true;
            }
        }
        if bad {
            continue;
        }
        gates.insert(
            name,
            Gate {
                require,
                reviewers: rg.reviewers.iter().map(|r| r.trim().to_string()).collect(),
                also: rg.also,
            },
        );
    }

    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(Workflow {
        version: raw.version,
        name: sanitize_display(&raw.name),
        authored_with: sanitize_display(&raw.authored_with),
        blocks,
        edges,
        gates,
    })
}

/// Read + validate `<repo>/.loomux/workflow.yml`.
///
/// - `Ok(None)` — no file (the common case): the caller synthesizes
///   [`default_roster`] and behaves exactly like pre-#222 loomux.
/// - `Err(errors)` — the file exists but is broken. The caller **audits and
///   skips it**, falling back to the default roster. A workflow file must never
///   be able to block a spawn.
pub fn load_workflow(repo: &str) -> Result<Option<Workflow>, Vec<String>> {
    let path = Path::new(repo).join(WORKFLOW_PATH);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| vec![format!("{} is unreadable: {e}", path.display())])?;
    parse_workflow(&text).map(Some)
}

/// The model a block runs, resolving the empty ("inherit") case: the block's
/// own `model:`, else the kind's default for its effective CLI.
pub fn model_of<'a>(block: &'a Block, agent_cli: &'a str) -> &'a str {
    if block.model.trim().is_empty() {
        default_model(cli_of(block, agent_cli), block.kind)
    } else {
        &block.model
    }
}

/// The CLI a block runs: its own `cli:`, else the group default `agent_cli`.
pub fn cli_of<'a>(block: &'a Block, agent_cli: &'a str) -> &'a str {
    if block.cli.trim().is_empty() {
        agent_cli
    } else {
        &block.cli
    }
}
