//! Roles as data: the **block model** and `<repo>/.orrerix/workflow.yml` (#222;
//! the legacy `.loomux/` spelling is still discovered — see [`workflow_path`]).
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
//! less than the word suggests — [`Role::containment`](crate::model::Role::containment)
//! is the exact per-class answer. A **planner** is structurally read-only — its
//! file-editing tools and `git commit`/`git push` are denied at the CLI level, so
//! `is_read_only()` is a real, mechanical guarantee. A **reviewer** is denied the
//! CLI's file-editing tools too (#462), but keeps the shell — running the tests
//! is its job — so its "never pushes" stays *instruction-backed*, and so does
//! "never writes a file" for anything a shell command can do. What the closed
//! enum guarantees is that a repo file cannot *change* which posture a block
//! gets — not that any posture is a sandbox. (See
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
//!     effort: xhigh        # OPTIONAL thinking level (#687); empty = the CLI's
//!     context: 1m          # own default. Both are closed enums, and both are
//!                          # a parse error on a CLI loomux can't set them on.
//!     prompt: |            # -> generated ~/.claude/agents/*.md + claude --agent
//!       Review ONLY for security defects: injection, authz, secrets.
//!
//!   - id: advisor            # role_hint: OPTIONAL (#250/#324/#891) — picks a
//!     kind: planner           # persona addendum/template/badge, plus a short
//!     role_hint: advisor      # enumerated list of MCP-tool exceptions; the
//!                             # CAPABILITY CLASS is `kind` alone, always.
//!                             # advisor requires kind: planner, process
//!                             # requires kind: worker, liaison requires
//!                             # kind: reviewer — anything else is a parse
//!                             # error.
//!
//!   - id: manager           # OPTIONAL, at most one (#1161). The human's own
//!     kind: manager         # interface pane: it converses, grooms feature
//!     model: opus           # requests into briefs, and relays. cli:/model:/
//!                           # effort:/context:/name: only — prompt:, profile:
//!                           # and allow: are parse errors here, as on the
//!                           # orchestrator block.
//!
//! edges:                   # ADVISORY: the declared happy path. The
//!   - { from: worker, to: [rev-security] }   # orchestrator still schedules.
//!
//! gates:                   # DECLARED here; ENFORCED by the gh shim (sub-PR 3).
//!   merge:
//!     require: all-pass    # or: threshold: 2
//!     reviewers: [rev-security]
//!
//! merge_queue:             # OPT-IN, default off (#581). Absent block = the
//!   enabled: true          # feature is off and behavior is byte-for-byte
//!   max_batch: 3           # unchanged. See [`MergeQueuePolicy`].
//!   checks_timeout_minutes: 60
//!
//! board:                   # OPT-IN, default off (#1175). Absent block = no
//!   wip:                   # limits at all. See [`BoardPolicy`].
//!     in-progress: 4       # per-status caps on how much work may sit there
//!     review: 3
//!   enforce: false         # false (the default) = warn + notify; true =
//!                          # AGENT writes crossing a cap are refused
//! ```
//!
//! `id` is immutable and human-meaningful and `name` is display-only on
//! purpose: n8n keys its graph by *display name*, so renaming a node silently
//! breaks every reference to it. Layout/coordinates live in a separate
//! `workflow.layout.json` beside it (the GUI pane's file, sub-PR 2) so a canvas
//! nudge never churns the semantic diff.

use crate::model::{cli_can_host, default_model, Role, SUPPORTED_CLIS};
use crate::notify::{
    clamp_expires_minutes, NOTIFY_EXPIRES_DEFAULT_MIN, NOTIFY_EXPIRES_MAX, NOTIFY_EXPIRES_MIN,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// A block's identity — immutable, human-meaningful, referenced by edges/gates.
pub type BlockId = String;

/// Where in the repo a workflow lives. Committed and shareable: a repo's
/// workflow is a property of the *project*, not of one developer's machine
/// (the #51 requirement).
pub const WORKFLOW_PATH: &str = ".orrerix/workflow.yml";

/// The pre-#1153 spelling, still discovered when `.orrerix/workflow.yml` is
/// absent — permanently, and never renamed on the repo's behalf: it is a
/// tracked file in somebody's git history. See [`crate::brand`] for the rule
/// and `doc/design/rebrand-filesystem.md` for the argument.
pub const LEGACY_WORKFLOW_PATH: &str = ".loomux/workflow.yml";

/// Which of the two spellings a given repo actually uses — the string every
/// message, audit line and preview must name, so that "this repo declares a
/// workflow (`…`)" points at the file that was really read.
pub fn workflow_path(repo: &str) -> &'static str {
    crate::brand::resolve_repo_file(repo, WORKFLOW_PATH, LEGACY_WORKFLOW_PATH)
}

/// The absolute workflow file for `repo`, resolved through [`workflow_path`].
pub fn workflow_file(repo: &str) -> std::path::PathBuf {
    Path::new(repo).join(workflow_path(repo))
}

/// Schema version this build understands. Recorded in the file so a future
/// breaking change can be detected rather than mis-parsed.
pub const SCHEMA_VERSION: u32 = 1;

/// The block ids that name a capability class, and so own that class's
/// instruction file. The first four are the built-in roster's, and they keep
/// their historic file names (`worker.md`, …) — which is what makes a
/// no-workflow group byte-for-byte identical to pre-#222 loomux.
///
/// `manager` (#1161) is the fifth, and it is NOT a built-in-roster id: no
/// default group has a manager block, and [`builtin_roster`] still synthesizes
/// exactly four. It is here because the rule this array encodes is "a class
/// name is a reserved id owning that class's file", and applying it to four of
/// five classes is how the fifth quietly acquires a different rule. Note what
/// this array does *not* do: what stops `- id: manager, kind: worker` is
/// [`kind_from_str`] in `parse_workflow`'s reserved-id check, which reads the
/// CLASS table rather than this one.
///
/// [`roster_is_custom`] deliberately does not read membership here as "nothing
/// a workflow file put there" — see its own doc.
pub const BUILTIN_IDS: [&str; 5] = ["orchestrator", "worker", "reviewer", "planner", "manager"];

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
    /// Inline persona (the `prompt:` key). Compiled into a loomux-generated
    /// custom-agent FILE on both CLIs (round #417 correction 6 for Claude;
    /// #416 for Copilot), or — on a directory-write failure — Claude's
    /// `--append-system-prompt-file` / Copilot's kickoff-prompt paste.
    pub prompt: Option<String>,
    /// Repo-relative path to a persona file (the `profile:` key), e.g.
    /// `.github/agents/worker.md`. A `.github/agents/*.md` file is what lets a
    /// Copilot block use its **native** `--agent <name>`.
    pub profile: Option<String>,
    /// Extra pre-approved tool patterns (`--allowedTools` / `--allow-tool`).
    /// Sanitized; may never re-grant what the capability class denies (deny
    /// rules beat allow rules on both CLIs).
    pub allow: Vec<String>,
    /// An optional persona/template marker (`advisor` | `process` |
    /// `liaison`, #250/#324/#891). `parse_workflow` requires it to pair with a
    /// specific `kind` (`advisor` needs `planner`, `process` needs `worker`,
    /// `liaison` needs `reviewer`; see [`role_hint_requires`]) so a workflow
    /// file cannot spell a combination nothing downstream will honor.
    ///
    /// The STRUCTURAL containment never reads it: `kind.containment()` and the
    /// CLI deny-flags take a `Role`, not a `Block`. `mcp::tool_defs` does read
    /// it, for a short list of exceptions enumerated in
    /// `doc/design/liaison.md` — two narrow (`session_digest` to `process`,
    /// `review_verdict` away from `liaison`) and two widen toward that same
    /// `liaison`, both otherwise orchestrator-only (`group_usage`; and
    /// `ask_human`, the pose only). A repo still
    /// cannot grant itself anything by writing one: it picks from a closed set
    /// and loomux's code decides the effect.
    /// `None` is today's behavior, byte for byte.
    pub role_hint: Option<String>,
    /// Thinking-effort level (the `effort:` key, #687) — one of
    /// [`crate::model::EFFORT_LEVELS`]. Empty means "the CLI's own default", which is
    /// today's behavior byte for byte: nothing is emitted at all, so a group on
    /// a CLI build that predates the flag is unaffected unless a human opts in.
    ///
    /// A **value-set pick**, like `model:` — it authors no text and pre-approves
    /// no tool, so it adds nothing to what a repo file can influence. What is
    /// enforced is the `role_hint` shape, twice: the value must be in loomux's
    /// closed vocabulary, and the block's own `cli:` must be one loomux can
    /// actually set effort on ([`CliCaps::effort_levels`](crate::model::CliCaps)).
    pub effort: String,
    /// Context-window variant (the `context:` key, #687) — one of
    /// [`crate::model::CONTEXT_VARIANTS`]. Empty = the model's own window.
    ///
    /// Deliberately NOT part of `model`: [`crate::model::sanitize_model_opt`] strips
    /// brackets, so a `sonnet[1m]` written as a model id would silently become
    /// the broken `sonnet1m`, and widening that sanitizer to admit brackets
    /// would put a POSIX-shell glob pattern on the command line. The suffix is
    /// composed at emit time instead — see `claude_model_token`.
    pub context: String,
}

/// The per-block model knobs that reach a spawn alongside the model itself
/// (#687): "how this block's model is tuned", as one value.
///
/// Bundled rather than threaded as two more positional `&str`s because they are
/// one concept, and because the next knob (gemini's `thinkingConfig`, deferred
/// pending live schema verification) lands here rather than as a third
/// argument. `default()` — both empty — is exactly the pre-#687 command line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelKnobs<'a> {
    pub effort: &'a str,
    pub context: &'a str,
}

impl Block {
    /// Agent-id prefix (`w-3`, `rev-4`). Moved off `Role` onto the block, but
    /// deliberately still *derived from* the capability class: agent ids are
    /// short, are parsed by the roster/badge conventions, and must stay
    /// byte-identical for the built-in roster. Block identity rides in
    /// `orchestration::AgentEntry`'s `block` field and the pane name instead.
    pub fn prefix(&self) -> &'static str {
        self.kind.prefix()
    }

    /// The file in the group dir that carries this block's loomux role
    /// contract, referenced by the kickoff prompt. The built-in blocks keep
    /// their historic names (`worker.md`, …) so a default group's kickoff text
    /// is unchanged; a custom block gets `<id>.md`.
    pub fn instructions_file(&self) -> String {
        if BUILTIN_IDS.contains(&self.id.as_str()) {
            crate::model::role_instructions_file(self.kind).to_string()
        } else {
            format!("{}.md", self.id)
        }
    }

    /// Whether this block's id is a reserved class name ([`BUILTIN_IDS`]) — so
    /// it owns that class's instruction file rather than a `<id>.md` of its
    /// own.
    ///
    /// For the four built-in classes this is also "is one of the built-in
    /// roster entries", which is what it was called before #1161 and what every
    /// pre-existing caller means by it. `- id: manager` satisfies it too and is
    /// NOT a built-in roster entry, which is why [`roster_is_custom`] asks a
    /// second question rather than reading this alone.
    pub fn is_builtin(&self) -> bool {
        BUILTIN_IDS.contains(&self.id.as_str())
    }

    /// A block with no persona behaves exactly like a pre-#222 role: no persona
    /// text to fold into the generated custom-agent file, nothing to inject
    /// into the kickoff — the CONTRACT itself still always compiles (#416).
    pub fn has_persona(&self) -> bool {
        self.prompt.is_some() || self.profile.is_some()
    }

    /// This block's model knobs (#687), as the one value the spawn path passes
    /// to `build_agent_command_ex` / `build_agent_argv_ex`.
    pub fn knobs(&self) -> ModelKnobs<'_> {
        ModelKnobs { effort: &self.effort, context: &self.context }
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
    /// The small-batch clause (#1174): the largest PR, in changed lines
    /// (additions + deletions), this gate will let through. `None` — the key
    /// absent — is the whole feature off, byte-for-byte the pre-#1174 flow.
    ///
    /// **A structured key rather than an `also:` token, deliberately.** `also:`
    /// is a closed vocabulary of *parameterless* conditions; a threshold is a
    /// number, and stuffing it into a token (`max-diff-800`) would put a
    /// parameter into a namespace whose whole safety property is that every
    /// entry either matches a known name or fails closed.
    ///
    /// Pure repo config (CLAUDE.md constraint 8): loomux never learns what 800
    /// means for this repo, only that this repo said 800.
    pub max_diff_lines: Option<u32>,
    /// Path-based reviewer routing (#1176) — rules that make the required
    /// reviewer set a function of the diff. Empty — the key absent — is the
    /// whole feature off, byte-for-byte the pre-#1176 flow, and
    /// [`route_reviewers`] never even looks at a changed-file list.
    ///
    /// **Additive, and only ever tightening**: the required set is
    /// [`reviewers`](Self::reviewers) ∪ the reviewers of every rule that
    /// matched. A rule that matches nothing costs nothing; a rule that matches
    /// adds a lane. Nothing here can make a gate easier to satisfy, which is
    /// why `routing:` and `require: threshold` are refused together at parse
    /// ([`parse_workflow`]) — see [`RoutingRule`].
    pub routing: Vec<RoutingRule>,
}

/// One path-based routing rule (#1176): *if this PR touched any of these paths,
/// these reviewers are required too.*
///
/// Deliberately loomux-native globs rather than the repo's own `CODEOWNERS`:
/// that file names GitHub users and teams, which are not workflow blocks, and
/// the mapping between them would be repo config anyway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutingRule {
    /// Path globs, matched against the PR's changed files with
    /// [`glob_match`]. Validated at parse through [`sanitize_glob`]; at least
    /// one, at most [`ROUTING_PATHS_MAX`].
    pub paths: Vec<String>,
    /// Block ids required when any path matches. Validated exactly as
    /// [`Gate::reviewers`] is — the block must exist, be `kind: reviewer`, and
    /// not be a liaison — because a rule naming anything else could never be
    /// satisfied.
    pub reviewers: Vec<BlockId>,
}

/// How many routing rules one gate may declare.
///
/// A bound rather than a preference: the shim evaluates **every** rule against
/// **every** changed file on **every** merge, in POSIX shell, so an unbounded
/// rule list is an unbounded cost on the merge path. Past this many lanes the
/// block has stopped routing and started listing, which is a different feature.
pub const ROUTING_RULES_MAX: usize = 32;

/// How many path globs one routing rule may carry — the other half of the
/// product [`ROUTING_RULES_MAX`] bounds.
pub const ROUTING_PATHS_MAX: usize = 32;

/// Longest path glob accepted. Generous next to [`MAX_ID_CHARS`] because a repo
/// path legitimately is: `crates/loomux-engine/src/**` is ordinary.
pub const MAX_GLOB_CHARS: usize = 200;

/// Why a block id may not be named as a gate reviewer — `None` when it may.
///
/// **One definition for both lists** (#1176): `gates.merge.reviewers` and every
/// `gates.merge.routing[].reviewers`. They are the same question — *could a
/// verdict for this id ever be recorded?* — and answering it twice is how the
/// static list ends up refusing a manager while a routing rule quietly accepts
/// one. `ctx` names which list is being read so the message points at the line
/// the author has to fix.
fn gate_reviewer_error(gate: &str, ctx: &str, rname: &str, blocks: &[Block]) -> Option<String> {
    match blocks.iter().find(|b| b.id == rname) {
        None => Some(format!("gates.{gate}: {ctx} {rname:?} names no block")),
        // The manager (#1161) is structurally caught by the arm below — it is
        // not reviewer-kind — but "that block's kind is manager, not a
        // reviewer" describes the type error and not the mistake. An author who
        // named the manager on a gate was reaching for "the human signs off",
        // which is a real thing they wanted and a real thing this gate cannot
        // express, so say that instead. The pane validator carries the same arm
        // (`validateWorkflow`, gate-not-a-reviewer), and the liaison arm below
        // is the shape both are modelled on.
        Some(b) if b.kind == Role::Manager => Some(format!(
            "gates.{gate}: {ctx} {:?} is a manager — the manager is the human's \
             interface, not a reviewer: it records no verdict, so a gate naming it \
             could never open. A gate reads REVIEWER verdicts; the human's own sign-off \
             is the merge gate loomux already applies on top of it.",
            b.id
        )),
        // A gate reads reviewer verdicts. Naming a worker would make it
        // permanently unsatisfiable — nothing would ever record a verdict for
        // it — which is the "dangling reference the UI happily saves" failure
        // this validation pass exists to prevent.
        Some(b) if b.kind != Role::Reviewer => Some(format!(
            "gates.{gate}: {ctx} {:?} is a {} block, not a reviewer — a gate can only require reviewer verdicts",
            b.id,
            b.kind.as_str()
        )),
        // The same unsatisfiable gate, one kind further in (#891). A liaison IS
        // reviewer-kind — it rides that class for its read-only, persistent
        // posture — but it is denied `review_verdict` at every layer precisely
        // because it reviews nothing. Naming one here would therefore wait
        // forever for a verdict no code path can produce, which is exactly the
        // failure the arm above refuses; caught at parse rather than discovered
        // as a merge gate that never opens.
        Some(b) if b.role_hint.as_deref() == Some("liaison") => Some(format!(
            "gates.{gate}: {ctx} {:?} is a liaison — it is reviewer-kind, but a \
             liaison never records a verdict (it presents the human's questions and \
             relays their answers), so a gate naming it could never open. Name a \
             reviewer that reviews.",
            b.id
        )),
        Some(_) => None,
    }
}

