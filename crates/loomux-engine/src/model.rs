//! The orchestration **data model** — the closed capability classes, the deny
//! tiers they map to, the per-CLI capability table, and the pure functions that
//! read them (#888 slice A2 batch 4).
//!
//! Everything here is data plus `match`: no I/O, no registry state, no
//! `AppHandle`. It moved ahead of the `workflow` cluster on purpose. `workflow`
//! is the next batch, and it reaches for exactly these symbols — `CLI_CAPS` for
//! its knob remedies, `EFFORT_LEVELS`/`CONTEXT_VARIANTS` for its closed
//! vocabularies, `sanitize_model_opt` for a block's `model:`, and [`Role`] for a
//! block's `kind:`. Had they stayed behind, moving `workflow` would have pointed
//! the arrow back into `src-tauri`, which is the one thing this crate exists to
//! prevent. Batch 3's finding applies again: an edge into `mod.rs` is not an
//! edge into the registry, and a pure callee is cut by moving the callee.
//!
//! # What deliberately did NOT come with `Role`
//!
//! `Role::template()` and `Role::instructions_file()` were inherent methods on
//! this enum, and they are now free functions (`role_template`,
//! `role_instructions_file`) in `src-tauri/src/orchestration/mod.rs`, next to
//! the `include_str!` templates they load.
//!
//! An inherent impl has to live in the crate that defines the type, so keeping
//! them as methods would have dragged `templates/*.md` — and with them the
//! byte-golden fixture root `src-tauri/tests/fixtures/pre222/` that pins those
//! four files — into this crate as a side effect of a data move. Role templates
//! are product content governed by their own design notes and their own
//! re-bless procedure; relocating their fixture root silently, as a consequence
//! of a refactor, is a failure mode nothing catches. Rewriting the call sites
//! is the other option, and it is one the compiler checks exhaustively: every
//! missed site is a build error, on CI, before review. A mechanical failure the
//! compiler finds beats a silent one no test watches. Same question batch 2
//! asked of `GroupId`'s tripwire, answered the other way round: there, the
//! guard had to follow the type; here, the content must not.
//!
//! # Widened on the way in
//!
//! `Role::prefix`, `Role::as_str`, `default_model` and `sanitize_model_opt`
//! were `pub(crate)` in `src-tauri`. Their callers stayed there, so no
//! visibility narrower than `pub` still reaches them — the same conversion
//! batch 3 made for `notify::check_is_pending`/`check_is_failing`, and the same
//! question to ask of it: whether the item is one this crate is content to
//! expose, not merely whether it compiles. All four are total functions over a
//! closed enum or a `&str`, with no state and no invariant a caller could
//! violate by holding them.

use serde::Serialize;

/// An agent's **capability class** — the closed enum (#222).
///
/// Before the block model this enum *was* an agent's identity: it decided the
/// persona, the template, the model, the CLI and the capabilities all at once.
/// Now identity is a `workflow::BlockId` and this enum carries only the part
/// that must stay closed: **what an agent is structurally allowed to do**.
///
/// That closure is the security spine of #222. Personas are unbounded data
/// authored in a repo file; capabilities are not. A workflow file *selects* a
/// class here — it can never define one, and there is no `read_only: false`
/// escape hatch. So a repo can declare five reviewers with five prompts and five
/// models, and it cannot make one of them anything but a reviewer: the deny-flags
/// (`build_agent_command`), the cwd rule (`spawn_agent_ex`) and the MCP tool
/// scope (`mcp::tool_defs`) all key off this enum, and it has exactly four
/// values.
///
/// What each class *is* varies, and the enum should not be read as promising more
/// than it enforces — read [`Role::containment`] for the exact per-class tier. A
/// planner is structurally read-only ([`Role::is_read_only`] — editing tools AND
/// `git commit`/`git push` denied at the CLI). A reviewer (#462) is structurally
/// denied the CLI's *file-editing tools*, but keeps the shell, so its "never
/// pushes" stays instruction-backed. The guarantee is over which posture a block
/// gets, not that every posture is a sandbox.
///
/// The name `Role` survives because ~72 call sites and the persisted wire
/// format use it; read it as "capability class".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Orchestrator,
    Worker,
    Reviewer,
    /// Read-only explorer: investigates the codebase and writes a structured
    /// implementation plan (as a GitHub issue comment), then reports and
    /// exits. A planner NEVER writes code, branches, or PRs. It counts as a
    /// delegate against the live-agent cap, like a worker/reviewer.
    Planner,
    /// A standalone (non-orchestration) pane given a channel-scoped MCP
    /// identity (#271 W3 addendum, part A). Its whole purpose is
    /// `tool_defs`/`call_tool` returning/dispatching exactly `channel_send` +
    /// `channel_status` — no group, no board, no delegates, zero group-scoped
    /// power. Lives in the reserved `__solo__` pseudo-group. Never counts
    /// against a live-agent cap (there is none for `__solo__`) and never
    /// traverses `spawn_agent_ex`/`build_agent_command` — it has no block, no
    /// persona, no role template.
    Solo,
}

