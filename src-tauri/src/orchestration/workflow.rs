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
//! So you can declare as many reviewers as you like — but every one of them is a
//! *reviewer* in the capability sense, and a repo file cannot make one anything
//! else.
//!
//! Be precise about what "the capability sense" buys, because the enum enforces
//! less than the word suggests: a **planner** is structurally read-only — its
//! file-editing tools and `git commit`/`git push` are denied at the CLI level, so
//! `is_read_only()` is a real, mechanical guarantee. A **reviewer**'s "never
//! pushes" is *instruction-backed*, exactly as it was before #222: it holds the
//! same write surface a worker does and is merely told not to use it. What the
//! closed enum guarantees is that a repo file cannot *change* which of those two
//! postures a block gets — not that every non-worker posture is a sandbox. (See
//! `doc/design/orchestration.md` on structural vs instruction-backed enforcement;
//! the capability table in `doc/design/workflows.md` is the honest summary.)
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
use serde::{Deserialize, Serialize};
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
/// in the `gh` shim** — see [`evaluate_merge_gate`] for the decision and
/// [`gate_file_text`] for the spec file the shim reads. The reviewer-attributed
/// state it keys off is written by the `review_verdict` MCP tool ([`Verdict`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate {
    pub require: GateRequire,
    /// Block ids of the reviewers whose verdicts the gate reads. Validated to
    /// exist and to be `kind: reviewer` — a gate naming a worker would be
    /// unsatisfiable.
    pub reviewers: Vec<BlockId>,
    /// Extra named conditions (e.g. `ci-green`). Sanitized at parse
    /// ([`sanitize_condition`]); a condition this build cannot check **fails
    /// closed** in the shim rather than silently passing — see
    /// [`KNOWN_CONDITIONS`].
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