// ── resources: named lock resources (#858) ─────────────────────────────────

/// Slots a resource gets when it declares none. One — the useful default is a
/// mutex, and a repo that wants a semaphore says so.
pub const RESOURCE_SLOTS_DEFAULT: u32 = 1;

/// Ceiling on `slots`. Not a resource constraint — a legibility one: past this
/// the declaration no longer serializes anything, and a repo that wrote `1000`
/// meant something other than what it said.
pub const RESOURCE_SLOTS_MAX: u32 = 64;

/// How long a hold may last before the sweep reclaims it, when the resource
/// declares nothing. Long enough for a real build or test run, short enough
/// that a crashed holder's slot comes back inside a working session.
pub const RESOURCE_MAX_HOLD_MINUTES_DEFAULT: u32 = 30;

/// Ceiling on `max_hold_minutes` (8h). A hold is a bound on a *fallible*
/// signal — "the holder will call release_lock" — and the lessons-file rule is
/// that such a bound exists and is finite. A repo may make it generous; it may
/// not make it decorative.
pub const RESOURCE_MAX_HOLD_MINUTES_MAX: u32 = 480;

/// How many resources one repo may declare. Every declared name is listed in
/// the `acquire_lock` tool description that every agent in the group reads, so
/// the cap bounds a per-agent context cost, not a memory one.
pub const RESOURCES_MAX: usize = 32;

/// One declared lock resource: how many agents may hold it at once, and how
/// long any one of them may hold it before loomux takes it back.
///
/// **Policy, not mechanism** (CLAUDE.md constraint 8). Nothing here names a
/// toolchain, a command, or a machine: `build` is a *string this repo chose*,
/// and loomux never learns what it means. The whole schema is two numbers.
///
/// **Restrict-only, like the resource guard #318 designed before it.** A
/// `resources:` block can make an agent wait; it can never grant one a
/// capability, name a program to run, or reach the capability-closure spine
/// (`blocks:`/`edges:`/`gates:`). The worst a hostile `.loomux/workflow.yml`
/// can do with it is declare `slots: 1` on something everyone needs and slow
/// the group down — and both the hold and the wait are bounded above, and
/// every acquire/release/reclaim is audited.
///
/// **An absent block means the feature is off**: no `resources:` at all and
/// the three lock tools are not even listed to the group's agents, so behavior
/// is byte-for-byte what it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourcePolicy {
    /// Concurrent holders allowed. `1` (the default) is a mutex.
    pub slots: u32,
    /// The reclaim deadline on a single hold.
    pub max_hold_minutes: u32,
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        ResourcePolicy {
            slots: RESOURCE_SLOTS_DEFAULT,
            max_hold_minutes: RESOURCE_MAX_HOLD_MINUTES_DEFAULT,
        }
    }
}

// ── merge_queue: the bisecting merge queue's policy (#581 §11.2) ───────────

/// Default batch size (§11.2). At `3`, a red batch costs at most 2 extra CI
/// runs to attribute (ceil(log2 3)).
pub const MERGE_QUEUE_MAX_BATCH_DEFAULT: u32 = 3;

/// The `merge_queue:` block — a sibling of [`Gate`]'s `gates:`, and the whole
/// of what a repo declares about the queue. Design note:
/// `doc/design/merge-queue.md` §11.2; the queue's own core is [`crate::mergeq`]
/// (here since #888 batch 6), its write primitives are [`crate::mqdriver`]
/// (batch 12a), and the loop that sequences them is [`crate::mqloop`] (batch
/// 12b). None of that wiring reaches a pane host: this line used to say it did,
/// and batch 9 re-measured it.
///
/// **Policy, not mechanism** (CLAUDE.md constraint 8). Nothing here names a
/// branch, a toolchain, or a verification command: the queue *observes* the
/// repo's own CI and never defines or runs it, and the target it lands on comes
/// from the first enqueued PR's live base rather than from this file (§4).
///
/// **An absent block means the feature is off and behavior is byte-for-byte
/// unchanged** — the same posture `gates:` takes, and the reversal mechanism
/// §12 names: delete the block and every queue path is unreachable.
///
/// **Adding the block breaks the file for builds that predate #581 slice C, and
/// that is deliberate.** [`RawWorkflow`] is `deny_unknown_fields`, so
/// `merge_queue:` is not a tolerated unknown key on an older build: it fails
/// the parse of the *whole* file, gates and all, down the loud `workflow-invalid`
/// path. It is the right behavior anyway — `workflow.yml` is human-authored
/// policy, and a key the build does not understand means a human believes a
/// policy is in force that is not. Note the deliberate asymmetry with
/// `merge_queue.json` (`mergeq::MergeQueueState`), which *tolerates and
/// preserves* unknown fields because it is machine-authored state: policy fails
/// loud, state degrades gracefully.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeQueuePolicy {
    /// Default **false**. The product default is off (§12).
    pub enabled: bool,
    /// How many approved sub-PRs one speculative batch may carry (§11.2).
    /// At least 1 — see [`parse_workflow`]. There is no upper bound here
    /// because none is needed: the effective ceiling is the queue's own entry
    /// cap (`mergeq::MAX_ENTRIES`, §10), so an oversized value degenerates to
    /// "batch everything queued" rather than being unsatisfiable.
    pub max_batch: u32,
    /// The backstop on waiting for a batch's checks (§5). The primary release
    /// is the checks going terminal; this bounds the case where a repo attaches
    /// no checks at all, so the batch surfaces as **unverifiable** rather than
    /// sitting pending in silence — the lessons-file rule that any suppression
    /// driven by a fallible signal must be bounded.
    pub checks_timeout_minutes: u32,
}

impl Default for MergeQueuePolicy {
    fn default() -> Self {
        MergeQueuePolicy {
            enabled: false,
            max_batch: MERGE_QUEUE_MAX_BATCH_DEFAULT,
            // Same default and same bounds as a notify watch's TTL, from the
            // one definition — see the clamp in [`parse_workflow`].
            checks_timeout_minutes: NOTIFY_EXPIRES_DEFAULT_MIN,
        }
    }
}

// ── board: per-status WIP limits (#1175 / #1170 A2) ────────────────────────

/// The smallest cap that means anything. `0` would say "nothing may ever enter
/// this status", which is a *stop*, not a work-in-progress limit — and under
/// `enforce` it would wedge the board rather than pace it. Refused at parse
/// time, the posture `merge_queue.max_batch` and `resources.slots` take.
pub const WIP_LIMIT_MIN: u32 = 1;

/// The one status a cap may **not** name, and the reason it may not.
///
/// `done` is terminal and it is the *relief valve*: every other cap is
/// relieved by work reaching it. A limit there would refuse the very
/// transition that unblocks the board — the exact inversion of what a WIP
/// limit is for — so the wire struct simply has no field for it and
/// `deny_unknown_fields` refuses `done:` with an error naming the statuses
/// that ARE cappable. Stated as a constant so the docs, the error path and
/// the test that pins the field set all read the same name.
pub const WIP_UNCAPPABLE_STATUS: &str = "done";

/// The `board:` block — what a repo declares about how much work may sit in
/// each board status at once (#1175; the practice is kanban's WIP limit, and
/// the loomux-specific motivation is in `doc/design/board-wip.md`).
///
/// **Policy, not mechanism** (CLAUDE.md constraint 8). Nothing here names a
/// toolchain, a branch, a repo path or an agent: the whole schema is a handful
/// of integers and one bool, keyed by loomux's own board statuses.
///
/// **Restrict-only, like `resources:` before it.** A `board:` block can make a
/// write wait or warn; it can never grant a capability, and it cannot reach
/// the capability-closure spine (`blocks:`/`edges:`/`gates:`). The worst a
/// hostile `.loomux/workflow.yml` can do with it is declare `in-progress: 1`
/// and slow a group down — and even that only bounces *agent* writes, never
/// the human's own board edits (see [`BoardPolicy::enforce`]).
///
/// **An absent block means the feature is off**: no `board:` at all leaves
/// [`BoardPolicy::wip`] empty, no write is ever counted, and behavior is
/// byte-for-byte what it was. Same posture — and the same
/// `deny_unknown_fields` consequence for older builds — as `merge_queue:`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoardPolicy {
    /// Per-status caps, keyed by the board status exactly as it is spelled on
    /// the wire (`in-progress`, `human-testing`, …). A status **absent from
    /// this map has no cap** — this is not a map with defaults, it is the set
    /// of limits the repo actually declared. Empty = the feature is off.
    ///
    /// Kept as a map rather than as the wire struct so every consumer
    /// (accounting, the refusal text, the board's own chips) stays
    /// status-generic: the closed struct exists to *validate* the names, not
    /// to be the shape the rest of loomux reasons about.
    pub wip: BTreeMap<String, u32>,
    /// **Default false**, which is warn-and-notify. `true` makes an
    /// **agent-origin** write that would cross a cap a hard refusal.
    ///
    /// It never applies to the human's own board edits, under either setting.
    /// The board's authority is the human's, not a queue discipline — the same
    /// reason `claim` is deliberately not exposed on the human's board command
    /// — and a limit a human set for their agents must not bounce the human
    /// who set it. Their crossing still warns and still audits, so the
    /// orchestrator learns the board moved; it is only ever the *refusal* that
    /// is agent-only.
    pub enforce: bool,
}

// ── intake: source + label vocabulary (#382 P1) ────────────────────────────
//
// Where autonomous work comes from and what its label vocabulary is called —
// the missing sibling of `gates:` ("what gates it" beside "where it comes
// from"). Inert vocabulary + an adapter choice from a fixed set, the same
// capability-closure argument as `blocks:` above: `intake:` can never grant a
// capability, and there is deliberately **no spelling that can disable the
// human merge gate** — that lives in the `gh` shim, keyed to group markers,
// and is not reachable from this file at all. `deny_unknown_fields` on
// `RawIntake`/`RawIntakeLabels` (below) makes a `human_gate: false`-style key
// a hard parse error by construction, not a line this schema has to
// specifically recognize and reject.

/// Where intake work comes from. A workflow file *selects* one of these; it
/// can never define a new one — same "reject, never coerce" posture as
/// [`kind_from_str`].
///
/// `Board` and `None` are **schema-reserved, not wired**: the #382 plan ships
/// the `github-labels` adapter fully in this slice and designs the other two
/// into the schema now so the config contract never churns, but their runtime
/// (a non-`gh` poll source, tracker-agnostic loop prose) is a follow-on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntakeSource {
    /// Poll `gh issue list`, match label strings client-side. Today's only
    /// wired behavior, and the built-in default.
    GithubLabels,
    /// The loomux task board is the queue. Schema-reserved (Phase B).
    Board,
    /// No autonomous intake at all — idle-tick still runs its other chores
    /// (PR-sweep, lost-notification backstop) but never polls for labelled
    /// work. Schema-reserved (Phase B); valid to declare today even though
    /// nothing yet reads it, since P4 wires the consumers.
    None,
}

impl IntakeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            IntakeSource::GithubLabels => "github-labels",
            IntakeSource::Board => "board",
            IntakeSource::None => "none",
        }
    }
}

impl Default for IntakeSource {
    fn default() -> Self {
        IntakeSource::GithubLabels
    }
}

/// Parse an `intake.source:` value. Trimmed empty string maps to the built-in
/// default (`github-labels`) — matching the plan's `source: github-labels
/// (default)` schema comment, and letting a repo override just the labels
/// without repeating a source it already means. Anything else unrecognized is
/// `None`, which the caller turns into a hard, allowed-set-naming error —
/// never coerced, the same shape [`kind_from_str`] enforces.
pub fn intake_source_from_str(s: &str) -> Option<IntakeSource> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "github-labels" => Some(IntakeSource::GithubLabels),
        "board" => Some(IntakeSource::Board),
        "none" => Some(IntakeSource::None),
        _ => None,
    }
}

/// The intake sources a workflow file may name, for error messages.
pub fn intake_source_names() -> String {
    "github-labels, board, none".to_string()
}

/// The resolved intake policy: **one source of truth**, always present (the
/// built-in default when a repo declares nothing, or declares only part of
/// it), persisted in `group.json` beside `blocks` and read by every consumer
/// that needs "what counts as intake" — the template renderer, `gh.rs`'s
/// label allow-list, `idle_tick_notice()`, and the #332 host poller (P2-P4,
/// separate PRs; this struct is the shared contract they all read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeProfile {
    pub source: IntakeSource,
    /// "Build this." The real repo label spelling in the built-in profile.
    pub ready: String,
    /// "Look, don't build."
    pub investigate: String,
    /// "Mine" — ownership marker.
    pub owned: String,
    /// Demo-gated. Optional in the sense that a repo may omit it (falls back
    /// to the built-in default) — every label field works this way, see
    /// [`sanitize_intake_label`].
    pub prototype: String,
    /// "Held by the human — do not start this" (#778). The one label whose
    /// meaning is a *veto* rather than a selector: under full autonomy the
    /// start default inverts (every open issue is eligible), and this is the
    /// boundary that stays opt-**out**. Vocabulary only, like every field
    /// here — it names which label the host poller must treat as a hold, and
    /// can never grant anything.
    pub hold: String,
}

impl Default for IntakeProfile {
    fn default() -> Self {
        builtin_intake_profile()
    }
}

/// The built-in `github-labels` profile — a checked-in, independent value,
/// not derived from anything else. This is the plan's dodge for the golden
/// self-reference trap (P2): the byte-golden fixture and this const are two
/// separate things that can each move, so a golden test comparing a live
/// render against frozen bytes catches either one drifting, instead of a
/// render-against-itself tautology.
///
/// Reproduces today's vocabulary exactly (`agent-ready` /
/// `agent-investigation` / `agent-managed` / `agent-prototype`) — the labels
/// this repo's own `orchestrator.md` prose and `gh.rs`'s `ALLOWED_LABELS`
/// hardcode today, pre-#382.
pub fn builtin_intake_profile() -> IntakeProfile {
    IntakeProfile {
        source: IntakeSource::GithubLabels,
        ready: "agent-ready".to_string(),
        investigate: "agent-investigation".to_string(),
        owned: "agent-managed".to_string(),
        prototype: "agent-prototype".to_string(),
        hold: "agent-hold".to_string(),
    }
}

/// A single `intake.labels.<field>:` value. Sanitized like a block id
/// ([`sanitize_id`]) — the same conservative alphabet, because a label string
/// eventually reaches a `gh issue list --label` argument and a template
/// substitution. **Rejected, not rewritten**, matching every other
/// user-authored identifier in this file: an author who wrote a label with a
/// space must see an error, not a silently different string their own repo's
/// labels no longer match.
///
/// Empty (omitted) is not a rejection — it falls back to `fallback` (the
/// built-in default for that field), which is what lets a repo override
/// `intake.labels.ready:` alone and inherit the other four.
///
/// **A LEADING `-` is rejected on top of [`sanitize_id`]'s alphabet**, which
/// permits `-` freely (rev-648 NB4). A label is not only compared against
/// GitHub's — the hold spelling becomes a **positional** argument to
/// `gh label create <name> …`, and a positional beginning with a dash is read
/// by cobra as an unknown flag. That is not an injection (nothing is executed,
/// and the create fails loudly), but it is a class of value that can never
/// work, and rejecting it here is what lets `gh.rs` state — truthfully — that
/// nothing flag-shaped reaches an argv. `--force` and `-x` are nonsense as
/// label names for all five fields, so nothing legitimate is lost. Interior and
/// trailing dashes (`agent-hold`, `do-not-touch`) are untouched.
fn sanitize_intake_label(field: &str, raw_val: &str, fallback: &str, errs: &mut Vec<String>) -> String {
    let v = raw_val.trim();
    if v.is_empty() {
        return fallback.to_string();
    }
    match sanitize_id(v) {
        Some(clean) if clean == v && !clean.starts_with('-') => clean,
        _ => {
            errs.push(format!(
                "intake.labels.{field}: {v:?} is not a usable label (letters, digits, '-', '_'; \
                 and it may not begin with '-')"
            ));
            fallback.to_string()
        }
    }
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
    /// Intake source + label vocabulary (#382 P1). Always resolved — the
    /// built-in default when the file declares no `intake:` block at all, or
    /// only part of one.
    pub intake: IntakeProfile,
    /// Merge-queue policy (#581 §11.2). Always resolved; the default is
    /// **disabled**, which is what an absent `merge_queue:` block means.
    pub merge_queue: MergeQueuePolicy,
    /// Named lock resources (#858), keyed by the repo's own name for each.
    /// **Empty** when the file declares no `resources:` block — which is what
    /// turns the lock tools off for the group entirely.
    pub resources: BTreeMap<String, ResourcePolicy>,
    /// Board policy — per-status WIP limits (#1175). Always resolved; the
    /// default carries **no limits at all**, which is what an absent `board:`
    /// block means.
    pub board: BoardPolicy,
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
    default_roster_ex(
        &pins.iter().map(|(k, c, m)| (*k, *c, *m, ModelKnobs::default())).collect::<Vec<_>>(),
    )
}