impl Role {
    /// Agent-id prefix (`w-3`). Reached through `workflow::Block::prefix` at
    /// the spawn sites — a block's prefix is derived from its class so ids stay
    /// short and the roster/badge conventions that parse them keep working.
    pub fn prefix(self) -> &'static str {
        match self {
            Role::Orchestrator => "orch",
            Role::Worker => "w",
            Role::Reviewer => "rev",
            Role::Planner => "plan",
            // Solo panes mint their id as `solo-N` directly (see
            // `OrchRegistry::solo_prepare`), never through `block.prefix()` —
            // they have no block. Never reached in practice.
            Role::Solo => "solo",
        }
    }
    /// Lowercase wire/label name (matches the `Serialize` rename). Unlike
    /// `role_template`/`role_instructions_file` (the two mappings that stayed in
    /// `src-tauri`, see this module's doc), this one IS reached for a solo
    /// member — `channel_member_label` formats it into the identity line every
    /// channel message/notice carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator",
            Role::Worker => "worker",
            Role::Reviewer => "reviewer",
            Role::Planner => "planner",
            Role::Solo => "solo",
        }
    }
    /// The **deny tier** this class launches its CLI under — the single place
    /// that decides what `build_agent_command`/`build_agent_argv` clamp (#462).
    ///
    /// This is a `match` on the closed enum on purpose: a fifth capability class
    /// cannot be added without deciding, at compile time, what it may do. That is
    /// the same property that makes `Role` the security spine of #222 — a
    /// containment tier is never *defaulted into*, and a workflow file selects a
    /// class, never a tier.
    pub fn containment(self) -> Containment {
        match self {
            // Both exist to change the repo: an orchestrator drives git/gh and a
            // worker writes the code. Denying either anything here would only
            // break the flow the group is for.
            Role::Orchestrator | Role::Worker => Containment::None,
            // #462. A reviewer reads a diff and judges it — it never has a
            // legitimate reason to reach for an editing tool, so that reach is
            // structurally removed rather than merely discouraged. Its shell
            // stays whole: running the tests and `gh pr checkout <n> --detach`
            // ARE the job (see `Containment::NoEdits` for what that costs).
            Role::Reviewer => Containment::None,
            Role::Planner => Containment::ReadOnly,
            // Solo panes never traverse `spawn_agent_ex`/`build_agent_command`
            // (see the variant's doc) — an arbitrary human-launched CLI loomux
            // only lends a channel identity to, so there is no spawn of ours to
            // clamp. Unreachable rather than a policy statement.
            Role::Solo => Containment::None,
        }
    }
    /// The capability that used to be spelled `role == Role::Planner` inline at
    /// the spawn site. Now it is a *property of the class*, which is what makes
    /// "a workflow file can never grant a capability" checkable: there is no
    /// other way to become read-only, and no way to stop being it.
    ///
    /// Narrower than "gets deny flags" since #462 — a reviewer gets deny flags
    /// too, but is NOT read-only (it keeps the shell, hence `git commit`). The
    /// callers that ask this question — the `allow:` ban in `parse_workflow` /
    /// `persona_inject` — mean the *fully* read-only class specifically, so they
    /// must not silently start matching reviewers; deriving it from
    /// [`Self::containment`] keeps the two definitions from drifting apart.
    pub fn is_read_only(self) -> bool {
        self.containment().is_read_only()
    }
}