/// A gate condition name (`ci-green`). Sub-PR 3 enforces gates inside the `gh`
/// PATH shim — a shell script — so these follow the same conservative alphabet
/// as a block id, with `.` allowed (CI check names carry it). Returns `None` for
/// a name with no usable characters; `parse_workflow` *rejects* anything this
/// would have changed rather than accepting the rewrite.
pub fn sanitize_condition(s: &str) -> Option<String> {
    let cleaned: String = s
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(MAX_ID_CHARS)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Display names are cosmetic (pane title, roster row) and are rendered via
/// `textContent`, never HTML — so this is hygiene, not a boundary: drop control
/// characters (a pasted name must not smuggle escape codes into a pane title)
/// and cap the length. Mirrors `sanitize_agent_name`.
pub fn sanitize_display(s: &str) -> String {
    // Braces go too (rev-11 F3). A display string is repo-authored text that gets
    // substituted INTO a `{{KEY}}` template — the block note, the orchestrator's
    // roster rows — and `render_template` is a dumb ordered replace with no idea
    // which text is template and which is data. Substitution order alone is not
    // enough to make that safe: it protects a name against the passes that come
    // *after* it, not against a template whose own later keys it can name. Nobody
    // needs a brace in a pane title, so the character never gets that far.
    s.trim()
        .chars()
        .filter(|c| !c.is_control() && *c != '{' && *c != '}')
        .take(40)
        .collect()
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

/// Confine a `profile:` path to the repo. A workflow file is repo-authored input
/// and its `profile:` names a file loomux **reads and injects into an agent's
/// system prompt** — so an absolute path or a `..` escape would let a repo pull
/// any file on the operator's disk into an agent's context.
///
/// **The rules are the same on every platform, deliberately.** A workflow file is
/// committed and shared between developers (the #51 requirement), so a `profile:`
/// that is an escape on Windows and an innocent relative path on Linux is exactly
/// the divergence to kill: `std::path` would happily read `C:/Windows/win.ini` as
/// a *relative* path called `C:` on Unix, and `\\server\share\x` as a filename.
/// Both are rejected everywhere. The `Component` walk below is then belt and
/// braces on the platform that does understand them.
pub fn resolve_profile_path(repo: &str, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err("profile path is empty".into());
    }
    // Platform-independent rejections, done on the STRING before `std::path` gets
    // a chance to interpret it differently per OS.
    let norm = rel.replace('\\', "/");
    if norm.starts_with('/') {
        return Err(format!("profile {rel:?} must be a repo-relative path, not absolute"));
    }
    if norm.chars().nth(1) == Some(':') && norm.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    {
        return Err(format!("profile {rel:?} must be a repo-relative path (no drive letter)"));
    }
    if norm.split('/').any(|seg| seg == "..") {
        return Err(format!("profile {rel:?} must stay inside the repo (no '..')"));
    }
    let p = Path::new(&norm);
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
    // Join the FORWARD-SLASH form: Windows accepts it, and it means a file
    // written `.github\agents\x.md` by a Windows author still resolves for a
    // colleague on Linux, where a backslash is an ordinary filename character.
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
        // THE ORCHESTRATOR BLOCK IS LOOMUX-OWNED. A repo may pin its `cli` and
        // `model` (sanitized like everywhere else) — but it may not author its
        // persona or pre-approve its tools.
        //
        // This is not a capability question: the orchestrator already holds every
        // tool, so a repo-authored prompt grants it nothing *new*. It is a TRUST
        // question. The orchestrator is the group's trust root — it runs
        // unsupervised under `auto_ops`, in the repo root with no worktree,
        // holding the privileged MCP surface (`spawn_agent`, `kill_agent`,
        // `set_state`). Letting `.loomux/workflow.yml` write its system prompt
        // would hand a cloned repo a direct prompt-injection seam into that root
        // (the #189 class) — and it would be the one orchestrator path with no
        // gate, in a feature whose entire security argument is that a repo file
        // never reconfigures trust. The rest of the model spends real effort
        // making a *second* orchestrator impossible; leaving the *first* one's
        // persona repo-writable would make that effort decorative.
        //
        // The declared feature ("five reviewers, five prompts") needs none of
        // this. If app-level orchestrator customization is ever wanted, it can
        // arrive as an explicit human opt-in — which is a different thing from a
        // file that arrives with a `git clone`.
        if kind == Role::Orchestrator {
            let offenders: Vec<&str> = [
                rb.prompt.is_some().then_some("prompt:"),
                rb.profile.is_some().then_some("profile:"),
                (!rb.allow.is_empty()).then_some("allow:"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !offenders.is_empty() {
                errs.push(format!(
                    "blocks[{i}] ({id}): an orchestrator block may not declare {} — the orchestrator \
                     is loomux's trust root and a repo file may not author its prompt or pre-approve \
                     its tools. Pin its cli:/model: if you need to; put personas on the blocks it spawns.",
                    offenders.join(" / ")
                ));
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
        // A gate's reviewer list is a set, not a sequence: `evaluate_merge_gate`
        // (below) walks it once per verdict lookup, so a name listed twice would
        // let that reviewer's single PASS count twice toward a `threshold: N`
        // gate — a gate-integrity gap, not a cosmetic one — and `gate_need`
        // would inflate the derived minimum the same way block-id duplicates
        // would. Rejected here, consistent with how a duplicate block id is
        // handled above, rather than silently deduped: a repo author who wrote
        // the same name twice most likely meant a different one, and silently
        // dropping the duplicate would hide that typo instead of surfacing it.
        let mut seen_reviewers: BTreeSet<String> = BTreeSet::new();
        for r in &rg.reviewers {
            let rname = r.trim();
            if !seen_reviewers.insert(rname.to_string()) {
                errs.push(format!(
                    "gates.{name}: reviewer {rname:?} is named more than once — name each reviewer once"
                ));
                bad = true;
                continue;
            }
            match blocks.iter().find(|b| b.id == rname) {
                None => {
                    errs.push(format!("gates.{name}: reviewer {rname:?} names no block"));
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
        // `also:` names extra gate conditions (`ci-green`, …). Sanitized HERE,
        // at the parse boundary, even though nothing consumes it yet: gate
        // enforcement lands in sub-PR 3, in the `gh` shim, and a shim is a shell
        // script. Whatever `parse_workflow` returns will be read there as already
        // clean — that is the contract every other field in this file already
        // honors, and the one moment to establish it is before a consumer exists
        // to assume it. Rejected, not rewritten: an author must be able to
        // reference the condition they actually wrote.
        let mut also: Vec<String> = Vec::new();
        for c in &rg.also {
            match sanitize_condition(c) {
                Some(clean) if clean == c.trim() => also.push(clean),
                _ => {
                    errs.push(format!(
                        "gates.{name}: condition {c:?} is not a usable name (letters, digits, '-', '_', '.')"
                    ));
                    bad = true;
                }
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
                also,
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

/// Whether the repo declares a workflow at all, asked without parsing it.
///
/// Used where the *existence* of the file is the whole question: `create_group`
/// audits that it deliberately ignored one (the advanced-orchestrator toggle is
/// off, #222), and the launcher's preview distinguishes "this repo has no
/// workflow" from "it has one and it is broken".
pub fn workflow_file_exists(repo: &str) -> bool {
    Path::new(repo).join(WORKFLOW_PATH).is_file()
}

/// Whether a block may carry a persona at all.
///
/// The orchestrator block is loomux-owned: a repo may pin its `cli`/`model`, never
/// author its persona or pre-approve its tools. `parse_workflow` rejects that
/// outright, and [`OrchRegistry::resolve_persona`](super::OrchRegistry::resolve_persona)
/// drops one that arrives from a hand-edited `group.json` — so the *only* honest
/// answer about an orchestrator block's persona is "there isn't one".
///
/// Anything that merely *reports* on a block therefore has to ask this too, or it
/// advertises a persona the spawn will deny (rev-11's preview nit). One predicate,
/// so the report and the spawn cannot disagree.
pub fn persona_allowed(block: &Block) -> bool {
    block.kind != Role::Orchestrator
}

/// Whether a roster carries anything a workflow file put there — a block outside
/// the built-in four, or a built-in one given a persona.
///
/// False for the synthesized default roster, and that is the point: it is the
/// single condition guarding every piece of workflow-aware text loomux emits (the
/// orchestrator's roster note, the workflow section of its instructions, a
/// delegate's block note). A group with no workflow reads exactly as it did
/// before blocks existed because this returns false and all of it collapses to
/// the empty string.
pub fn roster_is_custom(blocks: &[Block]) -> bool {
    blocks.iter().any(|b| !b.is_builtin() || b.has_persona())
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

// ── verdicts: the state a gate reads (#222 / #197) ──────────────────────────
//
// Before this, a review outcome was a *notification*: `report("done", "approved
// — looks good")`, untyped text typed into the orchestrator's pane. That is
// exactly how PR #151 merged on the first "approve" that arrived while a second,
// dedicated review was still running — and that second review was the one that
// found a real release-gate bypass (#196). #197 asks for the outcome to be
// **state**: durable, attributed to the reviewer that recorded it, and readable
// by something that can refuse a merge.

/// A recorded review outcome. **Deliberately not a boolean.** Dify's Human Input
/// node and Windmill's `resume[...]` both give each decision its own outgoing
/// edge and keep the approver's typed input readable downstream; the investigation
/// (§2d) says to model ours the same way. So a reviewer can say "this needs a
/// human", which is neither an approval nor a defect report — and the gate can
/// treat it as the blocker it is instead of forcing it into a pass/fail bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Reviewed; no blocking findings. The only verdict that satisfies a gate.
    Pass,
    /// Reviewed; blocking findings. Refuses the merge.
    Fail,
    /// Not a defect call — the reviewer is handing the decision to a human
    /// (out of its depth, an ambiguous requirement, a risk it won't sign off on).
    /// Refuses the merge, exactly like `fail`: a gate must never be satisfiable
    /// by a reviewer that declined to decide.
    Escalate,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Escalate => "escalate",
        }
    }

    /// Parse a verdict word. `None` for anything unrecognized — never coerced,
    /// and never defaulted to `pass`: a verdict loomux cannot read must not be
    /// able to open a gate.
    ///
    /// **Lowercase-strict, and that is a decision, not an oversight.** This is one
    /// half of a gate; the other half is the shim's `case "$v" in pass)`, which is
    /// a shell `case` and is case-sensitive. If this half lowercased, a
    /// hand-edited `PASS` in a verdict file would read as *satisfied* to the
    /// orchestrator (`list_verdicts`, `gate_status_line`) while the shim refused
    /// the merge — two halves of the same gate disagreeing about what a verdict
    /// *is*. One token definition, both sides, and the odd casing fails closed on
    /// both. Whitespace is trimmed because a trailing newline is file format, not
    /// content.
    pub fn parse(s: &str) -> Option<Verdict> {
        match s.trim() {
            "pass" => Some(Verdict::Pass),
            "fail" => Some(Verdict::Fail),
            "escalate" => Some(Verdict::Escalate),
            _ => None,
        }
    }

    /// Whether this verdict refuses a merge on its own. `fail` and `escalate`
    /// both do: **blockers beat approvals** (#197 Scope A.3) — with more than one
    /// reviewer, a disagreement resolves to "do not merge", and first-to-approve
    /// never wins.
    pub fn is_blocking(self) -> bool {
        !matches!(self, Verdict::Pass)
    }
}

/// The verdict words a reviewer may record, for error messages.
pub fn verdict_names() -> String {
    "pass, fail, escalate".to_string()
}

/// Longest verdict summary kept. The summary is durable state and is read back
/// into a gate refusal / the orchestrator's pane, not a transcript — a couple of
/// paragraphs is the useful range, and an unbounded one is a file-size footgun.
pub const MAX_SUMMARY_CHARS: usize = 4000;

/// A reviewer's summary is free prose that lands in a file loomux reads back and
/// re-renders. Drop control characters (they would ride into a terminal) but keep
/// newlines and tabs so the prose survives, and cap the length.
pub fn sanitize_summary(s: &str) -> String {
    s.trim()
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(MAX_SUMMARY_CHARS)
        .collect()
}

/// One durable, **reviewer-attributed** verdict: which block recorded it, which
/// agent instance that was, **which revision it reviewed**, when, and why. The
/// attribution is the point — #197's second requirement is that "the specific
/// dispatched reviewer's recorded verdict is the gate, not the first approve that
/// arrives from any agent".
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReviewVerdict {
    pub pr: u64,
    /// The reviewer **block** id (`rev-security`) — the identity a gate names.
    pub block: BlockId,
    /// The agent instance that recorded it (`rev-4`). Two spawns of the same
    /// block are the same gate slot; this says which one actually spoke.
    pub agent_id: String,
    pub verdict: Verdict,
    /// **The PR head commit this verdict reviewed** (`headRefOid`), captured when
    /// it was recorded.
    ///
    /// A verdict binds to a *revision*, not to a PR number. Without this a `pass`
    /// survives a re-push: two reviewers approve #7, the worker pushes "fixed
    /// lint" and "one more edge case", and the gate still reads green over commits
    /// nobody reviewed — #197's failure class exactly, and the reason GitHub's own
    /// review model dismisses stale approvals on new commits. The gate compares
    /// this against the PR's current head and treats a mismatch as **outstanding**.
    ///
    /// Empty when loomux could not resolve the head at record time (no gh, no
    /// network, a repo gh can't see). That is *not* treated as "unbound, therefore
    /// fine" — an empty head can never equal a real one, so it reads as stale and
    /// the reviewer must re-record. Fail closed, like everything else here.
    pub head: String,
    pub summary: String,
    pub ts_ms: u64,
}

impl ReviewVerdict {
    /// Whether this verdict reviewed the PR's current head. A blocking verdict is
    /// *revision-independent* — a `fail` recorded against an older commit still
    /// refuses the merge until the reviewer re-records, because "this PR has a
    /// defect" does not stop being true when the author pushes more code.
    pub fn reviewed(&self, head: &str) -> bool {
        !self.head.is_empty() && self.head == head
    }
}

/// Group-dir subdirectory holding recorded verdicts, one file per reviewer block:
/// `verdicts/pr-<N>/<block-id>`.
///
/// **Why a file tree and not JSON:** the enforcement point is the `gh` PATH shim
/// — a POSIX shell script with no `jq` — and the existing gate state it reads
/// (`autonomous`, `auto_merge`, `merge_grants/pr-<N>`) is already exactly this:
/// small files whose presence and first line say everything. A verdict file's
/// first line is the verdict word, so the shim's read is `head -n1`. Keeping the
/// durable record and the enforcement input as *one* artifact means they cannot
/// drift.
pub const VERDICTS_DIR: &str = "verdicts";

/// A commit id is compared against gh's `headRefOid` inside a shell `case`, so
/// keep it to what a git object id can actually be. Anything else stores as empty,
/// which reads as **stale** — never as "unbound, therefore fine".
pub fn sanitize_sha(s: &str) -> String {
    let s = s.trim();
    if !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        s.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// Serialize a verdict record for `verdicts/pr-<N>/<block>`. Line-oriented, with
/// the verdict word FIRST (the shim reads it with `head -n1`) and the reviewed
/// head SECOND; the summary runs to EOF, being the only field that may contain
/// newlines.
pub fn verdict_file_text(v: &ReviewVerdict) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n",
        v.verdict.as_str(),
        sanitize_sha(&v.head),
        v.ts_ms,
        v.agent_id,
        sanitize_summary(&v.summary)
    )
}

/// Read a verdict file back. `None` for anything that isn't a verdict this build
/// understands — an unparseable file is *not* a pass (see [`Verdict::parse`]).
/// `pr`/`block` come from the path, which is loomux-generated.
pub fn parse_verdict_file(pr: u64, block: &str, text: &str) -> Option<ReviewVerdict> {
    let mut lines = text.lines();
    let verdict = Verdict::parse(lines.next()?)?;
    let head = sanitize_sha(lines.next().unwrap_or(""));
    let ts_ms = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    let agent_id = lines.next().unwrap_or("").trim().to_string();
    let summary = lines.collect::<Vec<_>>().join("\n");
    Some(ReviewVerdict {
        pr,
        block: sanitize_id(block)?,
        agent_id,
        verdict,
        head,
        summary: sanitize_summary(&summary),
        ts_ms,
    })
}

// ── the merge gate: the decision, and the spec file the shim reads ──────────

/// Gate conditions this build knows how to check (`gates.merge.also`).
///
/// The list is short on purpose, and the rule for everything *not* on it is the
/// important half: a condition loomux cannot check **refuses the merge** rather
/// than passing it. A gate is a safety claim; silently ignoring a clause of it
/// would turn a stricter-looking workflow file into a weaker one, which is the
/// worst failure mode a gate can have.
pub const KNOWN_CONDITIONS: [&str; 1] = ["ci-green"];

/// Whether the shim can evaluate this `also:` condition. See [`KNOWN_CONDITIONS`].
pub fn condition_supported(c: &str) -> bool {
    KNOWN_CONDITIONS.contains(&c.trim())
}

/// Why a merge gate is (not) satisfied — the pure spec the shim's shell mirrors,
/// and what the `review_verdict` tool reports back to the reviewer that just voted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateOutcome {
    /// Every requirement met: the merge may proceed to the *other* gates (the
    /// human grant / autonomous markers) — this one never opens a merge by itself.
    Satisfied,
    /// At least one named reviewer recorded `fail`/`escalate`. Blockers beat
    /// approvals: this refuses the merge whatever the others recorded, and
    /// whatever the threshold is.
    Blocked { blocking: Vec<BlockId> },
    /// Not enough live PASS verdicts yet.
    ///
    /// - `outstanding` — named reviewers with **no verdict recorded at all**. The
    ///   #151 case: a merge landing while a dispatched review is still running.
    /// - `stale` — named reviewers whose `pass` was recorded against an **earlier
    ///   revision** of the PR (or against none at all). The branch moved under
    ///   them; what they approved is not what would merge.
    Short { passes: u32, need: u32, outstanding: Vec<BlockId>, stale: Vec<BlockId> },
    /// loomux could not resolve the PR's current head, so it cannot tell whether
    /// any recorded verdict reviewed the code that would merge. Refuses — the same
    /// fail-safe the human gate takes on an undeterminable base.
    UnknownRevision,
}

impl GateOutcome {
    pub fn satisfied(&self) -> bool {
        matches!(self, GateOutcome::Satisfied)
    }
}

/// How many PASS verdicts this gate needs: every named reviewer (`all-pass`) or
/// `threshold: N`.
pub fn gate_need(gate: &Gate) -> u32 {
    match gate.require {
        GateRequire::AllPass => gate.reviewers.len() as u32,
        GateRequire::Threshold(n) => n,
    }
}

/// Reviewer ids a gate names that the given roster cannot actually spawn — either
/// no block carries that id, or it exists under a different capability class
/// (`kind` != reviewer). A gate's reviewers are validated against a workflow
/// file's OWN blocks at parse time ([`parse_workflow`]), but the roster a live
/// group spawns from can diverge from the file that armed its gate: a broken or
/// absent `.loomux/workflow.yml` on a fresh launch keeps the group's last-known
/// gate but resets `blocks` to [`default_roster`] (see `create_group`'s
/// `merge-gate-retained` branch, and the live incident behind #316 — a gate
/// naming `rev-orch`/`rev-ui`/`rev-tests` with the running registry offering only
/// the built-in four, so `spawn_agent(block: "rev-orch")` failed with "unknown
/// block" and the gate could never be satisfied from inside that session). Pure,
/// so both the arm-time refusal and a live status read share one rule.
pub fn gate_missing_blocks(gate: &Gate, blocks: &[Block]) -> Vec<BlockId> {
    gate.reviewers
        .iter()
        .filter(|id| !blocks.iter().any(|b| &b.id == *id && b.kind == Role::Reviewer))
        .cloned()
        .collect()
}

/// The agent-capacity a declared workflow structurally needs (#255) — derived
/// from its roster and its `merge` gate (if any), so the launcher can warn
/// before a `max_agents` cap starves the workflow it just loaded rather than
/// discovering it two hours in as an orchestrator that keeps killing live
/// agents to make room (the #255 incident: a 3-reviewer `all-pass` gate plus a
/// two-tier worker roster under a cap of 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapacityRecommendation {
    /// What **one review round costs without evicting anything already
    /// live**: [`reviewers_needed`](Self::reviewers_needed) plus one worker
    /// slot to have something to review. Below this the orchestrator cannot
    /// complete a single rework loop without killing a live agent to free a
    /// slot.
    pub minimum: u32,
    /// What running **every declared tier concurrently** costs: every
    /// distinct worker block, every distinct reviewer block, and one more if
    /// the workflow declares a planner block. The orchestrator itself is
    /// exempt from `max_agents` (mcp.rs) and is never counted here.
    ///
    /// A workflow with two planner blocks still adds only one slot here — a
    /// repo declares a *second* planner to give it an alternate persona (a
    /// different model, a narrower prompt), not to run two plan-first phases
    /// at once; the orchestrator only ever has one active planning phase, so
    /// unlike workers/reviewers (genuinely fanned out for parallel lanes) a
    /// planner count would overstate what concurrency the roster needs. This
    /// also matches #255's literal spec: "+1 if a planner block exists".
    pub recommended: u32,
    /// The gate's reviewer requirement folded into `minimum` — [`gate_need`],
    /// or every declared reviewer block when the workflow names no `merge`
    /// gate. Kept as its own field (rather than making a caller subtract the
    /// worker slot back out of `minimum`, or recount reviewer *blocks*) so
    /// anything describing *why* `minimum` is what it is reads this instead of
    /// re-deriving a gate-derived number from the block list — conflating the
    /// two was exactly the bug rev-1 of #255's review caught in `roster.ts`'s
    /// warning text.
    pub reviewers_needed: u32,
}

/// Derive a [`CapacityRecommendation`] from a workflow's blocks and its
/// `gates.merge` clause (`None` when the workflow declares none).
///
/// Gate-aware, per #255's requirement: a roster with 5 reviewer blocks but
/// `require: threshold: 2` has a different (lower) minimum than one requiring
/// `all-pass` over the same 5 — [`gate_need`] is exactly that distinction.
///
/// With no gate declared, nothing *enforces* every reviewer block being live
/// at once — but nothing else tells loomux which subset would be, either, so
/// `minimum` conservatively falls back to every reviewer block the workflow
/// names. That is deliberately the erring-flag-not-erring-silent side: this
/// feature exists because a starved roster surfaced as nothing more than "a
/// slow run" (#255's incident), so a gateless roster warning at a cap that
/// merely *might* be enough is the safer of the two wrong answers.
pub fn recommend_capacity(blocks: &[Block], gate: Option<&Gate>) -> CapacityRecommendation {
    let workers = blocks.iter().filter(|b| b.kind == Role::Worker).count() as u32;
    let reviewers = blocks.iter().filter(|b| b.kind == Role::Reviewer).count() as u32;
    let has_planner = blocks.iter().any(|b| b.kind == Role::Planner);

    let reviewers_needed = gate.map_or(reviewers, gate_need);
    let worker_slot = u32::from(workers > 0);
    CapacityRecommendation {
        minimum: reviewers_needed + worker_slot,
        recommended: workers + reviewers + u32::from(has_planner),
        reviewers_needed,
    }
}

/// Which declared tiers `recommended` adds beyond `minimum` — i.e. what a cap
/// sitting at-or-above `minimum` but below `recommended` can never keep live
/// alongside a review round (#255's soft-warning tier). Each entry is a short
/// noun phrase (`"the planner"`, `"1 more worker tier"`) meant to be joined
/// into a sentence, not a standalone description.
///
/// Takes the same `reviewers_needed` [`recommend_capacity`] computed, rather
/// than re-deriving it from `gate`, so this can never disagree with the
/// `minimum` it is describing the excess over.
pub fn extra_tiers(blocks: &[Block], reviewers_needed: u32) -> Vec<String> {
    let workers = blocks.iter().filter(|b| b.kind == Role::Worker).count() as u32;
    let reviewers = blocks.iter().filter(|b| b.kind == Role::Reviewer).count() as u32;
    let has_planner = blocks.iter().any(|b| b.kind == Role::Planner);

    let mut out = Vec::new();
    // `minimum` budgets exactly one worker slot regardless of how many worker
    // blocks are declared — every worker tier beyond the first is "extra".
    let extra_workers = workers.saturating_sub(1);
    if extra_workers > 0 {
        out.push(format!("{extra_workers} more worker tier{}", if extra_workers > 1 { "s" } else { "" }));
    }
    // `minimum` only budgets the gate's requirement — every reviewer block
    // beyond that (an all-pass gate naming a subset, or extra unnamed ones)
    // is "extra".
    let extra_reviewers = reviewers.saturating_sub(reviewers_needed);
    if extra_reviewers > 0 {
        out.push(format!("{extra_reviewers} more reviewer{}", if extra_reviewers > 1 { "s" } else { "" }));
    }
    if has_planner {
        out.push("the planner".to_string());
    }
    out
}

/// English-join a short list of noun phrases: `"a"`, `"a and b"`, `"a, b, and
/// c"`. Used to turn [`extra_tiers`]'s list into one clause of a warning
/// sentence — pulled out so the audit note and the launcher's message build
/// the same phrase instead of each hand-rolling their own `.join(...)`.
pub fn join_with_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => {
            let (last, rest) = parts.split_last().expect("non-empty, matched above");
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

/// **The gate decision** (reviewer half; the `also:` conditions are checked in the
/// shim, which is the only place that can call `gh pr checks`). Pure, so the
/// semantics are pinned by fast tests and the shell mirror has something to agree
/// with. `head` is the PR's current head commit — `None` when loomux could not
/// resolve it.
///
/// Order matters, and it is the order #197 asks for:
///
/// 1. **A blocking verdict refuses the merge** — before any counting, and
///    regardless of which revision it was recorded against. One reviewer's `fail`
///    is not outvoted by two passes, and `threshold: 2` does not mean "two yeses
///    beat a no". (A `fail` against an older commit still stands: "this PR has a
///    defect" does not stop being true because the author pushed more code. The
///    reviewer clears it by re-reviewing and re-recording.)
/// 2. **A `pass` only counts for the revision it reviewed.** A pass recorded
///    against an earlier head is *stale*: the branch moved, and what that reviewer
///    approved is not what would merge. It counts as outstanding, not as a pass —
///    which is why GitHub's own review model dismisses stale approvals on new
///    commits, and it is the #197 failure class ("merging code no reviewer saw")
///    that a PR-keyed verdict would have left wide open.
/// 3. Then the live PASS count must reach [`gate_need`]. Under `all-pass` that
///    means every named reviewer has passed *this* revision — a reviewer that
///    hasn't recorded anything keeps the gate shut, which is precisely the bug that
///    produced #197.
///
/// `threshold: N` deliberately does *not* wait for the reviewers it doesn't need:
/// an author who writes `threshold: 2` over three reviewers has said, in the file,
/// that two passes are enough. They still cannot merge over a `fail` (rule 1), and
/// the passes still have to be for the code that would actually merge (rule 2).
/// `all-pass` — the default when `require:` is omitted — is the one that waits for
/// everybody.
pub fn evaluate_merge_gate(
    gate: &Gate,
    verdicts: &BTreeMap<BlockId, ReviewVerdict>,
    head: Option<&str>,
) -> GateOutcome {
    let mut blocking: Vec<BlockId> = Vec::new();
    let mut outstanding: Vec<BlockId> = Vec::new();
    let mut stale: Vec<BlockId> = Vec::new();
    let mut passes = 0u32;
    // No resolvable head → no way to know whether any pass reviewed the code that
    // would merge. Refuse, rather than fall back to "a pass is a pass" — that
    // fallback IS the bug this binding closes.
    let Some(head) = head else {
        return GateOutcome::UnknownRevision;
    };
    for r in &gate.reviewers {
        match verdicts.get(r) {
            Some(v) if v.verdict.is_blocking() => blocking.push(r.clone()),
            Some(v) if v.reviewed(head) => passes += 1,
            Some(_) => stale.push(r.clone()),
            None => outstanding.push(r.clone()),
        }
    }
    if !blocking.is_empty() {
        return GateOutcome::Blocked { blocking };
    }
    let need = gate_need(gate);
    if passes >= need {
        GateOutcome::Satisfied
    } else {
        GateOutcome::Short { passes, need, outstanding, stale }
    }
}

/// Group-dir file holding the declared merge gate, written from the repo's
/// `.loomux/workflow.yml` at group create/resume and read by the `gh` shim.
/// **Absent = no gate**, which is what makes a repo with no workflow file (or one
/// declaring no `gates.merge`) behave byte-for-byte as it did before #222.
pub const MERGE_GATE_FILE: &str = "merge_gate";

/// Serialize a gate for [`MERGE_GATE_FILE`].
///
/// Line-oriented `key value [value]`, because the reader is a POSIX `while read`
/// loop with no JSON parser — the same reason the verdicts are a file tree. Every
/// token written here is already sanitized: block ids through [`sanitize_id`] and
/// conditions through [`sanitize_condition`], both of which *reject* (never
/// rewrite) anything outside their alphabet at parse time. That is the contract
/// #225 established for exactly this consumer, and it is what lets the shim word-
/// split the line without quoting. Belt and braces anyway: a token that would not
/// survive its sanitizer is dropped here rather than written into a shell's
/// `for` loop.
///
/// **A token that fails its sanitizer poisons the file rather than vanishing from
/// it.** The first draft silently dropped such a token — which, if the parse
/// contract ever regressed, would have emitted a *weaker* gate than the repo
/// declared (a reviewer or a condition just disappears, and the gate goes green
/// one requirement short). Every other fork in this feature chooses fail-closed on
/// exactly that question; this one now does too. [`POISON_KEY`] is a line the shim
/// cannot parse, and an unparseable line refuses every merge until a human looks.
pub fn gate_file_text(gate: &Gate) -> String {
    let mut out = String::from(
        "# loomux merge gate — generated from .loomux/workflow.yml (#222). Do not edit.\n",
    );
    match gate.require {
        GateRequire::AllPass => out.push_str("require all-pass\n"),
        GateRequire::Threshold(n) => out.push_str(&format!("require threshold {n}\n")),
    }
    for r in &gate.reviewers {
        match sanitize_id(r) {
            Some(clean) if clean == *r => out.push_str(&format!("reviewer {r}\n")),
            _ => out.push_str(&format!("{POISON_KEY} unusable-reviewer-id\n")),
        }
    }
    for c in &gate.also {
        match sanitize_condition(c) {
            Some(clean) if clean == *c => out.push_str(&format!("also {c}\n")),
            _ => out.push_str(&format!("{POISON_KEY} unusable-condition\n")),
        }
    }
    out
}

/// The key [`gate_file_text`] writes when a token cannot be represented safely.
/// Nothing parses it — by design: the shim refuses any gate-file line whose key it
/// does not recognize, so an unrepresentable gate refuses merges instead of
/// silently becoming a laxer one. Unreachable while the parse contract holds
/// (`parse_workflow` rejects such tokens outright); this is what happens if it
/// ever stops holding.
pub const POISON_KEY: &str = "unrepresentable";

/// Read [`MERGE_GATE_FILE`] back into a [`Gate`] — the inverse of
/// [`gate_file_text`], used by the registry to report gate status to the agent
/// that just recorded a verdict (the shim does its own read, in shell).
///
/// `None` means **this file is not a usable gate**, which the callers must report
/// as "malformed — every merge refused" rather than as "no gate": the file is on
/// disk, the shim will read it, and the shim refuses on exactly the things that
/// return `None` here. Those are a file with no reviewers (nobody could ever
/// satisfy it) and any line whose key loomux does not recognize — a poison line
/// ([`POISON_KEY`]), a truncation, a hand edit. The two halves agree, and both fail
/// closed.
pub fn parse_gate_file(text: &str) -> Option<Gate> {
    let mut require = GateRequire::AllPass;
    let mut reviewers: Vec<BlockId> = Vec::new();
    let mut also: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split_whitespace();
        match (f.next(), f.next(), f.next()) {
            // A threshold that doesn't parse (or is 0) leaves `require` at
            // `all-pass` — the STRICTER of the two. A malformed gate line must
            // never be the reason a merge gets easier.
            (Some("require"), Some("threshold"), Some(n)) => {
                if let Some(n) = n.parse().ok().filter(|n| *n > 0) {
                    require = GateRequire::Threshold(n);
                }
            }
            (Some("require"), Some("all-pass"), _) => require = GateRequire::AllPass,
            (Some("reviewer"), Some(id), _) => match sanitize_id(id) {
                Some(id) => reviewers.push(id),
                None => return None,
            },
            (Some("also"), Some(c), _) => match sanitize_condition(c) {
                Some(c) => also.push(c),
                None => return None,
            },
            // Anything else — a poison line, a truncated key, a hand edit — makes
            // the whole file unusable. Skipping it would drop a requirement.
            _ => return None,
        }
    }
    (!reviewers.is_empty()).then_some(Gate { require, reviewers, also })
}