/// [`default_roster`] plus the per-role model knobs the launcher can pin
/// (#687). The 3-tuple form above stays, and stays the one most callers use:
/// every roster loomux synthesizes *on a group's behalf* (`clamped()`'s
/// guaranteed orchestrator, the legacy-group.json reconstruction, the built-in
/// roster) pins no knob by construction, so making them all spell
/// `ModelKnobs::default()` would be noise — and leaving those call sites
/// untouched keeps every existing assertion about their command lines a pin on
/// "no knobs ⇒ the pre-#687 line, byte for byte".
pub fn default_roster_ex(pins: &[(Role, &str, &str, ModelKnobs<'_>)]) -> Vec<Block> {
    pins.iter()
        .map(|(kind, cli, model, knobs)| Block {
            id: kind.as_str().to_string(),
            name: kind.as_str().to_string(),
            kind: *kind,
            cli: cli.trim().to_string(),
            model: model.trim().to_string(),
            prompt: None,
            profile: None,
            allow: Vec::new(),
            role_hint: None,
            // Raw as picked; `Guardrails::clamped` is what drops a value the
            // resolved CLI cannot honor (the same treatment `model` gets).
            effort: knobs.effort.trim().to_string(),
            context: knobs.context.trim().to_string(),
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

/// Block ids reach the shell (folded into a generated custom-agent file's
/// `loomux-<group>-<id>` handle, behind a `--agent <handle>` flag) and the
/// filesystem (`<id>.md` in the group dir, and as part of that generated
/// file's own name). Keep them to a conservative identifier alphabet so
/// neither surface can be escaped — the `sanitize_model` precedent, applied
/// to identity. Returns `None` for an id with no usable characters left.
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

/// A routing rule's path glob (#1176), on the same reject-never-rewrite
/// contract as [`sanitize_condition`]: the filtered string comes back, and
/// `parse_workflow` refuses anything the filter had to change.
///
/// **The alphabet is `A-Za-z0-9._-/` plus `*`, and nothing else.** The glob is
/// interpolated *unquoted into a POSIX `case` pattern* in the `gh` shim, so
/// every character it may contain has to be one the shell reads as either a
/// literal or `*`. `[`, `]`, `\`, `{`, `}`, whitespace and quotes are all
/// shell-pattern or word-splitting syntax, and none of them appears here.
///
/// **`?` is excluded deliberately, and it is the interesting omission.** It
/// would be trivial to allow — and it is the one character whose meaning the
/// two implementations could not be made to *provably* share: shell `case`
/// matches one character in the shell's locale (one byte in the C locale that
/// `sh` usually runs under), while a Rust mirror matches either a byte or a
/// `char`, and the two answers differ on the first non-ASCII path anybody
/// commits. With `*` as the only metacharacter, "any run of characters" and
/// "any run of bytes" are the same set, and the shim/mirror agreement is a
/// property of the alphabet rather than a claim in a PR body. Nobody routing
/// reviewers by area needs a single-character wildcard.
///
/// Three shapes are refused outright rather than filtered, all for one reason —
/// **a rule that could never fire is the unsatisfiable-gate failure this file
/// refuses everywhere else**, and a routing rule that never fires silently
/// removes a reviewer the repo asked for:
///
/// - a **leading `/`** — GitHub reports changed paths repo-relative, so
///   `/src/**` matches nothing;
/// - a **trailing `/`** — every changed path names a file, never a directory,
///   so `src/` matches nothing (`src/**` is what the author meant);
/// - a **`..` path segment** — no changed path GitHub reports contains one.
///   (Not a containment check: nothing here is joined onto a root. It is the
///   same never-fires argument.)
pub fn sanitize_glob(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.ends_with('/')
        || trimmed.split('/').any(|seg| seg == "..")
    {
        return None;
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '*'))
        .take(MAX_GLOB_CHARS)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Does one [`sanitize_glob`]-clean pattern match one repo-relative changed
/// path? **The whole glob contract of #1176, and the thing the shim's `case`
/// mirrors.**
///
/// 1. `*` matches any run of characters, **including `/`**.
/// 2. Every other character matches itself.
/// 3. A leading `**/` is **optional**: `**/Cargo.toml` matches `Cargo.toml` as
///    well as `crates/a/Cargo.toml`.
/// 4. The match is anchored at both ends — the pattern describes the whole
///    path, not a substring of it.
///
/// **Rule 1 is coarser than gitignore's on purpose**, and the argument is the
/// direction of the error. Routing decides *which reviewers are required*:
/// over-matching adds a lane (an extra review nobody needed), under-matching
/// skips one (a merge the repo said needed that lane). Only one of those is
/// survivable, so the semantics err toward matching. Coarse also buys the thing
/// this feature actually has to pay for: `*`-crossing-`/` is exactly what a
/// POSIX `case` does for free, so the shim and this function are the same
/// matcher rather than two hand-written ones that agree today.
///
/// Rule 3 exists because `**/X` is the natural spelling of "every X" and under
/// rules 1–2 alone it would silently miss the one at the repo root — a *skipped*
/// reviewer, the unsurvivable direction. `**` anywhere else is simply `*`
/// repeated, which matches the same set.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    if star_match(pattern.as_bytes(), path.as_bytes()) {
        return true;
    }
    // Rule 3. `strip_prefix` rather than a general "collapse `**`" pass: the
    // ONLY place `**` means more than `*` is this one, and a rule the shim can
    // state in a two-line `case` is a rule the two implementations can be shown
    // to share.
    match pattern.strip_prefix("**/") {
        Some(rest) => star_match(rest.as_bytes(), path.as_bytes()),
        None => false,
    }
}

/// Anchored `*`-only wildcard match, iterative with one backtrack point — the
/// classic greedy algorithm, so a pathological pattern cannot blow the stack the
/// way a naive recursive matcher can. Bytes rather than `char`s because the
/// alphabet [`sanitize_glob`] permits is ASCII and `*` spans whole runs either
/// way; see that function for why `?` is not in it.
fn star_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0usize, 0usize);
    // `star` = the last `*` in the pattern we can fall back to; `mark` = the
    // input position it had consumed up to when we took that branch.
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while i < s.len() {
        if p < pat.len() && pat[p] == s[i] {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = p;
            mark = i;
            p += 1;
        } else if star != usize::MAX {
            // Mismatch after a `*`: let that `*` swallow one more byte.
            p = star + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    // Trailing `*`s may match nothing; anything else left over is a mismatch,
    // which is what makes this anchored at the end.
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
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

/// Strips characters that could be structurally hazardous wherever persona
/// text ends up — a generated agent FILE (Claude's `~/.claude/agents/*.md`,
/// round #417 correction 6; Copilot's `~/.copilot/agents/*.agent.md`, #416)
/// or PTY-typed kickoff text (Copilot's write-failure fallback). Control
/// characters other than newline/tab are dropped outright: they have no
/// meaning in a persona and would ride straight into a terminal.
///
/// The `'` → typographic-apostrophe (U+2019) mapping predates round 6: it
/// protected the SINGLE-QUOTED shell token `claude --agents '<json>'` this
/// text used to ride on, before that mechanism was replaced with a
/// generated file (see `PersonaInject::claude_agent`'s doc for why — the
/// argv-length bug the replacement fixes). No current consumer is a raw
/// shell token, so this mapping is inert today — kept rather than removed,
/// both because it costs nothing (the prose still reads fine: "don't"
/// stays "don't", just with a curlier mark) and as defense-in-depth against
/// a future consumer reintroducing a shell-token use without re-deriving
/// this exact hazard from scratch. `ascii_escape_json`, which existed
/// solely to keep the OLD `--agents` JSON payload pure-ASCII on a
/// non-UTF-8 pane code page, had no other consumer and was removed
/// entirely alongside that mechanism, rather than left orphaned.
pub fn sanitize_persona(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\'' { '\u{2019}' } else { c })
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
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

#[derive(Deserialize, Serialize)]
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
    /// Intake source + label vocabulary (#382 P1). `None` when the file
    /// declares no `intake:` block — resolved to [`builtin_intake_profile`]
    /// entirely. **No `human_gate:`/disable-spelling key exists on this type
    /// or `RawIntakeLabels`** — `deny_unknown_fields` turns any attempt at one
    /// into a hard parse error rather than an ignored line. This is the whole
    /// enforcement of the CRITICAL invariant: the human merge gate is not
    /// reachable from this schema at all.
    #[serde(default)]
    intake: Option<RawIntake>,
    /// Merge-queue policy (#581 §11.2). `None` when the file declares no
    /// `merge_queue:` block — which resolves to
    /// [`MergeQueuePolicy::default`], i.e. **off**, and behavior is
    /// byte-for-byte unchanged.
    ///
    /// Like `intake:`, this block can never grant a capability: every field is
    /// a bool or a number, there is no spelling that names a branch to land on
    /// (§4 — the target comes from the enqueued PR's live base), and the
    /// default-branch refusals (§7) are not reachable from this schema at all.
    #[serde(default)]
    merge_queue: Option<RawMergeQueue>,
    /// Named lock resources (#858). Absent (or empty) means no group in this
    /// repo gets the lock tools at all.
    ///
    /// Like `intake:` and `merge_queue:`, this block can never grant a
    /// capability: every field is a number, there is no spelling that names a
    /// program, a path, or an agent, and `deny_unknown_fields` on
    /// [`RawResource`] makes an attempt at one a hard parse error rather than
    /// an ignored line.
    #[serde(default)]
    resources: BTreeMap<String, RawResource>,
    /// Board policy — per-status WIP limits (#1175). `None` when the file
    /// declares no `board:` block, which resolves to [`BoardPolicy::default`],
    /// i.e. **no limits**, and behavior is byte-for-byte unchanged.
    ///
    /// Like `intake:`, `merge_queue:` and `resources:`, this block can never
    /// grant a capability: every field is a bool or a number, there is no
    /// spelling that names a program, a path, an agent or a branch, and
    /// `deny_unknown_fields` on [`RawBoard`]/[`RawWip`] makes an attempt at
    /// one a hard parse error rather than an ignored line.
    #[serde(default)]
    board: Option<RawBoard>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawBoard {
    #[serde(default)]
    wip: Option<RawWip>,
    #[serde(default)]
    enforce: bool,
}

/// The wire shape of `board.wip` — **one optional field per cappable board
/// status**, deliberately closed rather than a `BTreeMap<String, u32>`.
///
/// A map would accept `in-porgress: 4` and declare a limit on nothing, in
/// silence, for the lifetime of the file: an open key namespace cannot tell a
/// typo from a status a newer loomux might have. The closed struct hands that
/// check to `deny_unknown_fields`, whose error already names every field it
/// *would* have accepted — so the repo that misspells a status is told which
/// spellings exist, at parse time, without this module writing the check.
///
/// The price is that the field list is a second copy of `TASK_STATUSES` (which
/// lives in `src-tauri`, on the other side of an arrow this crate may not
/// point back along). It is pinned, not trusted:
/// `src-tauri/tests/workflow.rs` asserts this struct's serde field names
/// are exactly `TASK_STATUSES` minus [`WIP_UNCAPPABLE_STATUS`], so a ninth
/// status reddens rather than quietly arriving uncappable.
///
/// `Option<u32>` rather than a defaulted number so "omitted" and "written as
/// 1" stay distinguishable: the parse refuses a zero, and refusing one the
/// author never wrote would be an error about nothing (the reasoning
/// [`RawResource::slots`] states).
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawWip {
    #[serde(default)]
    queued: Option<u32>,
    #[serde(default, rename = "in-progress")]
    in_progress: Option<u32>,
    #[serde(default)]
    review: Option<u32>,
    #[serde(default)]
    pr: Option<u32>,
    #[serde(default)]
    prototype: Option<u32>,
    #[serde(default, rename = "human-testing")]
    human_testing: Option<u32>,
    #[serde(default)]
    blocked: Option<u32>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawResource {
    /// `Option` rather than a defaulted number so "omitted" and "written as 1"
    /// are distinguishable — the parse below rejects a zero, and rejecting one
    /// the author never wrote would be an error about nothing. (Same reasoning
    /// as [`RawMergeQueue::max_batch`].)
    #[serde(default)]
    slots: Option<u32>,
    #[serde(default)]
    max_hold_minutes: Option<u32>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawMergeQueue {
    #[serde(default)]
    enabled: bool,
    /// `Option` rather than a defaulted number so "omitted" and "written as 3"
    /// are distinguishable — the parse below rejects a zero, and rejecting one
    /// the author never wrote would be an error about nothing.
    #[serde(default)]
    max_batch: Option<u32>,
    #[serde(default)]
    checks_timeout_minutes: Option<u32>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawIntake {
    #[serde(default)]
    source: String,
    #[serde(default)]
    labels: RawIntakeLabels,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct RawIntakeLabels {
    #[serde(default)]
    ready: String,
    #[serde(default)]
    investigate: String,
    #[serde(default)]
    owned: String,
    #[serde(default)]
    prototype: String,
    #[serde(default)]
    hold: String,
}

#[derive(Deserialize, Serialize)]
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
    #[serde(default)]
    role_hint: Option<String>,
    #[serde(default)]
    effort: String,
    #[serde(default)]
    context: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawEdge {
    from: String,
    to: OneOrMany,
}

#[derive(Deserialize, Serialize)]
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
    #[serde(default)]
    max_diff_lines: Option<u32>,
    #[serde(default)]
    routing: Vec<RawRoutingRule>,
}

/// One `gates.merge.routing[]` entry (#1176). `deny_unknown_fields` like every
/// other `Raw*` type: a misspelled `path:`/`reviewer:` is a refusal, not a rule
/// that silently routes nothing.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRoutingRule {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    reviewers: Vec<String>,
}

/// `to: worker` and `to: [rev-a, rev-b]` are both legal — a fan-out reads
/// naturally as a list and a single hand-off reads naturally as a scalar.
#[derive(Deserialize, Serialize)]
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

/// Every field name this schema accepts, per section, **derived from the types
/// that do the accepting** (#880). Section names match `src/workflow-schema.json`.
///
/// The GUI's failure mode this exists to kill: `allow:` has been a real
/// `RawBlock` field since #222 and the workflow pane never grew a control for
/// it — nor even knew the key — so a workflow that declared it looked, in the
/// pane, like a workflow that didn't. Nothing was wrong with either side; the
/// two simply had no way to disagree out loud. Now they do:
/// `tests/orchestration.rs` compares this against the committed manifest, so a
/// field added here without an editor is a red test rather than a hole nobody
/// finds until a human wonders where their line went.
///
/// **Serialized, not hand-listed**, deliberately: a hand-written list is
/// exactly the thing that drifts, and it would drift *silently* in the one
/// direction that matters (a new field, forgotten). serde already knows every
/// field name — `deny_unknown_fields` is that same knowledge pointed the other
/// way — so asking it is the only spelling of this that cannot go stale.
///
/// Every value below is populated rather than left at its zero — no `Option` is
/// `None`, no collection is empty — so that the key sets do not depend on what
/// the instances happen to hold. **What actually guarantees that is the parity
/// test, not the population**: there is no `skip_serializing_if` on any of these
/// types today, and one added tomorrow would turn that test RED (the manifest
/// still declares the row) rather than quietly shrinking this map. Maximal
/// values make the failure *impossible to reach by accident*; the test is what
/// makes it impossible to reach at all.
///
/// `#[doc(hidden)]` and `pub` only because the pin lives in an integration test
/// (CLAUDE.md constraint 4) and the `Raw*` types themselves stay private: this
/// is a test seam, not API.
#[doc(hidden)]
pub fn workflow_schema_keys() -> BTreeMap<String, Vec<String>> {
    fn keys_of<T: Serialize>(section: &str, v: &T) -> Vec<String> {
        match serde_json::to_value(v) {
            Ok(serde_json::Value::Object(map)) => map.keys().cloned().collect(),
            other => panic!("{section}: expected a mapping, got {other:?}"),
        }
    }
    let block = RawBlock {
        id: "b".into(),
        name: "B".into(),
        kind: "worker".into(),
        cli: "claude".into(),
        model: "opus".into(),
        prompt: Some("p".into()),
        profile: Some(".github/agents/b.md".into()),
        allow: vec!["Bash(gh pr view)".into()],
        role_hint: Some("process".into()),
        effort: "high".into(),
        context: "1m".into(),
    };
    let edge = RawEdge { from: "a".into(), to: OneOrMany::One("b".into()) };
    let gate = RawGate {
        require: Some("all-pass".into()),
        threshold: Some(1),
        reviewers: vec!["rev".into()],
        also: vec!["ci-green".into()],
        max_diff_lines: Some(800),
        routing: vec![RawRoutingRule {
            paths: vec!["src/**".into()],
            reviewers: vec!["rev".into()],
        }],
    };
    let labels = RawIntakeLabels {
        ready: "agent-ready".into(),
        investigate: "agent-investigation".into(),
        owned: "agent-managed".into(),
        prototype: "agent-prototype".into(),
        hold: "agent-hold".into(),
    };
    let intake = RawIntake { source: "github-labels".into(), labels: RawIntakeLabels::default() };
    let merge_queue =
        RawMergeQueue { enabled: true, max_batch: Some(3), checks_timeout_minutes: Some(60) };
    let resource = RawResource { slots: Some(1), max_hold_minutes: Some(30) };
    // Every field populated, per this function's docblock: a `None` here would
    // drop the key from the serialization and shrink the manifest silently.
    let wip = RawWip {
        queued: Some(8),
        in_progress: Some(4),
        review: Some(3),
        pr: Some(3),
        prototype: Some(2),
        human_testing: Some(2),
        blocked: Some(4),
    };
    let mut out = BTreeMap::new();
    out.insert("board.wip".to_string(), keys_of("board.wip", &wip));
    let board = RawBoard { wip: Some(wip), enforce: true };
    // Read before `workflow` takes ownership of its own copies below — the point
    // of populating them there is that no field of the top-level type is left at
    // a zero value.
    out.insert("block".to_string(), keys_of("block", &block));
    out.insert("edge".to_string(), keys_of("edge", &edge));
    out.insert("gate".to_string(), keys_of("gate", &gate));
    // #1176. Its own section for the same reason `intake.labels` has one: a
    // routing rule is a mapping with its own field set, and a section that only
    // said "gate.routing is a list" would leave `paths:`/`reviewers:` outside
    // every guarantee this manifest exists to give.
    out.insert(
        "gate.routing".to_string(),
        keys_of(
            "gate.routing",
            &RawRoutingRule { paths: vec!["src/**".into()], reviewers: vec!["rev".into()] },
        ),
    );
    out.insert("intake".to_string(), keys_of("intake", &intake));
    out.insert("intake.labels".to_string(), keys_of("intake.labels", &labels));
    out.insert("merge_queue".to_string(), keys_of("merge_queue", &merge_queue));
    out.insert("resource".to_string(), keys_of("resource", &resource));
    out.insert("board".to_string(), keys_of("board", &board));
    let workflow = RawWorkflow {
        version: SCHEMA_VERSION,
        name: "w".into(),
        authored_with: "loomux".into(),
        blocks: vec![block],
        edges: vec![edge],
        gates: BTreeMap::from([("merge".to_string(), gate)]),
        intake: Some(intake),
        merge_queue: Some(merge_queue),
        resources: BTreeMap::from([("build".to_string(), resource)]),
        board: Some(board),
    };
    out.insert("workflow".to_string(), keys_of("workflow", &workflow));
    out
}

/// What the engine says a field may CONTAIN — the other half of
/// [`workflow_schema_keys`] (#880 review finding 1), keyed `"section.field"`.
///
/// Names alone were never the whole manifest: `src/workflow-schema.json` also
/// carries each field's closed value set, its default and its bounds, and slice
/// C generates form controls from exactly those. A wrong enum row today is a
/// wrong `<select>` later — a `block.cli` picker with no option for "inherit the
/// group's CLI" cannot represent the state most blocks are actually in — so
/// these are pinned against the engine the same way the field names are.
///
/// **Derived wherever the engine has an accessor**: `SUPPORTED_CLIS`,
/// [`kind_names`], [`role_hint_names`], [`intake_source_names`],
/// [`builtin_intake_profile`], `MergeQueuePolicy::default()`,
/// `ResourcePolicy::default()`, and the `RESOURCE_*` / `NOTIFY_EXPIRES_*` /
/// `RESOURCES_MAX` constants. Wire defaults for the plain `#[serde(default)]`
/// fields are derived too — by deserializing a minimal document and serializing
/// it back, so serde states them rather than this function guessing.
///
/// **One set is hand-listed and it is named as such**: `gate.require`, whose
/// accepted spellings exist only as match arms in [`parse_workflow`] (there is
/// no accessor to ask). Adding an arm without adding it here leaves the manifest
/// stale with nothing red — the one hole left in this pin, stated out loud
/// rather than papered over.
#[doc(hidden)]
pub fn workflow_schema_field_facts() -> BTreeMap<String, serde_json::Value> {
    use serde_json::{json, Value};

    /// The values serde fills in for a field the document omits — asked of
    /// serde, not typed out here. `null` (an absent `Option`) is not a default:
    /// it means the key simply isn't there, which is a different statement from
    /// "it defaults to nothing".
    fn wire_defaults<T>(minimal: &str, required: &[&str]) -> Vec<(String, Value)>
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let parsed: T = serde_json::from_str(minimal)
            .unwrap_or_else(|e| panic!("the minimal document must deserialize: {e}"));
        match serde_json::to_value(&parsed) {
            Ok(Value::Object(map)) => map
                .into_iter()
                .filter(|(k, v)| !v.is_null() && !required.contains(&k.as_str()))
                .collect(),
            other => panic!("expected a mapping, got {other:?}"),
        }
    }

    let names = |csv: String| -> Vec<String> { csv.split(", ").map(str::to_string).collect() };
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    let mut fact = |key: &str, k: &str, v: Value| {
        let entry = out.entry(key.to_string()).or_insert_with(|| json!({}));
        entry[k] = v;
    };

    for (section, defaults) in [
        ("workflow", wire_defaults::<RawWorkflow>(r#"{"version":1}"#, &["version"])),
        ("block", wire_defaults::<RawBlock>(r#"{"id":"b","kind":"worker"}"#, &["id", "kind"])),
        ("gate", wire_defaults::<RawGate>("{}", &[])),
        ("gate.routing", wire_defaults::<RawRoutingRule>("{}", &[])),
        ("intake", wire_defaults::<RawIntake>("{}", &[])),
        ("intake.labels", wire_defaults::<RawIntakeLabels>("{}", &[])),
        ("merge_queue", wire_defaults::<RawMergeQueue>("{}", &[])),
        ("resource", wire_defaults::<RawResource>("{}", &[])),
        ("board", wire_defaults::<RawBoard>("{}", &[])),
        // Deliberately contributes nothing: every field of `RawWip` is an
        // `Option` with no wire default, because an omitted status is the
        // ABSENCE of a cap and not a cap with a default value. Listed anyway
        // so a field that ever does gain one is picked up here rather than
        // needing this loop remembered.
        ("board.wip", wire_defaults::<RawWip>("{}", &[])),
    ] {
        for (field, value) in defaults {
            // A nested section's own default is the section, not a value a form
            // ever renders — `intake.labels` is described by its own rows.
            if value.is_object() || (value.is_array() && section == "workflow") {
                continue;
            }
            fact(&format!("{section}.{field}"), "default", value);
        }
    }

    // Closed value sets, from the accessors that already state them for error
    // messages — so a fifth entry in SUPPORTED_CLIS reddens this rather than
    // leaving the manifest quietly stale.
    let mut cli_values = vec![String::new()]; // an empty `cli:` inherits the group's
    cli_values.extend(SUPPORTED_CLIS.iter().map(|c| c.to_string()));
    fact("block.cli", "values", json!(cli_values));
    fact("block.kind", "values", json!(names(kind_names())));
    fact("block.role_hint", "values", json!(names(role_hint_names())));
    let mut sources = vec![String::new()]; // `intake_source_from_str`: "" is the default source
    sources.extend(names(intake_source_names()));
    fact("intake.source", "values", json!(sources));
    // HAND-LISTED — see this function's docblock. Mirrors `parse_workflow`'s gate
    // match arms (`Some("all-pass") | Some("all")` and `Some("threshold")`).
    fact("gate.require", "values", json!(["all-pass", "all", "threshold"]));

    // Effective defaults: what the engine BEHAVES as when the key is omitted,
    // which is what a form shows as its placeholder. Distinct from the wire
    // defaults above — `merge_queue.max_batch` is `None` on the wire and 3 in
    // effect — and taken from the same `Default` impls the parse resolves against.
    fact("workflow.version", "default", json!(SCHEMA_VERSION));
    fact("gate.require", "default", json!("all-pass"));
    let intake = builtin_intake_profile();
    fact("intake.source", "default", json!(intake.source.as_str()));
    fact("intake.labels.ready", "default", json!(intake.ready));
    fact("intake.labels.investigate", "default", json!(intake.investigate));
    fact("intake.labels.owned", "default", json!(intake.owned));
    fact("intake.labels.prototype", "default", json!(intake.prototype));
    fact("intake.labels.hold", "default", json!(intake.hold));
    let mq = MergeQueuePolicy::default();
    fact("merge_queue.enabled", "default", json!(mq.enabled));
    fact("merge_queue.max_batch", "default", json!(mq.max_batch));
    fact("merge_queue.checks_timeout_minutes", "default", json!(mq.checks_timeout_minutes));
    let res = ResourcePolicy::default();
    fact("resource.slots", "default", json!(res.slots));
    fact("resource.max_hold_minutes", "default", json!(res.max_hold_minutes));

    // Bounds. `min` with no `max` is a floor the parse refuses below and nothing
    // above; the refuse-vs-clamp half is pinned behaviorally by the test, because
    // it is a fact about what `parse_workflow` DOES, not one this file can assert.
    fact("gate.threshold", "min", json!(1));
    // #1174. A floor, no ceiling: "how big is too big" is this repo's call and
    // loomux has no business inventing an upper bound for it. `0` is refused
    // rather than read as "unlimited" — a gate clause that gates nothing is a
    // typo, and the way to mean "no limit" is to omit the key.
    fact("gate.max_diff_lines", "min", json!(1));
    fact("merge_queue.max_batch", "min", json!(1));
    fact("merge_queue.checks_timeout_minutes", "min", json!(NOTIFY_EXPIRES_MIN));
    fact("merge_queue.checks_timeout_minutes", "max", json!(NOTIFY_EXPIRES_MAX));
    fact("resource.slots", "min", json!(1));
    fact("resource.slots", "max", json!(RESOURCE_SLOTS_MAX));
    fact("resource.max_hold_minutes", "min", json!(1));
    fact("resource.max_hold_minutes", "max", json!(RESOURCE_MAX_HOLD_MINUTES_MAX));
    // Cardinality: the sections with a cap on how many entries they may hold.
    fact("workflow.resources", "max_entries", json!(RESOURCES_MAX));
    // Every WIP cap has the same floor and deliberately no ceiling: a limit
    // above the board's own size degenerates to "no limit", which is what the
    // author asked for, so there is nothing for loomux to refuse (the posture
    // `merge_queue.max_batch` takes, and the opposite of `resources.slots`,
    // where the ceiling is a legibility claim about serialization). Derived
    // from the wire struct's own field list rather than re-typed here — the
    // eighth status must not arrive bound-less because this loop was written
    // out by hand.
    for status in workflow_schema_keys().get("board.wip").into_iter().flatten() {
        fact(&format!("board.wip.{status}"), "min", json!(WIP_LIMIT_MIN));
    }
    // #1176. Both are bounds on work the SHIM does on the merge path — every
    // rule against every changed file, in shell — not on what a form can render.
    fact("gate.routing", "max_entries", json!(ROUTING_RULES_MAX));
    fact("gate.routing.paths", "max_entries", json!(ROUTING_PATHS_MAX));

    out
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
        // #1161. Declarable, but not spawnable and not part of any built-in
        // roster: `spawn_agent` refuses it exactly as it refuses
        // `orchestrator`, and `builtin_roster` never synthesizes one.
        "manager" => Some(Role::Manager),
        _ => None,
    }
}

/// At most one `kind: manager` block per workflow (#1161).
///
/// Unlike reviewers — deliberately fanned out — two human interfaces is a
/// coherence bug rather than a configuration: the human would have two panes
/// each holding half a conversation, and everything downstream that says "the
/// manager" (the pane badge, the orchestrator's relay target, the mailbox in
/// M2) would have to pick one and silently ignore the other. Refused at parse,
/// where an author can still fix it.
pub const MANAGER_MAX: usize = 1;

/// The kinds a workflow file may name, for error messages.
///
/// Ordered built-ins-first: this string is also the source of the schema
/// manifest's `block.kind` values (`workflow_schema_field_facts`), which
/// `the_workflow_schema_manifest_matches_the_engines_values_defaults_and_bounds`
/// compares against `src/workflow-schema.json` as an ordered array — so the
/// order here is a contract with that file, not presentation.
pub fn kind_names() -> String {
    "orchestrator, worker, reviewer, planner, manager".to_string()
}

/// The capability class a `role_hint` REQUIRES — `None` for anything
/// unrecognized, the same "reject, never coerce" shape as [`kind_from_str`].
/// This function is the whole enforcement of the part that IS invariant: a
/// hint may only sit on an existing kind, so a workflow file can never spell a
/// fifth capability class. What a hint then MEANS is decided elsewhere, in
/// loomux's own code — see `doc/design/liaison.md` for the enumerated list of
/// MCP-tier exceptions, which today all narrow but are not guaranteed to.
pub fn role_hint_requires(hint: &str) -> Option<Role> {
    match hint.trim().to_ascii_lowercase().as_str() {
        "advisor" => Some(Role::Planner),
        "process" => Some(Role::Worker),
        // The human-facing liaison (#891): a pane the human converses with,
        // which reads the board and relays — so `reviewer`, the read-only
        // class that persists (a planner auto-closes on `report`, a worker
        // holds write authority the liaison is defined by NOT having).
        "liaison" => Some(Role::Reviewer),
        _ => None,
    }
}

/// Does this block actually REVIEW PRs? — reviewer-kind, minus the liaison
/// (#891).
///
/// `kind == Reviewer` answers "which capability class does it ride", which is
/// not the same question once a hint subtracts from its class: a liaison rides
/// the reviewer posture and reviews nothing, is denied `review_verdict`, and
/// cannot be named by a merge gate (`parse_workflow` refuses that outright).
/// Every place that means "the blocks a PR is fanned out to" — the
/// orchestrator's `{{REVIEWERS}}` list, a reviewer's "you are one of N" lane —
/// asks THIS, so both surfaces answer the same way; asking `kind` there sends a
/// PR to a pane that can neither record a verdict nor satisfy the gate it was
/// spawned for.
///
/// **`Guardrails::block_for` asks it too (#891 S4)**, for the neighbouring
/// question "which block does a bare `spawn_agent(kind: \"reviewer\")` open" —
/// a liaison declared first in roster order used to be that answer. Same
/// predicate deliberately: *which blocks review* must not have two answers.
/// The pane's own mirror is `isReviewingBlock` (`src/workflowmodel.ts`), which
/// keeps the workflow editor from offering a liaison as a gate reviewer.
///
/// Not used for the *capacity* advisories (`recommend_capacity`/`extra_tiers`),
/// which count live panes and are right to count a liaison as one — see
/// `doc/design/liaison.md`.
pub fn is_reviewing_block(b: &Block) -> bool {
    b.kind == Role::Reviewer && b.role_hint.as_deref() != Some("liaison")
}

/// **Which blocks the orchestrator may open with `spawn_agent(block: …)`** —
/// and therefore the only ones its kickoff roster and its instruction file may
/// list under "your delegates" (#1161 review B1).
///
/// The list those two surfaces render is not "every block in the file"; its own
/// sentence is *"pass `block: "<id>"` to spawn_agent to open one"*, so a block
/// `spawn_agent` refuses does not belong in it. Before this predicate the filter
/// was spelled inline as `kind != Orchestrator` at both call sites, which was
/// the same statement while there was exactly one unspawnable class — and the
/// moment a second arrived, the two surfaces went on advertising a route the
/// tool refuses. An orchestrator obeying its own instruction file would call it,
/// burn a turn on the refusal, and keep reading the same line on every turn
/// after that, re-grounding included.
///
/// So it is ONE predicate, named for the question, and the tool's refusals are
/// pinned against these surfaces in both directions by
/// `every_block_the_orchestrator_is_told_to_spawn_is_one_spawn_agent_accepts`.
/// Same discipline (and same reason) as [`is_reviewing_block`] next door: a
/// membership rule with two call sites must not be two rules.
///
/// - **Orchestrator** — a group has exactly one, opened at launch;
///   `spawn_agent` has refused `kind: "orchestrator"` since #222.
/// - **Manager** (#1161) — the human's own interface, declared in the workflow
///   file and opened for them; `spawn_agent` refuses it by `kind` and by
///   `block`. What the orchestrator is told about a declared manager instead is
///   M4's `{{MANAGER_NOTE}}`; until that lands it is told nothing, which is the
///   honest state — this slice ships no channel to it.
pub fn is_spawnable_block(b: &Block) -> bool {
    !matches!(b.kind, Role::Orchestrator | Role::Manager)
}

/// The role hints a workflow file may name, for error messages.
pub fn role_hint_names() -> String {
    "advisor, process, liaison".to_string()
}

/// Validate one block model knob — `effort:` or `context:` (#687).
///
/// Two checks, in this order, and both are **loud**: the value must be in
/// loomux's closed `vocabulary` (a typo is never coerced to a neighbouring
/// level), and — when the block names an explicit `cli:` — that CLI must be one
/// loomux can actually deliver the knob on, per its [`CliCaps`](crate::model::CliCaps)
/// row. A knob the CLI cannot honor is a parse error rather than a silent
/// no-op: the author asked for a thinking level and would otherwise never
/// learn they did not get one. `cli_supports` is `None` for a block that
/// inherits the group default CLI, which is not known at parse time (the same
/// deferral `cli_can_host` makes for the containment check) — `clamped()`
/// re-runs the CLI half at spawn, where the real CLI is in hand.
///
/// **The refusal also says what to do instead (#782).** An agent authoring a
/// `.loomux/workflow.yml` has no launcher to grey the knob out for it, so the
/// only rail it gets is this sentence — and "copilot cannot set effort" alone
/// leaves it guessing between deleting the key and changing the block's CLI.
/// The remedy is derived from [`CLI_CAPS`](crate::model::CLI_CAPS) by asking every
/// row loomux can actually spawn whether it carries THIS value, so a newly
/// wired seam (gemini's `thinkingConfig`, say) changes the message with no
/// edit here and no CLI named in this file — CLAUDE.md constraint 8.
///
/// Returns the normalized (trimmed, lowercased) value; empty for an absent key.
/// One function for both keys so the two can never drift on case handling or on
/// which check fires first.
fn validate_knob(
    field: &str,
    raw: &str,
    vocabulary: &[&str],
    cli: &str,
    cli_supports: Option<(&[&str], &str)>,
    knob_of: fn(&crate::model::CliCaps) -> &'static [&'static str],
) -> Result<String, String> {
    let want = raw.trim().to_ascii_lowercase();
    if want.is_empty() {
        return Ok(String::new());
    }
    if !vocabulary.contains(&want.as_str()) {
        return Err(format!(
            "unknown {field} {raw:?} — must be one of {}",
            vocabulary.join(", ")
        ));
    }
    if let Some((supported, note)) = cli_supports {
        if !supported.contains(&want.as_str()) {
            let alternatives: Vec<&str> = crate::model::CLI_CAPS
                .iter()
                .filter(|c| c.orchestration && knob_of(c).contains(&want.as_str()))
                .map(|c| c.cli)
                .collect();
            let remedy = if alternatives.is_empty() {
                format!(
                    "no cli loomux spawns can set {field} {want:?} today — drop the key and take \
                     the CLI's own default"
                )
            } else {
                format!(
                    "drop the key (the CLI's own default applies), or give this block a cli: that \
                     can set it — {}",
                    alternatives.join(", ")
                )
            };
            return Err(format!("cli {cli:?} cannot set {field} {want:?} — {note}. Fix: {remedy}"));
        }
    }
    Ok(want)
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
        // The FIVE class names are RESERVED as ids for their own class (#1161
        // added `manager`). Without
        // this, `- id: planner, kind: reviewer` is accepted and then two blocks
        // collide: `instructions_file()` keys "is this a built-in?" off the id but
        // names the file from the kind, so that block would write `reviewer.md` —
        // the real reviewer block's contract file — and whichever spawned last
        // would win. (`- id: orchestrator, kind: worker` breaks a second way: the
        // roster has no orchestrator *kind*, so `clamped()` synthesizes one with
        // the id `orchestrator`, and the duplicate id makes the repo's own block
        // permanently unreachable.) Coupling the two removes the whole class of
        // problem, and costs an author nothing: rename the block.
        //
        // **Widening `kind_from_str` widens THIS, and that is a breaking change
        // to already-written workflow files** (#1161): `- id: manager, kind:
        // worker` parsed clean before the class existed — the id was not a class
        // name, so it took the custom-id branch and wrote `manager.md` — and is
        // a parse error now, which fails the whole file and drops the repo back
        // to the built-in roster. Unavoidable once `manager` names a class (the
        // alternative is the `worker.md` collision above), and cheap to fix:
        // rename the block. `Guardrails::clamped` step 2 is the same rule
        // applied to an already-persisted `group.json`, where it drops the block
        // rather than the file.
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
        if !cli.is_empty() {
            // #267: the containment question comes FIRST, and deliberately so.
            // A CLI loomux has evaluated and recorded (`CLI_CAPS`) but cannot
            // let host this class deserves to be told why — "unknown cli" would
            // be both unhelpful and, for a CLI with a row, untrue. Membership
            // still catches everything this doesn't: `cli_can_host` returns
            // `Ok` for a CLI it has never heard of.
            //
            // Refused at LOAD time so a repo learns from its own workflow file
            // rather than from a spawn that fails hours later — the same reason
            // the CLI name itself is validated here as well as at spawn. Only
            // checked for an explicit `cli:`; an empty one inherits the group
            // default, which is not known here (the launcher picks it) and is
            // re-checked at spawn against the real value.
            if let Err(e) = cli_can_host(&cli, kind) {
                errs.push(format!("blocks[{i}] ({id}): {e}"));
                continue;
            }
            if !SUPPORTED_CLIS.contains(&cli.as_str()) {
                errs.push(format!(
                    "blocks[{i}] ({id}): unknown cli {cli:?} — supported: {}",
                    SUPPORTED_CLIS.join(", ")
                ));
                continue;
            }
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
        // THE ORCHESTRATOR BLOCK IS LOOMUX-OWNED. A repo may pin its `cli`,
        // `model`, `effort` and `context` (each sanitized/validated like
        // everywhere else) — but it may not author its persona or pre-approve
        // its tools.
        //
        // The pin list is exactly "picks from a value set loomux ships", which
        // is why #687's two knobs join it and `prompt:`/`profile:`/`allow:`
        // never can: a level from a closed enum authors no text and
        // pre-approves no tool, so it opens no injection seam into the trust
        // root — the most a hostile repo buys is an orchestrator that thinks
        // harder or holds more context, both of which the human is shown in
        // the launcher's roster preview before they opt in.
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
        //
        // **THE MANAGER BLOCK IS LOOMUX-OWNED FOR THE SAME REASON** (#1161,
        // decision D1 — human-blessed). The capability-closure table makes
        // persona text inert for most classes: a repo can say anything it likes
        // to a reviewer and the reviewer still cannot merge. That argument does
        // not transfer here, because the manager's entire output surface IS
        // persuasion — of the human in its pane, and of the orchestrator via
        // relayed directives that the trust root then acts on as if the human
        // had said them. A repo-authored persona there is a directive-laundering
        // seam of the #189 class arriving with a `git clone`, and it is the one
        // seam no capability table can close.
        //
        // A repo loses nothing it was promised: the elicitation method itself
        // (spec-driven, "grill-me") ships in loomux's own `manager.md`. Pinning
        // `cli:`/`model:`/`effort:`/`context:`/`name:` stays legal, on the same
        // "picks from a value set loomux ships" line drawn above. Relaxing this
        // later as an explicit human opt-in is cheap; tightening it later would
        // be a breaking change to every workflow file already written.
        if kind == Role::Orchestrator || kind == Role::Manager {
            let offenders: Vec<&str> = [
                rb.prompt.is_some().then_some("prompt:"),
                rb.profile.is_some().then_some("profile:"),
                (!rb.allow.is_empty()).then_some("allow:"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !offenders.is_empty() {
                let why = if kind == Role::Orchestrator {
                    "the orchestrator is loomux's trust root and a repo file may not author its \
                     prompt or pre-approve its tools"
                } else {
                    "a manager speaks to the human and relays their direction into the trust root, \
                     so a repo file authoring its persona could launder its own instructions into \
                     what the human is told and what the orchestrator is asked to do"
                };
                errs.push(format!(
                    "blocks[{i}] ({id}): a{n} {k} block may not declare {offenders} — {why}. Pin \
                     its cli:/model:/effort:/context: if you need to; put personas on the blocks \
                     the orchestrator spawns.",
                    n = if kind == Role::Orchestrator { "n" } else { "" },
                    k = kind.as_str(),
                    offenders = offenders.join(" / "),
                ));
                continue;
            }
        }
        // CAPABILITY CLOSURE. `allow:` pre-approves tool patterns, and the
        // read-only class is read-only by *denial of a fixed list* — Edit, Write,
        // NotebookEdit, `git commit`, `git push` (CLAUDE_EDIT_DENY_TOOLS +
        // CLAUDE_READONLY_DENY_GIT — #448 dropped `MultiEdit`, which matches no
        // real Claude Code tool).
        // Deny beats allow on both CLIs, so an allow pattern cannot re-grant anything on that list…
        // but it does not have to. `allow: Bash(python *)` (or `cp`, `tee`,
        // `sed -i`, …) hands a planner a shell that writes files and is named
        // nowhere in the deny list, and under `auto_ops` nobody approves the call.
        //
        // Enumerating every write-capable program is not a thing anyone can do.
        // So the rule is the other way round: **a read-only block may not declare
        // `allow:` at all.** That keeps "a workflow file can never grant a
        // capability" a statement about the code rather than about the deny list's
        // completeness.
        //
        // The ban stays keyed to `is_read_only()` — the FULLY read-only class —
        // and deliberately did not follow #462's deny flags onto reviewers. The
        // argument above does not apply to a reviewer: it keeps its shell by
        // design (running the tests is the job), so an `allow:` pattern names
        // nothing it could not already run, and the editing tools #462 denies it
        // cannot be re-granted anyway (deny beats allow on both CLIs). Banning
        // `allow:` there would cost real expressiveness — a reviewer block that
        // pre-approves `Bash(npm test *)` — and buy nothing. A worker holds the
        // whole surface outright, same conclusion.
        if !rb.allow.is_empty() && kind.is_read_only() {
            errs.push(format!(
                "blocks[{i}] ({id}): a {} block cannot declare allow: — its class is read-only, \
                 and a pre-approved tool pattern could hand it a shell that writes files. \
                 Move the work to a worker block.",
                kind.as_str()
            ));
            continue;
        }
        // role_hint (#250/#324) is a persona/template MARKER, never a
        // capability class of its own — it selects which addendum/template
        // fragment/badge a block gets, and `resolve_persona` keys off `kind`
        // alone. `mcp::tool_defs` and, since #946 Q4 / #1091 slice H, the
        // Claude CLI's `AskUserQuestion` deny (`claude_denies_interactive_
        // question`) DO additionally key off this field for the single
        // `liaison` hint — in BOTH directions: `tool_defs` GRANTS a plain
        // `kind: reviewer` block `group_usage`/`ask_human` once it also
        // carries `liaison` (mcp.rs, `tool_defs`'s liaison arm — a deliberate
        // widening of that block's tool surface, not a deny), while the
        // AskUserQuestion deny ADDS a restriction the same hint does not
        // otherwise carry. What neither direction ever touches is
        // `Role::containment()` — the edit/git denial tier `kind` alone
        // sets — so a liaison's containment is exactly a plain reviewer's
        // (`NoEdits`: the CLI's editing tools denied, the shell intact),
        // whatever its hint grants or denies elsewhere. What IS enforced
        // here is that a hint can only sit on the kind it is
        // meaningless without: an unrecognized value, or one paired with the
        // wrong kind, is a loud parse error — never coerced, never silently
        // dropped, the same shape `kind_from_str` itself enforces.
        let role_hint = match rb.role_hint.as_deref() {
            None => None,
            Some(raw) => {
                let hint = raw.trim().to_ascii_lowercase();
                let Some(required) = role_hint_requires(&hint) else {
                    errs.push(format!(
                        "blocks[{i}] ({id}): unknown role_hint {raw:?} — must be one of {}",
                        role_hint_names()
                    ));
                    continue;
                };
                if required != kind {
                    errs.push(format!(
                        "blocks[{i}] ({id}): role_hint {hint:?} requires kind: {} (this block is kind: {})",
                        required.as_str(),
                        kind.as_str()
                    ));
                    continue;
                }
                Some(hint)
            }
        };
        // `effort:` / `context:` (#687). Both are VALUE-SET picks — they author
        // no text and pre-approve no tool — so the capability-closure argument
        // is unchanged and they are legal on an orchestrator block too (see
        // that check above, and `doc/design/workflows.md`). `validate_knob`
        // carries the whole rule; the CLI half is checked only for an explicit
        // `cli:`, exactly like `cli_can_host` above.
        let caps = (!cli.is_empty()).then(|| crate::model::cli_caps(&cli)).flatten();
        let effort = match validate_knob(
            "effort",
            &rb.effort,
            crate::model::EFFORT_LEVELS,
            &cli,
            caps.map(|c| (c.effort_levels, c.effort_note)),
            |c| c.effort_levels,
        ) {
            Ok(v) => v,
            Err(e) => {
                errs.push(format!("blocks[{i}] ({id}): {e}"));
                continue;
            }
        };
        let context = match validate_knob(
            "context",
            &rb.context,
            crate::model::CONTEXT_VARIANTS,
            &cli,
            caps.map(|c| (c.context_variants, c.context_note)),
            |c| c.context_variants,
        ) {
            Ok(v) => v,
            Err(e) => {
                errs.push(format!("blocks[{i}] ({id}): {e}"));
                continue;
            }
        };
        let name = sanitize_display(&rb.name);
        blocks.push(Block {
            name: if name.is_empty() { id.clone() } else { name },
            id,
            kind,
            cli,
            model: crate::model::sanitize_model_opt(&rb.model),
            prompt: rb.prompt.as_deref().map(sanitize_persona).filter(|s| !s.trim().is_empty()),
            profile: rb.profile.as_ref().map(|p| p.trim().to_string()),
            allow: rb.allow.iter().filter_map(|a| crate::profiles::sanitize_allow(a)).collect(),
            role_hint,
            effort,
            context,
        });
    }

    if blocks.is_empty() && errs.is_empty() {
        errs.push("no blocks declared — a workflow needs at least one block".into());
    }

    // At most one manager (#1161) — see [`MANAGER_MAX`] for why this is a
    // coherence rule and not a capacity one. Checked after the loop rather than
    // inside it because it is a property of the ROSTER, not of any one block:
    // the second declaration is not more wrong than the first, and naming both
    // is what lets an author see which two they wrote.
    let managers: Vec<&str> = blocks
        .iter()
        .filter(|b| b.kind == Role::Manager)
        .map(|b| b.id.as_str())
        .collect();
    if managers.len() > MANAGER_MAX {
        errs.push(format!(
            "blocks: {} manager blocks declared ({}) — a workflow may declare at most {MANAGER_MAX}. \
             The manager is the human's single interface to this group: two of them would each hold \
             half a conversation, and everything that says \"the manager\" downstream would have to \
             pick one silently. Keep one and give the others another kind.",
            managers.len(),
            managers.join(", "),
        ));
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
            if let Some(e) = gate_reviewer_error(&name, "reviewer", rname, &blocks) {
                errs.push(e);
                bad = true;
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
        // #1174's small-batch clause. `0` is a parse error, not "unlimited":
        // the same rule `threshold` follows, and for the same reason — a bound
        // a repo wrote down must never be read as the absence of one. A
        // negative or fractional value never reaches here at all; serde refuses
        // the whole file at `Option<u32>`, which is exactly what `threshold: -1`
        // already does.
        if rg.max_diff_lines == Some(0) {
            errs.push(format!("gates.{name}: max_diff_lines must be a positive number — omit the key to declare no limit"));
            bad = true;
        }
        // #1176's path-based routing. Every refusal below is LOUD — a rule
        // loomux could not read is never a rule it quietly drops, because the
        // whole point of a routing rule is to ADD a required reviewer, and the
        // failure mode of silently dropping one is a merge that skipped a lane
        // the repo asked for.
        let mut routing: Vec<RoutingRule> = Vec::new();
        if !rg.routing.is_empty() {
            // `threshold: N` counts votes over a FIXED list; routing makes the
            // list a function of the diff. Together they have no honest meaning:
            // adding a lane would also add a candidate that could supply one of
            // the N passes, so declaring a routing rule could make the gate
            // EASIER to satisfy — the one direction a gate must never move. The
            // refusal says what to do instead (#782) rather than picking a
            // reading and hoping the author meant it.
            if matches!(require, GateRequire::Threshold(_)) {
                errs.push(format!(
                    "gates.{name}: routing: and require: threshold cannot both be declared — a \
                     threshold counts passes over a fixed reviewer list, and a routing rule makes \
                     that list depend on the diff, so together they would let an extra lane SUPPLY \
                     one of the required passes instead of adding one. Use require: all-pass with \
                     routing:, and let each rule name the lane its paths need."
                ));
                bad = true;
            }
            if rg.routing.len() > ROUTING_RULES_MAX {
                errs.push(format!(
                    "gates.{name}: {} routing rules — at most {ROUTING_RULES_MAX}. The shim \
                     evaluates every rule against every changed file on every merge; past this \
                     many lanes the block has stopped routing and started listing.",
                    rg.routing.len()
                ));
                bad = true;
            }
        }
        for (i, rr) in rg.routing.iter().enumerate() {
            // 1-based, matching the position an author counts to in their own
            // file — and the number every refusal downstream cites.
            let idx = i + 1;
            let ctx = format!("routing rule {idx} reviewer");
            if rr.paths.is_empty() {
                errs.push(format!(
                    "gates.{name}: routing rule {idx} declares no paths — a rule that matches \
                     nothing can never require anybody. Omit the rule, or give it a path glob."
                ));
                bad = true;
            }
            if rr.paths.len() > ROUTING_PATHS_MAX {
                errs.push(format!(
                    "gates.{name}: routing rule {idx} declares {} paths — at most {ROUTING_PATHS_MAX}.",
                    rr.paths.len()
                ));
                bad = true;
            }
            if rr.reviewers.is_empty() {
                errs.push(format!(
                    "gates.{name}: routing rule {idx} names no reviewers — a rule that requires \
                     nobody is not a rule."
                ));
                bad = true;
            }
            let mut paths: Vec<String> = Vec::new();
            let mut seen_paths: BTreeSet<String> = BTreeSet::new();
            for p in &rr.paths {
                // Rejected, never rewritten — the #225 contract. An author must
                // be able to reference the glob they actually wrote, and a glob
                // loomux silently narrowed is a lane loomux silently dropped.
                match sanitize_glob(p) {
                    Some(clean) if clean == p.trim() => {
                        if !seen_paths.insert(clean.clone()) {
                            errs.push(format!(
                                "gates.{name}: routing rule {idx} lists the path {p:?} more than \
                                 once — name each glob once."
                            ));
                            bad = true;
                            continue;
                        }
                        paths.push(clean);
                    }
                    _ => {
                        errs.push(format!(
                            "gates.{name}: routing rule {idx}: {p:?} is not a usable path glob. \
                             Use letters, digits, '.', '_', '-', '/' and '*' — and write a file \
                             glob, not a directory: 'src/**', never 'src/', '/src/**' or a '..' \
                             segment (GitHub reports changed paths repo-relative, so those match \
                             nothing at all)."
                        ));
                        bad = true;
                    }
                }
            }
            let mut reviewers: Vec<BlockId> = Vec::new();
            let mut seen_routed: BTreeSet<String> = BTreeSet::new();
            for r in &rr.reviewers {
                let rname = r.trim();
                // Same set-not-sequence rule the static list follows, and for a
                // milder version of the same reason: a name written twice in one
                // rule is a typo for a second lane, not an emphasis.
                if !seen_routed.insert(rname.to_string()) {
                    errs.push(format!(
                        "gates.{name}: routing rule {idx} names reviewer {rname:?} more than once"
                    ));
                    bad = true;
                    continue;
                }
                if let Some(e) = gate_reviewer_error(&name, &ctx, rname, &blocks) {
                    errs.push(e);
                    bad = true;
                    continue;
                }
                reviewers.push(rname.to_string());
            }
            routing.push(RoutingRule { paths, reviewers });
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
                max_diff_lines: rg.max_diff_lines,
                routing,
            },
        );
    }

    // Intake source + label vocabulary (#382 P1). `None` (no `intake:` block
    // at all) resolves straight to the built-in default; a declared block
    // resolves field by field, each label falling back to its built-in value
    // when omitted (`sanitize_intake_label`) so a repo can override one label
    // without repeating the rest.
    let default_intake = builtin_intake_profile();
    let intake = match &raw.intake {
        None => default_intake,
        Some(ri) => {
            let source = match intake_source_from_str(&ri.source) {
                Some(s) => s,
                None => {
                    errs.push(format!(
                        "intake.source: unknown source {:?} — must be one of {}",
                        ri.source,
                        intake_source_names()
                    ));
                    IntakeSource::GithubLabels
                }
            };
            IntakeProfile {
                source,
                ready: sanitize_intake_label("ready", &ri.labels.ready, &default_intake.ready, &mut errs),
                investigate: sanitize_intake_label(
                    "investigate",
                    &ri.labels.investigate,
                    &default_intake.investigate,
                    &mut errs,
                ),
                owned: sanitize_intake_label("owned", &ri.labels.owned, &default_intake.owned, &mut errs),
                prototype: sanitize_intake_label(
                    "prototype",
                    &ri.labels.prototype,
                    &default_intake.prototype,
                    &mut errs,
                ),
                hold: sanitize_intake_label("hold", &ri.labels.hold, &default_intake.hold, &mut errs),
            }
        }
    };

    // Merge-queue policy (#581 §11.2). `None` (no `merge_queue:` block at all)
    // resolves to the default, which is **disabled** — an absent block means
    // the feature is off and behavior is byte-for-byte unchanged.
    //
    // Two different postures on a bad value, both taken from the note:
    //
    // - `max_batch: 0` is a hard **error**. §11.2 says a malformed block never
    //   degrades to defaults, "because a queue running on silently-substituted
    //   policy is a queue nobody can reason about"; and it matches how the
    //   sibling `gates:` block treats a number that could never work
    //   (`threshold: 0`, `threshold` above the reviewer count).
    // - `checks_timeout_minutes` is **clamped**, because the note says clamped
    //   ("default 60, clamped like the notify TTLs") — and it is clamped by the
    //   notify TTL clamp *itself*, not by a second copy of those bounds. It is
    //   the same quantity: a bounded wait on a PR's checks. `None` (omitted)
    //   through the same call is where the 60-minute default comes from.
    let merge_queue = match &raw.merge_queue {
        None => MergeQueuePolicy::default(),
        Some(rq) => {
            let max_batch = match rq.max_batch {
                None => MERGE_QUEUE_MAX_BATCH_DEFAULT,
                Some(0) => {
                    errs.push(
                        "merge_queue.max_batch: must be at least 1 — a batch of no PRs could never land anything"
                            .to_string(),
                    );
                    MERGE_QUEUE_MAX_BATCH_DEFAULT
                }
                Some(n) => n,
            };
            MergeQueuePolicy {
                enabled: rq.enabled,
                max_batch,
                checks_timeout_minutes: clamp_expires_minutes(rq.checks_timeout_minutes),
            }
        }
    };

    // Named lock resources (#858). An absent block leaves this empty, which is
    // what makes the lock tools invisible to the group's agents.
    //
    // Every bad value here is a hard ERROR, never a silent substitution — the
    // same posture `merge_queue.max_batch` takes and for the same reason: a
    // repo declaring `slots: 0` believes its builds are serialized, and
    // quietly handing it the default would leave that belief in place while
    // the behaviour changed underneath it. Names are REJECTED rather than
    // rewritten (the `blocks[].id` rule): an author who wrote `heavy build`
    // must not end up with a resource called `heavybuild` that the
    // `acquire_lock` call in their own worker brief cannot name.
    let mut resources: BTreeMap<String, ResourcePolicy> = BTreeMap::new();
    if raw.resources.len() > RESOURCES_MAX {
        errs.push(format!(
            "resources: {} declared — at most {RESOURCES_MAX} are allowed (every name is listed \
             in the acquire_lock tool description every agent in the group reads)",
            raw.resources.len()
        ));
    }
    for (raw_name, rr) in &raw.resources {
        let trimmed = raw_name.trim();
        if trimmed.chars().count() > MAX_ID_CHARS {
            errs.push(format!(
                "resources: name {raw_name:?} is longer than {MAX_ID_CHARS} characters"
            ));
            continue;
        }
        let Some(name) = sanitize_id(trimmed) else {
            errs.push(format!(
                "resources: name {raw_name:?} has no usable characters (allowed: letters, digits, '-', '_')"
            ));
            continue;
        };
        if name != trimmed {
            errs.push(format!(
                "resources: name {raw_name:?} contains characters that are not allowed (letters, digits, '-', '_')"
            ));
            continue;
        }
        let slots = match rr.slots {
            None => RESOURCE_SLOTS_DEFAULT,
            Some(0) => {
                errs.push(format!(
                    "resources.{name}.slots: must be at least 1 — a resource with no slots could \
                     never be acquired by anyone"
                ));
                continue;
            }
            Some(n) if n > RESOURCE_SLOTS_MAX => {
                errs.push(format!(
                    "resources.{name}.slots: {n} is above the maximum of {RESOURCE_SLOTS_MAX} — \
                     past that a declaration serializes nothing, which is not what a `slots:` line means"
                ));
                continue;
            }
            Some(n) => n,
        };
        let max_hold_minutes = match rr.max_hold_minutes {
            None => RESOURCE_MAX_HOLD_MINUTES_DEFAULT,
            Some(0) => {
                errs.push(format!(
                    "resources.{name}.max_hold_minutes: must be at least 1 — a hold that expires \
                     the moment it is granted serializes nothing"
                ));
                continue;
            }
            Some(n) if n > RESOURCE_MAX_HOLD_MINUTES_MAX => {
                errs.push(format!(
                    "resources.{name}.max_hold_minutes: {n} is above the maximum of \
                     {RESOURCE_MAX_HOLD_MINUTES_MAX} — a hold on a scarce resource has to be \
                     bounded by something a working session outlives"
                ));
                continue;
            }
            Some(n) => n,
        };
        resources.insert(name, ResourcePolicy { slots, max_hold_minutes });
    }

    // Board policy — per-status WIP limits (#1175). `None` (no `board:` block
    // at all) resolves to the default, which declares no limits: the feature
    // is off and behavior is byte-for-byte unchanged.
    //
    // A bad value is a hard ERROR, never a silent substitution — the posture
    // `merge_queue.max_batch` and `resources.slots` take, for the reason §11.2
    // gives: a repo that wrote `review: 0` believes something about how its
    // board paces, and quietly handing it "no limit" would leave that belief
    // in place while the behaviour went the other way.
    //
    // The declared caps are read back out THROUGH serde rather than by
    // matching on `RawWip`'s seven fields here. Hand-listing them a second
    // time is how the eighth status would arrive parsed-but-unenforced: the
    // struct is the one place the field set is written down, and this loop
    // reads whatever that struct accepted.
    let board = match &raw.board {
        None => BoardPolicy::default(),
        Some(rb) => {
            let mut wip: BTreeMap<String, u32> = BTreeMap::new();
            if let Some(rw) = &rb.wip {
                let declared = match serde_json::to_value(rw) {
                    Ok(serde_json::Value::Object(map)) => map,
                    other => panic!("board.wip: expected a mapping, got {other:?}"),
                };
                for (status, value) in declared {
                    // `null` is the field the document simply never wrote —
                    // "no cap on this status", which is not a value to check.
                    let Some(n) = value.as_u64() else { continue };
                    if n < WIP_LIMIT_MIN as u64 {
                        errs.push(format!(
                            "board.wip.{status}: must be at least {WIP_LIMIT_MIN} — a cap of 0 is a \
                             stop, not a work-in-progress limit, and under `enforce` it would wedge \
                             the board rather than pace it"
                        ));
                        continue;
                    }
                    wip.insert(status, n as u32);
                }
            }
            BoardPolicy { wip, enforce: rb.enforce }
        }
    };

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
        intake,
        merge_queue,
        resources,
        board,
    })
}

/// Whether the repo declares a workflow at all, asked without parsing it.
///
/// Used where the *existence* of the file is the whole question: `create_group`
/// audits that it deliberately ignored one (the advanced-orchestrator toggle is
/// off, #222), and the launcher's preview distinguishes "this repo has no
/// workflow" from "it has one and it is broken".
pub fn workflow_file_exists(repo: &str) -> bool {
    workflow_file(repo).is_file()
}

/// Whether a block may carry a persona at all.
///
/// The orchestrator block is loomux-owned: a repo may pin its `cli`/`model`, never
/// author its persona or pre-approve its tools. `parse_workflow` rejects that
/// outright, and `orchestration::OrchRegistry::resolve_persona` (in `src-tauri`)
/// drops one that arrives from a hand-edited `group.json` — so the *only* honest
/// answer about an orchestrator block's persona is "there isn't one".
///
/// **The manager block is loomux-owned on the same terms** (#1161, decision
/// D1), so it answers `false` here too. The parse refusal is the visible half;
/// this is the half that holds when the parser is bypassed, and it has to,
/// because bypassing the parser is precisely the case a repo-authored persona
/// on the human's own interface would be worth attempting. See `parse_workflow`
/// for the argument.
///
/// Anything that merely *reports* on a block therefore has to ask this too, or it
/// advertises a persona the spawn will deny (rev-11's preview nit). One predicate,
/// so the report and the spawn cannot disagree.
pub fn persona_allowed(block: &Block) -> bool {
    !matches!(block.kind, Role::Orchestrator | Role::Manager)
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
///
/// **A declared manager counts, whatever its id** (#1161), and that third
/// clause is load-bearing rather than defensive. `manager` is a reserved id
/// ([`BUILTIN_IDS`]) so that a manager block owns `manager.md` — which means
/// the obvious spelling of a declared manager, `- id: manager, kind: manager`,
/// answers `is_builtin()` true and (D1 forbidding it a persona) `has_persona()`
/// false. Without this clause a workflow whose only addition to the built-in
/// four is a manager would report as "nothing a workflow file put there", and
/// every workflow-aware surface — the orchestrator's roster note, its workflow
/// section — would collapse to empty on the one roster that most needs to name
/// what it added. The default path is untouched: [`builtin_roster`] synthesizes
/// no manager, so this clause cannot fire for a group with no workflow file.
pub fn roster_is_custom(blocks: &[Block]) -> bool {
    blocks.iter().any(|b| !b.is_builtin() || b.has_persona() || b.kind == Role::Manager)
}

/// Read + validate the repo's workflow file ([`workflow_path`]).
///
/// - `Ok(None)` — no file (the common case): the caller synthesizes
///   [`default_roster`] and behaves exactly like pre-#222 loomux.
/// - `Err(errors)` — the file exists but is broken. The caller **audits and
///   skips it**, falling back to the default roster. A workflow file must never
///   be able to block a spawn.
pub fn load_workflow(repo: &str) -> Result<Option<Workflow>, Vec<String>> {
    let path = workflow_file(repo);
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
    /// **The PR body this verdict reviewed** (#565), as a sha256 of
    /// [`canonical_body`] — captured by the tool at record time, exactly like
    /// `head`, and never passed in by the reviewer.
    ///
    /// The head SHA pins the *code*. It does not pin the **PR body**, which on a
    /// squash-merging repo becomes the permanent commit message: reviewed content
    /// with the weight of a diff and none of a diff's version pinning. It moves in
    /// both directions — a reviewer passes a body and the author then edits it, so
    /// the merge carries text nobody reviewed; or a reviewer fails a body that has
    /// already been fixed, and the PR is blocked on a defect that no longer exists
    /// (the #525 incident that filed #565: review comment at 14:44:23Z, body edited
    /// at 14:47:49Z, and no mechanism could tell either agent).
    ///
    /// A digest rather than the body text: fixed size, and a mismatch is *exact*.
    /// Storing ~250 lines per verdict archives the artifact but still leaves a human
    /// to diff it by eye — which is the manual step that cost the round. A
    /// `updatedAt` timestamp was the other option and is worse than nothing: it
    /// moves for labels and assignees, so it cries wolf, and when it does fire it
    /// says *that* something changed, never *what*.
    ///
    /// Empty when loomux could not resolve the body at record time — read the same
    /// fail-closed way as an empty `head`: unknown, never "unbound, therefore fine".
    pub body_digest: String,
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

    /// Whether the PR body has changed since this verdict was recorded (#565).
    /// `None` when that cannot be *known* — either this verdict carries no digest
    /// (recorded by a build that predates #565, or with gh unable to read the body)
    /// or the current body could not be read now. Never `Some(false)` on a guess:
    /// "we could not check" and "it is unchanged" are different answers, and only
    /// one of them may quiet a warning.
    pub fn body_changed(&self, current_digest: Option<&str>) -> Option<bool> {
        let now = current_digest.filter(|d| !d.is_empty())?;
        (!self.body_digest.is_empty()).then(|| self.body_digest != now)
    }
}

/// The PR body reduced to the form both halves of the gate digest (#565).
///
/// Two normalizations, and **only** two, because the shim has to reproduce this
/// exactly in POSIX shell — a richer rule (per-line trailing whitespace, re-wrap
/// tolerance, markdown awareness) is one the two halves would eventually disagree
/// about, and a gate whose halves disagree is the failure mode this file keeps
/// coming back to:
///
/// 1. `\r` removed — a CRLF body and an LF body are the same commit message, and
///    which one `gh` hands back depends on the platform, not on the content.
///    Shell: `| tr -d '\r'`.
/// 2. Trailing newlines collapsed to exactly one. Shell: `$(…)` strips them all,
///    `printf '%s\n'` puts one back.
///
/// Everything else is content. In particular a re-wrapped paragraph **is** a
/// change: the body is about to become a permanent commit message, and the claim
/// this makes is "the bytes that will be recorded are the bytes that were
/// reviewed" — not "the meaning is close enough", which nothing could check.
pub fn canonical_body(body: &str) -> String {
    format!("{}\n", body.replace('\r', "").trim_end_matches('\n'))
}

/// sha256 of [`canonical_body`], lowercase hex. The one definition; the shim
/// pipes the same canonical bytes through `sha256sum`/`shasum`/`openssl`.
///
/// The two implementations are held together by an executed test, not by this
/// comment: case 1 of
/// `the_shim_refuses_a_merge_whose_body_moved_after_the_pass_when_the_repo_opts_in`
/// records a verdict through the real MCP tool and then merges through the real
/// shim over the SAME body — a body carrying CRLF, trailing blank lines, trailing
/// spaces, non-ASCII and `$`-bearing text — so the merge is allowed only if both
/// sides produced the same 64 characters. Disagreement surfaces as that case
/// failing with the gate's own refusal text.
pub fn body_digest(body: &str) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(canonical_body(body).as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A stored body digest is compared inside a shell `case`, so keep it to what a
/// sha256 can actually be: **exactly** 64 hex characters. Anything else — a
/// truncated write, a hand edit, the first line of a summary in a verdict file
/// written before #565 — stores/reads as empty, i.e. *unknown*, which the gate
/// treats as it treats an unknown head: refuse, never wave through.
///
/// Deliberately stricter than [`sanitize_sha`], which accepts any hex run up to
/// 64: a 40-char head oid must not be readable as a body digest.
pub fn sanitize_digest(s: &str) -> String {
    let s = s.trim();
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        s.to_ascii_lowercase()
    } else {
        String::new()
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
/// the verdict word FIRST (the shim reads it with `head -n1`), the reviewed head
/// SECOND and the reviewed body's digest FIFTH (`head -n5 | tail -n1`); the
/// summary runs to EOF, being the only field that may contain newlines — so every
/// fixed field has to sit above it.
pub fn verdict_file_text(v: &ReviewVerdict) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        v.verdict.as_str(),
        sanitize_sha(&v.head),
        v.ts_ms,
        v.agent_id,
        sanitize_digest(&v.body_digest),
        sanitize_summary(&v.summary)
    )
}

/// Read a verdict file back. `None` for anything that isn't a verdict this build
/// understands — an unparseable file is *not* a pass (see [`Verdict::parse`]).
/// `pr`/`block` come from the path, which is loomux-generated.
///
/// Line 5 (the body digest, #565) is read **tolerantly**: a file written before
/// #565 has the first line of its summary there, and swallowing it would mangle
/// durable prose a human reads. So a line 5 that is not a valid digest is handed
/// back to the summary, and the digest reads empty — *unknown*. The shim is
/// stricter (it takes line 5 as-is and refuses anything that isn't 64 hex), and
/// the two still agree on the only thing a gate decides: no readable digest means
/// the body cannot be shown unchanged, so `body-unchanged` refuses. The
/// divergence is confined to which text is displayed as the summary.
pub fn parse_verdict_file(pr: u64, block: &str, text: &str) -> Option<ReviewVerdict> {
    let mut lines = text.lines();
    let verdict = Verdict::parse(lines.next()?)?;
    let head = sanitize_sha(lines.next().unwrap_or(""));
    let ts_ms = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    let agent_id = lines.next().unwrap_or("").trim().to_string();
    let rest: Vec<&str> = lines.collect();
    let body_digest = rest.first().map(|l| sanitize_digest(l)).unwrap_or_default();
    let from = usize::from(!body_digest.is_empty());
    let summary = rest[from.min(rest.len())..].join("\n");
    Some(ReviewVerdict {
        pr,
        block: sanitize_id(block)?,
        agent_id,
        verdict,
        head,
        body_digest,
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
/// `body-unchanged` (#565) is **opt-in for a reason**: it only matters where the
/// PR body *becomes* the record — this repo squash-merges, so the body is the
/// permanent commit message. On a repo that merge-commits, the body is discussion,
/// and the check would be noise. Baking it in either way would be baking one
/// repo's merge habit into a generic tool (CLAUDE.md constraint 8), so it is a
/// clause a repo writes down.
/// `base-green` (#1174) is the stop-the-line clause: it refuses a merge while
/// the **base ref's HEAD** is red or its checks cannot be resolved, so a fleet
/// cannot pile work onto a branch that is already broken. Opt-in for the same
/// reason `body-unchanged` is: a repo with no CI would otherwise be refused
/// every merge forever by a clause it never asked for.
pub const KNOWN_CONDITIONS: [&str; 3] = ["ci-green", "body-unchanged", "base-green"];

/// Whether the shim can evaluate this `also:` condition. See [`KNOWN_CONDITIONS`].
pub fn condition_supported(c: &str) -> bool {
    KNOWN_CONDITIONS.contains(&c.trim())
}

/// The `base-green` reductions (#1174) — **one definition, two consumers**: the
/// `gh` shim interpolates these constants into its POSIX body, and
/// `mqdriver::base_check_runs_argv`/`base_status_argv` pass them to `gh --jq`.
///
/// They live here, with the rest of the gate contract, precisely because the
/// first cut had a *copy* in each place. The two were byte-identical, which
/// looked like the two-implementations-one-contract property holding — and it
/// was, but what the contract SAID was wrong in both, and nothing could have
/// told them apart from two copies that had drifted. A shared constant makes
/// "the shim and the queue ask GitHub the same question" a fact about the
/// program rather than a claim in a PR body.
///
/// Each reduces a JSON payload to ONE word from a closed vocabulary —
/// `red` | `truncated` | `pending` | `none` | `green` — because the shim has no
/// JSON parser and must decide a merge from a shell `case`.
///
/// **The clause order is the contract, and each step earns its place:**
///
/// 1. **`red` first, and only for COMPLETED runs.** A visible failure is the
///    most actionable answer, so it outranks everything below — but a run still
///    in progress carries `conclusion: null`, which the conclusion allow-list
///    would otherwise call red. Reporting "the base is RED" about a base that
///    is merely still building would be a false sentence in a refusal, so
///    `.status == "completed"` guards it.
/// 2. **`truncated` next — the #1181 review's blocking finding.**
///    `/commits/{ref}/check-runs` is **paginated**: `check_runs` is capped at
///    `per_page` while `total_count` counts them all. `any(.check_runs[]; …)`
///    therefore asks "is anything on THIS PAGE red", and before this clause a
///    base with more runs than one page — an ordinary OS x version matrix,
///    exactly the repo that adopts a stop-the-line gate — reported **green**
///    with its failures sitting on page 2. Reproduced against this repo's own
///    API: a commit with 3 `failure` runs answered `red` at full page size and
///    `green` at `?per_page=3`. A page that does not carry every run says
///    nothing about the runs it omits, so it is not an answer.
/// 3. `pending`, then `none`, then `green` — the residue, unchanged.
///
/// **The shape the payload is ASSUMED to have is checked, not assumed (#1181
/// rev-lead NB5).** `total_count` is documented as always present, and the
/// truncation clause above rests entirely on it — but jq sorts `null` below
/// every number, so an absent key makes `.total_count > (.check_runs|length)`
/// evaluate `null > N`, which is **false**, and the expression falls straight
/// through to `green`. That is round one's defect wearing a different hat: an
/// unstated assumption about the payload, failing open, in the one clause whose
/// whole premise is that unknown is never green. `has("total_count")` answers
/// `truncated` instead, so a payload that cannot support the question refuses
/// rather than passing.
///
/// **[`BASE_STATUS_JQ`] carries the same guard over ITS inputs**, which the
/// review did not ask for and this repo's own rule requires: a guard reads every
/// one of its inputs by one rule, and taking one signal from a checked shape and
/// the next from an unchecked one is a bypass exactly the width of that
/// asymmetry. `null | length` is `0` in jq rather than an error, so an absent
/// `statuses` would read as the *definite* claim "this commit has no legacy
/// statuses"; an absent `state` would fall to the `else` and report `red`,
/// which refuses but says something false about the base while doing it.
///
/// **Only the check-runs half needs the truncation clause**, and the asymmetry
/// is worth stating rather than leaving to be re-derived: the combined-status
/// endpoint carries a top-level `.state` that is GitHub's own rollup across
/// *all* statuses, so [`BASE_STATUS_JQ`] is pagination-proof by construction.
/// `check-runs` has no rollup field — only `total_count` — which is why one
/// half was safe and the other was not.
///
/// Green is an ALLOW-list of conclusions (`success`, `neutral`, `skipped`), so
/// a conclusion GitHub adds tomorrow reads as red rather than as green.
pub const BASE_CHECK_RUNS_JQ: &str = "if any(.check_runs[]; .status == \"completed\" and .conclusion != \"success\" and .conclusion != \"neutral\" and .conclusion != \"skipped\") then \"red\" elif (has(\"total_count\")|not) then \"truncated\" elif (.total_count > (.check_runs|length)) then \"truncated\" elif any(.check_runs[]; .status != \"completed\") then \"pending\" elif (.check_runs|length) == 0 then \"none\" else \"green\" end";

/// The combined-status reduction — see [`BASE_CHECK_RUNS_JQ`] for the shared
/// contract and for why this one needs no truncation clause.
///
/// `.state` is `pending` both when a context is pending and when there are no
/// statuses at all, so the count is read first and answers `none`.
pub const BASE_STATUS_JQ: &str = "if (has(\"statuses\")|not) or (has(\"state\")|not) then \"truncated\" elif (.statuses|length) == 0 then \"none\" elif .state == \"success\" then \"green\" elif .state == \"pending\" then \"pending\" else \"red\" end";

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
/// **Routed reviewers count too** (#1176). A rule naming a block this roster
/// cannot spawn makes the gate unsatisfiable for every PR whose paths match it —
/// which is the same #316 failure the static list is checked for, arriving on a
/// subset of PRs instead of all of them. Reported here rather than left to be
/// discovered as "the merge gate stopped opening on frontend PRs only".
pub fn gate_missing_blocks(gate: &Gate, blocks: &[Block]) -> Vec<BlockId> {
    let mut out: Vec<BlockId> = Vec::new();
    let named = gate.reviewers.iter().chain(gate.routing.iter().flat_map(|r| r.reviewers.iter()));
    for id in named {
        if !blocks.iter().any(|b| &b.id == id && b.kind == Role::Reviewer) && !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
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
    /// distinct worker block, every distinct reviewer block, one more if the
    /// workflow declares a planner block, and one more if it declares a manager
    /// block (#1161 — a manager is live for the whole session and, today,
    /// occupies a cap slot like any non-orchestrator pane). The orchestrator
    /// itself is exempt from `max_agents` (mcp.rs) and is never counted here.
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
    // #1161. A declared manager is a live pane for the whole session, and
    // today it occupies a `max_agents` slot exactly like a delegate
    // (`live_delegate_count` exempts only the orchestrator) — so a preview that
    // did not count it would under-advise by one on every roster that has one.
    // `any`, not a count, for the same reason as the planner: a workflow may
    // declare at most one (`MANAGER_MAX`), so the two spellings agree, and
    // `any` says which fact is being relied on.
    //
    // **M3 MUST INVERT THIS, NOT TICK IT OFF.** Decision D3 exempts the manager
    // from `max_agents`, and this `+1` is correct only while it does not have
    // that exemption. The moment `live_delegate_count` stops counting a manager,
    // the rule stated two lines up — "the orchestrator is exempt from
    // `max_agents` and is never counted here" — applies to the manager too, and
    // both this term and
    // `a_declared_manager_raises_the_recommended_capacity_and_is_named_in_the_advisory`
    // have to go the other way. Landed in M1 rather than M3 because a preview
    // that under-advises is wrong TODAY; it is not a head start on M3's work.
    let has_manager = blocks.iter().any(|b| b.kind == Role::Manager);

    // #1176. A gate that routes by path needs, in the WORST case, its declared
    // list plus every lane any rule can add — a PR that touches all of them. The
    // worst case is the one a capacity floor has to be built on: under-advising
    // here is how #255 happens, an orchestrator discovering two hours in that it
    // must kill a live agent to complete one review round. Deduped against the
    // declared list, and against itself, so a lane two rules both name counts once.
    let reviewers_needed = gate.map_or(reviewers, |g| {
        let mut extra: Vec<&BlockId> = Vec::new();
        for id in g.routing.iter().flat_map(|r| r.reviewers.iter()) {
            if !g.reviewers.contains(id) && !extra.contains(&id) {
                extra.push(id);
            }
        }
        gate_need(g) + extra.len() as u32
    });
    let worker_slot = u32::from(workers > 0);
    CapacityRecommendation {
        // `minimum` is deliberately untouched: it is what ONE REVIEW ROUND
        // costs, and a review round does not involve the manager.
        minimum: reviewers_needed + worker_slot,
        recommended: workers + reviewers + u32::from(has_planner) + u32::from(has_manager),
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
    // #1161: `minimum` budgets a review round, which never includes the
    // manager, so a declared one is entirely "extra" — and it is the tier a
    // human would most notice going missing, since it is the pane they talk to.
    if blocks.iter().any(|b| b.kind == Role::Manager) {
        out.push("the manager".to_string());
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
        // The source file is named GENERICALLY (#1153 phase 4): a repo may
        // declare its workflow at `.orrerix/workflow.yml` or the legacy
        // `.loomux/workflow.yml`, this function has no repo to resolve which,
        // and a header naming the wrong one would send a human editing a file
        // that isn't there. The `loomux` brand word in the first phrase is
        // protocol text and flips with #1153 phase 3, not here.
        "# loomux merge gate — generated from this repo's workflow file (#222). Do not edit.\n",
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
    // #1174. A `0` here would be a clause that gates nothing, and `parse_workflow`
    // has already refused it — so if one ever reaches this far the file is
    // poisoned rather than written with a limit the shim would ignore.
    match gate.max_diff_lines {
        None => {}
        Some(0) => out.push_str(&format!("{POISON_KEY} unusable-max-diff-lines\n")),
        Some(n) => out.push_str(&format!("{MAX_DIFF_LINES_KEY} {n}\n")),
    }
    // #1176's routing rules, one line per (rule, glob) and one per (rule,
    // reviewer), each carrying the rule's 1-based index.
    //
    // **Why not one line per rule.** The reader is a POSIX `while read -r k v w`
    // loop with no arrays: a rule packed onto one line would have to be re-split
    // inside the shell, and every spelling of that (an `IFS` swap, a `set --`)
    // either clobbers the shim's own positional parameters or introduces a
    // second delimiter for a glob alphabet to have to avoid. Three fixed fields
    // fit the loop that is already there, and the index is what stitches the
    // halves back together — see [`parse_gate_file`], which refuses any file
    // where they do not stitch.
    if !gate.routing.is_empty() {
        if matches!(gate.require, GateRequire::Threshold(_)) {
            // `parse_workflow` refuses this pair outright, so reaching here means
            // the parse contract has regressed. Poison rather than write a file
            // whose two halves would be read as a LAXER gate than either says.
            out.push_str(&format!("{POISON_KEY} routing-with-threshold\n"));
        }
        if gate.routing.len() > ROUTING_RULES_MAX {
            out.push_str(&format!("{POISON_KEY} too-many-routing-rules\n"));
        }
    }
    for (i, rule) in gate.routing.iter().enumerate() {
        let idx = i + 1;
        // A rule missing either half is unsatisfiable-or-vacuous, and both are
        // refused at parse. Poisoned here for the same reason the tokens below
        // are: the file must never be a weaker gate than the workflow declared.
        if rule.paths.is_empty() || rule.reviewers.is_empty() {
            out.push_str(&format!("{POISON_KEY} incomplete-routing-rule\n"));
        }
        for p in &rule.paths {
            match sanitize_glob(p) {
                Some(clean) if clean == *p => {
                    out.push_str(&format!("{ROUTE_PATH_KEY} {idx} {p}\n"))
                }
                _ => out.push_str(&format!("{POISON_KEY} unusable-routing-glob\n")),
            }
        }
        for r in &rule.reviewers {
            match sanitize_id(r) {
                Some(clean) if clean == *r => {
                    out.push_str(&format!("{ROUTE_REVIEWER_KEY} {idx} {r}\n"))
                }
                _ => out.push_str(&format!("{POISON_KEY} unusable-routing-reviewer\n")),
            }
        }
    }
    out
}

/// The [`MERGE_GATE_FILE`] key carrying one routing rule's path glob (#1176):
/// `route-path <1-based rule index> <glob>`. Hyphenated to match the file's own
/// spelling convention (`all-pass`, `max-diff-lines`), which is not the YAML
/// key's.
pub const ROUTE_PATH_KEY: &str = "route-path";

/// The [`MERGE_GATE_FILE`] key carrying one routing rule's required reviewer:
/// `route-reviewer <1-based rule index> <block id>`. See [`ROUTE_PATH_KEY`].
pub const ROUTE_REVIEWER_KEY: &str = "route-reviewer";

/// The [`MERGE_GATE_FILE`] key carrying [`Gate::max_diff_lines`]. Hyphenated to
/// match `all-pass`/`ci-green` — the file's own spelling convention, which is
/// not the YAML key's (`max_diff_lines`), because the two have different
/// readers and the shim's is a `case` over word-split tokens.
pub const MAX_DIFF_LINES_KEY: &str = "max-diff-lines";

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
    let mut max_diff_lines: Option<u32> = None;
    // #1176. Halves of a routing rule arrive on separate lines and are stitched
    // back together by index after the loop; `BTreeMap` so the stitch walks them
    // in rule order rather than file order.
    let mut rule_paths: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut rule_reviewers: BTreeMap<u32, Vec<BlockId>> = BTreeMap::new();
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
            // #1174. Unlike `require threshold`, an unusable number here has no
            // stricter fallback to land on — "no limit" is the LAXER reading, so
            // the whole file is unusable instead, which the callers report as
            // "malformed — every merge refused". Same direction as the
            // `reviewer`/`also` arms above.
            (Some(MAX_DIFF_LINES_KEY), Some(n), _) => {
                match n.parse::<u32>().ok().filter(|n| *n > 0) {
                    Some(n) => max_diff_lines = Some(n),
                    None => return None,
                }
            }
            // #1176. Same direction as every arm above: a routing line loomux
            // cannot read makes the whole file unusable, because the thing it
            // would have added is a REQUIRED reviewer. A dropped one is a merge
            // that skipped a lane, which is precisely the laxening this reader
            // refuses to perform.
            // **Rejected, never rewritten** — and the comparison is what makes
            // that true. `sanitize_glob`/`sanitize_id` FILTER: `src/[ab]` comes
            // back as `src/ab`, which is not a refusal, it is a DIFFERENT RULE
            // silently substituted for the one the file carries. `gate_file_text`
            // poisons rather than writes such a token, so anything reaching here
            // is a hand edit or a corruption — exactly the case this reader must
            // refuse rather than quietly reinterpret.
            //
            // A **fourth token** is refused for the same reason: neither
            // alphabet contains whitespace, so a line that has any has already
            // been truncated by the word split — `route-path 1 src/a b` reads as
            // the narrower glob `src/a`, which is clean and wrong. Exactly three
            // fields, or the file is not a gate.
            (Some(ROUTE_PATH_KEY), Some(i), Some(g)) => {
                let idx = routing_index(i)?;
                let clean = sanitize_glob(g)?;
                if clean != g || f.next().is_some() {
                    return None;
                }
                rule_paths.entry(idx).or_default().push(clean);
            }
            (Some(ROUTE_REVIEWER_KEY), Some(i), Some(r)) => {
                let idx = routing_index(i)?;
                let clean = sanitize_id(r)?;
                if clean != r || f.next().is_some() {
                    return None;
                }
                rule_reviewers.entry(idx).or_default().push(clean);
            }
            // Anything else — a poison line, a truncated key, a hand edit — makes
            // the whole file unusable. Skipping it would drop a requirement.
            _ => return None,
        }
    }
    // Stitch the two halves. The indices must be exactly 1..=N with BOTH halves
    // present for every one of them: a gap, a duplicate index that lost its
    // partner, or a `route-path` with no `route-reviewer` is a file that cannot
    // be read as the gate someone declared, and an unreadable gate refuses.
    let n = rule_paths.len().max(rule_reviewers.len());
    if n > ROUTING_RULES_MAX {
        return None;
    }
    let mut routing: Vec<RoutingRule> = Vec::new();
    for idx in 1..=n as u32 {
        let paths = rule_paths.remove(&idx)?;
        let reviewers = rule_reviewers.remove(&idx)?;
        if paths.is_empty() || reviewers.is_empty() || paths.len() > ROUTING_PATHS_MAX {
            return None;
        }
        routing.push(RoutingRule { paths, reviewers });
    }
    // Anything left over means the indices were not contiguous — an index past
    // `n`, which nothing above could have consumed.
    if !rule_paths.is_empty() || !rule_reviewers.is_empty() {
        return None;
    }
    // The pair `parse_workflow` refuses (see there for why). Refused here too,
    // rather than trusted to be impossible: this reader's whole job is to be the
    // half that does not assume the other half held.
    if !routing.is_empty() && matches!(require, GateRequire::Threshold(_)) {
        return None;
    }
    (!reviewers.is_empty()).then_some(Gate { require, reviewers, also, max_diff_lines, routing })
}

/// A `route-path`/`route-reviewer` line's rule index — 1-based, so `0` is not an
/// index and is refused with everything else that is not a plain positive number.
fn routing_index(s: &str) -> Option<u32> {
    s.parse::<u32>().ok().filter(|n| *n > 0)
}

/// What the small-batch clause (#1174) says about one PR — the pure decision
/// the shim's shell mirrors and the merge queue re-runs, so there is exactly
/// one definition of "too big" in this codebase and two readers of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSizeVerdict {
    /// No `max_diff_lines` declared, or the PR is within it.
    Ok,
    /// The PR's changed-line count exceeds the declared limit.
    TooLarge { lines: u64, limit: u32 },
    /// The limit is declared and the PR's size could not be read at all.
    /// **Refuses** — the same posture `ci-green` takes on unreadable checks and
    /// the queue takes on an unverifiable base: unknown is never "fine".
    Unknown { limit: u32 },
}

impl DiffSizeVerdict {
    pub fn ok(&self) -> bool {
        matches!(self, DiffSizeVerdict::Ok)
    }
}

/// Apply [`Gate::max_diff_lines`] to a PR whose changed-line count is `lines`
/// (additions + deletions), or `None` when that could not be resolved.
///
/// A gate that declares no limit answers [`DiffSizeVerdict::Ok`] **without
/// looking at `lines`** — the absent-config no-op that keeps every repo which
/// never declared the key on exactly the path it was on before #1174.
pub fn check_diff_size(gate: &Gate, lines: Option<u64>) -> DiffSizeVerdict {
    let Some(limit) = gate.max_diff_lines else {
        return DiffSizeVerdict::Ok;
    };
    match lines {
        None => DiffSizeVerdict::Unknown { limit },
        Some(lines) if lines > u64::from(limit) => DiffSizeVerdict::TooLarge { lines, limit },
        Some(_) => DiffSizeVerdict::Ok,
    }
}

// ── path-based reviewer routing (#1176) ─────────────────────────────────────

/// One routing rule that MATCHED — the "which rules fired and why" half of the
/// gate's own report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FiredRule {
    /// The rule's 1-based position in `gates.merge.routing`, so a refusal can
    /// name the line the author has to look at.
    pub index: u32,
    /// The rule's **declared** globs — all of them, not the one that happened to
    /// match. Deliberate: the shim streams the changed-file list and tests the
    /// globs per file, this walks the globs per rule, and "the first glob that
    /// matched" is therefore a different string on the two sides whenever a rule
    /// has more than one. The rule's own text is the same on both, and it is
    /// also the thing the author needs to see.
    pub paths: Vec<String>,
    /// The reviewers this rule requires.
    pub reviewers: Vec<BlockId>,
}

/// The required-reviewer set for one PR, once routing has been applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutingDecision {
    /// [`Gate::reviewers`] ∪ every fired rule's reviewers, in that order:
    /// the static list first, then the rules in declaration order, each new id
    /// appended once. The `gh` shim appends in exactly this order too, so the
    /// two produce the same list and not merely the same set.
    pub required: Vec<BlockId>,
    /// Which rules fired. Empty when the gate declares no routing, or when it
    /// declares routing and nothing matched.
    pub fired: Vec<FiredRule>,
}

impl RoutingDecision {
    /// `base` with its reviewer list replaced by [`required`](Self::required)
    /// and its routing spent — **the effective gate**, which is what everything
    /// downstream evaluates.
    ///
    /// Routing resolves to a reviewer list and then gets out of the way, so
    /// [`evaluate_merge_gate`], [`gate_need`] and the `body-unchanged` loop stay
    /// exactly one implementation each. A second gate decision that knew about
    /// routing would be a third implementation of the gate, which this codebase
    /// treats as a defect rather than an optimization.
    pub fn gate(&self, base: &Gate) -> Gate {
        Gate {
            require: base.require,
            reviewers: self.required.clone(),
            also: base.also.clone(),
            max_diff_lines: base.max_diff_lines,
            routing: Vec::new(),
        }
    }
}

/// Apply [`Gate::routing`] to one PR's changed files — **the pure decision the
/// `gh` shim's shell mirrors and the merge queue re-runs.**
///
/// `changed` is the PR's repo-relative changed paths, or `None` when that list
/// could not be resolved *or could not be shown to be complete* (see
/// [`ROUTING_FILES_JQ`]).
///
/// - A gate that declares **no routing** answers without looking at `changed` at
///   all — the absent-config no-op that keeps every repo which never wrote the
///   key on exactly the path it was on before #1176, and the same shape
///   [`check_diff_size`] takes.
/// - A gate that **does** declare routing and cannot see the file list answers
///   `None`: **refuse.** Unknown is never safe here in a particularly sharp way
///   — the unknown thing is *which reviewers are required*, so guessing "none of
///   the rules fired" is guessing in favour of merging.
pub fn route_reviewers(gate: &Gate, changed: Option<&[String]>) -> Option<RoutingDecision> {
    // RED EVIDENCE (#1176 scratch A): routing does nothing at all.
    if true {
        let _ = changed;
        return Some(RoutingDecision { required: gate.reviewers.clone(), fired: Vec::new() });
    }
    if gate.routing.is_empty() {
        return Some(RoutingDecision { required: gate.reviewers.clone(), fired: Vec::new() });
    }
    let changed = changed?;
    let mut required = gate.reviewers.clone();
    let mut fired: Vec<FiredRule> = Vec::new();
    for (i, rule) in gate.routing.iter().enumerate() {
        if !rule.paths.iter().any(|g| changed.iter().any(|f| glob_match(g, f))) {
            continue;
        }
        for r in &rule.reviewers {
            if !required.contains(r) {
                required.push(r.clone());
            }
        }
        fired.push(FiredRule {
            index: i as u32 + 1,
            paths: rule.paths.clone(),
            reviewers: rule.reviewers.clone(),
        });
    }
    Some(RoutingDecision { required, fired })
}

/// The first line [`ROUTING_FILES_JQ`] emits when — and only when — it can
/// account for every changed file on the PR.
pub const ROUTED_FILES_OK: &str = "ok";

/// The prefix on each path line [`ROUTING_FILES_JQ`] emits.
///
/// A prefix rather than a bare path so the status word and the data live in
/// different shapes: without it, a repo containing a file literally named `ok`
/// would emit a line indistinguishable from the header.
pub const ROUTED_FILES_PREFIX: &str = "p ";

/// Reduce `gh pr view --json files,changedFiles` to the changed-path list
/// routing needs — **one definition, two consumers**, the same arrangement
/// [`BASE_CHECK_RUNS_JQ`] established: the `gh` shim interpolates this constant
/// into its POSIX body and `mqdriver::pr_files_argv` passes it to `gh --jq`, so
/// the shim and the merge queue cannot ask GitHub different questions.
///
/// Output is a line-oriented protocol read by [`parse_routed_files`] in Rust and
/// by a `while read` loop in shell: the word [`ROUTED_FILES_OK`], then one
/// `p <path>` line per changed file. **Anything else is a refusal** — there is
/// no word for "some of the files", because a partial list is not an answer to
/// "did this PR touch `src/**`".
///
/// **The truncation clause is the whole reason this is a reduction and not a
/// plain `.files[].path`.** `gh pr view --json files` fetches ONE page: the
/// GraphQL `files` connection is capped at 100 while `changedFiles` counts them
/// all. Verified live against this repo — PR #1181 answered
/// `{changed: 32, listed: 32}` and PR #1018 answered `{changed: 135, listed:
/// 100}` — so on any PR past a hundred files the list silently omits the tail.
/// For a *size* gate that omission would be visible; for routing it fails
/// **open** and invisibly: the one file that would have matched a rule sits on
/// page two, no rule fires, and the lane the repo asked for is quietly not
/// required. That is #1181's own pagination finding wearing routing's hat, so it
/// gets the same answer — a page that cannot account for every file is not an
/// answer, and `!=` (not `<`) is the comparison, because a count that disagrees
/// in *either* direction means the payload is not the shape this question rests
/// on.
///
/// The shape it rests on is **checked, not assumed**, for the same reason
/// `BASE_CHECK_RUNS_JQ` checks its own: `null` sorts below every number in jq,
/// so an absent `changedFiles` would make a comparison read false and fall
/// through to the answer that merges. `has(...)` answers "unaccountable"
/// instead.
///
/// A path carrying a **newline or carriage return** is refused too. It cannot be
/// expressed in a one-path-per-line protocol at all, so a reader would silently
/// see two files where the repo has one — and the shim's reader is a merge gate
/// being fed a path that a fork PR's author chose.
pub const ROUTING_FILES_JQ: &str = "if (has(\"files\")|not) or (has(\"changedFiles\")|not) then \"unaccountable\" elif (.files|length) != .changedFiles then \"unaccountable\" elif any(.files[]; (.path|type) != \"string\" or (.path|length) == 0 or (.path|test(\"[\\n\\r]\"))) then \"unaccountable\" else ([\"ok\"] + (.files|map(\"p \" + .path)))[] end";

/// Read [`ROUTING_FILES_JQ`]'s output back into a changed-path list — the Rust
/// half of that protocol, mirrored in shell by the `gh` shim.
///
/// `None` for **anything** that is not a complete, well-formed answer: the
/// `unaccountable` word, an empty capture (gh failed, jq errored, the PR is
/// gone), a line without the [`ROUTED_FILES_PREFIX`], an empty path. Callers
/// hand that straight to [`route_reviewers`] as `None`, which refuses.
///
/// A PR with genuinely zero changed files is `Some(vec![])`, not `None`: that is
/// a complete answer that happens to be empty, and the distinction is the whole
/// difference between "no rule matched" and "loomux cannot say".
pub fn parse_routed_files(out: &str) -> Option<Vec<String>> {
    let mut lines = out.lines();
    if lines.next().map(str::trim) != Some(ROUTED_FILES_OK) {
        return None;
    }
    let mut files: Vec<String> = Vec::new();
    for line in lines {
        // A trailing blank line is how a capture ends, not a file.
        if line.trim().is_empty() {
            continue;
        }
        let path = line.strip_prefix(ROUTED_FILES_PREFIX)?;
        if path.is_empty() {
            return None;
        }
        files.push(path.to_string());
    }
    Some(files)
}

// ── schema field-inventory pin (#382 P1 rev-26 NB1) ─────────────────────────
//
// Pure logic, no windowing/PTY code touched — safe as an inline unit test
// (constraint 4: only tests that link the FULL LIB in a way that needs the
// comctl32-v6 manifest have to be integration tests; this needs neither the
// registry nor the manifest, see e.g. `winpath.rs`/`cliprobe.rs` for the same
// call).

#[cfg(test)]
mod tests {
    use super::*;

    /// **Field-inventory pin, not a runtime one.** The three `human_gate`
    /// denial tests in `tests/workflow.rs` catch a field *renamed* onto the
    /// reserved spelling — they would flip from red to a passing rejection if
    /// someone tried it. They do NOT catch a field *added* under a different,
    /// still gate-shaped name (`auto_merge:`, `skip_review:`, …): that field
    /// would pass every existing test, because nothing previously asserted
    /// the SET of fields these types accept, only that a few specific
    /// spellings are absent from it.
    ///
    /// This closes that gap at compile time instead of runtime: each
    /// destructure below names every field the type has right now and binds
    /// it to `_`, with no `..` to swallow the rest. Rust's exhaustive
    /// struct-pattern rule means a field ADDED to `RawWorkflow`, `RawIntake`
    /// or `RawIntakeLabels` without being named here is a **compile error**,
    /// not a silently passing test.
    ///
    /// NEW FIELD ADDED TO THE INTAKE SCHEMA — confirm it cannot weaken the
    /// human gate, then update this inventory (name the field in the
    /// matching destructure below and re-run this test to prove it still
    /// compiles).
    #[test]
    fn intake_schema_field_inventory_is_exhaustively_named() {
        fn raw_workflow_fields(v: RawWorkflow) {
            let RawWorkflow {
                version: _,
                name: _,
                authored_with: _,
                blocks: _,
                edges: _,
                gates: _,
                intake: _,
                merge_queue: _,
                // #858. Confirmed against the rule above before being named
                // here: `resources:` is a map of names to two NUMBERS
                // (`RawResource` — `slots`, `max_hold_minutes`, and
                // `deny_unknown_fields`). It names no branch, no reviewer, no
                // program and no agent, and nothing in the merge/release path
                // reads it — so there is no spelling of this block that can
                // weaken the human gate. What it CAN do is make an agent wait,
                // which is the whole of its restrict-only contract.
                resources: _,
                // #1175. Confirmed against the rule above before being named
                // here: `board:` is per-status WIP limits — a handful of
                // integers keyed by loomux's own board statuses, plus one
                // bool (`RawBoard`/`RawWip`, both `deny_unknown_fields`). It
                // names no branch, no reviewer, no program and no agent, and
                // nothing in the merge/release path reads it. What it CAN do
                // is make a board write warn, or refuse an AGENT's write —
                // never a human's, and never a merge.
                board: _,
            } = v;
        }
        // #1175: the same inventory rule one level down. A field added to
        // `RawBoard` is a new board policy, and a field added to `RawWip` is a
        // new cappable status — both are changes a reader of this file must
        // see, not ones that pass every existing test.
        fn raw_board_fields(v: RawBoard) {
            let RawBoard { wip: _, enforce: _ } = v;
        }
        fn raw_wip_fields(v: RawWip) {
            let RawWip {
                queued: _,
                in_progress: _,
                review: _,
                pr: _,
                prototype: _,
                human_testing: _,
                blocked: _,
            } = v;
        }
        fn raw_intake_fields(v: RawIntake) {
            let RawIntake { source: _, labels: _ } = v;
        }
        fn raw_intake_labels_fields(v: RawIntakeLabels) {
            let RawIntakeLabels { ready: _, investigate: _, owned: _, prototype: _, hold: _ } = v;
        }
        // #581 §11.2: `merge_queue:` is policy for a host-run queue that pushes
        // refs on the backend's own authority, so the inventory rule matters
        // here for the same reason it does for `intake:` — a field ADDED to
        // this schema must be a visible change, not one that passes every
        // existing test. Nothing here may ever name a branch or grant a
        // capability; the target comes from the enqueued PR's live base (§4)
        // and the default-branch refusals (§7) are not reachable from config.
        fn raw_merge_queue_fields(v: RawMergeQueue) {
            let RawMergeQueue { enabled: _, max_batch: _, checks_timeout_minutes: _ } = v;
        }
        // Referenced, never called — the compiler still type-checks (and
        // therefore exhaustiveness-checks) every function body above whether
        // or not it runs. This line only exists to avoid a dead-code warning.
        let _ = (
            raw_workflow_fields as fn(RawWorkflow),
            raw_intake_fields as fn(RawIntake),
            raw_intake_labels_fields as fn(RawIntakeLabels),
            raw_merge_queue_fields as fn(RawMergeQueue),
            raw_board_fields as fn(RawBoard),
            raw_wip_fields as fn(RawWip),
        );
    }
}