/// How hard loomux clamps an agent's CLI at spawn — the **deny tier**, selected
/// per capability class by [`Role::containment`] and consumed by
/// `OrchRegistry::build_agent_command` / `OrchRegistry::build_agent_argv`.
///
/// An ordered ladder, not a bag of independent switches: each tier is the one
/// below it plus more. Spelling it as an enum rather than a pair of bools is
/// what makes "which agents may edit files" answerable by reading one `match`
/// (`Role::containment`) instead of auditing every call site — and what makes a
/// new class a compile error instead of a default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Containment {
    /// No CLI-level denial at all: the agent holds whatever its permission mode
    /// gives it (orchestrator, worker).
    None,
    /// **The CLI's file-editing tools are denied; the shell is not** (#462,
    /// today: a reviewer). On Claude that is `--disallowedTools Edit Write
    /// NotebookEdit`; on Copilot the `write` category. Deny beats allow on both
    /// CLIs, so no persona `allow:` and no permission mode re-grants them.
    ///
    /// Be exact about the size of this guarantee, because it is smaller than
    /// "cannot write": it removes the *frictionless, default* path to editing a
    /// file — the one an agent takes without deciding to — and leaves the shell
    /// path (`sed -i`, a heredoc, `python -c`) reachable, because denying that
    /// would mean denying `Bash`, and `Bash` is how a reviewer runs the tests.
    /// So this is containment of the accident, not of the adversary; the
    /// no-push half of a reviewer's contract stays instruction-backed and
    /// bounded by its own worktree (#359), exactly as before.
    NoEdits,
    /// `NoEdits` **plus** the git-mutation subcommands (`commit`/`push`), and
    /// always unattended regardless of the group's `auto_ops` (today: a
    /// planner). `gh` deliberately stays reachable — the planner's deliverable
    /// is a `gh issue comment` — so even this tier is not a full sandbox.
    ReadOnly,
}

impl Containment {
    /// Where this tier sits on the ladder — `None` < `NoEdits` < `ReadOnly`.
    ///
    /// The doc above calls this type "an ordered ladder, not a bag of
    /// independent switches"; until #267 nothing could actually *ask* for that
    /// order, so [`cli_can_host`] would have had to re-encode it as a `match`
    /// that a fifth tier could silently fall out of. Spelled once, here.
    pub fn rank(self) -> u8 {
        match self {
            Containment::None => 0,
            Containment::NoEdits => 1,
            Containment::ReadOnly => 2,
        }
    }
    /// Deny the CLI's file-editing tools (`CLAUDE_EDIT_DENY_TOOLS` /
    /// `COPILOT_EDIT_DENY_TOOLS`).
    pub fn denies_edits(self) -> bool {
        !matches!(self, Containment::None)
    }
    /// Deny `git commit` / `git push` through the CLI's shell tool
    /// (`CLAUDE_READONLY_DENY_GIT` / `COPILOT_READONLY_DENY_GIT`).
    pub fn denies_git_mutation(self) -> bool {
        matches!(self, Containment::ReadOnly)
    }
    /// Run unattended (Auto perms + the git/gh pre-approval) even in a group
    /// that is not `auto_ops`. True only for [`Containment::ReadOnly`]: a
    /// planner has no human in its pane and nothing it can mutate, so gating it
    /// only deadlocks it. A reviewer is NOT promoted this way — it follows the
    /// group's `auto_ops` exactly as it did before #462, so extending the deny
    /// flags to reviewers never *widens* what a reviewer may do.
    pub fn forces_unattended(self) -> bool {
        matches!(self, Containment::ReadOnly)
    }
    /// The **fully** read-only tier — the only one
    /// `claude_effective_permission_mode` may be handed `true` for (#465).
    /// That function's own doc states the rule this predicate exists to keep
    /// honest: never for a non-read-only agent, because `dontAsk` auto-denies
    /// every tool call outside `--allowedTools`, and a reviewer's shell is
    /// exactly that. Same question [`Role::is_read_only`] answers, one rung
    /// down, so the two cannot drift.
    ///
    /// Three `ReadOnly`-only predicates is not redundancy: this,
    /// [`Self::denies_git_mutation`] and [`Self::forces_unattended`] name three
    /// *different* facts that happen to coincide at the top rung today. Each
    /// call site asks the one it actually means, so a future tier that split
    /// them apart would land correctly instead of silently.
    pub fn is_read_only(self) -> bool {
        matches!(self, Containment::ReadOnly)
    }
}

/// Which agent CLI a group runs. Each needs an adapter in
/// `build_agent_command` + `write_mcp_config`; anything unknown falls back
/// to Claude (explicitly, in `clamped`, never silently at spawn time).
///
/// Kept as a flat list because ~10 call sites read it as one (`contains`,
/// `join(", ")` in error messages). The *reasons* a CLI is or isn't in it —
/// and what it can do once it is — live in [`CLI_CAPS`], pinned against this
/// list by `supported_clis_match_the_capability_table`.
pub const SUPPORTED_CLIS: [&str; 4] = ["claude", "copilot", "gemini", "opencode"];

/// What loomux can actually make one agent CLI do — **the per-CLI capability
/// record** (#267 stage 2).
///
/// This exists because "loomux can spawn this CLI into a group" and "this CLI
/// can host every capability class" turned out to be two different questions,
/// and conflating them was already a latent panic: `solo_prepare` derived
/// `has_seam` straight from [`SUPPORTED_CLIS`] and then matched the per-CLI MCP
/// *flag string* with an `unreachable!()` arm, so the first CLI whose MCP config
/// is not argv-deliverable would have crashed a solo launch on the day it was
/// added. PR #323 named that gap when Ante hit it ("decouple 'is a legitimate
/// group-spawn CLI' from 'has an MCP flag seam'"); gemini is the CLI that makes
/// it real, and this table is that decoupling.
///
/// **Data, not special-cases** (CLAUDE.md constraint 8). A capability difference
/// between vendors is a fact about the vendor, so it is written down once, here,
/// and consulted — never re-derived as an `if cli == "..."` at a call site that
/// then silently disagrees with another.
///
/// Every field is a docs-verified claim about a specific CLI; see each row.
#[derive(Clone, Copy, Debug)]
pub struct CliCaps {
    /// The CLI's program name, as a block's `cli:` spells it.
    pub cli: &'static str,
    /// loomux has a group-spawn adapter for it (`build_agent_command` /
    /// `build_agent_argv` / `write_mcp_config`) — i.e. it is in
    /// [`SUPPORTED_CLIS`].
    pub orchestration: bool,
    /// Its per-agent MCP config can be delivered **entirely on argv**
    /// (claude's `--mcp-config`, copilot's `--additional-mcp-config`).
    ///
    /// False does not mean "no MCP": gemini's server is declared in a
    /// generated settings file pointed at by an environment variable
    /// (`GEMINI_CLI_SYSTEM_SETTINGS_PATH`), which a *group* spawn can set on
    /// the pane and a solo launch — which only appends a flag string to a
    /// command line the human owns — cannot. So this is the predicate
    /// `solo_prepare` needs, and it is NOT the same question as
    /// [`Self::orchestration`].
    pub mcp_argv_seam: bool,
    /// The deepest [`Containment`] tier loomux can actually *enforce* on this
    /// CLI at spawn. A class whose [`Role::containment`] sits above this may
    /// not run on it — see [`cli_can_host`].
    ///
    /// This is the field that keeps the #462 reviewer guarantee honest across
    /// vendors: containment is a property of the CLI's own permission engine,
    /// and a CLI that cannot express "deny the editing tools, keep the shell"
    /// cannot host a reviewer no matter how good its model is.
    pub max_containment: Containment,
    /// Why this CLI is limited to [`Self::max_containment`] — quoted into the
    /// refusal so a rejected workflow says what is actually missing instead of
    /// "unsupported".
    pub containment_note: &'static str,
    /// The thinking-effort levels loomux can actually **set** on this CLI at
    /// spawn — a subset of [`EFFORT_LEVELS`], or empty for a CLI with no
    /// loomux-usable seam (#687).
    ///
    /// Empty is a claim, not an omission, and it is why the note below is not
    /// optional: a knob loomux cannot deliver renders **disabled with a
    /// reason** in the launcher and is a **parse error** in a workflow file —
    /// never silently ignored, which is the failure mode where a human sets a
    /// thinking level and quietly does not get one.
    pub effort_levels: &'static [&'static str],
    /// How this CLI's effort level is (or is not) reachable — the launcher's
    /// hint text and the parse error's reason. Always non-empty; pinned by
    /// `every_cli_row_explains_both_knobs`.
    pub effort_note: &'static str,
    /// The context-window variants loomux can set on this CLI — a subset of
    /// [`CONTEXT_VARIANTS`], or empty. Same "empty is a claim" rule as
    /// [`Self::effort_levels`].
    pub context_variants: &'static [&'static str],
    /// How this CLI's context window is (or is not) reachable. Always
    /// non-empty.
    pub context_note: &'static str,
}

/// The closed vocabulary of a block's `effort:` (#687) — the thinking level.
///
/// This is loomux's vocabulary, not one CLI's: a value outside it is a parse
/// error whatever the CLI, and a value inside it is still refused on a CLI
/// whose [`CliCaps::effort_levels`] omits it. `ultracode` is deliberately
/// absent — it is a Claude Code orchestration mode, not a model effort level.
pub const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// The closed vocabulary of a block's `context:` (#687) — the context-window
/// variant. One entry today; the shape is a value set, not a boolean, so a
/// future tier is a row here rather than a new key.
pub const CONTEXT_VARIANTS: &[&str] = &["1m"];

/// The capability table. Rows exist for CLIs loomux does **not** spawn too:
/// a CLI that was evaluated and rejected is a finding worth keeping, and an
/// absent row is indistinguishable from an oversight.
///
/// Sources (fetched reference docs, per the `agent-cli-reference` skill —
/// verified 2026-08-01):
///
/// - **claude** — [CLI reference](https://code.claude.com/docs/en/cli-reference)
///   (`--mcp-config`, `--strict-mcp-config`, `--disallowedTools`); the deny
///   list is what `CLAUDE_EDIT_DENY_TOOLS` rests on.
/// - **copilot** — [CLI configuration guide](https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/configure-copilot-cli)
///   (`--additional-mcp-config`, `--deny-tool`); see
///   `KNOWN_COPILOT_DENY_CATEGORIES`.
/// - **gemini** — MCP servers are a `settings.json` key with no CLI-flag
///   equivalent ([configuration reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md)),
///   hence `mcp_argv_seam: false`; containment comes from the
///   [policy engine](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/policy-engine.md)
///   `deny` decision plus `tools.exclude`, both of which can name
///   `write_file`/`replace` while leaving `run_shell_command` — see
///   `GEMINI_EDIT_DENY_TOOLS`.
/// - **codex** — evaluated for #267 stage 2 and **rejected as a reviewer
///   host**. Its only containment axis is `sandbox_mode`
///   (`read-only | workspace-write | danger-full-access`,
///   [config reference](https://developers.openai.com/codex/config-reference)),
///   and its `tools` section exposes only `view_image` / `web_search` — there
///   is no way to deny the editing tool by name. `read-only` is not the
///   reviewer's tier either: per the
///   [permissions docs](https://developers.openai.com/codex/permissions), in
///   read-only mode Codex "can read files and answer questions, but requires
///   approval to make edits, **run commands, or access network**" — which
///   removes the tests and the `gh` a reviewer's job is made of. That leaves
///   `workspace-write`, i.e. no containment at all, so its ceiling is
///   [`Containment::None`] and a reviewer or planner block cannot run on it.
///
/// The `effort_*` / `context_*` fields (#687) are docs-verified the same way
/// (checked 2026-08-02):
///
/// - **claude** — `--effort <level>` is in the
///   [CLI reference](https://code.claude.com/docs/en/cli-reference); per
///   [model config](https://code.claude.com/docs/en/model-config) §Adjust
///   effort level, a model that does not support a level falls back to the
///   highest one it supports at or below it, so any of the five is safe to
///   emit. §Extended context documents the **`[1m]` model-alias suffix**
///   (`sonnet[1m]`, `claude-opus-4-8[1m]`) — there is no separate flag.
/// - **copilot** — the
///   [CLI programmatic reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference)
///   documents `effortLevel` in `~/.copilot/settings.json` and **no** flag or
///   environment variable for it, and the
///   [command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference)
///   puts the context window behind the interactive `/context` control.
/// - **gemini** — thinking level is
///   `modelConfigs.aliases.<alias>.thinkingConfig` in settings
///   ([configuration reference](https://geminicli.com/docs/reference/configuration/));
///   loomux already generates gemini's settings file, so the seam exists, but
///   the schema needs a live verification loomux's own agents may not perform
///   (CLAUDE.md constraint 3), so it stays unwired and the row says so.
/// - **opencode** — its knobs are read from the CLI's own source at the
///   version pin recorded in `doc/design/opencode.md`, because the published
///   docs describe reasoning effort only as per-model provider config. There
///   IS a session-scoped variant flag, but only on `opencode run`
///   (`--variant`, "model variant (provider-specific reasoning effort, e.g.,
///   high, max, minimal)"), and loomux spawns the TUI, whose option table has
///   no `variant`. The reachable seam on the TUI path is per-agent
///   `agent.<name>.variant` in the config document loomux already generates —
///   real, but its per-model vocabulary is provider-specific and unverified
///   against a live run, so the row stays empty and says exactly that rather
///   than claiming a level loomux might not be able to deliver.
pub const CLI_CAPS: &[CliCaps] = &[
    CliCaps {
        cli: "claude",
        orchestration: true,
        mcp_argv_seam: true,
        max_containment: Containment::ReadOnly,
        containment_note: "--disallowedTools denies tools by name and beats both the permission mode and the allow list",
        effort_levels: EFFORT_LEVELS,
        effort_note: "--effort <level> is a session-scoped flag; a model that lacks a level falls back to the highest one it supports at or below it",
        context_variants: CONTEXT_VARIANTS,
        context_note: "the [1m] model-alias suffix (sonnet[1m]) — access is plan- and credit-gated, so a tier the account cannot serve fails at the CLI, visibly in the pane",
    },
    CliCaps {
        cli: "copilot",
        orchestration: true,
        mcp_argv_seam: true,
        max_containment: Containment::ReadOnly,
        containment_note: "--deny-tool denies the write category and shell prefixes, and deny beats --allow-all-tools",
        effort_levels: &[],
        effort_note: "copilot reads effortLevel from ~/.copilot/settings.json — its programmatic reference documents no flag and no environment variable, and loomux never writes a user's global settings file",
        context_variants: &[],
        context_note: "copilot's context window is an interactive-only control (/context) with no argv or settings equivalent",
    },
    CliCaps {
        cli: "gemini",
        orchestration: true,
        mcp_argv_seam: false,
        max_containment: Containment::ReadOnly,
        containment_note: "policy-engine deny rules and tools.exclude both name built-in tools (write_file/replace) and shell command prefixes, and both outrank --approval-mode yolo",
        effort_levels: &[],
        effort_note: "gemini's thinking level is a settings-file key (modelConfigs.aliases.<alias>.thinkingConfig) — the generated-settings seam exists, but the schema is unverified against a live run, so loomux does not write it yet",
        context_variants: &[],
        context_note: "gemini's context window is model-determined; its compression knobs (model.compressionThreshold) are compaction, not window size",
    },
    CliCaps {
        cli: "opencode",
        orchestration: true,
        // Its MCP server is a config-document key with no CLI-flag equivalent,
        // and the document is delivered by an environment variable a *group*
        // spawn can set on the pane while a solo launch — which only appends a
        // flag string to a command line the human owns — cannot. Same shape as
        // gemini, so solo opencode panes stay delivery-only (#288).
        mcp_argv_seam: false,
        max_containment: Containment::ReadOnly,
        containment_note: "permission rules deny by key: `edit` is the key every file-modifying tool asks under, `bash` narrows by command pattern, and a deny is refused before any prompt — so deny outranks --auto",
        effort_levels: &[],
        effort_note: "opencode's reasoning effort is a model VARIANT: a session flag on `opencode run` (--variant) but absent from the TUI loomux spawns, and settable per-agent in loomux's generated config (agent.<name>.variant, observed values minimal|high|max) — the seam exists, but the per-model vocabulary is provider-specific and unverified against a live run, so loomux does not write it yet",
        context_variants: &[],
        context_note: "opencode's context window is model-determined; no session-scoped variant switch is documented or present in the TUI's options",
    },
    CliCaps {
        cli: "codex",
        orchestration: false,
        mcp_argv_seam: false,
        max_containment: Containment::None,
        containment_note: "codex has no tool-level edit deny (its `tools` config covers only view_image/web_search); its sandbox_mode is all-or-nothing, and its read-only rung also blocks running commands and network access, so a contained agent could neither edit nor run the tests it exists to run",
        effort_levels: &[],
        effort_note: "codex is evaluated but not spawned by loomux, so no knob is delivered on it at all (see max_containment)",
        context_variants: &[],
        context_note: "codex is evaluated but not spawned by loomux, so no knob is delivered on it at all (see max_containment)",
    },
];

/// The capability record for a CLI, or `None` for one loomux has never
/// evaluated. Case-sensitive on purpose: block `cli:` values are already
/// trimmed and matched exactly everywhere else.
pub fn cli_caps(cli: &str) -> Option<&'static CliCaps> {
    CLI_CAPS.iter().find(|c| c.cli == cli)
}

/// **May this CLI host this capability class?** — the containment gate (#267).
///
/// `Ok(())` or a message naming what the CLI is missing. Consulted at BOTH
/// ends: `parse_workflow` (so a repo learns at load time) and `spawn_agent_ex`
/// (so a hand-edited `group.json` can't route around the parser), the same
/// belt-and-braces the CLI check itself already gets.
///
/// The check is one comparison — `role.containment()` against the CLI's
/// [`CliCaps::max_containment`] — because [`Containment`] is an ordered ladder
/// (see its doc). That is deliberately the *whole* rule: a future CLI is
/// admitted by writing its row, not by editing this function.
pub fn cli_can_host(cli: &str, role: Role) -> Result<(), String> {
    let want = role.containment();
    let Some(caps) = cli_caps(cli) else {
        return Ok(()); // unknown CLIs are rejected by the SUPPORTED_CLIS check, not here
    };
    if want.rank() <= caps.max_containment.rank() {
        return Ok(());
    }
    Err(format!(
        "cli {cli:?} cannot host a {} block: that class runs under {want:?} containment and \
         {cli} tops out at {:?} — {}",
        role.as_str(),
        caps.max_containment,
        caps.containment_note
    ))
}

/// Default model for a capability class on a given CLI. Copilot picks its own
/// best model ("auto"); on Claude the reasoning-heavy classes (orchestrator,
/// planner) get the strong tier and the executing ones (worker, reviewer) the
/// mid tier.
pub fn default_model(cli: &str, role: Role) -> &'static str {
    if cli == "copilot" {
        return "auto";
    }
    // Gemini: `pro` for every class, deliberately, unlike claude's strong/mid
    // split. Its documented alias set is `pro` ("for complex reasoning tasks")
    // against `flash`/`flash-lite` (speed tiers) — per the
    // [CLI reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/cli-reference.md)'s
    // model-alias table — so there is no mid reasoning tier to map a worker or
    // reviewer onto. Defaulting a cross-model reviewer (#267) onto a speed tier
    // would hand back exactly the weakened second opinion the whole feature
    // exists to avoid. A block pins its own `model:` when it wants otherwise.
    //
    // An ALIAS, not a model id (`gemini-2.5-pro`): the alias is what tracks the
    // vendor's current best, and #329's lesson was that hardcoding a model
    // table ages badly.
    if cli == "gemini" {
        return "pro";
    }
    // OpenCode: EMPTY, deliberately — read by every emit path as "say nothing",
    // so the pane inherits whatever model the human configured (their
    // `opencode.json`, their `/models` pick). Unlike claude's strong/mid pair
    // or gemini's single `pro`, opencode has no vendor-neutral alias at all:
    // its ids are `provider_id/model_id` against a catalog of dozens of
    // providers, so any default loomux picked would be a hardcoded model table
    // — the thing #329 says ages badly — and would also silently override a
    // human who had already chosen. A block that wants a specific model pins
    // its own `model:`, and the launcher offers a curated list.
    if cli == "opencode" {
        return "";
    }
    match role {
        Role::Orchestrator | Role::Planner => "opus",
        Role::Worker | Role::Reviewer => "sonnet",
        // A solo pane's model is whatever the human picked in the launcher —
        // loomux never spawns or models it. Never reached.
        Role::Solo => unreachable!("solo panes are never spawned through the model-resolution path"),
    }
}

/// `sanitize_model` with no fallback: an empty/unusable model stays empty, which
/// a block reads as "inherit the class default for my CLI" (`workflow::model_of`).
/// The workflow parser needs this because a block's *effective* CLI isn't known
/// until the group default is in hand.
pub fn sanitize_model_opt(m: &str) -> String {
    m.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire/label name of every capability class, written out once as
    /// literals rather than derived from either producer.
    ///
    /// Deriving the expectation from `as_str()` would pin `Serialize` to
    /// whatever `as_str` happens to say, and vice versa — the table has to be a
    /// third party or neither test means anything. These five strings are a
    /// **persisted and cross-process contract**: they are what `agents.json`
    /// carries between app launches, what `list_agents`/`session_roles` hand
    /// the webview, and what the frontend matches on to decide a roster row's
    /// badge. Changing one is a breaking change to a state file, not a rename.
    const WIRE_NAMES: [(Role, &str); 5] = [
        (Role::Orchestrator, "orchestrator"),
        (Role::Worker, "worker"),
        (Role::Reviewer, "reviewer"),
        (Role::Planner, "planner"),
        (Role::Solo, "solo"),
    ];

    /// `Role`'s serde form is `rename_all = "lowercase"`, and this is what
    /// makes that attribute load-bearing rather than decorative.
    ///
    /// The integration suite reaches this representation only through
    /// `list_agents`, and only for the three classes it happens to spawn and
    /// read back (`a["role"] == "worker" | "reviewer" | "planner"` in
    /// `src-tauri/tests/orchestration.rs`). `Orchestrator` and `Solo` have no
    /// such assertion anywhere: an `#[serde(rename = "…")]` on either variant
    /// would change what every future `agents.json` records for it and the
    /// whole suite would stay green. That gap is per-variant, so it needs a
    /// per-variant pin.
    #[test]
    fn every_role_variant_serializes_to_its_documented_lowercase_wire_name() {
        for (role, want) in WIRE_NAMES {
            let got = serde_json::to_value(role).expect("Role is Serialize");
            assert_eq!(
                got,
                serde_json::Value::String(want.to_string()),
                "{role:?} serializes to {got} — the wire name is persisted in agents.json and \
                 matched by the frontend, so renaming a variant's serde form silently \
                 reinterprets every group already on disk"
            );
        }
    }

    /// `as_str`'s own doc says it "matches the `Serialize` rename". Until this
    /// test that was an unenforced claim in a comment, and the two producers
    /// are genuinely independent — `as_str` is a hand-written `match`, the
    /// serde form is an attribute — so nothing stopped one from drifting.
    ///
    /// They are not interchangeable in practice, which is why both matter:
    /// `session_roles` records a roster row through `as_str`, while
    /// `list_agents` emits the same agent's class through `Serialize`. A drift
    /// between them is a session browser and a roster disagreeing about what
    /// one agent is.
    #[test]
    fn as_str_returns_the_same_wire_name_serialization_does() {
        for (role, want) in WIRE_NAMES {
            assert_eq!(
                role.as_str(),
                want,
                "{role:?}.as_str() drifted from the wire name its doc promises to match"
            );
        }
    }

    /// The containment ladder, per class. `Role::containment` is the security
    /// spine of #222 — a workflow file selects a class and can never select a
    /// tier — so the mapping is asserted here as a table rather than left to
    /// whichever spawn-path test happens to observe a deny flag.
    ///
    /// The `Solo` row is the one worth naming: it is `None`, and that is a
    /// statement about reachability (a solo pane never traverses a loomux
    /// spawn) rather than a grant. If `Solo` ever *does* reach a spawn path,
    /// this row is where the decision has to be made deliberately.
    #[test]
    fn every_role_variant_maps_to_its_documented_containment_tier() {
        let want = [
            (Role::Orchestrator, Containment::None),
            (Role::Worker, Containment::None),
            (Role::Reviewer, Containment::NoEdits),
            (Role::Planner, Containment::ReadOnly),
            (Role::Solo, Containment::None),
        ];
        for (role, tier) in want {
            assert_eq!(role.containment(), tier, "{role:?} changed containment tier");
        }
        // And the ladder the gate compares on is ordered, since `cli_can_host`
        // is one `rank()` comparison and nothing else.
        assert!(Containment::None.rank() < Containment::NoEdits.rank());
        assert!(Containment::NoEdits.rank() < Containment::ReadOnly.rank());
    }
}
